//! The collab v3 wave-3 live exchange: the feed applier that drives the
//! hub's `GET /me/events` stream. `feed` (Task 5) owns the per-project
//! cursors, REST catch-up, resync, the 60 s versions vector and epoch
//! change; `holdings` (Task 6) is its holder side — the holder map, delta
//! resume, the claim outbox flush and digest reconciliation. Later tasks in
//! this wave add the deletion watcher and the local session that wires the
//! pieces to a running stream.

pub mod feed;
pub mod holdings;
