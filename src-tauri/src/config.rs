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

/// Base de Amautum para los enlaces que el agente abre en el navegador del
/// sistema (soporte, descargas). Es el mismo origen que ya autoriza el CORS de
/// arriba: el agente se distribuye para la solución Transcriptor de Amautum.
pub const AMAUTUM_BASE_URL: &str = "https://www.amautum.com";

/// Identificador de esta app en el canal de soporte. Amautum va a tener varias
/// apps de escritorio; todas abren el mismo buzón y se distinguen por esto.
pub const SUPPORT_APP_SLUG: &str = "transcriptor";

/// Dónde manda el agente a quien necesita ayuda. Es la MISMA página que el
/// sidebar de Amautum ("Soporte → Ayuda y tickets"): un solo buzón, con
/// historial de tickets y respuestas, en vez de un correo suelto que se pierde.
///
/// El enlace lleva la app, la versión, el sistema y —si hay— qué falló, para que
/// el formulario aparezca con los datos técnicos ya puestos y la persona solo
/// tenga que contar lo suyo. Contrato común a todas las apps de escritorio:
/// ver `lib/support/app-context.ts` en Amautum.
pub fn support_url(context: Option<&str>) -> String {
    let mut url = format!(
        "{AMAUTUM_BASE_URL}/dashboard/support?app={SUPPORT_APP_SLUG}&v={}&os={}",
        urlencode(version()),
        urlencode(&os_label()),
    );
    if let Some(ctx) = context {
        // Recortamos aquí también: el otro lado lo recorta, pero una URL de
        // varios kB se rompe en el camino antes de llegar.
        let trimmed: String = ctx.chars().take(400).collect();
        url.push_str(&format!("&ctx={}", urlencode(&trimmed)));
    }
    url
}

/// Sistema operativo en lenguaje de persona, no de compilador.
fn os_label() -> String {
    let os = if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Linux"
    };
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
    format!("{os} {arch}")
}

/// Percent-encoding mínimo para meter texto en un query string.
///
/// A mano y no con una dependencia nueva: son cuatro reglas y el alternativo es
/// arrastrar un crate entero para construir una URL.
fn urlencode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b' ' => out.push_str("%20"),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Guía de instalación paso a paso por sistema operativo.
pub fn downloads_url() -> String {
    format!("{AMAUTUM_BASE_URL}/downloads/transcriptor")
}

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

// ── Ritmos: latido, paciencia y bitácora ────────────────────────────────────
//
// El problema que resuelven estas constantes: en un Mac con Metal una audiencia
// de dos horas se transcribe en veinte minutos y el porcentaje sube cada pocos
// segundos. En un Windows sin GPU el mismo trabajo puede tardar seis horas y
// whisper.cpp reporta cada 5% — es decir, cada ~18 minutos de reloj. Sin latido
// propio, "trabajando bien pero lento" y "colgado" se ven EXACTAMENTE igual.

/// Cada cuánto el agente emite un evento de progreso al WebSocket aunque el
/// motor no haya reportado nada nuevo. La web marca "atascado" a los 30 s de
/// silencio, así que el latido tiene que ser bastante más corto que eso.
pub const HEARTBEAT_INTERVAL_SECS: u64 = 10;

/// Intervalo MÁXIMO entre POSTs de progreso al cloud. El disparo normal es por
/// salto de 5%; este techo garantiza que el panel del job (y el barrido de
/// trabajos huérfanos del backend) vea actividad aunque el 5% tarde horas.
pub const CLOUD_PROGRESS_MAX_INTERVAL_SECS: u64 = 120;

/// Si `whisper-cli` no escribe NADA (ni stdout ni stderr) durante este tiempo,
/// lo damos por colgado, lo matamos y fallamos con un mensaje accionable.
/// Generoso a propósito: cargar `large-v3` desde un disco lento puede tardar
/// varios minutos antes de la primera línea.
pub const ENGINE_SILENCE_TIMEOUT_SECS: u64 = 25 * 60;

/// Cuántas líneas de bitácora retenemos para la ventana y el diagnóstico.
pub const LOG_BUFFER_LINES: usize = 300;

/// Cuántos trabajos ya terminados seguimos mostrando en la ventana.
pub const JOB_HISTORY_LIMIT: usize = 8;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// El contexto viaja por la URL: si no se escapa, un error con `&` o un
    /// salto de línea corta el query string y el ticket llega mutilado.
    #[test]
    fn support_url_escapes_the_context() {
        let url = support_url(Some("audiencia & prueba\nlínea 2"));
        assert!(url.contains("app=transcriptor"), "{url}");
        assert!(url.contains("%26"), "el & sin escapar corta la URL: {url}");
        assert!(!url.contains('\n'), "salto de línea crudo en la URL: {url}");
        assert!(!url.contains(" & "), "{url}");
    }

    /// Sin contexto no se manda el parámetro vacío.
    #[test]
    fn support_url_without_context_has_no_ctx() {
        let url = support_url(None);
        assert!(!url.contains("ctx="), "{url}");
        assert!(url.contains("os="), "{url}");
    }
}
