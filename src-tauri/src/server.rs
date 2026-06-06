//! Servidor HTTP + WebSocket que escucha en `localhost:17173`.
//!
//! Endpoints:
//!   - `GET  /health`        — estado del agente (la ventana de estado y el
//!                              navegador lo usan).
//!   - `POST /jobs/start`    — la pestaña web dispara un job.
//!   - `WS   /ws?jobId=...`  — la pestaña se suscribe al stream de eventos.
//!
//! CORS estricto: solo `amautum.com` y `localhost:3000`. Otros orígenes
//! reciben 403 antes de tocar el handler.

use std::sync::atomic::Ordering;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
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
use crate::pipeline::{self, AppState};
use crate::types::AgentTriggerPayload;

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
        .route("/diagnostics", get(diagnostics))
        .route("/files/pick", post(files_pick))
        .route("/jobs/start", post(jobs_start))
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
    Json(json!({
        "ok": dependencies_ok,
        "version": config::version(),
        "busy": jobs > 0,
        "jobsRunning": jobs,
        "dependenciesOk": dependencies_ok,
    }))
}

/// Diagnóstico detallado del estado del agente: si los binarios sidecar están
/// disponibles, qué modelos están descargados, y dónde viven los archivos.
/// Lo usamos para depurar problemas de bundling/permisos sin pedirle al
/// usuario que abra una terminal.
async fn diagnostics(State(ctx): State<ServerCtx>) -> Json<serde_json::Value> {
    let ffmpeg = models::probe_sidecar(&ctx.app, config::FFMPEG_SIDECAR).await;
    let whisper = models::probe_sidecar(&ctx.app, config::WHISPER_SIDECAR).await;
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
    if !std::path::Path::new(&payload.audio_file_path).exists() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "El archivo de audio no existe en esta máquina",
                "path": payload.audio_file_path,
            })),
        ));
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
