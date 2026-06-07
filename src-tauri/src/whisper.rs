//! Wrapper sobre el binario `whisper-cli` de whisper.cpp.
//!
//! Lo invocamos pidiéndole un único output JSON estructurado (`--output-json`)
//! para no tener que parsear el formato libre del stdout. Mientras corre, sí
//! parseamos su stderr para extraer el porcentaje de progreso (`--print-progress`)
//! y un timestamp de "última línea procesada" que permite estimar ETA.
//!
//! whisper.cpp escribe el JSON al lado del archivo de entrada con la
//! extensión `.json` (`input.wav` → `input.wav.json`). Lo leemos al final.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use tauri::AppHandle;
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;
use tokio::fs;
use tokio::sync::Mutex;

use crate::config::WHISPER_SIDECAR;
use crate::types::{Segment, WhisperModel};

#[derive(thiserror::Error, Debug)]
pub enum WhisperError {
    #[error("Modelo GGML no encontrado en {0:?}")]
    ModelMissing(PathBuf),
    #[error("whisper-cli falló: {0}")]
    Process(String),
    #[error("No se generó el JSON esperado en {0:?}")]
    OutputMissing(PathBuf),
    #[error("JSON de whisper inválido: {0}")]
    InvalidJson(String),
    #[error("Error de E/S: {0}")]
    Io(#[from] std::io::Error),
    #[error("Error de Tauri: {0}")]
    Tauri(String),
}

#[derive(Debug, Clone, Copy)]
pub struct ProgressTick {
    pub percent: u8,
    pub last_offset_seconds: Option<f64>,
}

pub struct WhisperRun {
    pub language: String,
    pub model_used: String,
    pub segments: Vec<Segment>,
    pub full_text: String,
}

/// Ejecuta whisper-cli sobre `wav_path` y entrega los segmentos parseados.
/// `on_progress` recibe cada actualización de porcentaje que extraemos del
/// stderr — el pipeline lo usa para alimentar al WS y al callback cloud.
pub type ProgressCallback = Box<dyn FnMut(ProgressTick) + Send + 'static>;

pub async fn transcribe(
    app: &AppHandle,
    wav_path: &Path,
    model: WhisperModel,
    language: &str,
    models_dir: &Path,
    on_progress: Arc<Mutex<ProgressCallback>>,
) -> Result<WhisperRun, WhisperError> {
    let model_path = models_dir.join(model.ggml_filename());
    if !model_path.exists() {
        return Err(WhisperError::ModelMissing(model_path));
    }

    // whisper-cli `-of <base>` escribe `<base>.json` cuando se pasa
    // `--output-json`. Usamos el stem del WAV para que el JSON quede al lado
    // sin colisionar con otros artefactos del mismo directorio temporal.
    let json_out_base = wav_path.with_extension("");
    let json_path = json_out_base.with_extension("json");
    if json_path.exists() {
        let _ = fs::remove_file(&json_path).await;
    }

    let shell = app.shell();
    let args: Vec<String> = vec![
        "-m".into(),
        model_path.to_string_lossy().into(),
        "-f".into(),
        wav_path.to_string_lossy().into(),
        "-l".into(),
        language.to_string(),
        "--output-json".into(),
        "--output-file".into(),
        json_out_base.to_string_lossy().into(),
        "--print-progress".into(),
        "--no-prints".into(), // suprime banner; el progreso sigue saliendo
    ];

    let cmd = shell
        .sidecar(WHISPER_SIDECAR)
        .map_err(|e| WhisperError::Tauri(e.to_string()))?
        .args(args);

    let (mut rx, _child) = cmd
        .spawn()
        .map_err(|e| WhisperError::Tauri(e.to_string()))?;

    let mut last_stderr = String::new();
    let mut exit_code: Option<i32> = None;

    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(line) => {
                if let Some(tick) = parse_progress_line(&String::from_utf8_lossy(&line)) {
                    let mut cb = on_progress.lock().await;
                    (cb)(tick);
                }
            }
            CommandEvent::Stderr(line) => {
                let text = String::from_utf8_lossy(&line).into_owned();
                if let Some(tick) = parse_progress_line(&text) {
                    let mut cb = on_progress.lock().await;
                    (cb)(tick);
                }
                last_stderr.push_str(&text);
                last_stderr.push('\n');
            }
            CommandEvent::Terminated(payload) => {
                exit_code = payload.code;
            }
            CommandEvent::Error(err) => {
                return Err(WhisperError::Process(err));
            }
            _ => {}
        }
    }
    if exit_code != Some(0) {
        return Err(WhisperError::Process(tail(&last_stderr, 4)));
    }
    if !json_path.exists() {
        return Err(WhisperError::OutputMissing(json_path));
    }

    let bytes = fs::read(&json_path).await?;
    let parsed: WhisperJson =
        serde_json::from_slice(&bytes).map_err(|e| WhisperError::InvalidJson(e.to_string()))?;
    let segments: Vec<Segment> = parsed
        .transcription
        .into_iter()
        .map(|t| Segment {
            start: timestamp_to_seconds(&t.offsets.from),
            end: timestamp_to_seconds(&t.offsets.to),
            text: t.text.trim().to_string(),
            speaker: None,
        })
        .collect();

    let full_text = segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");

    Ok(WhisperRun {
        language: parsed.result.language.unwrap_or_else(|| language.to_string()),
        model_used: model.as_str().to_string(),
        segments,
        full_text,
    })
}

