//! Integration groups (spec §2 "Grouping keys"): partition a frame set's
//! LIGHT membership into the buckets the pipeline runs stage 1–9 over —
//! camera × colour mode × filter × binning × geometry, optionally split
//! further by exposure. Pure DB read, no disk I/O, no config resolution
//! beyond the `GroupingConfig` the caller already resolved (`config.rs`,
//! Plan 5a).

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::archive::path_layout::sanitize_for_filename;
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
/// from, and the two per-frame fields the grouping/clustering logic itself
/// consumes (`exposure_s`, `date_obs`).
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
}

/// One integration group: every LIGHT frame sharing camera, colour mode,
/// filter, binning and geometry (and, with `splitByExposure`, an exposure
/// cluster) — the unit stage 1–9 run over, one master light per group.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationGroup {
    pub key: String,
    pub instrume: Option<String>,
    pub color_mode: ColorMode,
    pub filter: Option<String>,
    pub binning: i64,
    pub width: i64,
    pub height: i64,
    pub exposure_s: Option<f64>,
    pub frames: Vec<GroupFrame>,
    pub total_exposure_s: f64,
}

/// The stable group-key string (spec §2): `<instrume>__<mono|osc>__<filter>
/// __bin<n>__<w>x<h>[__<exp>s]`, used in paths and `stacking_run_groups` rows.
///
/// `instrume`/`filter` sanitize through [`sanitize_for_filename`] the same
/// way the calibration library names its master files, so a group key and a
/// master filename token never disagree over the same camera/filter string.
/// `sanitize_for_filename` itself falls back to `""` for input that sanitizes
/// away to nothing (all-whitespace, control characters only) — this function
/// substitutes the grouping-specific fallbacks (`"unknown"` / `"NoFilter"`)
/// on top of that, since the archive feature's own fallback token
/// (`"Unknown"`) is a different literal.
pub fn group_key(
    instrume: Option<&str>,
    color: ColorMode,
    filter: Option<&str>,
    binning: i64,
    w: i64,
    h: i64,
    exposure_s: Option<f64>,
) -> String {
    let cam = sanitized_or(instrume, "unknown");
    let color_tok = match color {
        ColorMode::Mono => "mono",
        ColorMode::Osc => "osc",
    };
    let filt = sanitized_or(filter, "NoFilter");
    let mut key = format!("{cam}__{color_tok}__{filt}__bin{binning}__{w}x{h}");
    if let Some(exp) = exposure_s {
        key.push_str(&format!("__{exp:.0}s"));
    }
    key
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
    }
}

