//! The collab v3 wave-3 live exchange: the feed applier that drives the
//! hub's `GET /me/events` stream. `feed` (Task 5) owns the per-project
//! cursors, REST catch-up, resync, the 60 s versions vector and epoch
//! change; `holdings` (Task 6) is its holder side — the holder map, delta
//! resume, the claim outbox flush and digest reconciliation. Later tasks in
//! this wave add the deletion watcher and the local session that wires the
//! pieces to a running stream.

pub mod feed;
pub mod holdings;
// The storage marker's device-facing half (Task 7): whether a marker names
// one of this account's OWN devices (offering a replace) and the
// device-replace core (retire + marker rewrite + re-adoption by hash).
pub mod replace;
// The collab provider's serve oracle (Task 10): the catalog row + stamp the
// per-request serve check reads (spec §9.3).
pub mod serve_oracle;
// The storage engine (Task 9): the per-frame local state driven from the
// disk — settle, the L4 deletion window and its one reversible choice,
// "lost everywhere", quarantine of changed replicas, re-adoption by hash.
pub mod storage_task;

// Fixtures shared by every wave-3 `api::collab_live` test module (Tasks
// 7-18): a signed-in `ServiceContext` + `FakeHub` + a real relay-disabled
// iroh node with the Collaboration root mounted.
#[cfg(test)]
pub(crate) mod test_support;
