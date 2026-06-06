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
