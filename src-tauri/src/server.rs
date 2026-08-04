//! Servidor HTTP + WebSocket que escucha en `localhost:17173`.
//!
//! Endpoints:
//!   - `GET  /health`        — estado del agente (la ventana de estado y el
//!                              navegador lo usan).
//!   - `GET  /status`        — estado detallado para la ventana del agente.
//!   - `POST /jobs/start`    — la pestaña web dispara un job.
//!   - `POST /open/:target`  — abre soporte/descargas en el navegador (lista
//!                              cerrada de destinos, nunca una URL de fuera).
//!   - `WS   /ws?jobId=...`  — la pestaña se suscribe al stream de eventos.
//!
//! CORS estricto: solo `amautum.com` y `localhost:3000`. Otros orígenes
//! reciben 403 antes de tocar el handler.

use std::sync::atomic::Ordering;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{HeaderValue, Method, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;
use tokio::sync::broadcast::error::RecvError;
use tower_http::cors::CorsLayer;

use crate::config;
use crate::models;
use crate::pending;
use crate::pipeline::{self, AppState};
use crate::types::{AgentEvent, AgentTriggerPayload};

#[derive(Clone)]
struct ServerCtx {
    app: AppHandle,
    state: AppState,
}

pub async fn run(app: AppHandle, state: AppState) -> anyhow::Result<()> {
    let origins: Vec<HeaderValue> = config::ALLOWED_ORIGINS
        .iter()
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect();

    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers(tower_http::cors::Any)
        .allow_origin(origins);

    let ctx = ServerCtx { app, state };

    let router = Router::new()
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/diagnostics", get(diagnostics))
        .route("/files/pick", post(files_pick))
        .route("/jobs/start", post(jobs_start))
        .route("/jobs/:id/retry", post(jobs_retry))
        .route("/open/:target", post(open_target))
        .route("/update/install", post(update_install))
        .route("/ws", get(ws_handler))
        .with_state(ctx)
        .layer(cors);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], config::SERVER_PORT));
    tracing::info!(?addr, "Servidor del agente arrancado");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router.into_make_service()).await?;
    Ok(())
}

async fn health(State(ctx): State<ServerCtx>) -> Json<serde_json::Value> {
    let jobs = ctx.state.running.load(Ordering::SeqCst);
    let dependencies_ok = ctx.state.dependencies_ok.load(Ordering::SeqCst);
    // Jobs cuya acta quedó guardada en disco esperando subir (sin conexión).
    let pending_uploads = pending::list_job_ids(&ctx.app).await;
    Json(json!({
        "ok": dependencies_ok,
        "version": config::version(),
        "busy": jobs > 0,
        "jobsRunning": jobs,
        "dependenciesOk": dependencies_ok,
        "dependenciesError": ctx.state.dependencies_error.lock().clone(),
        "pendingUploads": pending_uploads,
    }))
}

/// Estado completo para la VENTANA del agente: trabajos vivos, historial
/// reciente, actas pendientes y bitácora.
///
/// La ventana antes solo sabía "hay N trabajos". Con un trabajo de seis horas
/// eso decía exactamente lo mismo a los dos minutos que a las cinco horas, y no
/// había en toda la interfaz un lugar donde apareciera un error.
///
/// Vive en el mismo servidor local que ya usa el navegador, así que la ventana
/// no necesita IPC con Rust: una sola fuente de verdad del runtime.
async fn status(State(ctx): State<ServerCtx>) -> Json<serde_json::Value> {
    Json(json!({
        "version": config::version(),
        "port": config::SERVER_PORT,
        "dependenciesOk": ctx.state.dependencies_ok.load(Ordering::SeqCst),
        "dependenciesError": ctx.state.dependencies_error.lock().clone(),
        "activeJobs": ctx.state.active_jobs(),
        "jobs": ctx.state.job_snapshot(),
        "pendingUploads": pending::list_details(&ctx.app).await,
        "logs": ctx.state.recent_logs(),
        "update": ctx.state.update.lock().clone(),
        "nowMs": crate::types::now_ms(),
    }))
}

/// Instala la actualización ahora, sin pasar por el navegador.
///
/// Normalmente no hace falta: el agente se actualiza solo cuando queda ocioso.
/// Este botón es para quien no quiere esperar al siguiente ciclo. Si hay trabajo
/// en curso responde que no, en vez de reiniciar y destruirlo.
///
/// Si el actualizador no está disponible (una compilación sin llave de firma, o
/// sin red), devuelve el error y la ventana ofrece el camino de siempre:
/// descargar el instalador del navegador.
async fn update_install(
    State(ctx): State<ServerCtx>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    match crate::updates::install_update(&ctx.app, &ctx.state, false).await {
        // Si el reinicio ocurre, esto no se llega a responder nunca.
        Ok(()) => Ok(Json(json!({ "ok": true }))),
        Err(err) => Err((
            StatusCode::CONFLICT,
            Json(json!({ "error": err, "fallback": crate::updates::releases_page() })),
        )),
    }
}

