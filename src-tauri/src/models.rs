//! Descarga y custodia de los modelos GGML de whisper.cpp.
//!
//! Si el modelo seleccionado por el operador (tiny, base, medium, large-v3…)
//! no está en disco, lo bajamos desde HuggingFace al directorio de datos del
//! usuario (`AppDataDir`). El descargas siguen reportando progreso al WS hub
//! para que el navegador muestre una barra y el operador sepa qué pasa.
//!
//! Una vez descargado, queda cacheado para corridas futuras del mismo modelo.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::StreamExt;
use once_cell::sync::Lazy;
use reqwest::Client;
use tauri::AppHandle;
use tauri::Manager;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::types::{AgentEvent, WhisperModel};
use crate::ws_hub::WsHub;

#[derive(thiserror::Error, Debug)]
pub enum ModelError {
    #[error("Error de red al descargar el modelo: {0}")]
    Network(#[from] reqwest::Error),
    #[error("Error de E/S: {0}")]
    Io(#[from] std::io::Error),
    #[error("HuggingFace devolvió {status} al bajar {model}")]
    Rejected { status: u16, model: String },
    #[error("Tauri no pudo resolver el directorio de datos: {0}")]
    Path(String),
}

static CLIENT: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .user_agent(crate::config::user_agent())
        .timeout(std::time::Duration::from_secs(60 * 60))
        .build()
        .expect("Could not build reqwest client for model downloads")
});

/// Resuelve la carpeta `models/` dentro del directorio de datos del usuario.
/// La creamos si no existe. Esa carpeta sobrevive a actualizaciones del
/// agente para que el usuario no tenga que volver a bajar el modelo cada
/// versión.
pub async fn models_dir(app: &AppHandle) -> Result<PathBuf, ModelError> {
    let base = app
        .path()
        .app_data_dir()
        .map_err(|e| ModelError::Path(e.to_string()))?;
    let dir = base.join("models");
    fs::create_dir_all(&dir).await?;
    Ok(dir)
}

/// Comprueba si el modelo ya está en disco; si no, lo descarga publicando
/// eventos de progreso al WS hub para el jobId dado.
pub async fn ensure_model(
    app: &AppHandle,
    model: WhisperModel,
    job_id: &str,
    ws_hub: Arc<WsHub>,
) -> Result<PathBuf, ModelError> {
    let dir = models_dir(app).await?;
    let dest = dir.join(model.ggml_filename());
    if dest.exists() {
        // Modelos pueden quedarse a medio descargar si el operador cierra el
        // agente a mitad. Validamos con un mínimo razonable y, si está
        // sospechosamente chico, re-descargamos.
        if let Ok(meta) = fs::metadata(&dest).await {
            if meta.len() >= min_expected_size(model) {
                return Ok(dest);
            }
            let _ = fs::remove_file(&dest).await;
        }
    }

    let url = download_url(model);
    let tmp = dir.join(format!("{}.part", model.ggml_filename()));
    let _ = fs::remove_file(&tmp).await;

    tracing::info!(model = model.as_str(), url, "Descargando modelo Whisper");

    let res = CLIENT.get(&url).send().await?;
    let status = res.status();
    if !status.is_success() {
        return Err(ModelError::Rejected {
            status: status.as_u16(),
            model: model.as_str().to_string(),
        });
    }

    let total = res.content_length();
    let mut stream = res.bytes_stream();
    let mut file = fs::File::create(&tmp).await?;
    let mut downloaded: u64 = 0;
    let mut last_report = 0u8;

    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        file.write_all(&bytes).await?;
        downloaded += bytes.len() as u64;

        let percent = match total {
            Some(t) if t > 0 => ((downloaded * 100) / t).min(100) as u8,
            _ => 0,
        };

        // Throttle de eventos: solo cada salto de 2% en porcentaje. Con HF
        // sirviendo a 50+ MB/s tendríamos miles de eventos/s sin throttle.
        if percent >= last_report + 2 || (last_report == 0 && downloaded > 0) {
            last_report = percent;
            ws_hub.publish(AgentEvent::ModelDownload {
                job_id: job_id.to_string(),
                model: model.as_str().to_string(),
                downloaded_bytes: downloaded,
                total_bytes: total,
                percent,
            });
        }
    }

    file.sync_all().await?;
    drop(file);
    fs::rename(&tmp, &dest).await?;

    // Anunciamos 100% explícito para que la web pinte el step completado.
    ws_hub.publish(AgentEvent::ModelDownload {
        job_id: job_id.to_string(),
        model: model.as_str().to_string(),
        downloaded_bytes: downloaded,
        total_bytes: total,
        percent: 100,
    });

    Ok(dest)
}

fn download_url(model: WhisperModel) -> String {
    format!(
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}",
        model.ggml_filename()
    )
}

/// Tamaño mínimo (en bytes) que esperamos del modelo en disco para considerarlo
/// no-truncado. Valores conservadores: los archivos reales son un poco más
/// grandes, pero quedan suficiente margen para descartar descargas a medias.
fn min_expected_size(model: WhisperModel) -> u64 {
    match model {
        WhisperModel::Tiny => 70 * 1024 * 1024,        // ~75 MB
        WhisperModel::Base => 140 * 1024 * 1024,       // ~142 MB
        WhisperModel::Small => 460 * 1024 * 1024,      // ~466 MB
        WhisperModel::Medium => 1_400 * 1024 * 1024,   // ~1.5 GB
        WhisperModel::LargeV3 => 3_000 * 1024 * 1024,  // ~3.1 GB
    }
}

/// Verifica que un sidecar binario sea ejecutable corriendo `--version`. Lo
/// usamos al arranque del agente para detectar problemas de bundling antes de
/// que el operador encole un job y se choque con ellos a medio camino.
pub async fn probe_sidecar(
    app: &AppHandle,
    name: &str,
) -> Result<String, String> {
    use tauri_plugin_shell::process::CommandEvent;
    use tauri_plugin_shell::ShellExt;

    let shell = app.shell();
    let cmd = shell.sidecar(name).map_err(|e| e.to_string())?.args(["--version"]);
    let (mut rx, _child) = cmd.spawn().map_err(|e| e.to_string())?;

    let mut output = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(line) | CommandEvent::Stderr(line) => {
                output.push_str(&String::from_utf8_lossy(&line));
            }
            CommandEvent::Terminated(_) => break,
            CommandEvent::Error(err) => return Err(err),
            _ => {}
        }
    }
    Ok(output.lines().next().unwrap_or("").trim().to_string())
}

/// Devuelve el primer modelo presente en disco (si existe alguno), útil para
/// el endpoint /diagnostics.
pub async fn first_available_model(dir: &Path) -> Option<String> {
    let mut entries = match fs::read_dir(dir).await {
        Ok(e) => e,
        Err(_) => return None,
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Some(name) = entry.file_name().to_str() {
            if name.starts_with("ggml-") && name.ends_with(".bin") {
                return Some(name.to_string());
            }
        }
    }
    None
}
