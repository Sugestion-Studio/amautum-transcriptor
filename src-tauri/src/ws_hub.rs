//! Broadcaster de eventos del pipeline hacia los clientes WebSocket.
//!
//! Por qué hub centralizado y no un canal por handler de WS: la pestaña web
//! puede desconectarse y reconectarse a mitad de un job; el job está corriendo
//! en una tarea aparte y necesita un buzón estable donde dejar los eventos.
//! Usamos un `tokio::sync::broadcast` (un canal multi-consumer por jobId).
//!
//! **Replay sticky**: además del broadcast vivo, guardamos el último evento
//! por job. Cuando un nuevo cliente se suscribe, le enviamos ese último evento
//! antes de empezar a recibir los nuevos. Sin esto se producía una race entre
//! la llamada HTTP `/jobs/start` (que dispara el pipeline) y la conexión WS
//! que viene unos ms después: los primeros eventos (`queued`, `preprocess`,
//! `started`) se publicaban antes de que el cliente estuviera escuchando y se
//! perdían — el panel mostraba "Esperando agente" durante 30s+ aunque el job
//! estuviera corriendo perfectamente.

use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::broadcast;

use crate::types::AgentEvent;

const CHANNEL_CAPACITY: usize = 128;

#[derive(Clone)]
struct JobChannel {
    tx: broadcast::Sender<AgentEvent>,
    /// Último evento publicado en este job, para replay al siguiente suscriptor.
    last_event: Arc<parking_lot::Mutex<Option<AgentEvent>>>,
}

#[derive(Clone, Default)]
pub struct WsHub {
    channels: Arc<DashMap<String, JobChannel>>,
}

impl WsHub {
    pub fn new() -> Self {
        Self::default()
    }

    fn channel(&self, job_id: &str) -> JobChannel {
        if let Some(c) = self.channels.get(job_id) {
            return c.clone();
        }
        let (tx, _rx_keepalive) = broadcast::channel(CHANNEL_CAPACITY);
        let channel = JobChannel {
            tx,
            last_event: Arc::new(parking_lot::Mutex::new(None)),
        };
        self.channels.insert(job_id.to_string(), channel.clone());
        channel
    }

    /// Devuelve el sender de eventos para `job_id`. Compatibilidad con código
    /// que retiene el sender mientras dura el pipeline (evita que el canal
    /// muera si todos los WS se desconectan antes del primer evento).
    pub fn sender(&self, job_id: &str) -> broadcast::Sender<AgentEvent> {
        self.channel(job_id).tx
    }

    /// Suscribe a un job. Si ya hubo un evento publicado en este job, lo
    /// devolvemos como `Some(...)` para que el suscriptor lo procese
    /// inmediatamente antes de empezar a leer del receiver. Esto cierra la
    /// race con `queued` / `preprocess` / `started`.
    pub fn subscribe(&self, job_id: &str) -> (broadcast::Receiver<AgentEvent>, Option<AgentEvent>) {
        let channel = self.channel(job_id);
        let last = channel.last_event.lock().clone();
        (channel.tx.subscribe(), last)
    }

    /// Publica un evento. Lo guardamos como "último evento" antes de hacer el
    /// broadcast — si nadie está suscrito, el broadcast falla silenciosamente
    /// pero el siguiente suscriptor lo recibe en su replay.
    pub fn publish(&self, event: AgentEvent) {
        let job_id = job_id_of(&event).to_string();
        let channel = self.channel(&job_id);
        *channel.last_event.lock() = Some(event.clone());
        let _ = channel.tx.send(event);
    }

    /// Limpia el canal cuando el pipeline termina (éxito o fallo). El WS que
    /// esté conectado recibe un `Closed` y puede cerrar limpio.
    pub fn close(&self, job_id: &str) {
        self.channels.remove(job_id);
    }
}

fn job_id_of(event: &AgentEvent) -> &str {
    match event {
        AgentEvent::Queued { job_id }
        | AgentEvent::ModelDownload { job_id, .. }
        | AgentEvent::Preprocess { job_id, .. }
        | AgentEvent::Started { job_id, .. }
        | AgentEvent::Progress { job_id, .. }
        | AgentEvent::Uploading { job_id }
        | AgentEvent::Completed { job_id, .. }
        | AgentEvent::Failed { job_id, .. } => job_id,
    }
}
