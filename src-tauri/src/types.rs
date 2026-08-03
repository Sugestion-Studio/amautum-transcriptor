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
    /// Si `true`, tras la transcripción corremos diarización (identificación de
    /// interlocutores) con el sidecar sherpa-onnx y etiquetamos cada segmento
    /// con su orador. whisper.cpp por sí solo no diariza audio mono; esto es un
    /// paso aparte. Default `false` para no penalizar a quien no lo necesita.
    #[serde(default)]
    pub diarize: bool,
    /// Número de interlocutores esperado. Si lo conoce el operador (p. ej. una
    /// audiencia con juez + 2 partes = 3), el clustering es mucho más preciso.
    /// Si es `None`, el diarizador estima el número solo por umbral.
    #[serde(default)]
    pub num_speakers: Option<u8>,
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
///
/// `rename_all` solo camelCasea los TAGS de variante (el valor de `event`); para
/// camelCasear también los CAMPOS de cada variante (`job_id` → `jobId`,
/// `transcript_id` → `transcriptId`…) hace falta `rename_all_fields`. Sin esto
/// el cliente web leía `ev.transcriptId` y obtenía `undefined`, navegando a
/// `…/transcriptor-transcript/undefined` (404) al terminar un job.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum AgentEvent {
    Queued {
        job_id: String,
    },
    /// El modelo Whisper que pidió el usuario no está en disco; lo estamos
    /// bajando de HuggingFace. La primera vez por modelo; siguientes corridas
    /// del mismo modelo lo reutilizan.
    ModelDownload {
        job_id: String,
        model: String,
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
        percent: u8,
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
        /// Nota legible de QUÉ está pasando ahora mismo ("tramo 5 de 12", "el
        /// motor lleva 12 min sin reportar"). El porcentaje solo no distingue
        /// "lento" de "colgado"; esta nota sí. Campo aditivo: un cliente que no
        /// lo entienda lo ignora.
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// Whisper terminó y arrancamos el paso de diarización (identificación de
    /// interlocutores). Solo se emite si el job pidió `diarize`.
    Diarizing {
        job_id: String,
    },
    Uploading {
        job_id: String,
    },
    /// La transcripción terminó pero la subida al cloud falló por red. El acta
    /// quedó GUARDADA en disco y se reintentará sola al volver la conexión — el
    /// usuario NO pierde el trabajo.
    UploadPending {
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
    // `default` para que el round-trip a disco (cola de pendientes) funcione:
    // al serializar omitimos `speaker` si es None, así que al releer puede faltar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
}

/// Salida estructurada que el agente sube al backend de Amautum cuando
/// termina. El backend la mapea 1:1 al `transcriptor-transcript` y referencia
/// al `transcriptor-job` por el id en la URL.
// `Deserialize` además de `Serialize` porque la cola de pendientes en disco
// (pending.rs) serializa esta estructura y la vuelve a leer para reintentar.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TranscriptUpload {
    pub language: String,
    pub model: String,
    pub duration_seconds: f64,
    pub full_text: String,
    pub segments: Vec<Segment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speakers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

// ── Estado observable del agente ────────────────────────────────────────────
//
// El WebSocket sirve a la pestaña web y muere con ella. La VENTANA del agente
// necesita algo distinto: poder abrirse a las tres horas de haber empezado y
// contar qué está pasando. Para eso el agente mantiene un registro vivo de sus
// trabajos y un buffer de bitácora, que sirve por `GET /status`.

/// En qué etapa está un trabajo. Es lo que la ventana pinta como titular.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JobStage {
    Queued,
    DownloadingModel,
    Preprocess,
    Transcribing,
    Diarizing,
    Uploading,
    UploadPending,
    Completed,
    Failed,
}

impl JobStage {
    pub fn is_terminal(self) -> bool {
        matches!(self, JobStage::Completed | JobStage::Failed)
    }
}

/// Foto del estado de un trabajo. `engine_seen_at_ms` es la clave del rediseño:
/// permite a la ventana decir "el motor reportó hace 14 minutos" en vez de dejar
/// una barra congelada sin explicación.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobStatus {
    pub job_id: String,
    pub file_name: String,
    pub model: String,
    pub hardware: Option<String>,
    pub stage: JobStage,
    pub progress: u8,
    pub eta_seconds: Option<u64>,
    /// Duración del audio, para poder mostrar "2 h 14 min de audio".
    pub audio_seconds: Option<f64>,
    /// Nota de la etapa actual (tramo N de M, etc.).
    pub note: Option<String>,
    pub started_at_ms: u64,
    /// Última vez que el MOTOR dio señales de vida (no el latido del agente).
    pub engine_seen_at_ms: u64,
    pub updated_at_ms: u64,
    pub error: Option<String>,
    /// Qué debe hacer la persona. Nunca dejamos un error sin una salida.
    pub hint: Option<String>,
}

/// Una línea de la bitácora que la ventana muestra y que el botón "Copiar
/// diagnóstico" vuelca al portapapeles para soporte.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub at_ms: u64,
    pub level: &'static str,
    pub text: String,
}

/// Milisegundos desde epoch. Un reloj que no puede fallar hacia atrás en la UI:
/// si el sistema devuelve algo anterior a epoch, devolvemos 0 en vez de romper.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regresión: los CAMPOS de cada variante de `AgentEvent` deben ir en
    /// camelCase, no solo el tag. Si esto se rompe, el cliente web lee
    /// `ev.transcriptId` como `undefined` y navega a `…/undefined` (404).
    #[test]
    fn agent_event_fields_are_camel_case() {
        let ev = AgentEvent::Completed {
            job_id: "j1".into(),
            transcript_id: "tx-abc".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"transcriptId\":\"tx-abc\""), "got: {json}");
        assert!(json.contains("\"jobId\":\"j1\""), "got: {json}");
        assert!(!json.contains("transcript_id"), "campo en snake_case: {json}");
    }

    /// La nota del progreso es ADITIVA: cuando no hay nada que contar, el evento
    /// tiene que salir idéntico al de antes para no confundir a un cliente viejo.
    #[test]
    fn progress_note_is_omitted_when_empty() {
        let sin_nota = AgentEvent::Progress {
            job_id: "j1".into(),
            progress: 42,
            eta_seconds: Some(600),
            last_segment: None,
            note: None,
        };
        let json = serde_json::to_string(&sin_nota).unwrap();
        assert!(!json.contains("note"), "no debería emitir `note`: {json}");

        let con_nota = AgentEvent::Progress {
            job_id: "j1".into(),
            progress: 42,
            eta_seconds: None,
            last_segment: None,
            note: Some("tramo 3 de 8".into()),
        };
        let json = serde_json::to_string(&con_nota).unwrap();
        assert!(json.contains("\"note\":\"tramo 3 de 8\""), "got: {json}");
    }

    /// La ventana decide qué podar y qué seguir mostrando a partir de esto.
    #[test]
    fn only_completed_and_failed_are_terminal() {
        assert!(JobStage::Completed.is_terminal());
        assert!(JobStage::Failed.is_terminal());
        assert!(!JobStage::Transcribing.is_terminal());
        // Un acta pendiente de subir NO está terminada: sigue habiendo trabajo
        // por hacer y la ventana tiene que ofrecer el reintento.
        assert!(!JobStage::UploadPending.is_terminal());
    }
}
