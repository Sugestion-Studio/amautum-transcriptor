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

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use tauri::AppHandle;
use tokio::sync::Mutex;

use crate::ffmpeg;
use crate::models;
use crate::types::{
    AgentEvent, AgentTriggerPayload, FailStage, FailUpload, Hardware, ProgressUpload, Segment,
    TranscriptUpload,
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
}

impl AppState {
    pub fn new() -> Self {
        Self {
            ws_hub: WsHub::new(),
            running: Arc::new(AtomicUsize::new(0)),
            dependencies_ok: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Lanza el job en una tarea aparte. La función vuelve inmediatamente — el
/// HTTP handler responde 202 sin esperar a que termine.
pub fn spawn_job(app: AppHandle, state: AppState, payload: AgentTriggerPayload) {
    state.running.fetch_add(1, Ordering::SeqCst);
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
            PipelineError::Amautum(_) => FailStage::Upload,
            PipelineError::Io(_) => FailStage::Unknown,
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
            return fail(&state, &payload, FailStage::Unknown, err.to_string()).await;
        }
    };

    let res = run_inner(app, &state, &payload, tempdir.path()).await;
    match res {
        Ok(transcript_id) => {
            state.ws_hub.publish(AgentEvent::Completed {
                job_id: payload.job_id.clone(),
                transcript_id,
            });
            Ok(())
        }
        Err(err) => {
            // No replicamos el último progreso aquí: el cloud ya lo tiene de
            // los POSTs de `progress` que disparó la callback de whisper.
            let stage = err.stage();
            fail_with_progress(&state, &payload, stage, err.to_string(), None).await
        }
    }
}

async fn run_inner(
    app: &AppHandle,
    state: &AppState,
    payload: &AgentTriggerPayload,
    tempdir: &std::path::Path,
) -> Result<String, PipelineError> {
    let source = PathBuf::from(&payload.audio_file_path);

    // ── 1) Pre-procesamiento ────────────────────────────────────────────────
    state.ws_hub.publish(AgentEvent::Preprocess {
        job_id: payload.job_id.clone(),
        stage: "ffmpeg".into(),
    });
    let pre = ffmpeg::convert_to_whisper_wav(app, &source, tempdir).await?;

    let hardware = Hardware::detect();
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
    let job_id_for_progress = payload.job_id.clone();
    let ws_hub_for_progress = state.ws_hub.clone();
    let callback_progress = payload.callbacks.progress.clone();
    let token_for_progress = payload.token.clone();
    let duration_secs = pre.duration_seconds;

    // Throttle: el cloud no necesita 100 POSTs; cada salto de >=5% disparamos.
    let last_pushed: Arc<Mutex<u8>> = Arc::new(Mutex::new(0));

    let on_progress: Arc<Mutex<whisper::ProgressCallback>> = Arc::new(Mutex::new(Box::new({
        let job_id = job_id_for_progress.clone();
        let ws = ws_hub_for_progress.clone();
        let last = last_pushed.clone();
        let callback = callback_progress.clone();
        let token = token_for_progress.clone();
        move |tick: whisper::ProgressTick| {
                ws.publish(AgentEvent::Progress {
                    job_id: job_id.clone(),
                    progress: tick.percent,
                    eta_seconds: estimate_eta(duration_secs, tick.percent),
                    last_segment: None,
                });
                let last_clone = last.clone();
                let callback = callback.clone();
                let token = token.clone();
                let hw = hardware_str(hardware);
                tauri::async_runtime::spawn(async move {
                    let mut prev = last_clone.lock().await;
                    if tick.percent >= *prev + 5 || tick.percent == 100 {
                        *prev = tick.percent;
                        let _ = amautum::post_progress(
                            &callback,
                            &token,
                            &ProgressUpload {
                                progreso: tick.percent,
                                tiempo_estimado: estimate_eta(duration_secs, tick.percent)
                                    .map(fmt_eta),
                                hardware: Some(hw),
                                agente_version: Some(config::version().to_string()),
                                estado: None,
                            },
                        )
                        .await;
                    }
                });
            }
        })));

    let run = whisper::transcribe(
        app,
        &pre.wav_path,
        payload.model,
        &payload.language,
        &models_dir,
        on_progress,
    )
    .await?;

    // ── 3) Upload al cloud ─────────────────────────────────────────────────
    state.ws_hub.publish(AgentEvent::Uploading {
        job_id: payload.job_id.clone(),
    });

    let upload = TranscriptUpload {
        language: run.language,
        model: run.model_used,
        duration_seconds: pre.duration_seconds,
        full_text: run.full_text,
        segments: run.segments,
        speakers: None,
        summary: None,
    };
    let resp = amautum::post_transcript(
        &payload.callbacks.transcript,
        &payload.token,
        &upload,
    )
    .await?;
    Ok(resp.transcript_id)
}

async fn fail(
    state: &AppState,
    payload: &AgentTriggerPayload,
    stage: FailStage,
    error: String,
) -> Result<(), ()> {
    fail_with_progress(state, payload, stage, error, None).await
}

async fn fail_with_progress(
    state: &AppState,
    payload: &AgentTriggerPayload,
    stage: FailStage,
    error: String,
    progress: Option<u8>,
) -> Result<(), ()> {
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
            progreso: progress,
        },
    )
    .await;
    Ok(())
}

fn estimate_eta(duration_secs: f64, percent: u8) -> Option<u64> {
    if percent == 0 || percent >= 100 || duration_secs <= 0.0 {
        return None;
    }
    // Asumimos el clásico ~0.5x realtime en CPU para `medium` (heurística
    // grosera). Esto se refinará cuando midamos en hardware real.
    let total = duration_secs * 2.0;
    let remaining = total * (100 - percent) as f64 / 100.0;
    Some(remaining as u64)
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

