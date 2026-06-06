//! Tipos compartidos entre el servidor HTTP, el pipeline y el cliente de
//! Amautum. Las formas reflejan exactamente los payloads documentados en
//! `app/api/transcriptor/jobs/*` del backend.

use serde::{Deserialize, Serialize};

/// Payload que la pestaña web envía al agente para arrancar un trabajo.
/// `audio_file_path` es la ruta ABSOLUTA al archivo en el disco del usuario;
/// la web la obtiene con la pieza nativa (Tauri/Electron del navegador no
/// tiene acceso, así que esto se hace con un selector de archivos del propio
/// agente, o bien el usuario pega la ruta).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTriggerPayload {
    pub job_id: String,
    pub token: String,
    pub audio_file_path: String,
    pub callbacks: AgentCallbacks,
    pub model: WhisperModel,
    #[serde(default = "default_language")]
    pub language: String,
    /// Tamaño esperado del archivo para validación temprana.
    #[serde(default)]
    pub file_size: Option<u64>,
}

fn default_language() -> String {
    "es".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCallbacks {
    pub progress: String,
    pub transcript: String,
    pub fail: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WhisperModel {
    Tiny,
    Base,
    Small,
    Medium,
    #[serde(rename = "large-v3")]
    LargeV3,
}

impl WhisperModel {
    /// Nombre del archivo del modelo GGML que el agente espera tener
    /// disponible (descargado por el instalador o por el usuario).
    pub fn ggml_filename(self) -> &'static str {
        match self {
            WhisperModel::Tiny => "ggml-tiny.bin",
            WhisperModel::Base => "ggml-base.bin",
            WhisperModel::Small => "ggml-small.bin",
            WhisperModel::Medium => "ggml-medium.bin",
            WhisperModel::LargeV3 => "ggml-large-v3.bin",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            WhisperModel::Tiny => "tiny",
            WhisperModel::Base => "base",
            WhisperModel::Small => "small",
            WhisperModel::Medium => "medium",
            WhisperModel::LargeV3 => "large-v3",
        }
    }
}

/// Hardware efectivo donde corrió la inferencia, detectado por el agente.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Hardware {
    Cpu,
    Cuda,
    Metal,
}

impl Hardware {
    pub fn detect() -> Self {
        // Detección simple: en macOS asumimos Metal; en otros sistemas con
        // variables CUDA visibles asumimos CUDA; default CPU. whisper-cli
        // hará el chequeo real al cargar el modelo; esto es una pista para
        // mostrarle al usuario qué se usó.
        if cfg!(target_os = "macos") {
            Hardware::Metal
        } else if std::env::var("CUDA_VISIBLE_DEVICES").is_ok() {
            Hardware::Cuda
        } else {
            Hardware::Cpu
        }
    }
}

/// Eventos que el pipeline emite hacia los clientes WebSocket de la pestaña.
/// Cada uno incluye `jobId` para que el cliente pueda filtrar si tiene varias
/// transcripciones corriendo en paralelo (poco común pero posible).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum AgentEvent {
    Queued {
        job_id: String,
    },
    Preprocess {
        job_id: String,
        stage: String,
    },
    Started {
        job_id: String,
        hardware: Hardware,
        model: String,
    },
    Progress {
        job_id: String,
        progress: u8,
        eta_seconds: Option<u64>,
        last_segment: Option<String>,
    },
    Uploading {
        job_id: String,
    },
    Completed {
        job_id: String,
        transcript_id: String,
    },
    Failed {
        job_id: String,
        stage: FailStage,
        error: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailStage {
    Ffmpeg,
    ModelLoad,
    Inference,
    Diarization,
    Upload,
    Unknown,
}

/// Un segmento del output de whisper.cpp después de parsearlo. Idéntico a la
/// forma que el backend acepta en `POST /jobs/[id]/transcript`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
}

/// Salida estructurada que el agente sube al backend de Amautum cuando
/// termina. El backend la mapea 1:1 al `transcriptor-transcript` y referencia
/// al `transcriptor-job` por el id en la URL.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct TranscriptUpload {
    pub language: String,
    pub model: String,
    pub duration_seconds: f64,
    pub full_text: String,
    pub segments: Vec<Segment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speakers: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Cuerpo del POST de progreso al backend (no del WS local). Lo mandamos a
/// menor frecuencia que los WS — la fuente de verdad para la pestaña es el WS;
/// esto es para que el progreso sobreviva un refresh del navegador.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ProgressUpload {
    pub progreso: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tiempo_estimado: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hardware: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agente_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estado: Option<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct FailUpload {
    pub error: String,
    pub stage: FailStage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progreso: Option<u8>,
}
