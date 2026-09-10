//! Integration groups (spec §2 "Grouping keys", rewritten 2026-09-10 —
//! owner decision: "different cameras can be integrated together, as long
//! as exposure (within the exposure threshold set in the integration
//! settings), camera TYPE (colour vs mono) and filter match"): partition a
//! frame set's LIGHT membership into the buckets the pipeline runs stage
//! 1–9 over — colour mode × filter × binning × exposure cluster. The
//! physical camera (`INSTRUME`) and native geometry (`NAXIS1/2`) are NOT
//! keys any more: a group may mix frames from several cameras/sensor sizes,
//! and registration warps every included frame onto the ONE run-wide
//! reference regardless. Exposure ALWAYS splits a group — there is no
//! opt-out toggle. Pure DB read, no disk I/O, no config resolution beyond
//! the `GroupingConfig` the caller already resolved (`config.rs`, Plan 5a).

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::archive::path_layout::sanitize_for_filename;
use crate::calibration_library::paths::fmt_num;
use crate::stacking::config::GroupingConfig;

/// Mono vs colour (CFA) sensor, decided from `frames.bayerpat` (spec §2:
/// "colour mode ... from the Bayer cards"). A non-empty, trimmed `bayerpat`
/// is OSC; anything else — `NULL`, empty, or whitespace-only — is mono.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum ColorMode {
    Mono,
    Osc,
}

/// One LIGHT frame as it sits inside an [`IntegrationGroup`] — the catalog
/// identity (`frame_id`/`file_id`), the file location the later stages read
/// from, the two per-frame fields the grouping/clustering logic itself
/// consumes (`exposure_s`, `date_obs`), and this frame's own native
/// geometry (`width`/`height` — a group's members may differ now that
/// camera/geometry are not grouping keys; `0` when `NAXIS1`/`NAXIS2` is
/// absent, the same fallback the old group-level fields used).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct GroupFrame {
    pub frame_id: i64,
    pub file_id: i64,
    pub filename: String,
    pub path: String,
    pub size: i64,
    pub modified_at: String,
    pub exposure_s: Option<f64>,
    pub date_obs: Option<String>,
    pub width: i64,
    pub height: i64,
}

/// One integration group: every LIGHT frame sharing colour mode, filter,
/// binning and an exposure cluster (owner decision 2026-09-10) — the unit
/// stage 1–9 run over, one master light per group. Camera and native
/// geometry are NOT part of what makes a group; `cameras` lists every
/// camera actually present (a group can mix them), and `instrume` is a
/// DISPLAY value only (the reference-anchor member's own camera — see
/// [`group_frames`]'s doc for how the anchor is chosen).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationGroup {
    pub key: String,
    /// The reference-anchor member's own camera (display/header only —
    /// never sanitized, never part of the key). `None` when that member
    /// carries no usable `INSTRUME`.
    pub instrume: Option<String>,
    pub color_mode: ColorMode,
    pub filter: Option<String>,
    pub binning: i64,
    /// Every distinct camera actually present in this group — trimmed raw
    /// labels (not sanitized), sorted, deduped. A single-camera group still
    /// carries exactly one entry here.
    pub cameras: Vec<String>,
    pub exposure_s: Option<f64>,
    pub frames: Vec<GroupFrame>,
    pub total_exposure_s: f64,
}

/// The stable group-key string (spec §2, owner decision 2026-09-10):
/// `<mono|osc>__<filter>__bin<n>__<exposure cluster>`, used in paths and
/// `stacking_run_groups` rows. Camera and geometry are deliberately absent —
/// a group can mix cameras/sensor sizes by design.
///
/// `filter` sanitizes through [`sanitize_for_filename`] the same way the
/// calibration library names its master files, so a group key and a master
/// filename token never disagree over the same filter string. The exposure
/// cluster's token reuses [`fmt_num`] — the SAME sub-second-exposure
/// formatter the calibration library's own naming and `plan.rs`'s
/// calibration-set label already use (`180s`, `0.39s`) — so a group key, a
/// calibration-set label and a master filename never format an exposure
/// three different ways. `exposure_s = None` means "no `EXPTIME`", not "no
/// exposure splitting" (there is no such mode any more): those frames get
/// their own cluster, keyed literally `unknown`.
///
/// This is the sanitizing half of key construction; [`compose_key`] is the
/// formatting half, shared with `group_frames`, which already has its
/// tokens (computed once, at bucketing time) and must not re-sanitize.
pub fn group_key(
    color: ColorMode,
    filter: Option<&str>,
    binning: i64,
    exposure_s: Option<f64>,
) -> String {
    let filt = sanitized_or(filter, "NoFilter");
    compose_key(color, &filt, binning, exposure_s)
}

