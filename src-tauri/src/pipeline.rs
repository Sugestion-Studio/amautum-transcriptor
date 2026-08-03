//! Orquestación de un trabajo de transcripción de punta a punta:
//!
//!   1. emite `queued`
//!   2. ffmpeg → WAV 16 kHz mono
//!   3. whisper.cpp → segmentos + texto (con progreso parcheando WS y cloud)
//!   4. POST de transcript al backend de Amautum
//!   5. emite `completed`
//!
//! Cualquier error en una etapa produce un `Failed` con el `FailStage` correcto
//! y reporta al backend vía `fail`. Eso garantiza que el job nunca queda en
//! limbo: o termina en `completado` o en `fallado`.
//!
//! Dos invariantes que este módulo defiende, y que antes no se cumplían:
//!
//!   **Un trabajo vivo lo demuestra.** El motor puede tardar veinte minutos en
//!   subir un punto de porcentaje en un equipo sin GPU. El agente late cada
//!   pocos segundos con lo que sabe, así que "lento" nunca se ve como "muerto".
//!
//!   **Un error siempre trae una salida.** Cada fallo lleva un `hint` en
//!   castellano con lo que la persona puede hacer a continuación.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use tauri::AppHandle;
use tokio::sync::Mutex;

use crate::diarize;
use crate::ffmpeg;
use crate::models;
use crate::pending;
use crate::types::{
    now_ms, AgentEvent, AgentTriggerPayload, FailStage, FailUpload, Hardware, JobStage, JobStatus,
    LogEntry, ProgressUpload, Segment, TranscriptUpload,
};
use crate::whisper;
use crate::ws_hub::WsHub;
use crate::{amautum, config};

#[derive(Clone)]
pub struct AppState {
    pub ws_hub: WsHub,
    pub running: Arc<AtomicUsize>,
    /// `true` cuando los sidecars (ffmpeg + whisper-cli) respondieron correctamente
    /// al `--version` durante el arranque. Si es `false`, /health devuelve
    /// `ok: false` para que la web alerte sin esperar a que el operador
    /// intente transcribir y choque con el error.
    pub dependencies_ok: Arc<AtomicBool>,
    /// Detalle del chequeo de sidecars, para poder decir CUÁL falló y por qué.
    /// "Faltan componentes" no le sirve a nadie; "falta el Visual C++
    /// Redistributable" sí.
    pub dependencies_error: Arc<parking_lot::Mutex<Option<String>>>,
    /// Trabajos vivos y recién terminados. Es lo que la ventana del agente
    /// pinta. Sin esto, abrir la ventana a mitad de una audiencia no contaba
    /// nada: solo decía "Procesando" y un contador de trabajos.
    pub jobs: Arc<DashMap<String, JobStatus>>,
    /// Orden de llegada de los trabajos, para poder podar el historial.
    pub job_order: Arc<parking_lot::Mutex<Vec<String>>>,
    /// Bitácora reciente: lo que la ventana muestra y el botón "Copiar
    /// diagnóstico" manda a soporte.
    pub logs: Arc<parking_lot::Mutex<VecDeque<LogEntry>>>,
    /// Última consulta de versión publicada. `None` mientras no se haya podido
    /// preguntar (sin red al arrancar, por ejemplo).
    pub update: Arc<parking_lot::Mutex<Option<crate::updates::UpdateInfo>>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            ws_hub: WsHub::new(),
            running: Arc::new(AtomicUsize::new(0)),
            // Optimista: defaulteamos a `true`. El probe corre en background
            // al arrancar y solo flippea a `false` si confirma un problema.
            // Sin esto el navegador veía `dependenciesOk: false` durante el
            // primer segundo y pintaba "Agente con componentes faltantes"
            // aunque todo estuviera bien.
            dependencies_ok: Arc::new(AtomicBool::new(true)),
            dependencies_error: Arc::new(parking_lot::Mutex::new(None)),
            jobs: Arc::new(DashMap::new()),
            job_order: Arc::new(parking_lot::Mutex::new(Vec::new())),
            logs: Arc::new(parking_lot::Mutex::new(VecDeque::new())),
            update: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    /// Anota una línea en la bitácora. Es lo primero que mira soporte, así que
    /// el texto va en castellano y describe hechos, no símbolos.
    pub fn log(&self, level: &'static str, text: impl Into<String>) {
        let text = text.into();
        match level {
            "error" => tracing::error!("{text}"),
            "warn" => tracing::warn!("{text}"),
            _ => tracing::info!("{text}"),
        }
        let mut logs = self.logs.lock();
        logs.push_back(LogEntry {
            at_ms: now_ms(),
            level,
            text,
        });
        while logs.len() > config::LOG_BUFFER_LINES {
            logs.pop_front();
        }
    }

