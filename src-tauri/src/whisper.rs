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
