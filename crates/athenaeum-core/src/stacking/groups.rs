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
use std::sync::Arc;

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::api::ApiError;
use crate::archive::path_layout::sanitize_for_filename;
use crate::calibration_library::paths::fmt_num;
use crate::db::stacking::StackSubject;
use crate::stacking::config::{ExposureMode, GroupingConfig, StackingConfig};
use crate::stacking::origin::{FrameOrigin, FrameOrigins};

/// Mono vs colour (CFA) sensor, decided from `frames.bayerpat` (spec §2:
/// "colour mode ... from the Bayer cards"). A non-empty, trimmed `bayerpat`
/// is OSC; anything else — `NULL`, empty, or whitespace-only — is mono.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum ColorMode {
    Mono,
    Osc,
}

/// Where a [`GroupFrame`]'s `pixel_scale_arcsec` came from (M4b, spec's
/// mixed-pixel-scale work). `Solve` — a stored `plate_solves` row for the
/// frame — always wins over `Header` — [`header_pixel_scale_arcsec`] from
/// the frame's own `FOCALLEN`/`XPIXSZ` — when both are available, since a
/// solve measures the actual optical scale rather than assuming the header
/// is correct/complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum ScaleSource {
    Solve,
    Header,
}

/// One LIGHT frame as it sits inside an [`IntegrationGroup`] — the catalog
/// identity (`frame_id`/`file_id`), the file location the later stages read
/// from, the two per-frame fields the grouping/clustering logic itself
/// consumes (`exposure_s`, `date_obs`), this frame's own native geometry
/// (`width`/`height` — a group's members may differ now that camera/
/// geometry are not grouping keys; `0` when `NAXIS1`/`NAXIS2` is absent,
/// the same fallback the old group-level fields used), and this frame's own
/// pixel scale (M4b — a stored plate solve's `pixel_scale_arcsec` when one
/// exists, else [`header_pixel_scale_arcsec`] from `FOCALLEN`/`XPIXSZ`;
/// `None`/`None` when neither is available).
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
    pub pixel_scale_arcsec: Option<f64>,
    pub scale_source: Option<ScaleSource>,
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
    /// The group's exposure-class token (spec §8, plan 6b-2 C3): `<t>s`,
    /// `st<t0>`, `eq<e0>`, `all` or `unknown` — the key's last segment.
    pub exposure_class: String,
    pub frames: Vec<GroupFrame>,
    pub total_exposure_s: f64,
    /// The group's own pixel scale (M4b): the median `pixel_scale_arcsec`
    /// of whichever members have one, `None` when none do. The plan gate
    /// (`plan.rs::build_plan`) compares this against the run's resolved
    /// reference to decide whether this group is "far" from it — a
    /// warning, never a blocker.
    pub pixel_scale_arcsec: Option<f64>,
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
    compose_key(color, &filt, binning, &exposure_token(exposure_s))
}

/// Format the group-key string from an ALREADY-sanitized filter token — the
/// one formatting authority both [`group_key`] (sanitizes the raw catalog
/// value itself) and `group_frames` (sanitizes once per bucket, then reuses
/// ITS OWN bucket's token here) go through, so a group's key can never
/// disagree with the token its own bucket was built from.
fn compose_key(color: ColorMode, filter_token: &str, binning: i64, class_token: &str) -> String {
    let color_tok = match color {
        ColorMode::Mono => "mono",
        ColorMode::Osc => "osc",
    };
    compose_key_token(color_tok, filter_token, binning, class_token)
}

