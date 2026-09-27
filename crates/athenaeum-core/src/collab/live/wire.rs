//! REST wire shapes of the live exchange (hub plan § Wire contract): holder
//! snapshot/delta/report, the CAS version calls, and the presence beat, plus
//! the `GET /me/events` event-stream types. All camelCase on the wire.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotFrameWire {
    pub seq: i32,
    pub uuid: String,
    pub content_version: i32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotDeviceWire {
    pub device: String,
    pub display_name: String,
    pub relay_url: Option<String>,
    /// Run-length claims: `[startSeq, runLength, contentVersion]`, sorted,
    /// never overlapping (hub § "Run-length claims").
    pub claims: Vec<[i32; 3]>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldersSnapshotWire {
    pub epoch: String,
    pub holder_seq: i64,
    pub version: i64,
    pub frames: Vec<SnapshotFrameWire>,
    pub devices: Vec<SnapshotDeviceWire>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HolderDeltaWire {
    pub device: String,
    #[serde(default)]
    pub add: Vec<(i32, i32)>,
    #[serde(default)]
    pub rm: Vec<i32>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeltaCursorWire {
    pub since: i64,
    pub after: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HolderDeltaPageWire {
    pub epoch: String,
    pub holder_seq: i64,
    pub floor: i64,
    pub deltas: Vec<HolderDeltaWire>,
    pub has_more: bool,
    pub next: Option<DeltaCursorWire>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimWire {
    pub uuid: String,
    pub content_version: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldersReportWire {
    pub report_seq: i64,
    pub full: bool,
    pub add: Vec<ClaimWire>,
    pub remove: Vec<String>,
    pub digest: String,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldersReportReplyWire {
    pub holder_seq: i64,
    pub digest_match: bool,
    pub next_flush_ms: u64,
    #[serde(default)]
    pub refused: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionInWire {
    pub uuid: String,
    pub expected_version: i32,
    pub blake3: String,
    pub byte_size: i64,
    pub xxh3: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionStatus {
    Ok,
    Conflict,
    NotFound,
    Forbidden,
    /// Amendment A6: the frame may be versioned only by its own device (or,
    /// when that one is out of service, the account's bound device).
    NotPublishingDevice,
    /// Any status this build does not know — a closed enum would fail to
    /// decode the WHOLE batch reply over one new per-entry status.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionResultWire {
    pub uuid: String,
    pub status: VersionStatus,
    pub content_version: i32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionsReplyWire {
    pub project_version: i64,
    pub results: Vec<VersionResultWire>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BeatWire {
    pub session_id: String,
    pub serving: std::collections::BTreeMap<String, bool>,
    pub relay_url: Option<String>,
}

/// Expand snapshot runs `[startSeq, runLength, contentVersion]` into
/// `(frameSeq, contentVersion)` pairs (hub § Wire contract "Run-length claims").
pub fn expand_runs(runs: &[[i32; 3]]) -> impl Iterator<Item = (i32, i32)> + '_ {
    runs.iter()
        .flat_map(|[start, len, cv]| (0..(*len).max(0)).map(move |i| (start + i, *cv)))
}

// ── Event stream (hub § Wire contract, spec §4.1: `GET /me/events`) ────────

/// One connected device's presence, as carried in `hello.projects[*].presence`
/// (exactly the account's currently connected devices for that project — the
/// hub's warm-up window sends `[]`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceEntry {
    pub device: String,
    pub serving: bool,
    pub relay_url: Option<String>,
}

/// One project's slice of the `hello` event — its cursors, digest and
/// presence list as of the moment the stream opened.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloProject {
    pub version: i64,
    pub holder_seq: i64,
    pub claim_count: i64,
    pub claim_digest: String,
    pub report_seq: i64,
    #[serde(default)]
    pub presence: Vec<PresenceEntry>,
}

/// The stream's first event: this device's session id, the account's epoch,
/// and every one of the account's projects now (hub § "first event").
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloEvent {
    pub session_id: String,
    pub epoch: String,
    pub account_id: String,
    pub projects: std::collections::BTreeMap<String, HelloProject>,
}

/// A `project` event's `kinds`: what changed in this commit. Always a subset
/// of `frames, meta, members, thresholds, dictionary, grid`, in that order;
/// `Unknown` is forward compatibility (I3) for a kind this build predates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    Frames,
    Meta,
    Members,
    Thresholds,
    Dictionary,
    Grid,
    #[serde(other)]
    Unknown,
}

/// A project's manifest/meta/members/… version bump. `frames` is present
/// only when every changed row was small enough to inline (≤ 50, all
/// `published`); otherwise `more: true` means pull the manifest delta.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectEvent {
    pub project_id: String,
    pub prev: i64,
    pub version: i64,
    pub kinds: Vec<ChangeKind>,
    #[serde(default)]
    pub frames: Option<Vec<crate::collab::hub_client::FrameViewWire>>,
    #[serde(default)]
    pub more: bool,
}

/// A `holders` event: one commit's per-device claim deltas.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldersEvent {
    pub project_id: String,
    pub prev: i64,
    pub seq: i64,
    pub deltas: Vec<HolderDeltaWire>,
}

/// One device's presence change inside a `presence` event.
/// `connected: false` always carries `serving: false` and the last known
/// `relay_url` (hub § "presence").
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceChange {
    pub device: String,
    pub connected: bool,
    pub serving: bool,
    pub relay_url: Option<String>,
}

/// A `presence` event. `replace: true` fires once, at the end of the hub's
/// warm-up window, and rebuilds the project's presence set from `changes`
/// alone; otherwise each change patches (or adds) one device.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceEvent {
    pub project_id: String,
    #[serde(default)]
    pub replace: bool,
    pub changes: Vec<PresenceChange>,
}

/// An `account` event's kind: this account's own membership on a project.
/// `Unknown` is forward compatibility (I3, T4 fix round 1) for a kind this
/// build predates — callers log and no-op rather than failing to decode the
/// whole event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountKind {
    Joined,
    Left,
    #[serde(other)]
    Unknown,
}

/// An `account` event: the account's own membership on `project_id` changed.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountEvent {
    pub kind: AccountKind,
    pub project_id: String,
}

/// A `resync` event's side: catch this side of the named project up over
/// REST rather than trusting the stream. `Unknown` is forward compatibility
/// (I3, T4 fix round 1) for a side this build predates — callers log and
/// no-op rather than failing to decode the whole event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResyncWhat {
    Project,
    Holders,
    #[serde(other)]
    Unknown,
}

/// A `resync` event: catch up `what` for `project_id` over REST.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResyncEvent {
    pub project_id: String,
    pub what: ResyncWhat,
}

