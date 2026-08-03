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

/// Asegura en disco los dos modelos ONNX que necesita el diarizador
/// (segmentación + embedding). Devuelve sus rutas. A diferencia de los GGML de
/// whisper —que pueden pesar GBs y justifican una barra de progreso— estos
/// suman ~44 MB y se bajan una sola vez, así que los traemos sin emitir eventos
/// de progreso al WS: solo dejamos rastro en los logs.
pub async fn ensure_diarization_models(
    app: &AppHandle,
) -> Result<(PathBuf, PathBuf), ModelError> {
    let dir = models_dir(app).await?;

    let seg = dir.join(crate::config::DIARIZE_SEGMENTATION_MODEL);
    ensure_downloaded(
        &seg,
        crate::config::DIARIZE_SEGMENTATION_URL,
        crate::config::DIARIZE_SEGMENTATION_MIN_BYTES,
        "segmentación",
    )
    .await?;

    let emb = dir.join(crate::config::DIARIZE_EMBEDDING_MODEL);
    ensure_downloaded(
        &emb,
        crate::config::DIARIZE_EMBEDDING_URL,
        crate::config::DIARIZE_EMBEDDING_MIN_BYTES,
        "embedding de voz",
    )
    .await?;

    Ok((seg, emb))
}

/// Descarga `url` a `dest` si no existe o quedó truncado bajo `min_bytes`.
/// Usa el mismo `.part` + rename atómico que `ensure_model` para no dejar
/// medio-archivos si el agente muere a mitad de la descarga.
async fn ensure_downloaded(
    dest: &Path,
    url: &str,
    min_bytes: u64,
    label: &str,
) -> Result<(), ModelError> {
    if let Ok(meta) = fs::metadata(dest).await {
        if meta.len() >= min_bytes {
            return Ok(());
        }
        let _ = fs::remove_file(dest).await;
    }

    let tmp = dest.with_extension("part");
    let _ = fs::remove_file(&tmp).await;
    tracing::info!(label, url, "Descargando modelo de diarización");

    let res = CLIENT.get(url).send().await?;
    let status = res.status();
    if !status.is_success() {
        return Err(ModelError::Rejected {
            status: status.as_u16(),
            model: format!("diarización/{label}"),
        });
    }

    let mut stream = res.bytes_stream();
    let mut file = fs::File::create(&tmp).await?;
    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        file.write_all(&bytes).await?;
    }
    file.sync_all().await?;
    drop(file);
    fs::rename(&tmp, dest).await?;
    Ok(())
}

/// Tamaño mínimo (en bytes) que esperamos del modelo en disco para considerarlo
/// no-truncado. Valores conservadores: los archivos reales son un poco más
/// grandes, pero quedan suficiente margen para descartar descargas a medias.
/// Cota inferior para considerar un modelo NO truncado. DEBE quedar por debajo
/// del tamaño real del archivo, o el chequeo de "ya descargado" falla siempre y
/// el modelo se re-baja en cada corrida. Usamos ~90 % del tamaño real (medido
/// contra HuggingFace) — suficiente para descartar descargas a medias y con
/// margen de sobra para que un archivo completo pase. Tamaños reales (bytes):
///   tiny 77_691_713 · base 147_951_465 · small 487_601_967 ·
///   medium 1_533_763_059 · large-v3 3_095_033_483
fn min_expected_size(model: WhisperModel) -> u64 {
    match model {
        WhisperModel::Tiny => 60 * 1024 * 1024,        // real ~77.7 MB
        WhisperModel::Base => 120 * 1024 * 1024,       // real ~148 MB
        WhisperModel::Small => 420 * 1024 * 1024,      // real ~487 MB
        WhisperModel::Medium => 1_300 * 1024 * 1024,   // real ~1.53 GB
        WhisperModel::LargeV3 => 2_700 * 1024 * 1024,  // real ~3.10 GB (antes 3000 → re-bajaba siempre)
    }
}

/// Flag con el que cada sidecar dice su versión.
///
/// **No son intercambiables.** `ffmpeg` no conoce `--version`: imprime su banner
/// y sale con **código 8**. `whisper-cli` sí lo conoce y sale con 0. Pasarle a
/// ffmpeg el flag de whisper es lo que rompió el chequeo en la v0.1.11: el
/// agente declaraba `dependenciesOk: false`, el asistente deshabilitaba el botón
/// de elegir archivo, y el cliente instalaba el programa para no poder usarlo —
/// con un ffmpeg que funcionaba perfectamente.
fn version_flag(name: &str) -> &'static str {
    if name == crate::config::FFMPEG_SIDECAR {
        "-version"
    } else {
        "--version"
    }
}

