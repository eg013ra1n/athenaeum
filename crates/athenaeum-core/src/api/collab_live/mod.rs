//! The collab v3 wave-3 live exchange: the feed applier that drives the
//! hub's `GET /me/events` stream. `feed` (Task 5) owns the per-project
//! cursors, REST catch-up, resync, the 60 s versions vector and epoch
//! change; `holdings` (Task 6) is its holder side — the holder map, delta
//! resume, the claim outbox flush and digest reconciliation. Later tasks in
//! this wave add the deletion watcher and the local session that wires the
//! pieces to a running stream.

#[cfg(all(feature = "render", feature = "solver"))]
pub mod feed;
#[cfg(all(feature = "render", feature = "solver"))]
pub mod holdings;
// The storage marker's device-facing half (Task 7): whether a marker names
// one of this account's OWN devices (offering a replace) and the
// device-replace core (retire + marker rewrite + re-adoption by hash).
#[cfg(all(feature = "render", feature = "solver"))]
pub mod replace;
// The collab provider's serve oracle (Task 10): the catalog row + stamp the
// per-request serve check reads (spec §9.3).
#[cfg(all(feature = "render", feature = "solver"))]
pub mod serve_oracle;
// The storage engine (Task 9): the per-frame local state driven from the
// disk — settle, the L4 deletion window and its one reversible choice,
// "lost everywhere", quarantine of changed replicas, re-adoption by hash.
#[cfg(all(feature = "render", feature = "solver"))]
pub mod storage_task;
// The command surface both hosts wrap (Task 16).
#[cfg(all(feature = "render", feature = "solver"))]
pub mod surface;

// The live orchestrator (Task 15): the event session (stream, beat,
// reconnect), the scheduler's executor, and the runtime loop that owns the
// feed, the holder side, the storage engine and the executor.
#[cfg(all(feature = "render", feature = "solver"))]
pub(crate) mod executor;
#[cfg(all(feature = "render", feature = "solver"))]
mod runtime;
#[cfg(all(feature = "render", feature = "solver"))]
pub(crate) mod session;
// The runtime's off-loop workers (Task 15 fix round 1, C1): the ordered
// feed worker and the storage task.
#[cfg(all(feature = "render", feature = "solver"))]
mod workers;
#[cfg(all(feature = "render", feature = "solver"))]
pub(crate) use runtime::{live_presence, live_synced_at};
#[cfg(all(feature = "render", feature = "solver"))]
pub use runtime::{
    notify_local_change, on_sign_out, set_receive_streams, shutdown, spawn_collab_live, status,
    sync_now,
};
#[cfg(all(test, feature = "render", feature = "solver"))]
pub(crate) use runtime::{spawn_with, GateSource, LiveConfig};
// Two live instances end to end on the fake hub (Task 15).
#[cfg(all(test, feature = "render", feature = "solver"))]
mod live_tests;

/// The live exchange's connection state (P27).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum LiveState {
    Off,
    Connecting,
    Live,
    Reconnecting,
    /// Three connects in a row failed; still retrying at the back-off cap.
    Unreachable,
    SignedOut,
    Outdated,
}

/// The Collaboration storage as the live status shows it (§9.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum StorageStateView {
    Available,
    ReadOnly,
    Unavailable,
    /// No Collaboration folder is designated.
    NotSet,
}

/// Payload of [`COLLAB_LIVE_STATUS_EVENT`] and the live status read (P27).
#[derive(Debug, Clone, PartialEq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabLiveStatus {
    pub state: LiveState,
    /// Seconds until the next reconnect attempt, while reconnecting.
    #[ts(type = "number | null")]
    pub retry_in_secs: Option<u64>,
    /// When `state` last changed (RFC 3339).
    pub since: String,
    pub storage: StorageStateView,
    pub storage_reason: Option<String>,
    /// Changes are seen by the periodic check only (§9.2).
    pub watcher_degraded: bool,
    pub network_volume: bool,
}

/// Emitted on every change of [`CollabLiveStatus`].
pub const COLLAB_LIVE_STATUS_EVENT: &str = "collab-live-status";
/// One non-blocking, reversible deletion choice (L4).
pub const COLLAB_DELETION_CHOICE_EVENT: &str = "collab-deletion-choice";
/// A frame no device holds any more (L4) — restore it from the Trash.
pub const COLLAB_FRAME_LOST_EVENT: &str = "collab-frame-lost";
/// A replica changed on disk and was quarantined (L5).
pub const COLLAB_FRAME_CHANGED_EVENT: &str = "collab-frame-changed";
/// A project's attention lists (changed files, not kept, awaiting a
/// choice, lost) changed — reload them.
pub const COLLAB_ATTENTION_EVENT: &str = "collab-attention-changed";
/// The live exchange's flows per project (spec 2026-09-29 §6.4): at most
/// once a second while something moves, then one quiet payload per project.
/// Ids only — names come from `get_collab_exchange`.
pub const COLLAB_EXCHANGE_PROGRESS_EVENT: &str = "collab-exchange-progress";