/// Abre en el navegador del sistema uno de los destinos que el agente conoce.
///
/// El parámetro es un NOMBRE de una lista cerrada, no una URL. La ventana pide
/// "abre soporte" y el agente decide a dónde. Un endpoint local que abriera una
/// URL arbitraria sería una puerta para que cualquier página que logre hablar
/// con este puerto lance enlaces en el equipo de la persona.
async fn open_target(
    State(ctx): State<ServerCtx>,
    Path(target): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    use tauri_plugin_opener::OpenerExt;

    let url = match target.as_str() {
        // El último fallo viaja con el enlace: quien pide ayuda no debería tener
        // que transcribir a mano un mensaje de error para que le respondan.
        "support" => config::support_url(ctx.state.last_error().as_deref()),
        "downloads" => config::downloads_url(),
        // Preferimos el enlace DIRECTO al instalador de esta plataforma; si
        // todavía no pudimos consultar la versión, caemos a la página de
        // releases (que siempre existe).
        "release" => ctx
            .state
            .update
            .lock()
            .as_ref()
            .map(|u| u.download_url.clone())
            .unwrap_or_else(crate::updates::releases_page),
        _ => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "Destino desconocido." })),
            ))
        }
    };

    match ctx.app.opener().open_url(url.clone(), None::<String>) {
        Ok(()) => Ok(Json(json!({ "ok": true, "url": url }))),
        // Que no se pueda abrir el navegador no es el fin: devolvemos la URL
        // para que la ventana la pueda mostrar y la persona la copie a mano.
        Err(err) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": format!("No pudimos abrir el navegador: {err}"),
                "url": url,
            })),
        )),
    }
}

/// La pestaña web pide reintentar la subida de un acta que quedó pendiente en
/// disco (botón "Reintentar"). Disparamos el reintento en background y
/// respondemos al instante; la web verá el resultado al poolear el listado.
async fn jobs_retry(
    State(ctx): State<ServerCtx>,
    Path(job_id): Path<String>,
) -> Json<serde_json::Value> {
    let app = ctx.app.clone();
    let ws = ctx.state.ws_hub.clone();
    let id = job_id.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(transcript_id) = pending::retry_one(&app, &id).await {
            ws.publish(AgentEvent::Completed {
                job_id: id,
                transcript_id,
            });
        }
    });
    Json(json!({ "ok": true, "jobId": job_id, "retrying": true }))
}

/// Diagnóstico detallado del estado del agente: si los binarios sidecar están
/// disponibles, qué modelos están descargados, y dónde viven los archivos.
/// Lo usamos para depurar problemas de bundling/permisos sin pedirle al
/// usuario que abra una terminal.
async fn diagnostics(State(ctx): State<ServerCtx>) -> Json<serde_json::Value> {
    let ffmpeg = models::probe_sidecar(&ctx.app, config::FFMPEG_SIDECAR).await;
    let whisper = models::probe_sidecar(&ctx.app, config::WHISPER_SIDECAR).await;
    let diarize = models::probe_sidecar(&ctx.app, config::DIARIZE_SIDECAR).await;
    let models_dir = models::models_dir(&ctx.app).await;
    let model_present = match &models_dir {
        Ok(dir) => models::first_available_model(dir).await,
        Err(_) => None,
    };
    Json(json!({
        "version": config::version(),
        "sidecars": {
            "ffmpeg": match ffmpeg {
                Ok(out) => json!({ "ok": true, "version": out }),
                Err(e) => json!({ "ok": false, "error": e }),
            },
            "whisperCli": match whisper {
                Ok(out) => json!({ "ok": true, "version": out }),
                Err(e) => json!({ "ok": false, "error": e }),
            },
            // Opcional: solo se usa cuando el job pide identificar interlocutores.
            // Que falte aquí no rompe la transcripción normal.
            "sherpaDiarize": match diarize {
                Ok(out) => json!({ "ok": true, "optional": true, "version": out }),
                Err(e) => json!({ "ok": false, "optional": true, "error": e }),
            },
        },
        "models": {
            "directory": match &models_dir {
                Ok(d) => d.to_string_lossy().to_string(),
                Err(e) => format!("(no resoluble: {e})"),
            },
            "downloaded": model_present,
        },
    }))
}