/// Verifica que un sidecar binario sea REALMENTE ejecutable. Lo usamos al
/// arranque del agente para detectar problemas de bundling antes de que el
/// operador encole un job y se choque con ellos a medio camino.
///
/// **Lo que se comprueba es que el binario ARRANQUE Y HABLE**, no que devuelva
/// cero. Esa distinción es todo el chequeo:
///
///   · Un binario sano imprime su versión, aunque el flag no le guste.
///   · Un binario al que le falta una DLL en Windows arranca, muere al instante
///     con `0xC0000135` y **no escribe una sola línea**. Ese es el caso que hay
///     que cazar, y el silencio es su firma.
///
/// Atarse al código de salida en vez de a la salida fue un error caro: un flag
/// equivocado bastaba para declarar roto un componente que funcionaba. Un código
/// distinto de cero se registra como aviso, no como fallo.
pub async fn probe_sidecar(
    app: &AppHandle,
    name: &str,
) -> Result<String, String> {
    use tauri_plugin_shell::process::CommandEvent;
    use tauri_plugin_shell::ShellExt;

    let shell = app.shell();
    let cmd = shell
        .sidecar(name)
        .map_err(|e| e.to_string())?
        .args([version_flag(name)]);
    let (mut rx, _child) = cmd.spawn().map_err(|e| e.to_string())?;

    let mut output = String::new();
    let mut code: Option<i32> = None;
    let mut terminated = false;
    // El probe no puede colgar el arranque del agente: si el binario se queda
    // pensando, lo damos por no verificado y seguimos.
    let deadline = std::time::Duration::from_secs(30);
    loop {
        let next = match tokio::time::timeout(deadline, rx.recv()).await {
            Ok(v) => v,
            Err(_) => {
                return Err(format!(
                    "{name} no respondió en 30 s (¿antivirus inspeccionando el binario?)"
                ))
            }
        };
        let Some(event) = next else { break };
        match event {
            CommandEvent::Stdout(line) | CommandEvent::Stderr(line) => {
                output.push_str(&String::from_utf8_lossy(&line));
                output.push('\n');
            }
            CommandEvent::Terminated(payload) => {
                terminated = true;
                code = payload.code;
            }
            CommandEvent::Error(err) => return Err(err),
            _ => {}
        }
    }

    let first_line = output
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string();

    // El silencio es la única señal fiable de un binario que no puede correr.
    if first_line.is_empty() {
        return Err(match code {
            Some(-1073741515) => format!(
                "{name} no arrancó: falta una biblioteca del sistema (DLL). Instala el «Microsoft \
                 Visual C++ Redistributable (x64)»."
            ),
            Some(c) => format!("{name} terminó con el código {c} sin imprimir nada"),
            None => format!("{name} se cerró sin código de salida (¿bloqueado por antivirus?)"),
        });
    }

    // Habló: el binario corre. Si además salió con un código raro lo dejamos en
    // el log, pero NO se bloquea a la persona por ello — puede ser simplemente
    // que a esta versión del binario no le guste el flag.
    if terminated && matches!(code, Some(c) if c != 0) {
        tracing::warn!(
            sidecar = name,
            code,
            first_line = %first_line,
            "El sidecar respondió pero con código distinto de cero"
        );
    }
    Ok(first_line)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Regresión de la v0.1.11: a `ffmpeg` se le pasaba `--version`, que no
    /// conoce — imprime su banner y sale con **código 8**. El chequeo lo tomó por
    /// un componente roto, el asistente deshabilitó el botón de elegir archivo, y
    /// los clientes instalaron un programa que no podían usar con un ffmpeg
    /// perfectamente sano.
    ///
    /// Si algún día se unifican los flags "por limpieza", esto salta.
    #[test]
    fn ffmpeg_uses_its_own_version_flag() {
        assert_eq!(version_flag(crate::config::FFMPEG_SIDECAR), "-version");
        assert_eq!(version_flag(crate::config::WHISPER_SIDECAR), "--version");
        assert_eq!(version_flag(crate::config::DIARIZE_SIDECAR), "--version");
    }
}