/// Spec 2026-10-01 §6.2: a project confirmed against the hub (or not).
/// At most one per project per second; a window that saw a failure is
/// never coalesced into an ok.
pub const COLLAB_PROJECT_SYNCED_EVENT: &str = "collab-project-synced";

/// Payload of [`COLLAB_PROJECT_SYNCED_EVENT`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabProjectSynced {
    pub project_id: String,
    /// RFC 3339; the newest successful confirmation in the window.
    pub synced_at: Option<String>,
    pub ok: bool,
    pub error: Option<String>,
    /// Something was applied: manifest rows, members, holders or the
    /// project snapshot (presence never counts).
    pub changed: bool,
}

/// Payload of [`COLLAB_DELETION_CHOICE_EVENT`].
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabDeletionChoice {
    pub count: usize,
    /// Sorted, no duplicates.
    pub project_ids: Vec<String>,
    /// `collab-deletion-choice:<projectId>[,<projectId>…]:<batch id>` — one
    /// key per OCCURRENCE (`notify({ dedupeKey })` suppresses a seen key for
    /// good): a re-emit of the same batch (a replay) is shown once, every new
    /// batch notifies (Task 16 fix round 1).
    pub dedupe_key: String,
}

impl CollabDeletionChoice {
    /// `batch_id`: the storage engine's id of this choice's batch.
    pub fn new(count: usize, mut project_ids: Vec<String>, batch_id: u64) -> Self {
        project_ids.sort();
        project_ids.dedup();
        let dedupe_key = format!(
            "{COLLAB_DELETION_CHOICE_EVENT}:{}:{batch_id}",
            project_ids.join(",")
        );
        Self {
            count,
            project_ids,
            dedupe_key,
        }
    }
}

/// Payload of [`COLLAB_FRAME_LOST_EVENT`].
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabFrameLost {
    pub project_id: String,
    pub frame_uuid: String,
    pub file_name: String,
    /// The frame's file still sits in the PREVIOUS Collaboration folder (a
    /// re-designation, owner rule A): the notice points there instead of
    /// the Trash, and the frame is fetched again if another holder serves it.
    pub in_previous_folder: bool,
    /// That file, when `in_previous_folder`.
    pub previous_path: Option<String>,
}

/// A frame's local state as the frames list shows it (spec §9, L4–L6) — the
/// catalog's `local_state`, spelled as stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum LocalStateView {
    Wanted,
    Held,
    Missing,
    AwaitingChoice,
    Quarantined,
    NotKept,
    Idle,
    OwnHeld,
    OwnMissing,
    OwnChanged,
}

impl From<crate::db::collab_frames::LocalState> for LocalStateView {
    fn from(s: crate::db::collab_frames::LocalState) -> Self {
        use crate::db::collab_frames::LocalState as S;
        match s {
            S::Wanted => Self::Wanted,
            S::Held => Self::Held,
            S::Missing => Self::Missing,
            S::AwaitingChoice => Self::AwaitingChoice,
            S::Quarantined => Self::Quarantined,
            S::NotKept => Self::NotKept,
            S::Idle => Self::Idle,
            S::OwnHeld => Self::OwnHeld,
            S::OwnMissing => Self::OwnMissing,
            S::OwnChanged => Self::OwnChanged,
        }
    }
}

/// Payload of [`COLLAB_FRAME_CHANGED_EVENT`].
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabFrameChanged {
    pub project_id: String,
    pub frame_uuid: String,
    pub file_name: String,
}

/// Payload of [`COLLAB_ATTENTION_EVENT`].
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabAttentionChanged {
    pub project_id: String,
}

// Fixtures shared by every wave-3 `api::collab_live` test module (Tasks
// 7-18): a signed-in `ServiceContext` + `FakeHub` + a real relay-disabled
// iroh node with the Collaboration root mounted.
#[cfg(all(test, feature = "render", feature = "solver"))]
pub(crate) mod test_support;

// Collab landing (Task 11): a new version replaces the old file atomically
// (DIRECT rename of a store-owned blob, or `<target>.athtmp` + rename +
// re-import), nothing lands over a quarantined file. UNGATED — every other
// child is render+solver (see `api/mod.rs`): the wave-2 fetch path in the
// headless `api::collab_exchange` calls it until Task 15.
pub mod landing;
