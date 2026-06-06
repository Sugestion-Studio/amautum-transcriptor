//! Broadcaster de eventos del pipeline hacia los clientes WebSocket.
//!
//! Por qué hub centralizado y no un canal por handler de WS: la pestaña web
//! puede desconectarse y reconectarse a mitad de un job; el job está corriendo
//! en una tarea aparte y necesita un buzón estable donde dejar los eventos.
//! Usamos un `tokio::sync::broadcast` (un canal multi-consumer por jobId) que
//! se mantiene vivo aunque no haya suscriptores: si el navegador refresca,
//! pierde los eventos retrasados pero el siguiente progreso o el `completed`
//! lo agarra otra vez.

use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::broadcast;

use crate::types::AgentEvent;

const CHANNEL_CAPACITY: usize = 128;

#[derive(Clone, Default)]
pub struct WsHub {
    channels: Arc<DashMap<String, broadcast::Sender<AgentEvent>>>,
}

impl WsHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Devuelve (o crea) el sender de eventos para `job_id`. Si era nuevo,
    /// también devuelve un primer receptor para que el pipeline lo retenga
    /// (sin retenerlo, el canal se cerraría si todos los WS se desconectan
    /// antes del primer evento).
    pub fn sender(&self, job_id: &str) -> broadcast::Sender<AgentEvent> {
        if let Some(s) = self.channels.get(job_id) {
            return s.clone();
        }
        let (tx, _rx_keepalive) = broadcast::channel(CHANNEL_CAPACITY);
        self.channels.insert(job_id.to_string(), tx.clone());
        tx
    }

    /// Suscribe a un job existente. Si el job no existe (aún no se ha
    /// arrancado), crea el canal — así el WS puede conectarse ANTES del
    /// trigger sin perder el primer evento.
    pub fn subscribe(&self, job_id: &str) -> broadcast::Receiver<AgentEvent> {
        self.sender(job_id).subscribe()
    }

    /// Publica un evento sin fallar si no hay suscriptores. broadcast::send
    /// devuelve Err cuando no hay receptores; lo tratamos como éxito porque
    /// el WS puede no estar abierto aún (típico en el primer "queued").
    pub fn publish(&self, event: AgentEvent) {
        let job_id = job_id_of(&event).to_string();
        let tx = self.sender(&job_id);
        let _ = tx.send(event);
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
        | AgentEvent::Preprocess { job_id, .. }
        | AgentEvent::Started { job_id, .. }
        | AgentEvent::Progress { job_id, .. }
        | AgentEvent::Uploading { job_id }
        | AgentEvent::Completed { job_id, .. }
        | AgentEvent::Failed { job_id, .. } => job_id,
    }
}