/// [`compose_key`] over a colour TOKEN (`mono` | `osc` | `cfa`): an attested
/// frame set's keys follow the project rule (plan 6b-2 C13), where a mosaic
/// kept as a mosaic is `cfa`.
fn compose_key_token(
    color_tok: &str,
    filter_token: &str,
    binning: i64,
    class_token: &str,
) -> String {
    format!("{color_tok}__{filter_token}__bin{binning}__{class_token}")
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

/// Pixel scale from the header's focal length and pixel size (M4b, ruling
/// R-M4b-1): `206.2648 * xpixsz_um / focallen_mm`, arcsec/px — the standard
/// plate-scale formula. `xpixsz_um` is trusted as already binned-effective
/// (an ASI294MM bin-2 light's `XPIXSZ` already reads twice the sensor's
/// native pixel size, the FITS convention every camera driver in the wild
/// follows) — there is deliberately NO separate binning factor here.
/// `None` when either input is missing, non-finite, or non-positive (a
/// zero/negative `focallen` would divide by zero or invert the sign,
/// neither a usable optical setup; a non-finite value is reachable from a
/// real header — the FITS parser passes a literal `1E999` through as `inf`
/// with no finiteness filter of its own, `fits_parser/fits_header_reader.rs`).
pub fn header_pixel_scale_arcsec(xpixsz_um: Option<f64>, focallen_mm: Option<f64>) -> Option<f64> {
    let xpixsz = xpixsz_um?;
    let focallen = focallen_mm?;
    if !xpixsz.is_finite() || !focallen.is_finite() || xpixsz <= 0.0 || focallen <= 0.0 {
        return None;
    }
    Some(206.2648 * xpixsz / focallen)
}

/// `sanitize_for_filename` a possibly-absent, possibly-blank value, falling
/// back to `fallback` when the value is `None`, empty/whitespace, or
/// sanitizes away to nothing.
pub(crate) fn sanitized_or(value: Option<&str>, fallback: &str) -> String {
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

/// The median `pixel_scale_arcsec` of whichever frames have one (Solve or
/// Header, see `GroupFrame::scale_source`); `None` when none do. Shared by
/// the frame-set and project group assembly.
pub(crate) fn median_frame_scale(frames: &[GroupFrame]) -> Option<f64> {
    let mut scales: Vec<f64> = frames.iter().filter_map(|f| f.pixel_scale_arcsec).collect();
    if scales.is_empty() {
        None
    } else {
        Some(median_f64(&mut scales))
    }
}

/// A group's display camera and camera list from its members' `INSTRUME`
/// values, in member order: the first member's trimmed camera (the
/// reference anchor, `None` when blank), and every distinct trimmed label,
/// sorted. A group whose members carry none lists `"unknown"` so `ATH_STKC`
/// and the Camera cell say something honest instead of an empty list.
pub(crate) fn camera_summary<'a>(
    instrumes: impl Iterator<Item = Option<&'a str>>,
) -> (Option<String>, Vec<String>) {
    let all: Vec<Option<String>> = instrumes.map(trimmed_or_none).collect();
    let instrume = all.first().cloned().flatten();
    let mut cameras: Vec<String> = all.into_iter().flatten().collect();
    cameras.sort();
    cameras.dedup();
    if cameras.is_empty() {
        cameras.push("unknown".to_string());
    }
    (instrume, cameras)
}

/// The group order of a plan: most frames first, then total exposure, then
/// key. Shared by the frame-set and project group assembly.
pub(crate) fn sort_groups(groups: &mut [IntegrationGroup]) {
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
///
/// A light whose file sits in the Black Hole is not a member here: the
/// Black Hole is how a user rejects a light (Blink, the Analysis table's
/// "Reject unusable frames"), and the frame-set views already list such a
/// light apart from the active ones. This join is the one light source of a
/// frame set's stacking plan AND its run ([`frame_source`]), so the plan's
/// counts and the frames the run reads cannot disagree. Restoring the file
/// from the Black Hole brings the light back. (Transfer dedup still counts a
/// blackholed file as present — a different question, not this one.)
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
    focallen: Option<f64>,
    xpixsz: Option<f64>,
}

fn load_group_members(conn: &Connection, frames_set_id: i64) -> Result<Vec<LightMember>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT DISTINCT f.id, fi.id, fi.filename, fi.path, fi.size, fi.modified_at,
                f.instrume, f.filter, f.xbinning, f.naxis1, f.naxis2, f.exptime, f.date_obs, f.bayerpat,
                f.focallen, f.xpixsz
         FROM session_members sm
         JOIN sessions s ON s.id = sm.session_id
         JOIN imaging_nights ino ON ino.id = s.imaging_night_id
         JOIN frames f ON f.id = sm.frame_id
         JOIN files fi ON fi.id = f.file_id
         WHERE ino.frames_set_id = ?1 AND f.imagetyp = 'Light'
           AND {not_blackholed}
         ORDER BY f.date_obs, f.id",
        not_blackholed = crate::db::FRAME_NOT_IN_BLACK_HOLE_SQL,
    ))?;
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
                focallen: r.get(14)?,
                xpixsz: r.get(15)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Bulk-load `plate_solves.pixel_scale_arcsec` for a set of frame ids — ONE
/// query per (up to) 500 ids, chunked well under SQLite's default bound
/// variable ceiling, never one query per frame. A frame with no `plate_solves`
/// row is simply absent from the returned map; [`group_frames`]'s caller then
/// keeps that frame's header-derived scale (if any).
/// A non-finite or non-positive stored `pixel_scale_arcsec` is dropped, not
/// inserted — an authoritative-looking but bogus DB value must not win over
/// (and suppress) a usable header-derived scale for the same frame;
/// [`to_group_frame`]'s caller falls back to [`header_pixel_scale_arcsec`]
/// for any `frame_id` this map has no entry for.
fn load_solved_scales(conn: &Connection, frame_ids: &[i64]) -> Result<HashMap<i64, f64>> {
    let mut out = HashMap::new();
    for chunk in frame_ids.chunks(500) {
        let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT frame_id, pixel_scale_arcsec FROM plate_solves WHERE frame_id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> =
            chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
        })?;
        for row in rows {
            let (frame_id, scale) = row?;
            if scale.is_finite() && scale > 0.0 {
                out.insert(frame_id, scale);
            }
        }
    }
    Ok(out)
}

/// The plain median of `values` (sorted in place) — the SAME odd/even
/// convention `weights.rs::median_of` uses, a separate copy rather than a
/// shared one since the two operate on unrelated quantities. `pub(crate)`
/// (M4b R-T6-7): `plan.rs::build_plan` reuses this SAME function to derive
/// a group's INCLUDED-only median pixel scale, rather than adding a third
/// copy of the same six lines.
pub(crate) fn median_f64(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        0.5 * (values[n / 2 - 1] + values[n / 2])
    }
}

fn color_mode_of(bayerpat: &Option<String>) -> ColorMode {
    match bayerpat.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => ColorMode::Osc,
        _ => ColorMode::Mono,
    }
}

/// `solved_scales` (built once per [`group_frames`] call, keyed by
/// `frame_id`) overrides the header-derived scale with `Solve` when this
/// member has a stored plate solve — a measured scale always outranks the
/// header's assumption.
fn to_group_frame(m: &LightMember, solved_scales: &HashMap<i64, f64>) -> GroupFrame {
    let (pixel_scale_arcsec, scale_source) = match solved_scales.get(&m.frame_id) {
        Some(&scale) => (Some(scale), Some(ScaleSource::Solve)),
        None => {
            let header_scale = header_pixel_scale_arcsec(m.xpixsz, m.focallen);
            let source = header_scale.map(|_| ScaleSource::Header);
            (header_scale, source)
        }
    };
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
        pixel_scale_arcsec,
        scale_source,
    }
}

/// One frame's input to [`classify_exposures`] (plan 6b-2 C3): its exposure
/// (`None` = no `EXPTIME`) and its focal ratio (`None` = unknown; read only in
/// `equivalent` mode). Source-agnostic, so a frame set and a project class
/// by one rule.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ClassInput {
    pub(crate) exposure_s: Option<f64>,
    pub(crate) f_ratio: Option<f64>,
}

