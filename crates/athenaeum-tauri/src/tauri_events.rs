use athenaeum_core::events::ProgressEmitter;
use tauri::{AppHandle, Emitter};

/// Tauri-backed progress emitter that sends events to the frontend via IPC.
pub struct TauriProgressEmitter(pub AppHandle);

impl ProgressEmitter for TauriProgressEmitter {
    fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
        // A failed send never fails the work that emitted it, but it is not dropped silently either.
        if let Err(e) = self.0.emit(event_name, &payload) {
            tracing::warn!(event = event_name, error = %e, "ui event not delivered");
        }
    }
}
