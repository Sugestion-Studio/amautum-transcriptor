//! Cliente HTTP hacia el backend de Amautum. Reusa el mismo `reqwest::Client`
//! (con pool de conexiones) para los tres callbacks de un job. Cada llamada
//! presenta el token efímero recibido en el trigger inicial como `Bearer`.

use once_cell::sync::Lazy;
use reqwest::Client;
use serde::Serialize;

use crate::config;
use crate::types::{FailUpload, ProgressUpload, TranscriptUpload};

#[derive(thiserror::Error, Debug)]
pub enum AmautumError {
    #[error("Error de red al hablar con Amautum: {0}")]
    Network(#[from] reqwest::Error),
    #[error("Amautum rechazó la llamada ({status}): {body}")]
    Rejected { status: u16, body: String },
}

static CLIENT: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .user_agent(config::user_agent())
        .timeout(std::time::Duration::from_secs(60))
        .pool_idle_timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("Could not build reqwest client")
});

pub async fn post_progress(
    callback_url: &str,
    token: &str,
    body: &ProgressUpload,
) -> Result<(), AmautumError> {
    post(callback_url, token, body).await?;
    Ok(())
}

pub async fn post_fail(
    callback_url: &str,
    token: &str,
    body: &FailUpload,
) -> Result<(), AmautumError> {
    post(callback_url, token, body).await?;
    Ok(())
}

#[derive(serde::Deserialize)]
pub struct TranscriptResponse {
    pub ok: bool,
    #[serde(rename = "transcriptId")]
    pub transcript_id: String,
    #[serde(default, rename = "alreadyDone")]
    pub already_done: bool,
}

pub async fn post_transcript(
    callback_url: &str,
    token: &str,
    body: &TranscriptUpload,
) -> Result<TranscriptResponse, AmautumError> {
    let res = post(callback_url, token, body).await?;
    let parsed: TranscriptResponse = res.json().await?;
    Ok(parsed)
}

async fn post<B: Serialize>(
    url: &str,
    token: &str,
    body: &B,
) -> Result<reqwest::Response, AmautumError> {
    let res = CLIENT
        .post(url)
        .bearer_auth(token)
        .json(body)
        .send()
        .await?;
    let status = res.status();
    if !status.is_success() {
        let text = res
            .text()
            .await
            .unwrap_or_else(|_| "(cuerpo no legible)".into());
        return Err(AmautumError::Rejected {
            status: status.as_u16(),
            body: text,
        });
    }
    Ok(res)
}