/// Abre el diálogo nativo de selección de archivo (NSOpenPanel en macOS,
/// IFileOpenDialog en Windows, GTK FileChooser en Linux) y devuelve la ruta
/// absoluta. La pestaña web NO puede leer rutas absolutas por seguridad del
/// navegador; este endpoint es el puente que les da una.
///
/// Filtros: por defecto restringe a extensiones de audio comunes; el usuario
/// puede pedir "todos" enviando `acceptAll: true`.
async fn files_pick(
    State(ctx): State<ServerCtx>,
    Json(opts): Json<PickOptions>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // tauri-plugin-dialog ofrece una API blocking que no es safe llamar desde
    // un runtime Tokio (bloquearía el reactor). La cruzamos con spawn_blocking
    // y un oneshot — el handler espera el resultado sin bloquear el reactor.
    let app = ctx.app.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();

    tauri::async_runtime::spawn_blocking(move || {
        let mut builder = app.dialog().file();
        builder = builder.set_title(opts.title.as_deref().unwrap_or("Elige el audio de la audiencia"));
        if !opts.accept_all {
            builder = builder.add_filter(
                "Audio",
                &["mp3", "m4a", "wav", "ogg", "opus", "flac", "aac", "mp4"],
            );
        }
        let result = builder.blocking_pick_file();
        let _ = tx.send(result);
    });

    let picked = match rx.await {
        Ok(p) => p,
        Err(_) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "El diálogo del agente se cerró sin respuesta." })),
            ))
        }
    };

    let Some(file_response) = picked else {
        return Ok(Json(json!({ "ok": true, "cancelled": true })));
    };

    let path_buf = match file_response.into_path() {
        Ok(p) => p,
        Err(err) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": format!("La ruta seleccionada no es un archivo local: {err}")
                })),
            ));
        }
    };

    let size = std::fs::metadata(&path_buf).ok().map(|m| m.len());
    let name = path_buf
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    Ok(Json(json!({
        "ok": true,
        "cancelled": false,
        "path": path_buf.to_string_lossy(),
        "name": name,
        "sizeBytes": size,
    })))
}

#[derive(Deserialize, Default)]
struct PickOptions {
    #[serde(default)]
    title: Option<String>,
    #[serde(default, rename = "acceptAll")]
    accept_all: bool,
}

async fn jobs_start(
    State(ctx): State<ServerCtx>,
    Json(payload): Json<AgentTriggerPayload>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if payload.job_id.is_empty() || payload.token.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "jobId y token son requeridos"})),
        ));
    }
    let audio_path = std::path::Path::new(&payload.audio_file_path);
    if !audio_path.exists() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "El archivo de audio no existe en esta máquina",
                "path": payload.audio_file_path,
            })),
        ));
    }

    // El navegador manda el tamaño que vio al elegir el archivo. Si en disco hay
    // otro, es que cambió entre medias: se movió a otra cosa, o se está copiando
    // todavía desde una unidad de red o un pendrive. Transcribir un archivo a
    // medio copiar produce un acta truncada que parece buena, y eso se descubre
    // leyéndola. Mejor pararlo aquí, que es donde se puede explicar.
    if let Some(expected) = payload.file_size {
        if let Ok(meta) = std::fs::metadata(audio_path) {
            if meta.len() != expected {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": "El archivo cambió de tamaño desde que lo elegiste. \
                                  Si lo estás copiando desde otra unidad, espera a que \
                                  termine y vuelve a intentarlo.",
                        "path": payload.audio_file_path,
                        "expectedBytes": expected,
                        "actualBytes": meta.len(),
                    })),
                ));
            }
        }
    }
    let job_id = payload.job_id.clone();
    pipeline::spawn_job(ctx.app, ctx.state, payload);
    Ok(Json(json!({
        "ok": true,
        "jobId": job_id,
        "ws": format!("ws://localhost:{}/ws?jobId={}", config::SERVER_PORT, job_id),
    })))
}

#[derive(Deserialize)]
struct WsQuery {
    #[serde(rename = "jobId")]
    job_id: String,
}

async fn ws_handler(
    State(ctx): State<ServerCtx>,
    Query(q): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_session(socket, ctx.state, q.job_id))
}

async fn ws_session(socket: WebSocket, state: AppState, job_id: String) {
    let (mut tx, mut rx) = socket.split();
    let (mut events, last_event) = state.ws_hub.subscribe(&job_id);

    // Replay del último evento publicado en este job (si lo hubo). Cierra la
    // race entre `POST /jobs/start` y la conexión WS: sin esto, los primeros
    // 1-3 eventos del pipeline se perdían y el cliente quedaba en silencio.
    if let Some(ev) = last_event {
        if let Ok(payload) = serde_json::to_string(&ev) {
            if tx.send(Message::Text(payload)).await.is_err() {
                return;
            }
        }
    }

    // Tarea de ping para evitar que un proxy local mate la conexión idle.
    let ping = tokio::spawn(async move {
        // Estructura mínima: solo lectura para detectar `Close` del cliente.
        while let Some(msg) = rx.next().await {
            if let Ok(Message::Close(_)) = msg {
                break;
            }
        }
    });

    loop {
        match events.recv().await {
            Ok(event) => {
                let payload = match serde_json::to_string(&event) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                if tx.send(Message::Text(payload)).await.is_err() {
                    break;
                }
            }
            Err(RecvError::Lagged(_)) => {
                // El cliente se quedó atrás. Continuamos con los más nuevos.
                continue;
            }
            Err(RecvError::Closed) => break,
        }
    }
    let _ = ping.await;
}
