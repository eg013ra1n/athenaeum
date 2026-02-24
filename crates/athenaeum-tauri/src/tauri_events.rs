use athenaeum_core::events::ProgressEmitter;
use tauri::{AppHandle, Emitter};

/// Tauri-backed progress emitter that sends events to the frontend via IPC.
pub struct TauriProgressEmitter(pub AppHandle);

impl ProgressEmitter for TauriProgressEmitter {
    fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
        let _ = self.0.emit(event_name, &payload);
    }
}