/// Partition one bucket's members into exposure clusters. Without
/// `splitByExposure` the whole bucket is one cluster labelled `None` — mixed
/// exposures merge into one group, left to the weights/normalization stages
/// (spec §2). With it: sort ascending by exposure (missing `EXPTIME` reads as
/// `0.0`, so those frames cluster together at the low end — their own
/// `"__0s"` group, never dropped), and start a new cluster whenever the next
/// exposure is more than `exposure_tolerance_sec` past the CURRENT cluster's
/// first (label) value — not the previous frame's value, so a slow drift
/// across many frames cannot walk arbitrarily far from where the cluster
/// started. Each cluster's member indices are restored to `members`' own
/// order (date_obs then id) before being handed back, so the exposure sort
/// used to detect clusters never leaks into a group's frame order.
fn cluster_indices(
    members: &[LightMember],
    cfg: &GroupingConfig,
) -> Vec<(Option<f64>, Vec<usize>)> {
    if !cfg.split_by_exposure {
        return vec![(None, (0..members.len()).collect())];
    }
    let mut order: Vec<usize> = (0..members.len()).collect();
    order.sort_by(|&a, &b| {
        let ea = members[a].exptime.unwrap_or(0.0);
        let eb = members[b].exptime.unwrap_or(0.0);
        ea.partial_cmp(&eb).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut clusters: Vec<(f64, Vec<usize>)> = Vec::new();
    for idx in order {
        let e = members[idx].exptime.unwrap_or(0.0);
        match clusters.last_mut() {
            Some(last) if e - last.0 <= cfg.exposure_tolerance_sec => last.1.push(idx),
            _ => clusters.push((e, vec![idx])),
        }
    }
    for (_, idxs) in &mut clusters {
        idxs.sort_unstable();
    }
    clusters
        .into_iter()
        .map(|(label, idxs)| (Some(label), idxs))
        .collect()
}

/// Every integration group in a frame set (spec §2): the LIGHT membership,
/// bucketed by camera / colour mode / filter / binning / geometry and,
/// optionally, exposure, then sorted by frame count desc, total exposure
/// desc, key — so the largest, most-representative group always leads.
pub fn group_frames(
    conn: &Connection,
    frames_set_id: i64,
    cfg: &GroupingConfig,
) -> Result<Vec<IntegrationGroup>> {
    let members = load_group_members(conn, frames_set_id)?;

    // Bucketed by a plain tuple rather than a dedicated struct: `ColorMode`
    // deliberately does not derive `Hash` (it is a wire type, not a map key
    // elsewhere), so the colour axis rides along as a `bool` (`true` = OSC)
    // instead of pulling that derive in just for this.
    type BucketKey = (Option<String>, bool, Option<String>, i64, i64, i64);
    let mut buckets: HashMap<BucketKey, Vec<LightMember>> = HashMap::new();
    for m in members {
        let is_osc = color_mode_of(&m.bayerpat) == ColorMode::Osc;
        let key: BucketKey = (
            m.instrume.clone(),
            is_osc,
            m.filter.clone(),
            m.xbinning.unwrap_or(1),
            m.naxis1.unwrap_or(0),
            m.naxis2.unwrap_or(0),
        );
        buckets.entry(key).or_default().push(m);
    }

    let mut groups = Vec::new();
    for ((instrume, is_osc, filter, binning, width, height), bucket_members) in buckets {
        let color = if is_osc {
            ColorMode::Osc
        } else {
            ColorMode::Mono
        };
        for (label, idxs) in cluster_indices(&bucket_members, cfg) {
            let frames: Vec<GroupFrame> = idxs
                .iter()
                .map(|&i| to_group_frame(&bucket_members[i]))
                .collect();
            let total_exposure_s = frames.iter().map(|f| f.exposure_s.unwrap_or(0.0)).sum();
            let key = group_key(
                instrume.as_deref(),
                color,
                filter.as_deref(),
                binning,
                width,
                height,
                label,
            );
            groups.push(IntegrationGroup {
                key,
                instrume: instrume.clone(),
                color_mode: color,
                filter: filter.clone(),
                binning,
                width,
                height,
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

    Ok(groups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stacking::test_fixtures::{self, LightSpec};

    #[test]
    fn key_format() {
        assert_eq!(
            group_key(
                Some("ZWO ASI2600MC Duo"),
                ColorMode::Osc,
                None,
                1,
                6248,
                4176,
                None
            ),
            "ZWO_ASI2600MC_Duo__osc__NoFilter__bin1__6248x4176"
        );
        assert_eq!(
            group_key(
                Some("cam"),
                ColorMode::Mono,
                Some("Ha"),
                2,
                10,
                10,
                Some(180.0)
            ),
            "cam__mono__Ha__bin2__10x10__180s"
        );
        assert_eq!(
            group_key(None, ColorMode::Mono, Some(""), 1, 1, 1, None),
            "unknown__mono__NoFilter__bin1__1x1"
        );
    }

    #[test]
    fn groups_by_camera_colour_filter_binning_geometry() {
        let f = test_fixtures::frame_set("LDN 1272");
        for i in 0..3 {
            test_fixtures::add_light(
                &f,
                &LightSpec {
                    stem: &format!("m{i}"),
                    instrume: "ATR2600M",
                    filter: None,
                    binning: 1,
                    width: 64,
                    height: 48,
                    exptime: 180.0,
                    date_obs: "2025-10-18T02:00:00",
                    bayerpat: None,
                    write_file: false,
                },
            );
        }
        for i in 0..2 {
            test_fixtures::add_light(
                &f,
                &LightSpec {
                    stem: &format!("o{i}"),
                    instrume: "ZWO ASI2600MC Duo",
                    filter: None,
                    binning: 1,
                    width: 64,
                    height: 48,
                    exptime: 180.0,
                    date_obs: "2025-09-14T02:00:00",
                    bayerpat: Some("RGGB"),
                    write_file: false,
                },
            );
        }
        let g = group_frames(&f.conn, f.set_id, &GroupingConfig::default()).unwrap();
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].frames.len(), 3, "largest first");
        assert_eq!(g[0].color_mode, ColorMode::Mono);
        assert_eq!(g[1].key, "ZWO_ASI2600MC_Duo__osc__NoFilter__bin1__64x48");
        assert_eq!(g[1].color_mode, ColorMode::Osc);
        assert!((g[0].total_exposure_s - 540.0).abs() < 1e-9);
    }

    #[test]
    fn exposure_split_clusters_within_tolerance() {
        let f = test_fixtures::frame_set("s");
        for (i, e) in [180.0, 181.0, 300.0, 301.5].iter().enumerate() {
            test_fixtures::add_light(
                &f,
                &LightSpec {
                    stem: &format!("f{i}"),
                    instrume: "c",
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
        let cfg = GroupingConfig {
            split_by_exposure: true,
            exposure_tolerance_sec: 2.0,
        };
        let g = group_frames(&f.conn, f.set_id, &cfg).unwrap();
        assert_eq!(g.len(), 2);
        assert!(g.iter().any(|x| x.key.ends_with("__180s")));
        assert!(g.iter().any(|x| x.key.ends_with("__300s")));
        assert_eq!(
            group_frames(&f.conn, f.set_id, &GroupingConfig::default())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn set_slug_sanitizes() {
        assert_eq!(set_slug("LDN 1272"), sanitize_for_filename("LDN 1272"));
        assert_eq!(set_slug("   "), "set");
    }
}