    pub fn recent_logs(&self) -> Vec<LogEntry> {
        self.logs.lock().iter().cloned().collect()
    }

    /// Registra un trabajo nuevo y poda el historial de los ya terminados.
    pub fn register_job(&self, status: JobStatus) {
        let job_id = status.job_id.clone();
        self.jobs.insert(job_id.clone(), status);
        let mut order = self.job_order.lock();
        order.retain(|id| id != &job_id);
        order.push(job_id);

        // Podamos SOLO terminados: un trabajo vivo nunca se cae del listado por
        // antigüedad, por largo que sea.
        let mut finished: Vec<String> = order
            .iter()
            .filter(|id| {
                self.jobs
                    .get(*id)
                    .map(|j| j.value().stage.is_terminal())
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        while finished.len() > config::JOB_HISTORY_LIMIT {
            let oldest = finished.remove(0);
            self.jobs.remove(&oldest);
            order.retain(|id| id != &oldest);
        }
    }

    /// Muta el estado de un trabajo. `updated_at_ms` se refresca siempre: es el
    /// reloj que la ventana usa para decir hace cuánto se supo algo.
    pub fn update_job(&self, job_id: &str, f: impl FnOnce(&mut JobStatus)) {
        if let Some(mut entry) = self.jobs.get_mut(job_id) {
            f(entry.value_mut());
            entry.value_mut().updated_at_ms = now_ms();
        }
    }

    /// Foto de los trabajos: los vivos primero, del más reciente al más viejo.
    pub fn job_snapshot(&self) -> Vec<JobStatus> {
        let order = self.job_order.lock().clone();
        let mut out: Vec<JobStatus> = order
            .iter()
            .rev()
            .filter_map(|id| self.jobs.get(id).map(|j| j.value().clone()))
            .collect();
        out.sort_by_key(|j| j.stage.is_terminal());
        out
    }

    /// Último fallo del que tenemos noticia, para adjuntarlo al ticket de
    /// soporte. Primero un componente roto —bloquea todo, así que es lo que hay
    /// que contar— y si no, el error del trabajo más reciente que falló.
    pub fn last_error(&self) -> Option<String> {
        if let Some(deps) = self.dependencies_error.lock().clone() {
            return Some(format!("Componentes: {deps}"));
        }
        let order = self.job_order.lock().clone();
        for id in order.iter().rev() {
            if let Some(job) = self.jobs.get(id) {
                if let Some(err) = job.value().error.clone() {
                    return Some(format!("{} — {err}", job.value().file_name));
                }
            }
        }
        None
    }

    pub fn active_jobs(&self) -> usize {
        self.jobs
            .iter()
            .filter(|j| !j.value().stage.is_terminal())
            .count()
    }
}

/// Estado compartido del progreso de un trabajo. Lo tocan tres actores: el
/// callback (síncrono) que lee la salida del motor, el latido (asíncrono) y el
/// que decide cuándo postear al cloud. Mutex síncrono a propósito: nadie hace
/// `await` con el candado tomado.
struct ProgressState {
    percent: u8,
    note: Option<String>,
    /// Última vez que el MOTOR dijo algo. Distinto del latido del agente: es la
    /// diferencia entre "seguimos vivos" y "el motor sigue trabajando".
    engine_seen_at: Instant,
    started_at: Instant,
    last_cloud_push_at: Instant,
    last_cloud_percent: u8,
}

impl ProgressState {
    /// ETA MEDIDA, no adivinada.
    ///
    /// La versión anterior asumía que transcribir tardaba el doble que el audio
    /// (`duración × 2`), fijo. Esa constante venía de medir en un Mac con Metal.
    /// En un Windows sin GPU el mismo trabajo tarda tres o cuatro veces más, así
    /// que el panel prometía "faltan 40 minutos" durante horas — y la persona,
    /// con razón, concluía que estaba colgado.
    ///
    /// Ahora extrapolamos del ritmo real de ESTA corrida en ESTE equipo.
    fn eta_seconds(&self) -> Option<u64> {
        if self.percent < 2 || self.percent >= 100 {
            return None;
        }
        let elapsed = self.started_at.elapsed().as_secs_f64();
        if elapsed < 5.0 {
            return None;
        }
        let per_point = elapsed / self.percent as f64;
        Some((per_point * (100 - self.percent) as f64).max(0.0) as u64)
    }
}

/// Lanza el job en una tarea aparte. La función vuelve inmediatamente — el
/// HTTP handler responde 202 sin esperar a que termine.
pub fn spawn_job(app: AppHandle, state: AppState, payload: AgentTriggerPayload) {
    state.running.fetch_add(1, Ordering::SeqCst);

    let file_name = std::path::Path::new(&payload.audio_file_path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| payload.audio_file_path.clone());

    let now = now_ms();
    state.register_job(JobStatus {
        job_id: payload.job_id.clone(),
        file_name: file_name.clone(),
        model: payload.model.as_str().to_string(),
        hardware: None,
        stage: JobStage::Queued,
        progress: 0,
        eta_seconds: None,
        audio_seconds: None,
        note: None,
        started_at_ms: now,
        engine_seen_at_ms: now,
        updated_at_ms: now,
        error: None,
        hint: None,
    });
    state.log(
        "info",
        format!("Trabajo recibido: {file_name} (modelo {})", payload.model.as_str()),
    );

    let state2 = state.clone();
    tauri::async_runtime::spawn(async move {
        let job_id = payload.job_id.clone();
        let result = run_job(&app, state.clone(), payload).await;
        if let Err(err) = result {
            tracing::error!(job_id, ?err, "Pipeline falló");
        }
        state2.running.fetch_sub(1, Ordering::SeqCst);
        state2.ws_hub.close(&job_id);
    });
}

#[derive(thiserror::Error, Debug)]
pub enum PipelineError {
    #[error("ffmpeg: {0}")]
    Ffmpeg(#[from] ffmpeg::FfmpegError),
    #[error("whisper: {0}")]
    Whisper(#[from] whisper::WhisperError),
    #[error("diarización: {0}")]
    Diarize(#[from] diarize::DiarizeError),
    #[error("modelo: {0}")]
    Model(#[from] models::ModelError),
    #[error("amautum: {0}")]
    Amautum(#[from] amautum::AmautumError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl PipelineError {
    fn stage(&self) -> FailStage {
        match self {
            PipelineError::Ffmpeg(_) => FailStage::Ffmpeg,
            PipelineError::Model(_) => FailStage::ModelLoad,
            PipelineError::Whisper(whisper::WhisperError::ModelMissing(_)) => FailStage::ModelLoad,
            PipelineError::Whisper(_) => FailStage::Inference,
            PipelineError::Diarize(_) => FailStage::Diarization,
            PipelineError::Amautum(_) => FailStage::Upload,
            PipelineError::Io(_) => FailStage::Unknown,
        }
    }

    /// Qué puede hacer la persona. Un error sin salida es un callejón: la queja
    /// que originó este trabajo fue literalmente "no hay indicios de qué hacer".
    fn hint(&self) -> &'static str {
        match self {
            PipelineError::Ffmpeg(_) => {
                "Revisa que el archivo se reproduzca en tu reproductor habitual. Si es un video, \
                 prueba exportando solo el audio a MP3 y vuelve a intentar."
            }
            PipelineError::Model(_) => {
                "No pudimos preparar el motor de transcripción. Revisa tu conexión a internet y el \
                 espacio libre en disco (el modelo `medium` ocupa 1,5 GB) y reintenta."
            }
            PipelineError::Whisper(whisper::WhisperError::Stalled(_)) => {
                "Desactiva la suspensión automática del equipo, autoriza «Amautum Transcriptor» en \
                 tu antivirus y reintenta. Con un modelo más pequeño (base o small) el trabajo \
                 termina en una fracción del tiempo."
            }
            PipelineError::Whisper(_) => {
                "Reintenta el trabajo. Si vuelve a fallar, prueba con un modelo más pequeño (base o \
                 small) y copia el diagnóstico desde la ventana del agente."
            }
            PipelineError::Diarize(_) => {
                "Vuelve a lanzar el trabajo sin identificación de interlocutores: la transcripción \
                 sale igual, solo sin las etiquetas de orador."
            }
            PipelineError::Amautum(_) => {
                "Revisa tu conexión a internet y que tu sesión de Amautum siga activa. Si el acta \
                 quedó guardada en este equipo, la ventana del agente te deja reintentar la subida."
            }
            PipelineError::Io(_) => {
                "Revisa el espacio libre en disco y los permisos de la carpeta temporal, y \
                 reintenta."
            }
        }
    }
}

async fn run_job(
    app: &AppHandle,
    state: AppState,
    payload: AgentTriggerPayload,
) -> Result<(), ()> {
    let job_id = payload.job_id.clone();
    state.ws_hub.publish(AgentEvent::Queued {
        job_id: job_id.clone(),
    });

    // Sandbox de trabajo: directorio temporal por job que se borra al final.
    // tempfile::TempDir destruye al drop, así que lo retenemos hasta el final.
    let tempdir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(err) => {
            return fail(
                &state,
                &payload,
                FailStage::Unknown,
                format!("No pudimos crear la carpeta temporal de trabajo: {err}"),
                "Revisa el espacio libre en disco y los permisos de la carpeta temporal del \
                 sistema, y reintenta.",
            )
            .await;
        }
    };

    let res = run_inner(app, &state, &payload, tempdir.path()).await;
    match res {
        Ok(JobOutcome::Completed(transcript_id)) => {
            state.update_job(&job_id, |j| {
                j.stage = JobStage::Completed;
                j.progress = 100;
                j.eta_seconds = None;
                j.note = None;
                j.error = None;
                j.hint = None;
            });
            state.log("info", "Trabajo terminado y entregado a Amautum.");
            state.ws_hub.publish(AgentEvent::Completed {
                job_id: payload.job_id.clone(),
                transcript_id,
            });
            Ok(())
        }
        Ok(JobOutcome::UploadPending) => {
            // El acta se guardó en disco y se reintentará sola. NO reportamos
            // fallo al cloud (igual no hay conexión) ni perdemos el trabajo: el
            // cloud queda en "procesando" hasta que el reintento la suba.
            state.update_job(&job_id, |j| {
                j.stage = JobStage::UploadPending;
                j.progress = 100;
                j.eta_seconds = None;
                j.note = None;
                j.hint = Some(
                    "La transcripción está a salvo en este equipo. Se sube sola en cuanto vuelva \
                     la conexión; también puedes forzar el reintento desde aquí."
                        .to_string(),
                );
            });
            state.log(
                "warn",
                "El acta terminó pero no se pudo subir. Guardada en este equipo para reintentar.",
            );
            state.ws_hub.publish(AgentEvent::UploadPending {
                job_id: payload.job_id.clone(),
            });
            Ok(())
        }
        Err(err) => {
            // No replicamos el último progreso aquí: el cloud ya lo tiene de
            // los POSTs de `progress` que disparó la callback de whisper.
            let stage = err.stage();
            let hint = err.hint();
            fail(&state, &payload, stage, err.to_string(), hint).await
        }
    }
}

/// Resultado de procesar un job de punta a punta.
enum JobOutcome {
    /// El acta se subió al cloud. Trae el `transcript_id` que devolvió el backend.
    Completed(String),
    /// La transcripción terminó pero la subida falló por red; el acta quedó
    /// guardada en disco (cola de pendientes) para reintentar más tarde.
    UploadPending,
}

async fn run_inner(
    app: &AppHandle,
    state: &AppState,
    payload: &AgentTriggerPayload,
    tempdir: &std::path::Path,
) -> Result<JobOutcome, PipelineError> {
    let source = PathBuf::from(&payload.audio_file_path);

    // ── 1) Pre-procesamiento ────────────────────────────────────────────────
    state.update_job(&payload.job_id, |j| j.stage = JobStage::Preprocess);
    state.log("info", "Convirtiendo el audio a WAV 16 kHz mono.");
    state.ws_hub.publish(AgentEvent::Preprocess {
        job_id: payload.job_id.clone(),
        stage: "ffmpeg".into(),
    });
    let pre = ffmpeg::convert_to_whisper_wav(app, &source, tempdir).await?;

    let hardware = Hardware::detect();
    state.update_job(&payload.job_id, |j| {
        j.hardware = Some(hardware_str(hardware).to_string());
        j.audio_seconds = if pre.duration_seconds > 0.0 {
            Some(pre.duration_seconds)
        } else {
            None
        };
    });
    state.ws_hub.publish(AgentEvent::Started {
        job_id: payload.job_id.clone(),
        hardware,
        model: payload.model.as_str().to_string(),
    });

    // Informa al cloud que arrancamos a procesar (estado `procesando`). No
    // bloqueamos si falla — el WS local sigue siendo la fuente de verdad para
    // la pestaña; este POST es para que el panel sobreviva un refresh.
    let _ = amautum::post_progress(
        &payload.callbacks.progress,
        &payload.token,
        &ProgressUpload {
            progreso: 0,
            tiempo_estimado: None,
            hardware: Some(hardware_str(hardware)),
            agente_version: Some(config::version().to_string()),
            estado: Some("procesando"),
        },
    )
    .await;

    // ── 2) Asegurar modelo en disco ────────────────────────────────────────
    // Si es la primera vez con este modelo, lo bajamos de HuggingFace con
    // progreso visible. Siguientes corridas del mismo modelo lo reutilizan.
    state.update_job(&payload.job_id, |j| j.stage = JobStage::DownloadingModel);
    let ws_hub_arc = Arc::new(state.ws_hub.clone());
    let model_path = models::ensure_model(
        app,
        payload.model,
        &payload.job_id,
        ws_hub_arc.clone(),
    )
    .await?;
    let models_dir = model_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    // ── 3) Inferencia ──────────────────────────────────────────────────────
    let job_id = payload.job_id.clone();
    let duration_secs = pre.duration_seconds;
    let hw = hardware_str(hardware);

    state.update_job(&job_id, |j| j.stage = JobStage::Transcribing);
    state.log(
        "info",
        format!(
            "Transcribiendo con el modelo {} en {} — {} de audio",
            payload.model.as_str(),
            hw,
            fmt_duration(duration_secs)
        ),
    );

    let progress = Arc::new(parking_lot::Mutex::new(ProgressState {
        percent: 0,
        note: None,
        engine_seen_at: Instant::now(),
        started_at: Instant::now(),
        // Arrancamos "recién posteado": acabamos de mandar el estado
        // `procesando` unas líneas más arriba.
        last_cloud_push_at: Instant::now(),
        last_cloud_percent: 0,
    }));

    let on_progress: Arc<Mutex<whisper::ProgressCallback>> = Arc::new(Mutex::new(Box::new({
        let job_id = job_id.clone();
        let ws = state.ws_hub.clone();
        let state = state.clone();
        let progress = progress.clone();
        let callback = payload.callbacks.progress.clone();
        let token = payload.token.clone();
        move |tick: whisper::ProgressTick| {
            let (percent, note, eta, should_push) = {
                let mut p = progress.lock();
                // Monotónico: el mapeo por tramos puede retroceder un punto al
                // cruzar una frontera, y una barra que retrocede alarma.
                p.percent = tick.percent.max(p.percent);
                p.note = tick.note.clone();
                p.engine_seen_at = Instant::now();
                let eta = p.eta_seconds();
                // Disparamos al cloud por salto de 5% O por techo de tiempo. El
                // techo importa: con 5% cada veinte minutos, el panel del job
                // parecía abandonado y el barrido de trabajos huérfanos del
                // backend lo habría dado por muerto.
                let should_push = p.percent >= p.last_cloud_percent.saturating_add(5)
                    || p.last_cloud_push_at.elapsed().as_secs()
                        >= config::CLOUD_PROGRESS_MAX_INTERVAL_SECS
                    || p.percent == 100;
                if should_push {
                    p.last_cloud_percent = p.percent;
                    p.last_cloud_push_at = Instant::now();
                }
                (p.percent, p.note.clone(), eta, should_push)
            };

            state.update_job(&job_id, |j| {
                j.progress = percent;
                j.eta_seconds = eta;
                j.note = note.clone();
                j.engine_seen_at_ms = now_ms();
            });
            ws.publish(AgentEvent::Progress {
                job_id: job_id.clone(),
                progress: percent,
                eta_seconds: eta,
                last_segment: None,
                note: note.clone(),
            });

            if should_push {
                let callback = callback.clone();
                let token = token.clone();
                let version = config::version().to_string();
                tauri::async_runtime::spawn(async move {
                    let _ = amautum::post_progress(
                        &callback,
                        &token,
                        &ProgressUpload {
                            progreso: percent,
                            tiempo_estimado: eta.map(fmt_eta),
                            hardware: Some(hw),
                            agente_version: Some(version),
                            estado: None,
                        },
                    )
                    .await;
                });
            }
        }
    })));

    // ── Latido ─────────────────────────────────────────────────────────────
    // Republica lo último que sabemos cada pocos segundos. La pestaña web marca
    // "atascado" a los 30 s de silencio; el motor en CPU puede tardar veinte
    // minutos entre reportes. Sin este latido, un trabajo perfectamente sano se
    // veía roto durante horas — que es exactamente lo que reportaban.
    let heartbeat = {
        let job_id = job_id.clone();
        let ws = state.ws_hub.clone();
        let state = state.clone();
        let progress = progress.clone();
        let callback = payload.callbacks.progress.clone();
        let token = payload.token.clone();
        tauri::async_runtime::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(
                config::HEARTBEAT_INTERVAL_SECS,
            ));
            ticker.tick().await; // el primero dispara de inmediato; lo saltamos
            loop {
                ticker.tick().await;
                let (percent, note, eta, silent_secs, push_cloud) = {
                    let mut p = progress.lock();
                    let eta = p.eta_seconds();
                    let silent = p.engine_seen_at.elapsed().as_secs();
                    let push = p.last_cloud_push_at.elapsed().as_secs()
                        >= config::CLOUD_PROGRESS_MAX_INTERVAL_SECS;
                    if push {
                        p.last_cloud_push_at = Instant::now();
                    }
                    (p.percent, p.note.clone(), eta, silent, push)
                };
                // La nota dice cuánto lleva el motor sin reportar. Es lo que
                // convierte "no pasa nada" en "sigue trabajando; reporta cada 5%
                // y en este equipo cada 5% son ~18 minutos".
                let note = match (note, silent_secs >= 60) {
                    (Some(n), true) => Some(format!(
                        "{n} · el motor no reporta desde hace {}",
                        fmt_duration(silent_secs as f64)
                    )),
                    (Some(n), false) => Some(n),
                    (None, true) => Some(format!(
                        "El motor sigue trabajando; reporta cada 5% y lleva {} sin reportar",
                        fmt_duration(silent_secs as f64)
                    )),
                    (None, false) => None,
                };
                state.update_job(&job_id, |j| {
                    j.eta_seconds = eta;
                    j.note = note.clone();
                });
                ws.publish(AgentEvent::Progress {
                    job_id: job_id.clone(),
                    progress: percent,
                    eta_seconds: eta,
                    last_segment: None,
                    note,
                });
                if push_cloud {
                    let _ = amautum::post_progress(
                        &callback,
                        &token,
                        &ProgressUpload {
                            progreso: percent,
                            tiempo_estimado: eta.map(fmt_eta),
                            hardware: Some(hw),
                            agente_version: Some(config::version().to_string()),
                            estado: None,
                        },
                    )
                    .await;
                }
            }
        })
    };

    // Para audios largos el `transcribe_chunked` divide el WAV internamente y
    // libera RAM entre chunks. Para audios cortos cae a `transcribe` directo
    // sin overhead. Esta es la estrategia por defecto desde v0.1.6 — evita
    // los crasheos por OOM que afectaban a usuarios Windows con audios > 1 h
    // incluso con modelos pequeños.
    let run = whisper::transcribe_chunked(
        app,
        &pre.wav_path,
        payload.model,
        &payload.language,
        &models_dir,
        duration_secs,
        on_progress,
    )
    .await;
    // El latido se apaga pase lo que pase: si el motor falló, seguir publicando
    // "sigue trabajando" sería mentir.
    heartbeat.abort();
    let mut run = run?;

    // ── 4) Diarización (opcional) ───────────────────────────────────────────
    // Si el operador pidió identificar interlocutores, corremos sherpa-onnx
    // sobre el MISMO WAV mono 16 kHz que usó whisper y etiquetamos cada
    // segmento con su orador. Se hace aquí, no en whisper, porque whisper.cpp
    // no diariza audio mono — es un paso aparte con sus propios modelos ONNX.
    let mut speakers: Option<Vec<String>> = None;
    if payload.diarize {
        state.update_job(&payload.job_id, |j| {
            j.stage = JobStage::Diarizing;
            j.note = Some("Identificando quién habla en cada tramo".to_string());
        });
        state.log("info", "Transcripción lista — identificando interlocutores.");
        state.ws_hub.publish(AgentEvent::Diarizing {
            job_id: payload.job_id.clone(),
        });
        // OPCIONAL Y NO-FATAL: si la diarización falla (red al bajar el modelo
        // ONNX, sidecar ausente, audio con mucho ruido…) NO perdemos el acta ya
        // transcrita. Degradamos a "sin oradores" y seguimos al upload. Antes un
        // `?` aquí tiraba el job entero y el usuario perdía toda la transcripción
        // después de esperar a que whisper terminara.
        match run_diarization(app, &pre.wav_path, payload.num_speakers, &mut run.segments).await {
            Ok(result) => {
                tracing::info!(
                    labeled = result.labeled_segments,
                    speakers = result.speakers.len(),
                    "Diarización completada"
                );
                if !result.speakers.is_empty() {
                    speakers = Some(result.speakers);
                }
            }
            Err(err) => {
                tracing::warn!(
                    ?err,
                    "Diarización falló — se sube la transcripción SIN oradores (no se pierde el acta)"
                );
            }
        }
    }

    // ── 5) Upload al cloud ─────────────────────────────────────────────────
    state.update_job(&payload.job_id, |j| {
        j.stage = JobStage::Uploading;
        j.progress = 100;
        j.eta_seconds = None;
        j.note = None;
    });
    state.ws_hub.publish(AgentEvent::Uploading {
        job_id: payload.job_id.clone(),
    });

    let upload = TranscriptUpload {
        language: run.language,
        model: run.model_used,
        duration_seconds: pre.duration_seconds,
        full_text: run.full_text,
        segments: run.segments,
        speakers,
        summary: None,
    };
    // El upload es el ÚNICO paso que realmente necesita internet: la
    // transcripción ya está hecha en local. Reintentamos ante caídas de red o
    // 5xx (backoff 2/4/8/16 s). Si los agotamos, en vez de perder el acta la
    // GUARDAMOS en disco para reintentar al volver la conexión.
    //
    // Un 4xx (token vencido, payload inválido) SÍ es fallo real —esperando no se
    // arregla— pero igual guardamos el acta antes de reportarlo. La regla es una
    // sola: **el texto transcrito nunca se descarta**, ni siquiera cuando ya no
    // hay forma automática de entregarlo.
    let mut attempt: u32 = 0;
    loop {
        match amautum::post_transcript(&payload.callbacks.transcript, &payload.token, &upload).await
        {
            Ok(resp) => return Ok(JobOutcome::Completed(resp.transcript_id)),
            Err(e) => {
                let retriable = matches!(&e, amautum::AmautumError::Network(_))
                    || matches!(&e, amautum::AmautumError::Rejected { status, .. } if *status >= 500);
                if !retriable {
                    // Un 4xx no se arregla esperando, pero eso NO es motivo para
                    // tirar el acta. El caso real: en un Windows sin GPU el
                    // trabajo tarda más que la vida del token, el POST vuelve
                    // 401 y hasta ahora la transcripción —seis horas de CPU— se
                    // descartaba con el proceso, sin dejar rastro. Ahora la
                    // escribimos en disco igual: el texto existe, y una persona
                    // puede recuperarlo aunque este token ya no sirva.
                    let parked = pending::save(
                        app,
                        pending::PendingUpload {
                            job_id: payload.job_id.clone(),
                            transcript_callback: payload.callbacks.transcript.clone(),
                            token: payload.token.clone(),
                            upload,
                            saved_at: 0,
                        },
                    )
                    .await;
                    match parked {
                        Ok(()) => state.log(
                            "error",
                            format!(
                                "Amautum rechazó el acta ({e}). La transcripción NO se perdió: \
                                 quedó guardada en este equipo."
                            ),
                        ),
                        Err(err) => state.log(
                            "error",
                            format!("Amautum rechazó el acta ({e}) y tampoco pudimos guardarla en disco: {err}"),
                        ),
                    }
                    return Err(e.into());
                }
                if attempt < 4 {
                    attempt += 1;
                    let secs = 2u64.pow(attempt);
                    tracing::warn!(?e, attempt, secs, "Upload del acta falló — reintentando");
                    state.ws_hub.publish(AgentEvent::Uploading {
                        job_id: payload.job_id.clone(),
                    });
                    tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                    continue;
                }
                // Agotamos los reintentos inmediatos con un error transitorio:
                // persistimos el acta y salimos como "pendiente de subida".
                tracing::warn!(error = %e, "Sin conexión para subir — guardando acta en disco");
                pending::save(
                    app,
                    pending::PendingUpload {
                        job_id: payload.job_id.clone(),
                        transcript_callback: payload.callbacks.transcript.clone(),
                        token: payload.token.clone(),
                        upload,
                        saved_at: 0,
                    },
                )
                .await?;
                return Ok(JobOutcome::UploadPending);
            }
        }
    }
}

/// Ejecuta el paso de diarización completo (asegurar modelos ONNX + correr el
/// sidecar y etiquetar segmentos). Devuelve un `Result` para que el llamador
/// decida qué hacer si falla — en el pipeline lo tratamos como NO-FATAL: un
/// fallo aquí no debe perder la transcripción.
async fn run_diarization(
    app: &AppHandle,
    wav_path: &std::path::Path,
    num_speakers: Option<u8>,
    segments: &mut [Segment],
) -> Result<diarize::DiarizationResult, PipelineError> {
    let (seg_model, emb_model) = models::ensure_diarization_models(app).await?;
    let result =
        diarize::diarize_segments(app, wav_path, &seg_model, &emb_model, num_speakers, segments)
            .await?;
    Ok(result)
}

/// Cierra el trabajo como fallado. `hint` no es opcional a propósito: todo
/// error que llega a una persona tiene que traer su salida.
async fn fail(
    state: &AppState,
    payload: &AgentTriggerPayload,
    stage: FailStage,
    error: String,
    hint: &str,
) -> Result<(), ()> {
    state.log("error", format!("Trabajo fallado ({stage:?}): {error}"));
    state.update_job(&payload.job_id, |j| {
        j.stage = JobStage::Failed;
        j.eta_seconds = None;
        j.note = None;
        j.error = Some(error.clone());
        j.hint = Some(hint.to_string());
    });
    state.ws_hub.publish(AgentEvent::Failed {
        job_id: payload.job_id.clone(),
        stage,
        error: error.clone(),
    });
    let _ = amautum::post_fail(
        &payload.callbacks.fail,
        &payload.token,
        &FailUpload {
            error,
            stage,
            progreso: None,
        },
    )
    .await;
    Ok(())
}

/// Duración en castellano para la ventana y la bitácora.
fn fmt_duration(seconds: f64) -> String {
    if seconds <= 0.0 {
        return "duración desconocida".into();
    }
    let total = seconds as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h} h {m:02} min")
    } else if m > 0 {
        format!("{m} min {s:02} s")
    } else {
        format!("{s} s")
    }
}

fn fmt_eta(seconds: u64) -> String {
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{seconds}s")
    }
}

fn hardware_str(h: Hardware) -> &'static str {
    match h {
        Hardware::Cpu => "cpu",
        Hardware::Cuda => "cuda",
        Hardware::Metal => "metal",
    }
}

