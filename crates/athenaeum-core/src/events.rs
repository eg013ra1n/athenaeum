use serde::Serialize;

/// Trait for emitting progress events from core business logic.
/// Implementations provide the transport layer (Tauri events, SSE, no-op for tests).
///
/// Uses `serde_json::Value` to remain dyn-compatible (object-safe).
pub trait ProgressEmitter: Send + Sync {
    fn emit_json(&self, event_name: &str, payload: serde_json::Value);
}

/// Convenience function to serialize and emit a typed payload.
pub fn emit_event<T: Serialize>(emitter: &dyn ProgressEmitter, event_name: &str, payload: &T) {
    if let Ok(value) = serde_json::to_value(payload) {
        emitter.emit_json(event_name, value);
    }
}

/// No-op emitter for tests and headless usage.
pub struct NullEmitter;

impl ProgressEmitter for NullEmitter {
    fn emit_json(&self, _event_name: &str, _payload: serde_json::Value) {}
}
