//! Configuración estática del agente. Mantenemos esto en un solo módulo para
//! que ajustar puerto/orígenes CORS no requiera tocar la lógica de negocio.

/// Puerto del servidor HTTP+WS local. Elegido fuera del rango efímero típico,
/// fácil de recordar (T-3 → 1717.3). El frontend Vite usa 17172 (uno menos).
pub const SERVER_PORT: u16 = 17173;

/// Orígenes permitidos para CORS. Cualquier otra `Origin` recibe 403 a nivel
/// de tower-http. El agente es local pero el navegador es público — esto
/// evita que un sitio malicioso abra una pestaña y dispare transcripciones.
pub const ALLOWED_ORIGINS: &[&str] = &[
    "https://www.amautum.com",
    "https://amautum.com",
    "http://localhost:3000",
];

/// User-Agent que el agente envía al hacer callback al backend de Amautum.
/// Se usa para identificar la versión del agente en logs del servidor.
pub fn user_agent() -> String {
    format!("AmautumTranscriptorAgent/{}", env!("CARGO_PKG_VERSION"))
}

/// Versión del agente expuesta en `/health` y en los callbacks.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Sample rate al que Whisper espera el audio (Hz).
pub const WHISPER_SAMPLE_RATE: u32 = 16_000;

/// Canales del audio que Whisper espera (mono).
pub const WHISPER_CHANNELS: u32 = 1;

/// Nombres de los sidecar binaries declarados en `tauri.conf.json`.
/// Sin subfolder: Tauri 2.0 resuelve correctamente nombres planos en runtime;
/// con `binaries/<name>` el `shell.sidecar()` devuelve "No such file or
/// directory" en macOS (y probablemente Windows). Para devops: los archivos
/// en disco siguen viviendo en `src-tauri/<name>-<triple>[.exe]`.
pub const WHISPER_SIDECAR: &str = "whisper-cli";
pub const FFMPEG_SIDECAR: &str = "ffmpeg";

/// Sidecar de diarización: el binario `sherpa-onnx-offline-speaker-diarization`
/// de sherpa-onnx (k2-fsa), renombrado al instalar para seguir la convención
/// de Tauri. Solo se usa cuando el operador pide identificar interlocutores;
/// el agente arranca sin él (su ausencia NO marca `dependenciesOk: false`).
pub const DIARIZE_SIDECAR: &str = "sherpa-diarize";

// ── Modelos ONNX de diarización ──────────────────────────────────────────────
//
// sherpa-onnx hace diarización en dos etapas, cada una con su modelo ONNX:
//   1. Segmentación (VAD por speaker): pyannote segmentation-3.0.
//   2. Embedding de voz: un modelo de speaker-verification cuyo vector se
//      clusteriza para agrupar segmentos del mismo hablante.
//
// Los embeddings capturan timbre de voz, no fonética, así que un modelo
// entrenado en otro idioma agrupa bien hablantes en español. Bajamos los ONNX
// a la misma carpeta `models/` que los GGML de whisper, al primer uso.
//
// IMPORTANTE (devops): confirma estos nombres/URLs contra la página de releases
// de sherpa-onnx antes del primer build con diarización — igual que los TODO de
// whisper-cli/ffmpeg en release.yml. Si cambian, solo se toca aquí.

/// Modelo de segmentación pyannote (ONNX). ~6 MB.
pub const DIARIZE_SEGMENTATION_MODEL: &str = "sherpa-onnx-pyannote-segmentation-3-0.onnx";
pub const DIARIZE_SEGMENTATION_URL: &str = "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/main/model.onnx";
/// Cota inferior de tamaño para detectar descargas truncadas (~5.7 MB reales).
pub const DIARIZE_SEGMENTATION_MIN_BYTES: u64 = 4 * 1024 * 1024;

/// Modelo de embedding de voz (ONNX). ~38 MB. 3D-Speaker eres2net.
pub const DIARIZE_EMBEDDING_MODEL: &str = "3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx";
pub const DIARIZE_EMBEDDING_URL: &str = "https://huggingface.co/csukuangfj/speaker-embedding-models/resolve/main/3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx";
/// Cota inferior de tamaño para detectar descargas truncadas (~38 MB reales).
pub const DIARIZE_EMBEDDING_MIN_BYTES: u64 = 20 * 1024 * 1024;
