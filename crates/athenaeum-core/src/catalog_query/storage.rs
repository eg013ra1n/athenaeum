//! Files and bytes of a scope per scan root, per folder and per archive state (MCP plan, Task 3, O4). Files in the
//! Black Hole are reported apart and left out of every other line: the app's folder overview counts them, a
//! question about where the catalog's bytes live does not.

use std::collections::BTreeMap;

use rusqlite::Connection;
use serde::Serialize;

use super::{none_of, ScopedFrame, GROUPS_SHOWN};
use crate::scope::FrameState;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageLine {
    pub label: String,
    pub files: usize,
    pub bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageSummary {
    /// By the scan root the file lives under, most bytes first.
    pub by_scan_root: Vec<StorageLine>,
    /// At most [`GROUPS_SHOWN`] folders, most bytes first.
    pub by_folder: Vec<StorageLine>,
    /// The folders not listed, summed (`N other folders`); `None` when all are listed.
    pub other_folders: Option<StorageLine>,
    /// The three states outside the Black Hole that hold files: (state, files, bytes).
    pub by_state: Vec<(FrameState, usize, i64)>,
    /// The files in the Black Hole: (files, bytes). In no other line.
    pub black_hole: (usize, i64),
}

fn lines(totals: BTreeMap<String, (usize, i64)>) -> Vec<StorageLine> {
    let mut out: Vec<StorageLine> =
        totals.into_iter().map(|(label, (files, bytes))| StorageLine { label, files, bytes }).collect();
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.label.cmp(&b.label)));
    out
}

fn add(map: &mut BTreeMap<String, (usize, i64)>, key: String, bytes: i64) {
    let e = map.entry(key).or_default();
    e.0 += 1;
    e.1 += bytes;
}

/// The storage of `frames`. Everything it reports is in the frames themselves (their size, folder, scan root and
/// state); `conn` is the reader connection (R11) a question is handed, kept so the signature matches the others.
pub fn storage_summary(_conn: &Connection, frames: &[ScopedFrame]) -> anyhow::Result<StorageSummary> {
    let mut roots: BTreeMap<String, (usize, i64)> = BTreeMap::new();
    let mut folders: BTreeMap<String, (usize, i64)> = BTreeMap::new();
    let mut states: BTreeMap<u8, (FrameState, usize, i64)> = BTreeMap::new();
    let mut black_hole = (0usize, 0i64);

    for f in frames {
        let bytes = f.file.file.size;
        if f.state == FrameState::BlackHole {
            black_hole.0 += 1;
            black_hole.1 += bytes;
            continue;
        }
        add(&mut roots, f.scan_root.as_ref().map_or_else(|| none_of("scan root"), |(p, _)| p.clone()), bytes);
        add(&mut folders, f.folder.clone(), bytes);
        let order = match f.state {
            FrameState::Catalog => 0,
            FrameState::SoftArchived => 1,
            _ => 2,
        };
        let e = states.entry(order).or_insert((f.state, 0, 0));
        e.1 += 1;
        e.2 += bytes;
    }

    let mut by_folder = lines(folders);
    let other_folders = (by_folder.len() > GROUPS_SHOWN).then(|| {
        let rest = by_folder.split_off(GROUPS_SHOWN);
        StorageLine {
            label: format!("{} other folders", rest.len()),
            files: rest.iter().map(|l| l.files).sum(),
            bytes: rest.iter().map(|l| l.bytes).sum(),
        }
    });

    Ok(StorageSummary {
        by_scan_root: lines(roots),
        by_folder,
        other_folders,
        by_state: states.into_values().collect(),
        black_hole,
    })
}