/// One exposure class: its group-key token, the SECONDS value its group's
/// `exposure_s` carries — the shortest member's exposure, never an `eq`
/// anchor, because the EXPTIME warning, `PlanGroup`, `stacking_run_groups.exposure`
/// and the master file name all read it as seconds — and its members'
/// indices in input order.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ExposureClass {
    pub(crate) token: String,
    pub(crate) exposure_s: Option<f64>,
    pub(crate) members: Vec<usize>,
}

/// Partition one bucket's frames into exposure classes (spec §8, plan 6b-2
/// C3). Frames without `EXPTIME` form their own `unknown` class in every
/// mode — a missing value is not "0 s" — so the EXPTIME warning stays
/// keyed on `exposure_s == None`. `cfg.stops_width` is trusted: `resolve_config`
/// clamps it to [`crate::stacking::config::MIN_STOPS_WIDTH`].
pub(crate) fn classify_exposures(
    inputs: &[ClassInput],
    cfg: &GroupingConfig,
) -> Vec<ExposureClass> {
    let (known, unknown): (Vec<usize>, Vec<usize>) =
        (0..inputs.len()).partition(|&i| inputs[i].exposure_s.is_some());
    let exposure = |i: usize| inputs[i].exposure_s.expect("known exposures only");
    let mut classes = match cfg.exposure_mode {
        ExposureMode::Seconds => seconds_classes(inputs, &known, cfg.exposure_tolerance_sec),
        ExposureMode::Stops => ratio_classes(inputs, &known, exposure, cfg.stops_width, "st"),
        ExposureMode::Equivalent => {
            let (with_n, without_n): (Vec<usize>, Vec<usize>) =
                known.iter().partition(|&&i| inputs[i].f_ratio.is_some());
            let equivalent = |i: usize| {
                let n = inputs[i]
                    .f_ratio
                    .expect("partitioned on a known focal ratio");
                exposure(i) / (n * n)
            };
            let mut classes = ratio_classes(inputs, &with_n, equivalent, cfg.stops_width, "eq");
            classes.extend(seconds_classes(
                inputs,
                &without_n,
                cfg.exposure_tolerance_sec,
            ));
            classes
        }
        ExposureMode::NoSplit => {
            if known.is_empty() {
                Vec::new()
            } else {
                let shortest = known
                    .iter()
                    .map(|&i| exposure(i))
                    .fold(f64::INFINITY, f64::min);
                vec![ExposureClass {
                    token: "all".to_string(),
                    exposure_s: Some(shortest),
                    members: known,
                }]
            }
        }
    };
    if !unknown.is_empty() {
        classes.push(ExposureClass {
            token: "unknown".to_string(),
            exposure_s: None,
            members: unknown,
        });
    }
    classes
}

