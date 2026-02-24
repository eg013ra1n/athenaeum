use athenaeum_core::events::ProgressEmitter;
use tokio::sync::broadcast;

/// An SSE event payload to be broadcast to connected clients.
#[derive(Clone, Debug)]
pub struct SseEvent {
    pub event_name: String,
    pub data: serde_json::Value,
}

/// ProgressEmitter implementation that broadcasts events via SSE.
pub struct SseProgressEmitter {
    tx: broadcast::Sender<SseEvent>,
}

impl SseProgressEmitter {
    pub fn new(tx: broadcast::Sender<SseEvent>) -> Self {
        Self { tx }
    }
}

impl ProgressEmitter for SseProgressEmitter {
    fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
        let _ = self.tx.send(SseEvent {
            event_name: event_name.to_string(),
            data: payload,
        });
    }
}