/// whisper-cli imprime líneas del tipo `whisper_print_progress_callback: progress = 42%`.
/// La regex sería más limpia pero quiero evitar dependencia: parseo manual.
fn parse_progress_line(line: &str) -> Option<ProgressTick> {
    let lower = line.to_ascii_lowercase();
    let idx = lower.find("progress")?;
    let after = &line[idx..];
    let eq = after.find('=')?;
    let rest = &after[eq + 1..];
    let pct_str: String = rest
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if pct_str.is_empty() {
        return None;
    }
    // Turbofish explícito porque Rust 2024 endurece la inferencia y la cadena
    // `.parse().ok()?.min(100)` no le da pistas suficientes al compilador para
    // resolver el tipo intermedio del Result.
    let percent: u8 = pct_str.parse::<u8>().ok()?.min(100);
    Some(ProgressTick {
        percent,
        last_offset_seconds: None,
    })
}

fn tail(s: &str, n: usize) -> String {
    s.lines()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(n)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ")
}

// ── JSON de whisper.cpp ───────────────────────────────────────────────────────
//
// Estructura simplificada: solo leemos los campos que necesitamos. El JSON
// completo incluye tokens por segmento, probabilidades, etc., que ignoramos.

#[derive(Debug, Deserialize)]
struct WhisperJson {
    #[serde(default)]
    result: WhisperResult,
    transcription: Vec<WhisperTranscriptionItem>,
}

#[derive(Debug, Deserialize, Default)]
struct WhisperResult {
    #[serde(default)]
    language: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WhisperTranscriptionItem {
    offsets: WhisperOffsets,
    text: String,
}

#[derive(Debug, Deserialize)]
struct WhisperOffsets {
    from: WhisperOffset,
    to: WhisperOffset,
}

/// whisper.cpp emite el offset como milisegundos (número) o como "00:00:01,234"
/// (string), según la versión. Aceptamos ambos.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WhisperOffset {
    Ms(u64),
    Ts(String),
}

fn timestamp_to_seconds(off: &WhisperOffset) -> f64 {
    match off {
        WhisperOffset::Ms(ms) => (*ms as f64) / 1000.0,
        WhisperOffset::Ts(s) => parse_timestamp(s).unwrap_or(0.0),
    }
}

fn parse_timestamp(s: &str) -> Option<f64> {
    let s = s.replace(',', ".");
    let mut parts = s.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let sec: f64 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + sec)
}

// ── Transcripción chunked para audios largos ────────────────────────────────
//
// Para audios > CHUNKING_THRESHOLD_SECONDS (10 min) dividimos el WAV en
// trozos de CHUNK_LENGTH_SECONDS (5 min) con CHUNK_OVERLAP_SECONDS de overlap
// y los procesamos secuencialmente. El consumo de RAM queda acotado al de un
// chunk: en vez de cargar 1 h en memoria + el modelo `medium` (~6 GB peak),
// procesamos cinco minutos y liberamos antes del siguiente.
//
// Trade-offs:
//   - El modelo se carga UNA vez por chunk (~3-5 s con caché en disco).
//   - Hay un riesgo pequeño de cortar una frase en la frontera; el overlap
//     ayuda a que la siguiente corrida capture el contexto.
//   - Los `start`/`end` de los segmentos se desplazan al timeline global.

