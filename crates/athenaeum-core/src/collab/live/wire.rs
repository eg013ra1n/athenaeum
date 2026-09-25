//! REST wire shapes of the live exchange (hub plan § Wire contract): holder
//! snapshot/delta/report, the CAS version calls, and the presence beat. All
//! camelCase on the wire. The event-stream types are appended in a later
//! task of this wave.

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