/// A `versions` event: project id → `(version, holderSeq)`, sent every 60 s
/// (hub-owned cadence) so a missed `project`/`holders` event is caught fast.
pub type VersionsEvent = std::collections::BTreeMap<String, (i64, i64)>;

/// One decoded event of the hub's `/me/events` stream (hub § Wire contract).
#[derive(Debug, Clone, PartialEq)]
pub enum LiveEvent {
    Hello(HelloEvent),
    Project(ProjectEvent),
    Holders(HoldersEvent),
    Presence(PresenceEvent),
    Account(AccountEvent),
    Resync(ResyncEvent),
    Versions(VersionsEvent),
    /// An event name this build does not know — forward compatibility (I3);
    /// the stream pump logs and drops it rather than tearing down the
    /// connection.
    Unknown(String),
}

/// Decode one SSE frame's `(event, data)` pair. An unrecognized event name
/// decodes to [`LiveEvent::Unknown`] instead of erroring, so a future hub
/// event never breaks an older app build.
pub fn decode_event(name: &str, data: &str) -> Result<LiveEvent, serde_json::Error> {
    Ok(match name {
        "hello" => LiveEvent::Hello(serde_json::from_str(data)?),
        "project" => LiveEvent::Project(serde_json::from_str(data)?),
        "holders" => LiveEvent::Holders(serde_json::from_str(data)?),
        "presence" => LiveEvent::Presence(serde_json::from_str(data)?),
        "account" => LiveEvent::Account(serde_json::from_str(data)?),
        "resync" => LiveEvent::Resync(serde_json::from_str(data)?),
        "versions" => LiveEvent::Versions(serde_json::from_str(data)?),
        other => LiveEvent::Unknown(other.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_of_the_contract_decodes() {
        let hello = r#"{"sessionId":"0123456789abcdef0123456789abcdef","epoch":"e1","accountId":"acc","projects":{"p1":{"version":42,"holderSeq":17,"claimCount":3,"claimDigest":"00000000000000000000000000000000","reportSeq":120,"presence":[{"device":"AAA=","serving":true,"relayUrl":"https://r"}]}}}"#;
        let LiveEvent::Hello(h) = decode_event("hello", hello).unwrap() else {
            panic!()
        };
        assert_eq!(h.projects["p1"].report_seq, 120);
        let project = r#"{"projectId":"p1","prev":41,"version":42,"kinds":["frames","members","grid"],"more":true}"#;
        let LiveEvent::Project(p) = decode_event("project", project).unwrap() else {
            panic!()
        };
        assert_eq!(
            p.kinds,
            vec![ChangeKind::Frames, ChangeKind::Members, ChangeKind::Grid]
        );
        assert!(p.frames.is_none() && p.more);
        let holders = r#"{"projectId":"p1","prev":16,"seq":17,"deltas":[{"device":"AAA=","add":[[12,1],[13,1]],"rm":[7]}]}"#;
        let LiveEvent::Holders(hd) = decode_event("holders", holders).unwrap() else {
            panic!()
        };
        assert_eq!(hd.deltas[0].add, vec![(12, 1), (13, 1)]);
        let presence = r#"{"projectId":"p1","replace":false,"changes":[{"device":"AAA=","connected":false,"serving":false,"relayUrl":null}]}"#;
        assert!(matches!(
            decode_event("presence", presence).unwrap(),
            LiveEvent::Presence(_)
        ));
        assert!(matches!(
            decode_event("account", r#"{"kind":"left","projectId":"p1"}"#).unwrap(),
            LiveEvent::Account(AccountEvent {
                kind: AccountKind::Left,
                ..
            })
        ));
        assert!(matches!(
            decode_event("resync", r#"{"projectId":"p1","what":"holders"}"#).unwrap(),
            LiveEvent::Resync(ResyncEvent {
                what: ResyncWhat::Holders,
                ..
            })
        ));
        let LiveEvent::Versions(v) = decode_event("versions", r#"{"p1":[42,17]}"#).unwrap() else {
            panic!()
        };
        assert_eq!(v["p1"], (42, 17));
        assert!(
            matches!(decode_event("future", "{}").unwrap(), LiveEvent::Unknown(n) if n == "future")
        );
    }
}