const CHUNKING_THRESHOLD_SECONDS: f64 = 10.0 * 60.0;
const CHUNK_LENGTH_SECONDS: f64 = 5.0 * 60.0;
const CHUNK_OVERLAP_SECONDS: f64 = 5.0;

pub async fn transcribe_chunked(
    app: &AppHandle,
    wav_path: &Path,
    model: WhisperModel,
    language: &str,
    models_dir: &Path,
    duration_seconds: f64,
    on_progress: Arc<Mutex<ProgressCallback>>,
) -> Result<WhisperRun, WhisperError> {
    // Audios cortos: usamos la ruta clásica directo. Sin overhead extra.
    if duration_seconds <= CHUNKING_THRESHOLD_SECONDS {
        return transcribe(app, wav_path, model, language, models_dir, on_progress).await;
    }

    let chunk_count = ((duration_seconds / CHUNK_LENGTH_SECONDS).ceil() as usize).max(1);
    tracing::info!(
        duration_seconds,
        chunk_count,
        chunk_length = CHUNK_LENGTH_SECONDS,
        "Procesando audio largo en chunks"
    );

    let parent = wav_path
        .parent()
        .ok_or_else(|| WhisperError::Io(std::io::Error::new(std::io::ErrorKind::Other, "wav sin parent")))?
        .to_path_buf();

    let mut all_segments: Vec<Segment> = Vec::new();
    let mut combined_text = String::new();
    let mut detected_language: Option<String> = None;
    let mut model_used: Option<String> = None;

    for i in 0..chunk_count {
        let chunk_start = (i as f64) * CHUNK_LENGTH_SECONDS;
        let chunk_end_target = chunk_start + CHUNK_LENGTH_SECONDS + CHUNK_OVERLAP_SECONDS;
        let chunk_end = chunk_end_target.min(duration_seconds);
        let chunk_duration = chunk_end - chunk_start;
        if chunk_duration < 0.1 {
            continue;
        }

        let chunk_path = parent.join(format!("chunk_{:03}.wav", i));
        crate::ffmpeg::extract_chunk(app, wav_path, chunk_start, chunk_duration, &chunk_path)
            .await
            .map_err(|e| WhisperError::Process(format!("ffmpeg chunking: {e}")))?;

        // El progreso de Whisper para este chunk va de 0-100. Lo mapeamos a
        // la fracción del job total que corresponde a este chunk.
        let chunk_index = i;
        let inner_cb_storage = on_progress.clone();
        let inner_cb: ProgressCallback = Box::new(move |tick: ProgressTick| {
            let chunk_pct = tick.percent as f64;
            let global_pct =
                ((chunk_index as f64 * 100.0) + chunk_pct) / (chunk_count as f64);
            let global_tick = ProgressTick {
                percent: global_pct.min(100.0).max(0.0) as u8,
                last_offset_seconds: None,
            };
            // Re-emit usando el callback original. Usamos try_lock para no
            // bloquear si está siendo invocado en cadena.
            if let Ok(mut outer) = inner_cb_storage.try_lock() {
                (outer)(global_tick);
            }
        });
        let inner_arc: Arc<Mutex<ProgressCallback>> = Arc::new(Mutex::new(inner_cb));

        let run = transcribe(app, &chunk_path, model, language, models_dir, inner_arc).await?;

        // Desplaza timestamps al timeline global. El overlap puede duplicar
        // segmentos al final del chunk anterior, pero whisper.cpp suele
        // entregar segmentos coherentes — preferimos un poco de duplicación
        // antes que perder texto en el corte.
        for mut seg in run.segments {
            seg.start += chunk_start;
            seg.end += chunk_start;
            all_segments.push(seg);
        }
        if !combined_text.is_empty() && !combined_text.ends_with(' ') {
            combined_text.push(' ');
        }
        combined_text.push_str(run.full_text.trim());

        if detected_language.is_none() {
            detected_language = Some(run.language);
        }
        if model_used.is_none() {
            model_used = Some(run.model_used);
        }

        // Limpia el WAV intermedio para no llenar el disco temporal.
        let _ = fs::remove_file(&chunk_path).await;
    }

    Ok(WhisperRun {
        language: detected_language.unwrap_or_else(|| language.to_string()),
        model_used: model_used.unwrap_or_else(|| model.as_str().to_string()),
        segments: all_segments,
        full_text: combined_text,
    })
}
