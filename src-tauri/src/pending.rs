//! Cola en disco de actas pendientes de subir.
//!
//! La transcripción es 100 % local; lo único que necesita internet es el POST
//! final del acta al backend de Amautum. Si ese POST falla por red (tras los
//! reintentos inmediatos del pipeline), NO perdemos el trabajo: serializamos el
//! acta —junto con su callback y token— a `app_data_dir/pending/<jobId>.json` y
//! la reintentamos más tarde (al arrancar el agente, cada cierto tiempo, o
//! cuando el usuario pulsa "Reintentar"). Si la conexión vuelve dentro de la
//! ventana de validez del token (6 h), el job se completa solo.
//!
//! Límite: el token vence a las 6 h. Pasado eso el reintento dará 401 y el
//! archivo queda en disco hasta que se limpie o se re-emita un token (fase 2).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tokio::fs;

use crate::amautum;
use crate::types::{AgentEvent, TranscriptUpload};
use crate::ws_hub::WsHub;

/// Un acta lista que no se pudo subir todavía. Guarda TODO lo necesario para
/// reintentar la subida sin re-procesar el audio.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingUpload {
    pub job_id: String,
    /// URL de callback del transcript (la que dio el backend al crear el job).
    pub transcript_callback: String,
    /// Token Bearer efímero firmado por Amautum (válido ~6 h desde la creación).
    pub token: String,
    /// El acta ya transcrita, lista para subir tal cual.
    pub upload: TranscriptUpload,
    /// Epoch (s) en que se guardó — informativo para la UI.
    pub saved_at: u64,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Carpeta `pending/` dentro del directorio de datos del usuario. Sobrevive a
/// actualizaciones del agente (mismo identifier).
pub async fn pending_dir(app: &AppHandle) -> std::io::Result<PathBuf> {
    let base = app
        .path()
        .app_data_dir()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    let dir = base.join("pending");
    fs::create_dir_all(&dir).await?;
    Ok(dir)
}

fn job_file(dir: &std::path::Path, job_id: &str) -> PathBuf {
    // Sanitizamos por las dudas: el jobId es un UUID, pero no queremos que un
    // valor raro escape de la carpeta.
    let safe: String = job_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    dir.join(format!("{safe}.json"))
}

/// Guarda (o reemplaza) un acta pendiente en disco.
pub async fn save(app: &AppHandle, mut pending: PendingUpload) -> std::io::Result<()> {
    let dir = pending_dir(app).await?;
    pending.saved_at = now_secs();
    let bytes = serde_json::to_vec_pretty(&pending)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let path = job_file(&dir, &pending.job_id);
    fs::write(&path, bytes).await?;
    tracing::info!(job_id = %pending.job_id, ?path, "Acta guardada en disco (pendiente de subir)");
    Ok(())
}

/// IDs de los jobs con acta pendiente en disco.
pub async fn list_job_ids(app: &AppHandle) -> Vec<String> {
    let dir = match pending_dir(app).await {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    if let Ok(mut entries) = fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if let Some(name) = entry.file_name().to_str() {
                if let Some(stem) = name.strip_suffix(".json") {
                    out.push(stem.to_string());
                }
            }
        }
    }
    out
}

/// Resumen de un acta pendiente para pintarla en la ventana del agente. Solo
/// IDs no alcanza: la persona necesita ver de qué acta se trata, cuánto lleva
/// esperando y qué tan grande es lo que está en riesgo.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingSummary {
    pub job_id: String,
    pub saved_at: u64,
    pub duration_seconds: f64,
    pub characters: usize,
    pub language: String,
    pub model: String,
}

pub async fn list_details(app: &AppHandle) -> Vec<PendingSummary> {
    let mut out = Vec::new();
    for job_id in list_job_ids(app).await {
        if let Some(p) = load(app, &job_id).await {
            out.push(PendingSummary {
                job_id: p.job_id,
                saved_at: p.saved_at,
                duration_seconds: p.upload.duration_seconds,
                characters: p.upload.full_text.chars().count(),
                language: p.upload.language,
                model: p.upload.model,
            });
        }
    }
    out.sort_by_key(|p| p.saved_at);
    out
}

async fn load(app: &AppHandle, job_id: &str) -> Option<PendingUpload> {
    let dir = pending_dir(app).await.ok()?;
    let bytes = fs::read(job_file(&dir, job_id)).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

async fn remove(app: &AppHandle, job_id: &str) {
    if let Ok(dir) = pending_dir(app).await {
        let _ = fs::remove_file(job_file(&dir, job_id)).await;
    }
}

/// Reintenta subir UN acta pendiente. Si lo logra, borra el archivo y devuelve
/// el `transcript_id`. Si falla (red, token vencido…), lo deja en disco.
pub async fn retry_one(app: &AppHandle, job_id: &str) -> Option<String> {
    let pending = load(app, job_id).await?;
    match amautum::post_transcript(&pending.transcript_callback, &pending.token, &pending.upload)
        .await
    {
        Ok(resp) => {
            remove(app, job_id).await;
            tracing::info!(job_id, transcript_id = %resp.transcript_id, "Acta pendiente subida con éxito");
            Some(resp.transcript_id)
        }
        Err(e) => {
            tracing::warn!(job_id, error = %e, "Reintento de subida falló — el acta sigue en disco");
            None
        }
    }
}

/// Reintenta TODAS las actas pendientes. Por cada éxito publica `Completed` al
/// WS hub (por si la pestaña sigue abierta) — el backend de todas formas queda
/// con el acta, así que la UI que poolea el listado lo verá completado.
pub async fn retry_all(app: &AppHandle, ws_hub: &WsHub) {
    let ids = list_job_ids(app).await;
    if ids.is_empty() {
        return;
    }
    tracing::info!(count = ids.len(), "Reintentando actas pendientes de subir");
    for job_id in ids {
        if let Some(transcript_id) = retry_one(app, &job_id).await {
            ws_hub.publish(AgentEvent::Completed {
                job_id: job_id.clone(),
                transcript_id,
            });
        }
    }
}