/// Format the group-key string from an ALREADY-sanitized filter token — the
/// one formatting authority both [`group_key`] (sanitizes the raw catalog
/// value itself) and `group_frames` (sanitizes once per bucket, then reuses
/// ITS OWN bucket's token here) go through, so a group's key can never
/// disagree with the token its own bucket was built from.
fn compose_key(
    color: ColorMode,
    filter_token: &str,
    binning: i64,
    exposure_s: Option<f64>,
) -> String {
    let color_tok = match color {
        ColorMode::Mono => "mono",
        ColorMode::Osc => "osc",
    };
    format!(
        "{color_tok}__{filter_token}__bin{binning}__{}",
        exposure_token(exposure_s)
    )
}

/// The exposure component of a group key / master filename: the cluster's
/// first exposure formatted via [`fmt_num`] plus `s` (`"180s"`, `"0.39s"`),
/// or the literal `"unknown"` for the cluster of frames with no `EXPTIME`.
fn exposure_token(exposure_s: Option<f64>) -> String {
    match exposure_s {
        Some(e) => format!("{}s", fmt_num(e)),
        None => "unknown".to_string(),
    }
}

/// `sanitize_for_filename` a possibly-absent, possibly-blank value, falling
/// back to `fallback` when the value is `None`, empty/whitespace, or
/// sanitizes away to nothing.
fn sanitized_or(value: Option<&str>, fallback: &str) -> String {
    let trimmed = value.map(str::trim).filter(|s| !s.is_empty());
    match trimmed {
        Some(s) => {
            let sanitized = sanitize_for_filename(s);
            if sanitized.is_empty() {
                fallback.to_string()
            } else {
                sanitized
            }
        }
        None => fallback.to_string(),
    }
}

