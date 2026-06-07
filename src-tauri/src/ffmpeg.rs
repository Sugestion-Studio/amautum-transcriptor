//! Pre-procesamiento de audio con FFmpeg (sidecar): convierte cualquier
//! formato declarado por el usuario (mp3/m4a/ogg/flac/wav…) a un WAV PCM
//! 16 kHz mono, que es la entrada que whisper.cpp espera.
//!
//! Hacemos esto en una sola corrida sin reencodes intermedios para minimizar
//! pérdida de calidad. Si el archivo origen ya está en el formato correcto,
//! ffmpeg igualmente lo copia: aceptamos el coste a cambio de un pipeline
//! homogéneo (siempre el mismo path para whisper).

use std::path::{Path, PathBuf};

use tauri::AppHandle;
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;
use tokio::fs;

use crate::config::{FFMPEG_SIDECAR, WHISPER_CHANNELS, WHISPER_SAMPLE_RATE};

#[derive(thiserror::Error, Debug)]
pub enum FfmpegError {
    #[error("El archivo de audio no existe: {0}")]
    NotFound(PathBuf),
    #[error("Falló FFmpeg: {0}")]
    Process(String),
    #[error("Error de E/S: {0}")]
    Io(#[from] std::io::Error),
    #[error("Error de Tauri: {0}")]
    Tauri(String),
}

pub struct Preprocessed {
    /// Ruta al WAV temporal. El llamador es dueño de borrarlo cuando ya no lo
    /// necesite (lo hace `pipeline` con un `tempfile::TempDir`).
    pub wav_path: PathBuf,
    /// Duración del WAV en segundos, según `ffprobe` interno de FFmpeg.
    /// La calculamos del propio stderr de ffmpeg para no invocar ffprobe aparte.
    pub duration_seconds: f64,
}

pub async fn convert_to_whisper_wav(
    app: &AppHandle,
    source: &Path,
    output_dir: &Path,
) -> Result<Preprocessed, FfmpegError> {
    if !source.exists() {
        return Err(FfmpegError::NotFound(source.to_path_buf()));
    }
    let output = output_dir.join("input.wav");
    if output.exists() {
        let _ = fs::remove_file(&output).await;
    }

    let shell = app.shell();
    let cmd = shell
        .sidecar(FFMPEG_SIDECAR)
        .map_err(|e| FfmpegError::Tauri(e.to_string()))?
        .args([
            "-y", // sobrescribe sin preguntar
            "-i",
            source.to_string_lossy().as_ref(),
            "-vn", // sin video
            "-ac",
            &WHISPER_CHANNELS.to_string(),
            "-ar",
            &WHISPER_SAMPLE_RATE.to_string(),
            "-f",
            "wav",
            "-acodec",
            "pcm_s16le",
            output.to_string_lossy().as_ref(),
        ]);

    let (mut rx, _child) = cmd
        .spawn()
        .map_err(|e| FfmpegError::Tauri(e.to_string()))?;

    let mut stderr_buf = String::new();
    let mut exit_code: Option<i32> = None;
    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stderr(line) => {
                // FFmpeg emite TODO por stderr (también el progreso). Lo
                // acumulamos por si necesitamos un mensaje de error legible.
                stderr_buf.push_str(&String::from_utf8_lossy(&line));
                stderr_buf.push('\n');
            }
            CommandEvent::Stdout(_) => {}
            CommandEvent::Terminated(payload) => {
                exit_code = payload.code;
            }
            CommandEvent::Error(err) => {
                return Err(FfmpegError::Process(err));
            }
            _ => {}
        }
    }
    if exit_code != Some(0) {
        return Err(FfmpegError::Process(extract_ffmpeg_error(&stderr_buf)));
    }

    let duration = parse_duration_from_stderr(&stderr_buf).unwrap_or(0.0);
    Ok(Preprocessed {
        wav_path: output,
        duration_seconds: duration,
    })
}

/// Extrae un trozo del WAV ya pre-procesado entre `start_seconds` y
/// `start_seconds + duration_seconds`. Lo usa el pipeline cuando el audio es
/// largo y lo dividimos en chunks para no agotar la RAM de Whisper.
///
/// El output queda en `output_path`. Usa `-c copy` para no re-codificar y
/// que el split sea instantáneo aunque el WAV pese cientos de MB.
pub async fn extract_chunk(
    app: &AppHandle,
    source_wav: &Path,
    start_seconds: f64,
    duration_seconds: f64,
    output_path: &Path,
) -> Result<(), FfmpegError> {
    if !source_wav.exists() {
        return Err(FfmpegError::NotFound(source_wav.to_path_buf()));
    }
    if output_path.exists() {
        let _ = fs::remove_file(output_path).await;
    }

    let shell = app.shell();
    let cmd = shell
        .sidecar(FFMPEG_SIDECAR)
        .map_err(|e| FfmpegError::Tauri(e.to_string()))?
        .args([
            "-y",
            "-ss",
            &format!("{:.3}", start_seconds),
            "-i",
            source_wav.to_string_lossy().as_ref(),
            "-t",
            &format!("{:.3}", duration_seconds),
            "-c",
            "copy",
            output_path.to_string_lossy().as_ref(),
        ]);

    let (mut rx, _child) = cmd
        .spawn()
        .map_err(|e| FfmpegError::Tauri(e.to_string()))?;

    let mut stderr_buf = String::new();
    let mut exit_code: Option<i32> = None;
    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stderr(line) => {
                stderr_buf.push_str(&String::from_utf8_lossy(&line));
                stderr_buf.push('\n');
            }
            CommandEvent::Terminated(payload) => exit_code = payload.code,
            CommandEvent::Error(err) => return Err(FfmpegError::Process(err)),
            _ => {}
        }
    }
    if exit_code != Some(0) {
        return Err(FfmpegError::Process(extract_ffmpeg_error(&stderr_buf)));
    }
    Ok(())
}

/// FFmpeg emite líneas tipo `Duration: 00:42:13.50, start: …`. La parseamos
/// si está. Si el archivo es streaming sin metadata, devolvemos None y el
/// pipeline calcula la duración luego desde whisper.cpp.
fn parse_duration_from_stderr(stderr: &str) -> Option<f64> {
    for line in stderr.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("Duration:") {
            let token = rest.split(',').next()?.trim();
            return parse_hhmmss(token);
        }
    }
    None
}

fn parse_hhmmss(s: &str) -> Option<f64> {
    let mut parts = s.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let sec: f64 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + sec)
}

fn extract_ffmpeg_error(stderr: &str) -> String {
    // Toma las últimas 3 líneas no vacías de stderr — donde FFmpeg pone el
    // motivo real del fallo (codec no soportado, archivo corrupto, etc.).
    stderr
        .lines()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ")
}