/// Today's clustering (owner decision 2026-09-10), byte for byte: sorted
/// ascending, a new class whenever the next exposure is more than
/// `tolerance` past the CURRENT class's first value (not the previous
/// frame's, so a slow drift cannot walk away from where the class started).
/// Members restored to input order. `tolerance` is clamped upstream to
/// `MIN_EXPOSURE_TOLERANCE_SEC`, which keeps `fmt_num`'s tokens distinct.
fn seconds_classes(inputs: &[ClassInput], idx: &[usize], tolerance: f64) -> Vec<ExposureClass> {
    let mut sorted: Vec<usize> = idx.to_vec();
    sorted.sort_by(|&a, &b| {
        inputs[a]
            .exposure_s
            .partial_cmp(&inputs[b].exposure_s)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut clusters: Vec<(f64, Vec<usize>)> = Vec::new();
    for i in sorted {
        let e = inputs[i].exposure_s.expect("known exposures only");
        match clusters.last_mut() {
            Some(last) if e - last.0 <= tolerance => last.1.push(i),
            _ => clusters.push((e, vec![i])),
        }
    }
    clusters
        .into_iter()
        .map(|(label, mut members)| {
            members.sort_unstable();
            ExposureClass {
                token: exposure_token(Some(label)),
                exposure_s: Some(label),
                members,
            }
        })
        .collect()
}

/// `stops` / `equivalent` (spec §8): values sorted ascending; a class starts
/// at the shortest remaining value `v0` and takes every value
/// `≤ max(v0, v0 · 2^W)` (the `max` keeps a zero or negative anchor from
/// splitting equal values).
/// Token `<prefix><class_token_num(v0)>`; the class's seconds value is its
/// shortest member's exposure.
fn ratio_classes(
    inputs: &[ClassInput],
    idx: &[usize],
    value: impl Fn(usize) -> f64,
    width_stops: f64,
    prefix: &str,
) -> Vec<ExposureClass> {
    let factor = 2f64.powf(width_stops);
    let mut sorted: Vec<(f64, usize)> = idx.iter().map(|&i| (value(i), i)).collect();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut out: Vec<ExposureClass> = Vec::new();
    let mut anchor: Option<f64> = None;
    for (v, i) in sorted {
        if anchor.is_none_or(|a| v > a.max(a * factor)) {
            anchor = Some(v);
            out.push(ExposureClass {
                token: format!("{prefix}{}", class_token_num(v)),
                exposure_s: None,
                members: Vec::new(),
            });
        }
        out.last_mut()
            .expect("a class was pushed above")
            .members
            .push(i);
    }
    for class in &mut out {
        class.members.sort_unstable();
        class.exposure_s = class
            .members
            .iter()
            .filter_map(|&i| inputs[i].exposure_s)
            .reduce(f64::min);
    }
    out
}

/// The number in a `st`/`eq` class token (plan 6b-2 C3): four significant
/// digits, trailing zeros trimmed, never an exponent. Two classes of one
/// bucket never print the same: consecutive anchors differ by more than
/// `2^MIN_STOPS_WIDTH` (×1.07), and rounding to four significant digits
/// moves a value by at most 0.05 %. A non-positive or non-finite anchor (an
/// `EXPTIME` of 0 in a frame set) prints in full, which is injective, and
/// `ratio_classes` closes such a class over equal values (`max(a, a·2^W)`),
/// so two classes never share an anchor.
pub(crate) fn class_token_num(x: f64) -> String {
    if !x.is_finite() || x <= 0.0 {
        return format!("{x}");
    }
    let magnitude = x.log10().floor() as i32;
    let decimals = (3 - magnitude).max(0) as usize;
    let s = format!("{x:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// Whether a class token is a seconds token (`<t>s`) — in `equivalent` mode,
/// the classes of frames whose focal ratio is unknown. A convention on the
/// string: the other four tokens never end in `s` (`st<num>`, `eq<num>`,
/// `all`, `unknown`), and a new token must keep it so (review M4).
pub(crate) fn is_seconds_token(token: &str) -> bool {
    token.ends_with('s')
}

/// `N = focal length / aperture` when both are known, finite and positive
/// (spec §8).
pub(crate) fn f_ratio(focal_mm: Option<f64>, aperture_mm: Option<f64>) -> Option<f64> {
    match (focal_mm, aperture_mm) {
        (Some(f), Some(a)) if f.is_finite() && a.is_finite() && f > 0.0 && a > 0.0 => Some(f / a),
        _ => None,
    }
}

/// `APTDIA` of each frame from its stored header blob (spec §8: `frames` has
/// no aperture column). One query per 500 ids; read only in `equivalent`
/// mode. The first `fits_header` row of a frame that carries one wins.
fn load_apertures(conn: &Connection, frame_ids: &[i64]) -> Result<HashMap<i64, f64>> {
    let mut out = HashMap::new();
    for chunk in frame_ids.chunks(500) {
        let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT f.id, fi.format, fh.header FROM frames f
             JOIN files fi ON fi.id = f.file_id
             JOIN fits_header fh ON fh.file_id = f.file_id
             WHERE f.id IN ({placeholders}) ORDER BY f.id, fh.id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> =
            chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (frame_id, format, header) = row?;
            if out.contains_key(&frame_id) {
                continue;
            }
            if let Some(a) =
                crate::fits_parser::stored_header::aperture_mm_from_stored_header(&format, &header)
            {
                out.insert(frame_id, a);
            }
        }
    }
    let missing = frame_ids.len() - out.len();
    if missing > 0 {
        tracing::debug!(count = missing, "frames without a usable aperture");
    }
    Ok(out)
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
/// [`seconds_classes`]). Plan time has no weights to pick a real
/// best-weighted member from (spec's own "at plan time the first member"
/// note); a run may later prefer a different member once stage 3 has
/// weighed the group, but that does not change this DISPLAY field.
pub fn group_frames(
    conn: &Connection,
    frames_set_id: i64,
    cfg: &GroupingConfig,
) -> Result<Vec<IntegrationGroup>> {
    group_frames_with(conn, frames_set_id, cfg, None)
}

/// [`group_frames`] with the colour rule of an ATTESTED frame set (spec §13,
/// plan 6b-2 C13): `Some(debayer_osc)` derives each light's colour token the
/// way a project does ([`crate::stacking::project_source::attested_color_token`]:
/// catalog BAYERPAT, else the file's own plane count), so externally
/// debayered three-plane lights group as `osc`. `None` is the catalog rule,
/// byte-identical to before, with no file probe.
pub(crate) fn group_frames_with(
    conn: &Connection,
    frames_set_id: i64,
    cfg: &GroupingConfig,
    attested_debayer_osc: Option<bool>,
) -> Result<Vec<IntegrationGroup>> {
    let members = load_group_members(conn, frames_set_id)?;
    let total_frames = members.len();

    // M4b: one bulk query for every member's stored plate-solve scale,
    // before the members are bucketed/moved below — see
    // `load_solved_scales`'s own doc for why this is one chunked query,
    // never one per frame.
    let frame_ids: Vec<i64> = members.iter().map(|m| m.frame_id).collect();
    let solved_scales = load_solved_scales(conn, &frame_ids)?;
    let apertures = if cfg.exposure_mode == ExposureMode::Equivalent {
        load_apertures(conn, &frame_ids)?
    } else {
        HashMap::new()
    };

    // Bucketed by a plain tuple rather than a dedicated struct: `ColorMode`
    // deliberately does not derive `Hash` (it is a wire type, not a map key
    // elsewhere), so the colour axis rides along as its key token
    // (`mono` | `osc` | `cfa`, a `&'static str`) instead of pulling that
    // derive in just for this. `filter` is the
    // SANITIZED token (never `None` — `sanitized_or` always resolves to a
    // concrete string, its own fallback included), so two members can only
    // land in different buckets when `group_key` would actually emit
    // different keys for them.
    type BucketKey = (&'static str, String, i64);
    let mut buckets: HashMap<BucketKey, Bucket> = HashMap::new();
    for m in members {
        let token: &'static str = match attested_debayer_osc {
            Some(debayer_osc) => crate::stacking::project_source::attested_color_token(
                frames_set_id,
                m.frame_id,
                color_mode_of(&m.bayerpat) == ColorMode::Osc,
                std::path::Path::new(&m.path),
                debayer_osc,
            ),
            None if color_mode_of(&m.bayerpat) == ColorMode::Osc => "osc",
            None => "mono",
        };
        let filter_token = sanitized_or(m.filter.as_deref(), "NoFilter");
        let key: BucketKey = (token, filter_token, m.xbinning.unwrap_or(1));
        let bucket = buckets.entry(key).or_insert_with(|| Bucket {
            filter_raw: trimmed_or_none(m.filter.as_deref()),
            members: Vec::new(),
        });
        bucket.members.push(m);
    }

    let mut groups = Vec::new();
    for ((color_token, filter_token, binning), bucket) in buckets {
        let color = if color_token == "mono" {
            ColorMode::Mono
        } else {
            ColorMode::Osc
        };
        let inputs: Vec<ClassInput> = bucket
            .members
            .iter()
            .map(|m| ClassInput {
                exposure_s: m.exptime,
                f_ratio: f_ratio(m.focallen, apertures.get(&m.frame_id).copied()),
            })
            .collect();
        for class in classify_exposures(&inputs, cfg) {
            let idxs = class.members;
            let label = class.exposure_s;
            let frames: Vec<GroupFrame> = idxs
                .iter()
                .map(|&i| to_group_frame(&bucket.members[i], &solved_scales))
                .collect();
            let total_exposure_s = frames.iter().map(|f| f.exposure_s.unwrap_or(0.0)).sum();

            // The group's own pixel scale: the median of whichever members
            // have one (Solve or Header — see `GroupFrame::scale_source`);
            // `None` when none do.
            let pixel_scale_arcsec = median_frame_scale(&frames);

            // The reference-anchor member is the cluster's own first member
            // (idxs is already restored to date_obs/id order — see
            // `seconds_classes`); a cluster is never empty by construction.
            let (instrume, cameras) =
                camera_summary(idxs.iter().map(|&i| bucket.members[i].instrume.as_deref()));

            let key = compose_key_token(color_token, &filter_token, binning, &class.token);
            groups.push(IntegrationGroup {
                key,
                instrume,
                color_mode: color,
                filter: bucket.filter_raw.clone(),
                binning,
                cameras,
                exposure_s: label,
                exposure_class: class.token,
                frames,
                total_exposure_s,
                pixel_scale_arcsec,
            });
        }
    }

    sort_groups(&mut groups);

    tracing::debug!(
        frame_set_id = frames_set_id,
        count = groups.len(),
        frames = total_frames,
        "integration groups built"
    );

    Ok(groups)
}

/// The frame source of a stacking subject: its integration groups plus the
/// origin of every frame in them (wave 6 spec §4.3). A frame set reads the
/// catalog through [`group_frames`] (an attested set, whose
/// `calibrated_externally` flag rides every origin, spec §13, through
/// [`group_frames_with`] and the project colour rule); a project reads its manifest mirror through
/// [`crate::stacking::project_source::project_frame_source`].
pub(crate) fn frame_source(
    conn: &Connection,
    subject: &StackSubject,
    cfg: &StackingConfig,
) -> Result<(Vec<IntegrationGroup>, FrameOrigins), ApiError> {
    match subject {
        StackSubject::FrameSet(frames_set_id) => {
            let attested = crate::db::collab::frames_set_attested(conn, *frames_set_id)?;
            // Only an attested set takes the project colour rule (C13); a
            // plain frame set is grouped by the catalog alone, no file probe.
            let groups = group_frames_with(
                conn,
                *frames_set_id,
                &cfg.grouping,
                attested.then_some(cfg.calibration.debayer_osc),
            )?;
            let origins = groups
                .iter()
                .flat_map(|g| g.frames.iter())
                .map(|f| {
                    (
                        f.frame_id,
                        FrameOrigin::Catalog {
                            file_id: f.file_id,
                            attested,
                        },
                    )
                })
                .collect();
            Ok((groups, Arc::new(origins)))
        }
        StackSubject::Project(project_id) => {
            let source =
                crate::stacking::project_source::project_frame_source(conn, project_id, cfg)?;
            Ok((source.groups, source.origins))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stacking::test_fixtures::{self, LightSpec};

    /// Plan 6b-2 C3: a class token is formatted at a precision that never
    /// merges two anchors — `fmt_num`'s two decimals merge these, and a
    /// merged token would collide on `stacking_run_groups`'
    /// `UNIQUE(run_id, group_key)`.
    #[test]
    fn class_tokens_never_merge_two_anchors_fmt_num_would() {
        use crate::calibration_library::paths::fmt_num;
        assert_eq!(
            fmt_num(0.002),
            fmt_num(0.004),
            "the premise: fmt_num merges them"
        );
        assert_eq!(
            fmt_num(0.005),
            fmt_num(0.0101),
            "the premise: fmt_num merges them"
        );
        assert_ne!(class_token_num(0.002), class_token_num(0.004));
        assert_ne!(class_token_num(0.005), class_token_num(0.0101));
        assert_eq!(class_token_num(300.0), "300");
        assert_eq!(class_token_num(0.5), "0.5");
        assert_eq!(class_token_num(2.4), "2.4");
        assert_eq!(class_token_num(9.566326530612244), "9.566");
        assert_eq!(class_token_num(0.005), "0.005");
        assert_eq!(class_token_num(0.0101), "0.0101");
        assert_eq!(class_token_num(12345.6), "12346");
    }

    /// Plan 6b-2 C3: at the `stopsWidth` floor, a dense sweep over five
    /// orders of magnitude never yields two classes with one token.
    #[test]
    fn stop_classes_never_share_a_token() {
        use crate::stacking::config::{ExposureMode, MIN_STOPS_WIDTH};
        let cfg = GroupingConfig {
            exposure_mode: ExposureMode::Stops,
            stops_width: MIN_STOPS_WIDTH,
            ..GroupingConfig::default()
        };
        let inputs: Vec<ClassInput> = (0..7000)
            .map(|k| ClassInput {
                exposure_s: Some(0.001 * 1.003f64.powi(k)),
                f_ratio: None,
            })
            .collect();
        let classes = classify_exposures(&inputs, &cfg);
        let mut tokens: Vec<&str> = classes.iter().map(|c| c.token.as_str()).collect();
        let n = tokens.len();
        assert!(n > 100, "the sweep spans many classes: {n}");
        tokens.sort_unstable();
        tokens.dedup();
        assert_eq!(tokens.len(), n, "two classes share a token");
    }

    /// Plan 6b-2 C3 / spec §8: the four modes, their tokens, the seconds
    /// value each class carries (never an `eq` anchor), and frames without
    /// EXPTIME in their own `unknown` class in every mode.
    #[test]
    fn the_four_exposure_modes_class_and_token_as_the_spec_says() {
        use crate::stacking::config::ExposureMode;
        let input = |t: Option<f64>, n: Option<f64>| ClassInput {
            exposure_s: t,
            f_ratio: n,
        };
        let inputs = [
            input(Some(60.0), Some(5.0)),
            input(Some(90.0), Some(5.0)),
            input(Some(120.0), None),
            input(Some(180.0), Some(2.0)),
            input(Some(300.0), Some(2.0)),
            input(Some(600.0), Some(2.0)),
            input(None, None),
        ];
        let run = |mode| {
            let cfg = GroupingConfig {
                exposure_mode: mode,
                ..GroupingConfig::default()
            };
            classify_exposures(&inputs, &cfg)
                .into_iter()
                .map(|c| (c.token, c.exposure_s, c.members))
                .collect::<Vec<_>>()
        };
        let row = |t: &str, e: Option<f64>, m: &[usize]| (t.to_string(), e, m.to_vec());

        assert_eq!(
            run(ExposureMode::Seconds),
            vec![
                row("60s", Some(60.0), &[0]),
                row("90s", Some(90.0), &[1]),
                row("120s", Some(120.0), &[2]),
                row("180s", Some(180.0), &[3]),
                row("300s", Some(300.0), &[4]),
                row("600s", Some(600.0), &[5]),
                row("unknown", None, &[6]),
            ]
        );
        assert_eq!(
            run(ExposureMode::Stops),
            vec![
                row("st60", Some(60.0), &[0, 1, 2]),
                row("st180", Some(180.0), &[3, 4]),
                row("st600", Some(600.0), &[5]),
                row("unknown", None, &[6]),
            ]
        );
        // e = t / N²: 2.4, 3.6 | 45, 75 | 150; the frame with no N is classed by seconds.
        assert_eq!(
            run(ExposureMode::Equivalent),
            vec![
                row("eq2.4", Some(60.0), &[0, 1]),
                row("eq45", Some(180.0), &[3, 4]),
                row("eq150", Some(600.0), &[5]),
                row("120s", Some(120.0), &[2]),
                row("unknown", None, &[6]),
            ]
        );
        assert_eq!(
            run(ExposureMode::NoSplit),
            vec![
                row("all", Some(60.0), &[0, 1, 2, 3, 4, 5]),
                row("unknown", None, &[6]),
            ]
        );
        assert!(is_seconds_token("120s"));
        assert!(!is_seconds_token("st60") && !is_seconds_token("eq2.4"));
        assert!(!is_seconds_token("all") && !is_seconds_token("unknown"));
    }

    /// Plan 6b-2 C3 (fix round 1): a zero or negative anchor must still close
    /// its class over equal values, or two classes share one token and collide
    /// on `UNIQUE(run_id, group_key)`.
    #[test]
    fn non_positive_anchors_never_open_duplicate_classes() {
        use crate::stacking::config::ExposureMode;
        let cfg = GroupingConfig {
            exposure_mode: ExposureMode::Stops,
            stops_width: 1.0,
            ..GroupingConfig::default()
        };
        let run = |values: [f64; 3]| {
            let inputs: Vec<ClassInput> = values
                .iter()
                .map(|&t| ClassInput {
                    exposure_s: Some(t),
                    f_ratio: None,
                })
                .collect();
            classify_exposures(&inputs, &cfg)
                .into_iter()
                .map(|c| (c.token, c.members))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            run([-5.0, -5.0, 60.0]),
            vec![
                ("st-5".to_string(), vec![0, 1]),
                ("st60".to_string(), vec![2])
            ]
        );
        assert_eq!(
            run([0.0, 0.0, 60.0]),
            vec![
                ("st0".to_string(), vec![0, 1]),
                ("st60".to_string(), vec![2])
            ]
        );
    }

    /// Plan 6b-2 C3: `stops_width` has no upper bound, so a huge value turns
    /// the class factor into infinity — one class, no panic.
    #[test]
    fn a_huge_stops_width_is_one_class() {
        use crate::stacking::config::ExposureMode;
        let cfg = GroupingConfig {
            exposure_mode: ExposureMode::Stops,
            stops_width: 1.0e9,
            ..GroupingConfig::default()
        };
        let inputs: Vec<ClassInput> = [1.0, 60.0, 3600.0]
            .iter()
            .map(|&t| ClassInput {
                exposure_s: Some(t),
                f_ratio: None,
            })
            .collect();
        let classes = classify_exposures(&inputs, &cfg);
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].token, "st1");
        assert_eq!(classes[0].members, vec![0, 1, 2]);
    }

    /// Plan 6b-2 C3: a frame set in `equivalent` mode reads APTDIA from the
    /// stored header blob and FOCALLEN from the column; 60 s at f/5 and 240 s
    /// at f/10 are one class.
    #[test]
    fn a_frame_set_in_equivalent_mode_classes_by_focal_ratio() {
        use crate::stacking::config::ExposureMode;
        let f = test_fixtures::frame_set("LDN 1272");
        let spec = |stem: &'static str, exptime: f64| LightSpec {
            stem,
            instrume: "cam",
            filter: Some("L"),
            binning: 1,
            width: 64,
            height: 48,
            exptime,
            date_obs: "2025-01-01T20:00:00",
            bayerpat: None,
            write_file: false,
        };
        let (fast, _) = test_fixtures::add_light(&f, &spec("fast", 60.0));
        let (slow, _) = test_fixtures::add_light(&f, &spec("slow", 240.0));
        for (id, focal) in [(fast, 500.0), (slow, 1000.0)] {
            f.conn
                .execute(
                    "UPDATE frames SET focallen = ?2 WHERE id = ?1",
                    rusqlite::params![id, focal],
                )
                .unwrap();
            let card_text = format!(
                "{:<80}{:<80}",
                "APTDIA  =                100.0 / aperture diameter (mm)", "END"
            );
            f.conn
                .execute(
                    "INSERT INTO fits_header (file_id, header)
                     VALUES ((SELECT file_id FROM frames WHERE id = ?1), ?2)",
                    rusqlite::params![id, card_text],
                )
                .unwrap();
        }
        let seconds = group_frames(&f.conn, f.set_id, &GroupingConfig::default()).unwrap();
        assert_eq!(seconds.len(), 2, "seconds keeps 60 s and 240 s apart");
        let cfg = GroupingConfig {
            exposure_mode: ExposureMode::Equivalent,
            ..GroupingConfig::default()
        };
        let groups = group_frames(&f.conn, f.set_id, &cfg).unwrap();
        assert_eq!(groups.len(), 1, "{groups:?}");
        assert_eq!(groups[0].key, "mono__L__bin1__eq2.4");
        assert_eq!(groups[0].exposure_class, "eq2.4");
        assert_eq!(
            groups[0].exposure_s,
            Some(60.0),
            "the class's seconds value is its shortest member's"
        );
    }

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
            ..GroupingConfig::default()
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

    /// M4b, ruling R-M4b-1: `206.2648 * xpixsz_um / focallen_mm`, no binning
    /// factor — `XPIXSZ` is already binned-effective by FITS convention
    /// (an ASI294MM bin-2 light's `XPIXSZ` already reads twice the native
    /// pixel size). Values hand-computed to full precision so the test
    /// actually pins the constant, not just round-trips the formula.
    #[test]
    fn header_pixel_scale() {
        let a = header_pixel_scale_arcsec(Some(3.76), Some(1000.0)).unwrap();
        assert!((a - 0.775555648).abs() < 1e-9, "{a}");

        // An ASI294MM bin-2 light: XPIXSZ already effective at 4.63 um.
        let b = header_pixel_scale_arcsec(Some(4.63), Some(1000.0)).unwrap();
        assert!((b - 0.955006024).abs() < 1e-9, "{b}");

        assert_eq!(header_pixel_scale_arcsec(None, Some(1000.0)), None);
        assert_eq!(header_pixel_scale_arcsec(Some(3.76), None), None);
        assert_eq!(header_pixel_scale_arcsec(None, None), None);
        assert_eq!(header_pixel_scale_arcsec(Some(0.0), Some(1000.0)), None);
        assert_eq!(header_pixel_scale_arcsec(Some(-1.0), Some(1000.0)), None);
        assert_eq!(header_pixel_scale_arcsec(Some(3.76), Some(0.0)), None);
        assert_eq!(header_pixel_scale_arcsec(Some(3.76), Some(-1.0)), None);

        // Fix round 1: a non-finite input (reachable from a real header —
        // the FITS parser passes `1E999` through as `inf`) must not slip
        // past the `<= 0.0` guard, which NaN/inf both fail (`NaN <= 0.0` and
        // `inf <= 0.0` are both `false`).
        assert_eq!(
            header_pixel_scale_arcsec(Some(f64::NAN), Some(1000.0)),
            None
        );
        assert_eq!(
            header_pixel_scale_arcsec(Some(f64::INFINITY), Some(1000.0)),
            None
        );
        assert_eq!(
            header_pixel_scale_arcsec(Some(f64::NEG_INFINITY), Some(1000.0)),
            None
        );
        assert_eq!(header_pixel_scale_arcsec(Some(3.76), Some(f64::NAN)), None);
        assert_eq!(
            header_pixel_scale_arcsec(Some(3.76), Some(f64::INFINITY)),
            None
        );
        assert_eq!(
            header_pixel_scale_arcsec(Some(3.76), Some(f64::NEG_INFINITY)),
            None
        );
    }

    /// M4b: a member with only header data gets `Header`; a member that ALSO
    /// carries a stored plate solve gets `Solve` — the measured scale (0.80)
    /// wins over what the header would have implied (≈0.7756) for that same
    /// frame. The group's own scale is the median of the members that have
    /// one — with two members here (even count), the mean of the two.
    #[test]
    fn group_frame_pixel_scale_solve_overrides_header() {
        let f = test_fixtures::frame_set("s");
        let mut ids = Vec::new();
        for i in 0..2 {
            let (id, _) = test_fixtures::add_light(
                &f,
                &LightSpec {
                    stem: &format!("f{i}"),
                    instrume: "cam",
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
                    "UPDATE frames SET focallen = 1000.0, xpixsz = 3.76 WHERE id = ?1",
                    params![id],
                )
                .unwrap();
            ids.push(id);
        }
        // Only the second frame gets a stored plate solve — its header alone
        // would imply ~0.7756, but the solve (0.80) must win.
        test_fixtures::seed_plate_solve_scale(&f.conn, ids[1], 0.80);

        let g = group_frames(&f.conn, f.set_id, &GroupingConfig::default()).unwrap();
        assert_eq!(g.len(), 1, "{g:?}");

        let header_expected = header_pixel_scale_arcsec(Some(3.76), Some(1000.0)).unwrap();
        let header_frame = g[0]
            .frames
            .iter()
            .find(|gf| gf.frame_id == ids[0])
            .expect("first frame present");
        assert!(
            (header_frame.pixel_scale_arcsec.unwrap() - header_expected).abs() < 1e-9,
            "{:?}",
            header_frame.pixel_scale_arcsec
        );
        assert_eq!(header_frame.scale_source, Some(ScaleSource::Header));

        let solved_frame = g[0]
            .frames
            .iter()
            .find(|gf| gf.frame_id == ids[1])
            .expect("second frame present");
        assert_eq!(solved_frame.pixel_scale_arcsec, Some(0.80));
        assert_eq!(solved_frame.scale_source, Some(ScaleSource::Solve));

        let expected_median = (header_expected + 0.80) / 2.0;
        assert!(
            (g[0].pixel_scale_arcsec.unwrap() - expected_median).abs() < 1e-9,
            "{:?}",
            g[0].pixel_scale_arcsec
        );
    }

    /// Fix round 1: a stored `plate_solves` row with a non-finite or
    /// non-positive `pixel_scale_arcsec` (a corrupt/impossible value) must
    /// not win over a usable header-derived scale for the same frame — the
    /// bogus DB value is dropped, and the frame falls back to `Header`
    /// exactly as if no solve row existed at all.
    #[test]
    fn group_frame_invalid_solve_scale_falls_back_to_header() {
        let f = test_fixtures::frame_set("s");
        let mut ids = Vec::new();
        for (i, bad_scale) in [0.0, -3.0, f64::INFINITY].iter().enumerate() {
            let (id, _) = test_fixtures::add_light(
                &f,
                &LightSpec {
                    stem: &format!("f{i}"),
                    instrume: "cam",
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
                    "UPDATE frames SET focallen = 1000.0, xpixsz = 3.76 WHERE id = ?1",
                    params![id],
                )
                .unwrap();
            test_fixtures::seed_plate_solve_scale(&f.conn, id, *bad_scale);
            ids.push(id);
        }

        let g = group_frames(&f.conn, f.set_id, &GroupingConfig::default()).unwrap();
        assert_eq!(g.len(), 1, "{g:?}");

        let header_expected = header_pixel_scale_arcsec(Some(3.76), Some(1000.0)).unwrap();
        for id in ids {
            let gf = g[0]
                .frames
                .iter()
                .find(|gf| gf.frame_id == id)
                .expect("frame present");
            assert_eq!(
                gf.scale_source,
                Some(ScaleSource::Header),
                "a non-finite/non-positive solve scale must fall back to Header: {gf:?}"
            );
            assert!(
                (gf.pixel_scale_arcsec.unwrap() - header_expected).abs() < 1e-9,
                "{:?}",
                gf.pixel_scale_arcsec
            );
        }
    }

    /// A member with neither a header focal length/pixel size nor a stored
    /// plate solve gets no scale at all, and the group's own scale stays
    /// `None` too (no members to take a median over).
    #[test]
    fn group_frame_with_no_scale_data_has_no_pixel_scale() {
        let f = test_fixtures::frame_set("s");
        test_fixtures::add_light(
            &f,
            &LightSpec {
                stem: "f0",
                instrume: "cam",
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
        let g = group_frames(&f.conn, f.set_id, &GroupingConfig::default()).unwrap();
        assert_eq!(g.len(), 1, "{g:?}");
        assert_eq!(g[0].frames[0].pixel_scale_arcsec, None);
        assert_eq!(g[0].frames[0].scale_source, None);
        assert_eq!(g[0].pixel_scale_arcsec, None);
    }

    /// Plan 6b-2 C3 (parked from Task 3): the f-ratio needs both lengths
    /// finite and positive.
    #[test]
    fn f_ratio_needs_two_finite_positive_lengths() {
        assert_eq!(f_ratio(Some(800.0), Some(200.0)), Some(4.0));
        assert_eq!(f_ratio(None, Some(200.0)), None);
        assert_eq!(f_ratio(Some(800.0), None), None);
        assert_eq!(f_ratio(Some(0.0), Some(200.0)), None);
        assert_eq!(f_ratio(Some(800.0), Some(0.0)), None);
        assert_eq!(f_ratio(Some(-800.0), Some(200.0)), None);
        assert_eq!(f_ratio(Some(800.0), Some(-1.0)), None);
        assert_eq!(f_ratio(Some(f64::NAN), Some(200.0)), None);
        assert_eq!(f_ratio(Some(800.0), Some(f64::INFINITY)), None);
        assert_eq!(f_ratio(Some(f64::INFINITY), Some(200.0)), None);
    }

    /// Parked from Task 3: "the first `fits_header` row of a frame that
    /// carries an `APTDIA` wins"; a row without one is skipped, a frame with
    /// none is absent.
    #[test]
    fn load_apertures_takes_the_first_row_carrying_aptdia() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let header = |cards: &[&str]| -> String {
            let mut out = String::new();
            for c in cards {
                out.push_str(&format!("{c:<80}"));
            }
            out.push_str(&format!("{:<80}", "END"));
            out
        };
        let apt = |v: &str| format!("APTDIA  = {v:>20} / aperture");
        let mut ids = Vec::new();
        for (n, rows) in [
            // Frame 1: no APTDIA in the first row, then 150, then 200.
            (
                1,
                vec![
                    header(&[]),
                    header(&[&apt("150.0")]),
                    header(&[&apt("200.0")]),
                ],
            ),
            // Frame 2: one row with 80.
            (2, vec![header(&[&apt("80.0")])]),
            // Frame 3: rows but no usable APTDIA.
            (3, vec![header(&[&apt("0")])]),
        ] {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
                 VALUES (?1, ?1, 1, '2026-01-01T00:00:00Z', 'FITS')",
                [format!("/f{n}.fits")],
            )
            .unwrap();
            let file_id = conn.last_insert_rowid();
            conn.execute("INSERT INTO frames (file_id) VALUES (?1)", [file_id])
                .unwrap();
            ids.push(conn.last_insert_rowid());
            for h in rows {
                conn.execute(
                    "INSERT INTO fits_header (file_id, header) VALUES (?1, ?2)",
                    rusqlite::params![file_id, h],
                )
                .unwrap();
            }
        }
        let out = load_apertures(&conn, &ids).unwrap();
        assert_eq!(out.get(&ids[0]), Some(&150.0), "the first row carrying one");
        assert_eq!(out.get(&ids[1]), Some(&80.0));
        assert_eq!(out.get(&ids[2]), None);
    }
}