/// Trim a possibly-absent catalog value, collapsing an empty/whitespace-only
/// string to `None` — the "honest raw label" a group carries in
/// `IntegrationGroup.instrume`/`filter`/`cameras` (`'Ha'`, not the sanitized
/// token `sanitized_or` computes for the KEY).
fn trimmed_or_none(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Sluggify a frame set's name for use in stacking working/output paths.
/// `sanitize_for_filename`, `"set"` when that sanitizes away to nothing (an
/// all-whitespace or empty name).
pub fn set_slug(name: &str) -> String {
    let s = sanitize_for_filename(name);
    if s.is_empty() {
        "set".to_string()
    } else {
        s
    }
}

/// One row of the LIGHT-membership join — the catalog columns grouping
/// needs, pulled once per frame set. Mirrors
/// `api::lights::load_light_members`'s join (`session_members → sessions →
/// imaging_nights(frames_set_id) → frames(imagetyp = 'Light') → files`) plus
/// the six extra columns groups need that the lights export path does not.
struct LightMember {
    frame_id: i64,
    file_id: i64,
    filename: String,
    path: String,
    size: i64,
    modified_at: String,
    instrume: Option<String>,
    filter: Option<String>,
    xbinning: Option<i64>,
    naxis1: Option<i64>,
    naxis2: Option<i64>,
    exptime: Option<f64>,
    date_obs: Option<String>,
    bayerpat: Option<String>,
}

fn load_group_members(conn: &Connection, frames_set_id: i64) -> Result<Vec<LightMember>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT f.id, fi.id, fi.filename, fi.path, fi.size, fi.modified_at,
                f.instrume, f.filter, f.xbinning, f.naxis1, f.naxis2, f.exptime, f.date_obs, f.bayerpat
         FROM session_members sm
         JOIN sessions s ON s.id = sm.session_id
         JOIN imaging_nights ino ON ino.id = s.imaging_night_id
         JOIN frames f ON f.id = sm.frame_id
         JOIN files fi ON fi.id = f.file_id
         WHERE ino.frames_set_id = ?1 AND f.imagetyp = 'Light'
         ORDER BY f.date_obs, f.id",
    )?;
    let rows = stmt
        .query_map(params![frames_set_id], |r| {
            Ok(LightMember {
                frame_id: r.get(0)?,
                file_id: r.get(1)?,
                filename: r.get(2)?,
                path: r.get(3)?,
                size: r.get(4)?,
                modified_at: r.get(5)?,
                instrume: r.get(6)?,
                filter: r.get(7)?,
                xbinning: r.get(8)?,
                naxis1: r.get(9)?,
                naxis2: r.get(10)?,
                exptime: r.get(11)?,
                date_obs: r.get(12)?,
                bayerpat: r.get(13)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn color_mode_of(bayerpat: &Option<String>) -> ColorMode {
    match bayerpat.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => ColorMode::Osc,
        _ => ColorMode::Mono,
    }
}

fn to_group_frame(m: &LightMember) -> GroupFrame {
    GroupFrame {
        frame_id: m.frame_id,
        file_id: m.file_id,
        filename: m.filename.clone(),
        path: m.path.clone(),
        size: m.size,
        modified_at: m.modified_at.clone(),
        exposure_s: m.exptime,
        date_obs: m.date_obs.clone(),
        width: m.naxis1.unwrap_or(0),
        height: m.naxis2.unwrap_or(0),
    }
}

/// Partition one bucket's members into exposure clusters (spec §2, owner
/// decision 2026-09-10 — exposure ALWAYS splits, there is no opt-out any
/// more). Members with a known `EXPTIME`: sort ascending, then start a new
/// cluster whenever the next exposure is more than `exposure_tolerance_sec`
/// past the CURRENT cluster's first (label) value — not the previous
/// frame's, so a slow drift across many frames cannot walk arbitrarily far
/// from where the cluster started. Members with NO `EXPTIME` never join a
/// numeric cluster — a missing value is not "0 s", and conflating the two
/// would silently merge a real bias-like 0 s exposure with a frame whose
/// header simply lacks the keyword — they form their own single cluster,
/// labelled `None` (`"unknown"` in the key; `build_plan` turns this into a
/// warning naming the frames, never a blocker). Each cluster's member
/// indices are restored to `members`' own order (date_obs then id) before
/// being handed back, so the exposure sort used to detect clusters never
/// leaks into a group's frame order.
///
/// `cfg.exposure_tolerance_sec` itself is trusted here — `config::
/// resolve_config` (fix round 1, minor 5) already clamps it to
/// [`crate::stacking::config::MIN_EXPOSURE_TOLERANCE_SEC`] before a
/// `GroupingConfig` ever reaches this function, so a zero/negative/NaN
/// stored value (which would let two distinct clusters format to the same
/// `fmt_num` token and collide on `stacking_run_groups`'s
/// `UNIQUE(run_id, group_key)`) can never arrive here.
fn cluster_indices(
    members: &[LightMember],
    cfg: &GroupingConfig,
) -> Vec<(Option<f64>, Vec<usize>)> {
    let mut known: Vec<usize> = Vec::new();
    let mut unknown: Vec<usize> = Vec::new();
    for (i, m) in members.iter().enumerate() {
        if m.exptime.is_some() {
            known.push(i);
        } else {
            unknown.push(i);
        }
    }
    known.sort_by(|&a, &b| {
        members[a]
            .exptime
            .partial_cmp(&members[b].exptime)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut clusters: Vec<(f64, Vec<usize>)> = Vec::new();
    for idx in known {
        let e = members[idx].exptime.expect("filtered to Some above");
        match clusters.last_mut() {
            Some(last) if e - last.0 <= cfg.exposure_tolerance_sec => last.1.push(idx),
            _ => clusters.push((e, vec![idx])),
        }
    }
    for (_, idxs) in &mut clusters {
        idxs.sort_unstable();
    }
    let mut result: Vec<(Option<f64>, Vec<usize>)> = clusters
        .into_iter()
        .map(|(label, idxs)| (Some(label), idxs))
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        result.push((None, unknown));
    }
    result
}

/// One bucket: every LIGHT member sharing a (sanitized-token) colour /
/// filter / binning combination, plus one representative "honest" raw
/// filter label (`filter_raw`, trimmed but NOT sanitized) — the first
/// member's own spelling wins, since the SQL pulls members in
/// `(date_obs, id)` order and that's as good a tie-break as any for a value
/// that, by construction, every member of the bucket agrees on ONLY after
/// sanitizing. Camera is deliberately absent here — it is not a bucketing
/// axis any more, and a bucket's members can carry several different ones.
#[derive(Default)]
struct Bucket {
    filter_raw: Option<String>,
    members: Vec<LightMember>,
}

/// Every integration group in a frame set (spec §2, owner decision
/// 2026-09-10): the LIGHT membership, bucketed by colour mode / filter /
/// binning, then split into exposure clusters, then sorted by frame count
/// desc, total exposure desc, key — so the largest, most-representative
/// group always leads.
///
/// Bucketing keys on the SAME sanitized filter token [`group_key`] emits
/// (computed once per member here, then reused via [`compose_key`] when a
/// group is built), not on the raw `frames.filter` value — that can
/// disagree between rows that must still be one group (`NULL` vs `''` vs
/// `'Ha '` vs `'Ha'`; a trailing-space filter name from an XISF header,
/// which stores keyword values verbatim, or user-typed metadata). Bucketing
/// on the raw value would silently split one logical group into several,
/// each getting the SAME final key string once `group_key` sanitized it —
/// two `IntegrationGroup`s colliding on one `stacking_run_groups(run_id,
/// group_key)` row and on one output path. Keying on the token instead makes
/// "one key ⇔ one group" true by construction.
///
/// Each final group's "reference-anchor member" — the source of its
/// `instrume` display value — is the first member of its own cluster in
/// `(date_obs, id)` order (the cluster's own restored order, see
/// [`cluster_indices`]). Plan time has no weights to pick a real
/// best-weighted member from (spec's own "at plan time the first member"
/// note); a run may later prefer a different member once stage 3 has
/// weighed the group, but that does not change this DISPLAY field.
pub fn group_frames(
    conn: &Connection,
    frames_set_id: i64,
    cfg: &GroupingConfig,
) -> Result<Vec<IntegrationGroup>> {
    let members = load_group_members(conn, frames_set_id)?;
    let total_frames = members.len();

    // Bucketed by a plain tuple rather than a dedicated struct: `ColorMode`
    // deliberately does not derive `Hash` (it is a wire type, not a map key
    // elsewhere), so the colour axis rides along as a `bool` (`true` = OSC)
    // instead of pulling that derive in just for this. `filter` is the
    // SANITIZED token (never `None` — `sanitized_or` always resolves to a
    // concrete string, its own fallback included), so two members can only
    // land in different buckets when `group_key` would actually emit
    // different keys for them.
    type BucketKey = (bool, String, i64);
    let mut buckets: HashMap<BucketKey, Bucket> = HashMap::new();
    for m in members {
        let is_osc = color_mode_of(&m.bayerpat) == ColorMode::Osc;
        let filter_token = sanitized_or(m.filter.as_deref(), "NoFilter");
        let key: BucketKey = (is_osc, filter_token, m.xbinning.unwrap_or(1));
        let bucket = buckets.entry(key).or_insert_with(|| Bucket {
            filter_raw: trimmed_or_none(m.filter.as_deref()),
            members: Vec::new(),
        });
        bucket.members.push(m);
    }

    let mut groups = Vec::new();
    for ((is_osc, filter_token, binning), bucket) in buckets {
        let color = if is_osc {
            ColorMode::Osc
        } else {
            ColorMode::Mono
        };
        for (label, idxs) in cluster_indices(&bucket.members, cfg) {
            let frames: Vec<GroupFrame> = idxs
                .iter()
                .map(|&i| to_group_frame(&bucket.members[i]))
                .collect();
            let total_exposure_s = frames.iter().map(|f| f.exposure_s.unwrap_or(0.0)).sum();

            // The reference-anchor member is the cluster's own first member
            // (idxs is already restored to date_obs/id order — see
            // `cluster_indices`); a cluster is never empty by construction.
            let instrume = idxs
                .first()
                .and_then(|&i| trimmed_or_none(bucket.members[i].instrume.as_deref()));
            let mut cameras: Vec<String> = idxs
                .iter()
                .filter_map(|&i| trimmed_or_none(bucket.members[i].instrume.as_deref()))
                .collect();
            cameras.sort();
            cameras.dedup();
            // Fix round 1, minor 6: every member of this cluster carries no
            // usable INSTRUME (NULL/blank) — the same "unknown" fallback
            // the key itself used before camera left it, so `ATH_STKC` and
            // the Camera cell say something honest instead of an empty list.
            if cameras.is_empty() {
                cameras.push("unknown".to_string());
            }

            let key = compose_key(color, &filter_token, binning, label);
            groups.push(IntegrationGroup {
                key,
                instrume,
                color_mode: color,
                filter: bucket.filter_raw.clone(),
                binning,
                cameras,
                exposure_s: label,
                frames,
                total_exposure_s,
            });
        }
    }

    groups.sort_by(|a, b| {
        b.frames
            .len()
            .cmp(&a.frames.len())
            .then_with(|| {
                b.total_exposure_s
                    .partial_cmp(&a.total_exposure_s)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.key.cmp(&b.key))
    });

    tracing::debug!(
        frame_set_id = frames_set_id,
        count = groups.len(),
        frames = total_frames,
        "integration groups built"
    );

    Ok(groups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stacking::test_fixtures::{self, LightSpec};

    #[test]
    fn key_format() {
        assert_eq!(
            group_key(ColorMode::Mono, None, 1, Some(180.0)),
            "mono__NoFilter__bin1__180s"
        );
        assert_eq!(
            group_key(ColorMode::Mono, Some("Ha"), 2, Some(180.0)),
            "mono__Ha__bin2__180s"
        );
        assert_eq!(
            group_key(ColorMode::Osc, Some("Ha "), 2, None),
            "osc__Ha__bin2__unknown"
        );
        assert_eq!(
            group_key(ColorMode::Mono, Some(""), 1, Some(0.39)),
            "mono__NoFilter__bin1__0.39s"
        );
    }

    #[test]
    fn two_mono_cameras_with_one_exposure_form_one_group() {
        let f = test_fixtures::frame_set("LDN 1272");
        test_fixtures::add_light(
            &f,
            &LightSpec {
                stem: "m0",
                instrume: "ATR2600M",
                filter: None,
                binning: 1,
                width: 6224,
                height: 4168,
                exptime: 180.0,
                date_obs: "2025-10-18T02:00:00",
                bayerpat: None,
                write_file: false,
            },
        );
        test_fixtures::add_light(
            &f,
            &LightSpec {
                stem: "m1",
                instrume: "Other",
                filter: None,
                binning: 1,
                width: 6248,
                height: 4176,
                exptime: 180.0,
                date_obs: "2025-10-18T02:05:00",
                bayerpat: None,
                write_file: false,
            },
        );
        let g = group_frames(&f.conn, f.set_id, &GroupingConfig::default()).unwrap();
        assert_eq!(g.len(), 1, "{g:?}");
        assert_eq!(g[0].frames.len(), 2);
        assert_eq!(
            g[0].cameras,
            vec!["ATR2600M".to_string(), "Other".to_string()]
        );
        assert_eq!(g[0].instrume.as_deref(), Some("ATR2600M"));
        assert_eq!(g[0].key, "mono__NoFilter__bin1__180s");
    }

    /// Fix round 1, minor 6: every member of a cluster with no usable
    /// `INSTRUME` (NULL, seeded here via a direct `UPDATE` — `LightSpec`'s
    /// own `instrume` field is a plain `&str`, so a NULL row isn't
    /// expressible through the shared fixture builder) must not leave
    /// `cameras` empty — `ATH_STKC` and the Camera cell need something
    /// honest to show, the same "unknown" fallback the key itself used to
    /// fall back to before camera left it.
    #[test]
    fn all_null_instrume_group_falls_back_to_unknown_camera() {
        let f = test_fixtures::frame_set("s");
        let (id, _) = test_fixtures::add_light(
            &f,
            &LightSpec {
                stem: "noinstrume",
                instrume: "placeholder",
                filter: None,
                binning: 1,
                width: 8,
                height: 8,
                exptime: 60.0,
                date_obs: "2025-01-01T00:00:00",
                bayerpat: None,
                write_file: false,
            },
        );
        f.conn
            .execute(
                "UPDATE frames SET instrume = NULL WHERE id = ?1",
                params![id],
            )
            .unwrap();

        let g = group_frames(&f.conn, f.set_id, &GroupingConfig::default()).unwrap();
        assert_eq!(g.len(), 1, "{g:?}");
        assert_eq!(g[0].cameras, vec!["unknown".to_string()]);
        assert_eq!(
            g[0].instrume, None,
            "the display field stays honestly absent"
        );
    }

    #[test]
    fn exposure_outside_the_tolerance_splits() {
        let cfg = GroupingConfig {
            exposure_tolerance_sec: 2.0,
        };

        let apart = test_fixtures::frame_set("s");
        for (i, e) in [180.0, 300.0].iter().enumerate() {
            test_fixtures::add_light(
                &apart,
                &LightSpec {
                    stem: &format!("f{i}"),
                    instrume: "cam",
                    filter: None,
                    binning: 1,
                    width: 8,
                    height: 8,
                    exptime: *e,
                    date_obs: "2025-01-01T00:00:00",
                    bayerpat: None,
                    write_file: false,
                },
            );
        }
        let apart_groups = group_frames(&apart.conn, apart.set_id, &cfg).unwrap();
        assert_eq!(
            apart_groups.len(),
            2,
            "180 and 300 differ well past tolerance"
        );

        let close = test_fixtures::frame_set("s2");
        for (i, e) in [180.0, 181.5].iter().enumerate() {
            test_fixtures::add_light(
                &close,
                &LightSpec {
                    stem: &format!("g{i}"),
                    instrume: "cam",
                    filter: None,
                    binning: 1,
                    width: 8,
                    height: 8,
                    exptime: *e,
                    date_obs: "2025-01-01T00:00:00",
                    bayerpat: None,
                    write_file: false,
                },
            );
        }
        let close_groups = group_frames(&close.conn, close.set_id, &cfg).unwrap();
        assert_eq!(
            close_groups.len(),
            1,
            "180 and 181.5 are within a 2s tolerance"
        );
    }

    #[test]
    fn colour_mode_and_filter_still_split() {
        let f = test_fixtures::frame_set("s");
        let base = |stem: &'static str,
                    filter: Option<&'static str>,
                    bayerpat: Option<&'static str>| LightSpec {
            stem,
            instrume: "cam",
            filter,
            binning: 1,
            width: 8,
            height: 8,
            exptime: 60.0,
            date_obs: "2025-01-01T00:00:00",
            bayerpat,
            write_file: false,
        };
        test_fixtures::add_light(&f, &base("mono_nofilter", None, None));
        test_fixtures::add_light(&f, &base("osc_nofilter", None, Some("RGGB")));
        test_fixtures::add_light(&f, &base("mono_ha", Some("Ha"), None));
        test_fixtures::add_light(&f, &base("mono_oiii", Some("OIII"), None));

        let g = group_frames(&f.conn, f.set_id, &GroupingConfig::default()).unwrap();
        assert_eq!(
            g.len(),
            4,
            "mono/osc and each distinct filter stay separate groups: {g:?}"
        );
    }

    #[test]
    fn a_frame_without_exptime_gets_its_own_unknown_cluster_and_a_warning() {
        let f = test_fixtures::frame_set("s");
        let (no_exp_id, _) = test_fixtures::add_light(
            &f,
            &LightSpec {
                stem: "noexp",
                instrume: "cam",
                filter: None,
                binning: 1,
                width: 8,
                height: 8,
                exptime: 0.0,
                date_obs: "2025-01-01T00:00:00",
                bayerpat: None,
                write_file: false,
            },
        );
        f.conn
            .execute(
                "UPDATE frames SET exptime = NULL WHERE id = ?1",
                params![no_exp_id],
            )
            .unwrap();
        test_fixtures::add_light(
            &f,
            &LightSpec {
                stem: "known",
                instrume: "cam",
                filter: None,
                binning: 1,
                width: 8,
                height: 8,
                exptime: 180.0,
                date_obs: "2025-01-01T00:05:00",
                bayerpat: None,
                write_file: false,
            },
        );

        let g = group_frames(&f.conn, f.set_id, &GroupingConfig::default()).unwrap();
        assert_eq!(
            g.len(),
            2,
            "the unknown-EXPTIME frame gets its own cluster, apart from 180s: {g:?}"
        );
        let unknown = g
            .iter()
            .find(|x| x.exposure_s.is_none())
            .expect("an unknown-exposure cluster exists");
        assert_eq!(unknown.frames.len(), 1);
        assert_eq!(unknown.frames[0].filename, "noexp.fits");
        assert_eq!(unknown.key, "mono__NoFilter__bin1__unknown");
        assert!(g.iter().any(|x| x.key == "mono__NoFilter__bin1__180s"));
        // `group_frames` itself has no warnings channel — `build_plan`
        // (plan.rs) is what turns this cluster into a plan warning naming
        // the frame; see `plan_warns_about_frames_without_exptime`.
    }

    #[test]
    fn set_slug_sanitizes() {
        assert_eq!(set_slug("LDN 1272"), sanitize_for_filename("LDN 1272"));
        assert_eq!(set_slug("   "), "set");
    }

    /// Fix round 1: `NULL`/`''`/`'Ha '`/`'Ha'` are four distinct raw values
    /// but only two sanitized tokens (`NoFilter`, `Ha`) — bucketing on the
    /// raw value used to produce four groups sharing two colliding keys.
    #[test]
    fn filter_spellings_collapse_to_one_group() {
        let f = test_fixtures::frame_set("s");
        let filters: [Option<&str>; 4] = [None, Some(""), Some("Ha "), Some("Ha")];
        for (i, filt) in filters.iter().enumerate() {
            test_fixtures::add_light(
                &f,
                &LightSpec {
                    stem: &format!("f{i}"),
                    instrume: "cam",
                    filter: *filt,
                    binning: 1,
                    width: 8,
                    height: 8,
                    exptime: 60.0,
                    date_obs: "2025-01-01T00:00:00",
                    bayerpat: None,
                    write_file: false,
                },
            );
        }
        let g = group_frames(&f.conn, f.set_id, &GroupingConfig::default()).unwrap();
        assert_eq!(g.len(), 2, "one group for NoFilter, one for Ha — not four");
        let no_filter = g
            .iter()
            .find(|x| x.key == "mono__NoFilter__bin1__60s")
            .expect("NULL and '' collapse into the NoFilter group");
        assert_eq!(no_filter.frames.len(), 2);
        let ha = g
            .iter()
            .find(|x| x.key == "mono__Ha__bin1__60s")
            .expect("'Ha ' and 'Ha' collapse into the Ha group");
        assert_eq!(ha.frames.len(), 2);
        assert_ne!(no_filter.key, ha.key);
    }
}
