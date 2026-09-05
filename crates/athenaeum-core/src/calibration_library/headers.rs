//! Consolidate a master's FITS header from its source calibration set +
//! member frames (spec §3 step 3, arch-doc B3).

use crate::fits_parser::stored_header::parse_stored_header_keys;
use crate::fits_writer::keywords::{Bayer, FrameKind, HeaderBuilder};
use crate::fits_writer::{Card, FitsWriteError};
// The per-channel measurement streams master pixels through the render-gated
// integration engine; the card BUILDER below is ungated (it only ever receives
// the finished `[R, G, B]` triple), so only the measurement is gated.
#[cfg(feature = "render")]
use crate::integration::cfa::{central_third_channel_means, CfaGeometry};
use crate::models::FileFormat;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use tracing::{debug, warn};

/// Everything needed to consolidate a master header from its source set.
/// Loaded with [`load_header_inputs`].
pub struct MasterHeaderInputs {
    pub kind: FrameKind,
    /// The `calibration_set.id` these inputs came from — kept so the card
    /// builder's degraded-path warnings can name the set.
    pub source_set_id: i64,
    pub instrume: Option<String>,
    pub telescop: Option<String>,
    pub filter: Option<String>,
    pub exptime: Option<f64>,
    pub gain: Option<f64>,
    pub offset: Option<f64>,
    pub xbinning: Option<i64>,
    pub ybinning: Option<i64>,
    pub xpixsz: Option<f64>,
    pub ypixsz: Option<f64>,
    pub focallen: Option<f64>,
    pub egain: Option<f64>,
    pub bayerpat: Option<String>,
    pub xbayroff: Option<i64>,
    pub ybayroff: Option<i64>,
    pub roworder: Option<String>,
    pub temp_mean: Option<f64>,
    pub temp_min: Option<f64>,
    pub temp_max: Option<f64>,
    pub date_obs_midpoint: Option<DateTime<Utc>>,
    pub frame_count: u32,
    pub source_set_uuid: String,
}

/// The Bayer/CFA geometry a calibration set's member frames agree on.
/// Produced by [`load_bayer_consensus`]; every field is `None` unless at
/// least one member actually declared it.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct BayerConsensus {
    pub bayerpat: Option<String>,
    pub xbayroff: Option<i64>,
    pub ybayroff: Option<i64>,
    pub roworder: Option<String>,
    /// Column names whose members did not all agree — the majority was taken
    /// and a `warn!` was emitted for each. Empty is the normal case.
    pub disagreements: Vec<&'static str>,
    /// True when BAYERPAT came from the stored-header blob instead of the
    /// `frames.bayerpat` column (pre-Task-1 catalogs that have not rescanned).
    pub bayerpat_from_blob: bool,
}

/// Majority value of one `frames` column across a calibration set's members.
///
/// Returns `(winner, disagreement)`. NULLs are excluded, so a column no member
/// declared yields `None` — never a fabricated default. Ordering is
/// `count DESC, value ASC`, so a tie breaks deterministically and the same set
/// always consolidates to the same master header.
///
/// `fold_case` groups a text column on `UPPER(TRIM(...))` and returns the
/// folded spelling. Capture software disagrees about spelling, not about the
/// sensor: `"rggb"` and `" RGGB "` are one claim, and counting them apart
/// reported a disagreement that does not exist and then let a genuine
/// minority value win the tie-break. Integer columns compare as stored.
///
/// A BLANK text value is not a claim and gets no vote. A writer that emits
/// `BAYERPAT= ''` (or an XISF `<FITSKeyword name="BAYERPAT" value=""/>`) said
/// nothing about the sensor, yet two such members used to outvote one genuine
/// `RGGB` — the winning `""` was then dropped further down and the master
/// shipped with no BAYERPAT at all, having also reported a "disagreement"
/// between a claim and a silence. Excluded from the GROUP BY rather than after
/// the vote, so the counting itself is honest. Integer columns keep every
/// non-NULL row: 0 is a real CFA phase, not a blank.
///
/// `col` is interpolated into the SQL. Every call site passes one of four
/// hard-coded literals (`bayerpat`, `xbayroff`, `ybayroff`, `roworder`); it is
/// never user input.
fn consensus<T: rusqlite::types::FromSql>(
    conn: &Connection,
    set_id: i64,
    col: &'static str,
    fold_case: bool,
) -> Result<(Option<T>, bool)> {
    let key = if fold_case {
        format!("UPPER(TRIM(fr.{col}))")
    } else {
        format!("fr.{col}")
    };
    let blank_guard = if fold_case {
        format!(" AND TRIM(fr.{col}) <> ''")
    } else {
        String::new()
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT {key} k, COUNT(*) c FROM calibration_set_frames csf
         JOIN frames fr ON fr.id = csf.frame_id
         WHERE csf.set_id = ?1 AND fr.{col} IS NOT NULL{blank_guard}
         GROUP BY k ORDER BY c DESC, k ASC"
    ))?;
    let values: Vec<T> = stmt
        .query_map([set_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let disagreement = values.len() > 1;
    if disagreement {
        warn!(set_id, field = col, "bayer metadata disagrees across set members — using majority");
    }
    Ok((values.into_iter().next(), disagreement))
}

/// Consolidate the members' Bayer columns (Task 1: `frames.bayerpat`,
/// `xbayroff`, `ybayroff`, `roworder`) into one set-level answer.
///
/// The columns are queried directly rather than through a `Frame` struct: the
/// index-based list readers hardcode `None` for these four fields regardless of
/// what is stored (see `models.rs`), so a `Frame` round-trip would silently
/// erase them.
pub(crate) fn load_bayer_consensus(conn: &Connection, set_id: i64) -> Result<BayerConsensus> {
    let mut out = BayerConsensus::default();
    let (bayerpat, dis_pat) = consensus::<String>(conn, set_id, "bayerpat", true)?;
    let (xbayroff, dis_x) = consensus::<i64>(conn, set_id, "xbayroff", false)?;
    let (ybayroff, dis_y) = consensus::<i64>(conn, set_id, "ybayroff", false)?;
    let (roworder, dis_row) = consensus::<String>(conn, set_id, "roworder", true)?;
    for (col, dis) in [
        ("bayerpat", dis_pat), ("xbayroff", dis_x), ("ybayroff", dis_y), ("roworder", dis_row),
    ] {
        if dis {
            out.disagreements.push(col);
        }
    }
    // The vote already dropped blanks; this catches the residue SQLite's TRIM()
    // does not — it strips spaces only, so a tab- or newline-only value is still
    // a group there while Rust's `trim` calls it empty.
    out.bayerpat = bayerpat.filter(|s| !s.trim().is_empty());
    out.xbayroff = xbayroff;
    out.ybayroff = ybayroff;
    out.roworder = roworder.filter(|s| !s.trim().is_empty());

    // Fallback, BAYERPAT only: a catalog scanned before the columns existed
    // has them all NULL until a rescan, but its stored raw header still holds
    // the pattern. There is deliberately NO blob fallback for the offsets or
    // row order — those were never parsed before, so re-reading one arbitrary
    // member's blob here would reintroduce exactly the LIMIT-1 nondeterminism
    // this consensus replaces. Absent beats guessed.
    if out.bayerpat.is_none() {
        out.bayerpat = bayerpat_from_stored_header(conn, set_id);
        if out.bayerpat.is_some() {
            out.bayerpat_from_blob = true;
            debug!(set_id, "bayerpat column empty for all members — using stored-header blob fallback");
        }
    }
    Ok(out)
}

/// BAYERPAT out of one member's raw stored header. `fits_header.header` stores
/// three shapes (FITS 80-col cards, raw XISF XML, ASIAIR-style "KEY = value"
/// dumps) and `parse_stored_header_keys` is the format-aware accessor that
/// handles all of them (same call pattern as db::operations'
/// `clear_override_for_unchanged_frames` / `get_frame_metadata_originals`).
fn bayerpat_from_stored_header(conn: &Connection, set_id: i64) -> Option<String> {
    // ORDER BY csf.frame_id: this reads ONE arbitrary member, but which one
    // must not be left to SQLite's row order — two runs over the same set
    // would otherwise be free to consolidate differently.
    let row = conn.query_row(
        "SELECT fi.format, fh.header FROM calibration_set_frames csf
         JOIN frames f ON f.id = csf.frame_id
         JOIN files fi ON fi.id = f.file_id
         JOIN fits_header fh ON fh.file_id = f.file_id
         WHERE csf.set_id = ?1 ORDER BY csf.frame_id LIMIT 1",
        [set_id],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    );
    let (format_str, header) = match row {
        Ok(v) => v,
        // No member has a stored header — the ordinary shape, nothing to say.
        Err(rusqlite::Error::QueryReturnedNoRows) => return None,
        Err(e) => {
            // Everything else (a locked or corrupt catalog) used to collapse
            // into the same silent None as "no blob". Same behavior — the
            // fallback is best-effort — but no longer invisible.
            debug!(set_id, error = %e, "stored-header read failed — bayerpat blob fallback skipped");
            return None;
        }
    };
    let format = match format_str.as_str() {
        "FITS" => FileFormat::FITS,
        "XISF" => FileFormat::XISF,
        _ => return None, // Unknown format — skip rather than guess.
    };
    // Returned map is keyed UPPERCASE (see parse_stored_header_keys docs).
    parse_stored_header_keys(format, &header)
        .remove("BAYERPAT")
        .filter(|s| !s.trim().is_empty())
}

fn master_kind_for(imagetyp: &str) -> Option<FrameKind> {
    match imagetyp {
        "Dark" | "MasterDark" => Some(FrameKind::MasterDark),
        "Flat" | "MasterFlat" => Some(FrameKind::MasterFlat),
        "Bias" | "MasterBias" => Some(FrameKind::MasterBias),
        "DarkFlat" | "MasterDarkFlat" => Some(FrameKind::MasterDarkFlat),
        _ => None,
    }
}

pub fn load_header_inputs(conn: &Connection, source_set_id: i64) -> Result<MasterHeaderInputs> {
    // Set-level values (already aggregated by the scanner's clustering).
    // `binning` (calibration_set's text summary, e.g. "1x1") is intentionally
    // unused here: the numeric xbinning/ybinning consumed by MasterHeaderInputs
    // come from the frame-level aggregate query below instead.
    let (imagetyp, exptime, filter, ccd_temp, gain, offset, _binning, instrume, telescop,
         temp_min, temp_max, frame_count, focallen, uuid): (
        String, Option<f64>, Option<String>, Option<f64>, Option<f64>, Option<f64>,
        Option<String>, Option<String>, Option<String>, Option<f64>, Option<f64>,
        i64, Option<f64>, String,
    ) = conn.query_row(
        "SELECT imagetyp, exptime, filter, ccd_temp, gain, offset, binning, instrume,
                telescop, temp_min, temp_max, frame_count, focallen, uuid
         FROM calibration_set WHERE id = ?1",
        [source_set_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?,
                r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?,
                r.get(12)?, r.get(13)?)),
    )?;
    let kind = master_kind_for(&imagetyp)
        .ok_or_else(|| anyhow!("set {source_set_id} has non-calibration imagetyp {imagetyp}"))?;

    // Frame-level aggregates: temp mean, date midpoint, binning ints, pixel size.
    // Exactly the 7 real frame aggregates. The Bayer columns are NOT among them
    // — MAX() over a CFA pattern is meaningless; they go through
    // load_bayer_consensus (majority per column) below.
    let (temp_mean, min_dt, max_dt, xbin, ybin, xpixsz, ypixsz): (
        Option<f64>, Option<String>, Option<String>, Option<i64>, Option<i64>,
        Option<f64>, Option<f64>,
    ) = conn.query_row(
        "SELECT AVG(f.ccd_temp), MIN(f.date_obs), MAX(f.date_obs),
                MAX(f.xbinning), MAX(f.ybinning), MAX(f.xpixsz), MAX(f.ypixsz)
         FROM calibration_set_frames csf
         JOIN frames f ON f.id = csf.frame_id
         WHERE csf.set_id = ?1",
        [source_set_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
    )?;
    // Bayer/CFA geometry by member majority (frames.bayerpat/xbayroff/
    // ybayroff/roworder — all four are real columns since Task 1).
    let bayer = load_bayer_consensus(conn, source_set_id)?;

    let midpoint = match (parse_dt(min_dt.as_deref()), parse_dt(max_dt.as_deref())) {
        (Some(a), Some(b)) => Some(a + (b - a) / 2),
        (Some(a), None) => Some(a),
        _ => None,
    };

    Ok(MasterHeaderInputs {
        kind,
        source_set_id,
        instrume, telescop, filter, exptime,
        gain, offset,
        xbinning: xbin, ybinning: ybin,
        xpixsz, ypixsz, focallen,
        egain: None, // EGAIN is not columnized; omitted from masters (additive later)
        bayerpat: bayer.bayerpat,
        xbayroff: bayer.xbayroff, ybayroff: bayer.ybayroff,
        roworder: bayer.roworder,
        temp_mean: temp_mean.or(ccd_temp),
        temp_min, temp_max,
        date_obs_midpoint: midpoint,
        frame_count: frame_count as u32,
        source_set_uuid: uuid,
    })
}

/// CFA phase is mod 2, so an offset outside {0, 1} means the source header (or
/// our reading of it) is off. Emit it verbatim anyway — normalizing silently
/// would substitute our guess for the camera's statement — but say so.
fn checked_offset(set_id: i64, field: &'static str, value: i64) -> i64 {
    if !(0..=1).contains(&value) {
        warn!(set_id, field, value, "bayer offset outside 0/1 — emitting verbatim");
    }
    value
}

fn parse_dt(s: Option<&str>) -> Option<DateTime<Utc>> {
    s.and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|d| d.with_timezone(&Utc))
}

/// Per-channel central-third means of a freshly integrated master flat — the
/// `[R, G, B]` values behind `ATH_FNR`/`ATH_FNG`/`ATH_FNB`. `data` is the
/// master's own plane, in the same units and over the same window as the
/// global `ATH_FNRM`.
///
/// `None` means "stamp nothing per channel": the set declares no CFA pattern
/// (mono, or a spelling [`Bayer::parse`] cannot vouch for — `build_master_cards`
/// warns about that same value), or the measurement came back degenerate.
/// These constants are DIVISORS downstream, so a non-finite or non-positive
/// one is worse than absent: without them a consumer falls back to the global
/// constant, with them it would divide a whole channel by garbage.
///
/// **The CFA phase defaults to (0, 0) when the members declared none.** That
/// is a guess, and it is allowed here and nowhere else: it only decides which
/// pixels are averaged together — a wrong guess costs a colour cast the
/// operator can see — whereas the same guess written into `XBAYROFF` would be
/// a fabricated claim every future debayer acts on.
#[cfg(feature = "render")]
pub(crate) fn measure_flat_channel_norms(
    inputs: &MasterHeaderInputs,
    data: &[f32],
    width: usize,
    height: usize,
) -> Option<[f64; 3]> {
    let set_id = inputs.source_set_id;
    let pattern = Bayer::parse(inputs.bayerpat.as_deref()?)?;
    let assumed = match (inputs.xbayroff, inputs.ybayroff) {
        (Some(_), Some(_)) => None,
        (None, Some(_)) => Some("xbayroff"),
        (Some(_), None) => Some("ybayroff"),
        (None, None) => Some("xbayroff,ybayroff"),
    };
    if let Some(field) = assumed {
        debug!(set_id, field, "cfa phase not declared — per-channel flat means assume 0");
    }
    let geom = CfaGeometry {
        pattern,
        xoff: inputs.xbayroff.unwrap_or(0),
        yoff: inputs.ybayroff.unwrap_or(0),
    };
    let means = central_third_channel_means(data, width, height, geom);
    if !means.iter().all(|m| m.is_finite() && *m > 0.0) {
        warn!(set_id, value = ?means, "per-channel flat constants degenerate — omitted");
        return None;
    }
    Some(means)
}

pub fn build_master_cards(
    inputs: &MasterHeaderInputs,
    app_version: &str,
    recipe_summary: &str, // e.g. "winsorized(3.0,3.0) n=24"
    member_hash: &str,
    flat_norm: Option<f64>, // stamps ATH_FNRM when Some
    // stamps ATH_FNR/ATH_FNG/ATH_FNB when Some — see
    // `measure_flat_channel_norms`, which is what produces a vouched-for
    // triple. Ignored without `flat_norm`: the per-channel constants qualify
    // the global one and are meaningless on their own.
    flat_channel_norms: Option<[f64; 3]>,
) -> std::result::Result<Vec<Card>, FitsWriteError> {
    let mut b = HeaderBuilder::new(inputs.kind).swcreate(app_version);
    if let Some(v) = inputs.exptime {
        b = b.exptime(v);
    }
    if let Some(dt) = inputs.date_obs_midpoint {
        b = b.date_obs(dt);
    }
    if let Some(t) = inputs.temp_mean {
        b = b.ccd_temp(t);
    }
    if let Some(g) = inputs.gain {
        b = b.gain(g.round() as i64);
    }
    if let Some(o) = inputs.offset {
        b = b.offset(o.round() as i64);
    }
    if let (Some(x), Some(y)) = (inputs.xbinning, inputs.ybinning) {
        b = b.binning(x, y);
    }
    if let (Some(x), Some(y)) = (inputs.xpixsz, inputs.ypixsz) {
        b = b.pixel_size(x, y);
    }
    if let Some(v) = &inputs.instrume {
        b = b.instrume(v);
    }
    if let Some(v) = &inputs.telescop {
        b = b.telescop(v);
    }
    if let Some(v) = inputs.focallen {
        b = b.focallen(v);
    }
    if let Some(v) = &inputs.filter {
        b = b.filter(v);
    }
    let set_id = inputs.source_set_id;
    if let Some(p) = &inputs.bayerpat {
        match Bayer::parse(p) {
            Some(bp) => {
                b = b.bayer_pattern(bp);
                // The offsets are the CFA phase of THAT pattern, so they ride
                // with it — and only when the members actually declared one.
                // A missing offset stays missing: XBAYROFF=0 is a positive
                // claim about the mosaic, and a wrong one swaps R and B.
                if let Some(x) = inputs.xbayroff {
                    b = b.bayer_x_offset(checked_offset(set_id, "xbayroff", x));
                }
                if let Some(y) = inputs.ybayroff {
                    b = b.bayer_y_offset(checked_offset(set_id, "ybayroff", y));
                }
            }
            None => warn!(
                set_id, bayerpat = %p,
                "unrecognized bayer pattern — bayer cards omitted from master"
            ),
        }
    } else if inputs.xbayroff.is_some() || inputs.ybayroff.is_some() {
        // An offset is the phase OF a pattern; with no pattern to attach it
        // to it describes nothing a debayer can act on, so it is dropped.
        // Correct — but it is the signature of a half-populated header, so
        // say it rather than leave the operator wondering where it went.
        debug!(set_id, "bayer offsets without a pattern — offset cards omitted");
    }
    // ROWORDER is independent of the CFA pattern (mono frames have a row order
    // too) and the integration engine writes bands in source order, so the
    // members' row order carries into the master unchanged.
    if let Some(r) = &inputs.roworder {
        // The column stores the parsed value verbatim; normalize quoting and
        // case before matching, and emit the canonical spelling.
        let normalized = r.trim().trim_matches('\'').trim().to_ascii_uppercase();
        match normalized.as_str() {
            "TOP-DOWN" | "BOTTOM-UP" => b = b.roworder(&normalized),
            _ => warn!(
                set_id, roworder = %r,
                "unrecognized row order — ROWORDER omitted from master"
            ),
        }
    }
    b = b
        .ath_src(&inputs.source_set_uuid)
        .ath_n(inputs.frame_count)
        .ath_rej(recipe_summary)
        .ath_ver(app_version)
        .ath_hsh(member_hash);
    if let (Some(min), Some(max)) = (inputs.temp_min, inputs.temp_max) {
        b = b.ath_temp_span(min, max);
    }
    if let Some(n) = flat_norm {
        b = b.custom(
            Card::new("ATH_FNRM", crate::fits_writer::CardValue::Real(n))?
                .with_comment("central-third mean of this master flat"),
        );
        // The per-channel constants ride WITH the global one: same window,
        // same units, and every reader needs ATH_FNRM as its fallback anyway
        // (mono masters and imported flats never carry these three).
        if let Some(ch) = flat_channel_norms {
            for (kw, v) in [("ATH_FNR", ch[0]), ("ATH_FNG", ch[1]), ("ATH_FNB", ch[2])] {
                b = b.custom(
                    Card::new(kw, crate::fits_writer::CardValue::Real(v))?
                        .with_comment("per-channel flat normalization constant"),
                );
            }
        }
    }
    b.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::keywords::FrameKind;
    use rusqlite::Connection;

    fn seed(conn: &Connection) -> i64 {
        crate::db::schema::init_db(conn).unwrap();
        conn.execute(
            "INSERT INTO calibration_set
             (imagetyp, exptime, ccd_temp, gain, offset, binning, instrume, telescop,
              date, date_start, date_end, temp_min, temp_max, frame_count, focallen)
             VALUES ('Dark', 300.0, -10.0, 100.0, 50.0, '1x1', 'TestCam', 'TestScope',
              '2026-06-28', '2026-06-28T20:00:00Z', '2026-06-28T22:00:00Z',
              -10.6, -9.4, 2, 540.0)",
            [],
        ).unwrap();
        let set_id = conn.last_insert_rowid();
        for (i, dt) in ["2026-06-28T20:00:00Z", "2026-06-28T22:00:00Z"].iter().enumerate() {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
                 VALUES (?1, ?2, 10, '2026-06-28', 'FITS')",
                rusqlite::params![format!("/d/f{i}.fits"), format!("f{i}.fits")],
            ).unwrap();
            let file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp, instrume, exptime, gain, offset,
                                     binning, xbinning, ybinning, ccd_temp, date_obs, xpixsz, ypixsz)
                 VALUES (?1, 'Dark', 'TestCam', 300.0, 100.0, 50.0, '1x1', 1, 1, ?2, ?3, 3.76, 3.76)",
                rusqlite::params![file_id, -10.0 - (i as f64) * 0.5, dt],
            ).unwrap();
            let frame_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![set_id, frame_id],
            ).unwrap();
        }
        set_id
    }

    /// Pins the exact path that broke in the field: the REAL
    /// `recipe.describe()`-derived summary (not a hand-written ASCII stand-in)
    /// must survive `build_master_cards` → `format_card` for every recipe.
    #[test]
    fn recipe_describe_summary_survives_card_formatting() {
        use crate::fits_writer::card::format_card;
        use crate::integration::combine::{IntegrationRecipe, Rejection};
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        let inputs = load_header_inputs(&conn, set_id).unwrap();
        let recipes = [
            IntegrationRecipe::average(Rejection::None),
            IntegrationRecipe::median(Rejection::None),
            IntegrationRecipe::average(Rejection::PercentileClip { low: 0.2, high: 0.1 }),
            IntegrationRecipe::average(Rejection::SigmaClip { sigma_low: 4.0, sigma_high: 3.0 }),
            IntegrationRecipe::average(Rejection::WinsorizedSigma { sigma_low: 3.0, sigma_high: 3.0 }),
            IntegrationRecipe::median(Rejection::LinearFitClip { sigma_low: 5.0, sigma_high: 2.5 }),
        ];
        for r in recipes {
            let summary = format!("{} n=24", r.describe());
            let cards = build_master_cards(&inputs, "0.5.0", &summary, "cafe", None, None).unwrap();
            for c in &cards {
                format_card(c).unwrap_or_else(|e| {
                    panic!("card {} failed to format for summary {summary:?}: {e}", c.keyword)
                });
            }
        }
    }

    #[test]
    fn consolidated_cards_cover_the_vocabulary() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        let inputs = load_header_inputs(&conn, set_id).unwrap();
        assert_eq!(inputs.kind, FrameKind::MasterDark);
        assert_eq!(inputs.frame_count, 2);
        // midpoint of 20:00 and 22:00 is 21:00
        assert_eq!(inputs.date_obs_midpoint.unwrap().to_rfc3339(), "2026-06-28T21:00:00+00:00");
        let cards = build_master_cards(&inputs, "0.2.5", "winsorized(3.0,3.0) n=2", "cafe", None, None).unwrap();
        let find = |k: &str| cards.iter().find(|c| c.keyword == k);
        assert!(find("IMAGETYP").is_some());
        assert!(find("INSTRUME").is_some());
        assert!(find("EXPTIME").is_some());
        assert!(find("CCD-TEMP").is_some());
        assert!(find("ATH_TMIN").is_some() && find("ATH_TMAX").is_some());
        assert!(find("ATH_SRC").is_some());
        assert!(find("ATH_N").is_some());
        assert!(find("ATH_REJ").is_some());
        assert!(find("ATH_HSH").is_some());
        assert!(find("SWCREATE").is_some());
        assert!(find("ATH_FNRM").is_none(), "darks carry no flat norm");
    }

    #[test]
    fn flat_norm_card_present_for_flats() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        conn.execute("UPDATE calibration_set SET imagetyp='Flat', filter='L' WHERE id=?1", [set_id]).unwrap();
        let inputs = load_header_inputs(&conn, set_id).unwrap();
        assert_eq!(inputs.kind, FrameKind::MasterFlat);
        let cards = build_master_cards(&inputs, "0.2.5", "percentile(0.2,0.02) n=2", "cafe", Some(1234.5), None).unwrap();
        let f = cards.iter().find(|c| c.keyword == "ATH_FNRM").expect("ATH_FNRM");
        assert!(matches!(f.value, Some(crate::fits_writer::CardValue::Real(v)) if (v - 1234.5).abs() < 1e-9));
        assert!(cards.iter().any(|c| c.keyword == "FILTER"));
    }

    /// Appends one more member frame to a set seeded by [`seed`]. The majority
    /// tests need three members for a majority to exist at all; [`seed`] is
    /// left at two so the date-midpoint assertion above keeps its fixture.
    fn add_member(conn: &Connection, set_id: i64, tag: &str, date_obs: &str) -> i64 {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format)
             VALUES (?1, ?2, 10, '2026-06-28', 'FITS')",
            rusqlite::params![format!("/d/{tag}.fits"), format!("{tag}.fits")],
        ).unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp, instrume, exptime, gain, offset,
                                 binning, xbinning, ybinning, ccd_temp, date_obs, xpixsz, ypixsz)
             VALUES (?1, 'Dark', 'TestCam', 300.0, 100.0, 50.0, '1x1', 1, 1, -10.0, ?2, 3.76, 3.76)",
            rusqlite::params![file_id, date_obs],
        ).unwrap();
        let frame_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![set_id, frame_id],
        ).unwrap();
        frame_id
    }

    /// Stamp Task-1's per-frame Bayer columns on the set's members, one tuple
    /// (bayerpat, xbayroff, ybayroff, roworder) per member in frame-id order.
    fn set_member_bayer(
        conn: &Connection,
        set_id: i64,
        rows: &[(Option<&str>, Option<i64>, Option<i64>, Option<&str>)],
    ) {
        let ids: Vec<i64> = conn
            .prepare("SELECT frame_id FROM calibration_set_frames WHERE set_id = ?1 ORDER BY frame_id")
            .unwrap()
            .query_map([set_id], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(ids.len(), rows.len(), "fixture must cover every member frame");
        for (id, (bp, x, y, ro)) in ids.into_iter().zip(rows) {
            conn.execute(
                "UPDATE frames SET bayerpat = ?2, xbayroff = ?3, ybayroff = ?4, roworder = ?5
                 WHERE id = ?1",
                rusqlite::params![id, bp, x, y, ro],
            ).unwrap();
        }
    }

    fn cards_for(conn: &Connection, set_id: i64) -> Vec<Card> {
        let inputs = load_header_inputs(conn, set_id).unwrap();
        build_master_cards(&inputs, "0.2.5", "median n=3", "cafe", None, None).unwrap()
    }

    fn card_str<'a>(cards: &'a [Card], kw: &str) -> Option<&'a str> {
        cards.iter().find(|c| c.keyword == kw).and_then(|c| match &c.value {
            Some(crate::fits_writer::CardValue::Str(v)) => Some(v.as_str()),
            _ => None,
        })
    }

    fn card_int(cards: &[Card], kw: &str) -> Option<i64> {
        cards.iter().find(|c| c.keyword == kw).and_then(|c| match &c.value {
            Some(crate::fits_writer::CardValue::Integer(v)) => Some(*v),
            _ => None,
        })
    }

    fn card_real(cards: &[Card], kw: &str) -> Option<f64> {
        cards.iter().find(|c| c.keyword == kw).and_then(|c| match &c.value {
            Some(crate::fits_writer::CardValue::Real(v)) => Some(*v),
            _ => None,
        })
    }

    /// GAIN and OFFSET are what make a master matchable: both default to
    /// Exact + required for lights, and a set that declares neither is
    /// refused by the matcher as "cannot compare" — which is why every
    /// imported master in the owner's catalog had zero links (2026-09-05
    /// design, Finding 3). A master this app builds must never join them,
    /// so the two cards are pinned here.
    #[test]
    fn master_cards_carry_gain_and_offset_from_the_source_set() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        let inputs = load_header_inputs(&conn, set_id).unwrap();
        let cards = build_master_cards(&inputs, "0.5.5", "winsorized(3,3) n=2", "beef", None, None)
            .unwrap();
        assert_eq!(card_int(&cards, "GAIN"), Some(100), "GAIN comes from the source set");
        assert_eq!(card_int(&cards, "OFFSET"), Some(50), "OFFSET comes from the source set");
    }

    /// A source set that declares neither must not have the cards invented
    /// for it — a fabricated GAIN would match the wrong darks silently.
    #[test]
    fn master_cards_omit_gain_and_offset_when_the_source_declares_none() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        conn.execute("UPDATE calibration_set SET gain = NULL, offset = NULL WHERE id = ?1", [set_id])
            .unwrap();
        let inputs = load_header_inputs(&conn, set_id).unwrap();
        let cards = build_master_cards(&inputs, "0.5.5", "winsorized(3,3) n=2", "beef", None, None)
            .unwrap();
        assert!(!has_card(&cards, "GAIN"));
        assert!(!has_card(&cards, "OFFSET"));
    }

    fn has_card(cards: &[Card], kw: &str) -> bool {
        cards.iter().any(|c| c.keyword == kw)
    }

    /// The whole point of the columns: a master must carry the members' REAL
    /// CFA phase and row order as VALUES, not merely "some card is present".
    /// A fabricated XBAYROFF=0 on a genuinely phase-1 sensor swaps colour
    /// channels for every downstream debayer.
    #[test]
    fn master_bayer_cards_carry_real_offsets_and_roworder() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        add_member(&conn, set_id, "f2", "2026-06-28T21:00:00Z");
        set_member_bayer(
            &conn,
            set_id,
            &[
                (Some("RGGB"), Some(1), Some(0), Some("BOTTOM-UP")),
                (Some("RGGB"), Some(1), Some(0), Some("BOTTOM-UP")),
                (Some("RGGB"), Some(1), Some(0), Some("BOTTOM-UP")),
            ],
        );
        let cards = cards_for(&conn, set_id);
        assert_eq!(card_str(&cards, "BAYERPAT"), Some("RGGB"));
        assert_eq!(card_int(&cards, "XBAYROFF"), Some(1), "real phase, not a fabricated 0");
        assert_eq!(card_int(&cards, "YBAYROFF"), Some(0));
        assert_eq!(card_str(&cards, "ROWORDER"), Some("BOTTOM-UP"));
    }

    /// Members can disagree (a set re-grouped across a firmware change, a
    /// hand-edited header). Majority wins, deterministically, and the
    /// disagreement is reported — never silently averaged away.
    #[test]
    fn member_disagreement_warns_and_uses_majority() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        add_member(&conn, set_id, "f2", "2026-06-28T21:00:00Z");
        set_member_bayer(
            &conn,
            set_id,
            &[
                (Some("RGGB"), Some(1), None, None),
                (Some("RGGB"), Some(1), None, None),
                (Some("BGGR"), Some(0), None, None),
            ],
        );
        let bayer = load_bayer_consensus(&conn, set_id).unwrap();
        assert_eq!(bayer.bayerpat.as_deref(), Some("RGGB"), "majority pattern");
        assert_eq!(bayer.xbayroff, Some(1), "majority offset");
        assert_eq!(
            bayer.disagreements, vec!["bayerpat", "xbayroff"],
            "both split columns must be reported, silent columns must not"
        );
        let cards = cards_for(&conn, set_id);
        assert_eq!(card_str(&cards, "BAYERPAT"), Some("RGGB"));
        assert_eq!(card_int(&cards, "XBAYROFF"), Some(1));
    }

    /// A 2-vs-2 split has no majority. The tie-break is `value ASC`, so the
    /// same set always consolidates to the same master header instead of
    /// depending on SQLite's row order.
    #[test]
    fn tied_disagreement_breaks_deterministically() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        add_member(&conn, set_id, "f2", "2026-06-28T21:00:00Z");
        add_member(&conn, set_id, "f3", "2026-06-28T21:30:00Z");
        set_member_bayer(
            &conn,
            set_id,
            &[
                (Some("RGGB"), None, None, None),
                (Some("BGGR"), None, None, None),
                (Some("BGGR"), None, None, None),
                (Some("RGGB"), None, None, None),
            ],
        );
        let bayer = load_bayer_consensus(&conn, set_id).unwrap();
        assert_eq!(bayer.bayerpat.as_deref(), Some("BGGR"), "ties break on value ASC");
        assert_eq!(bayer.disagreements, vec!["bayerpat"]);
    }

    /// Writers disagree about SPELLING, not about the sensor: `"rggb"`,
    /// `" RGGB "` and `"RGGB"` are one claim. Counting them as three groups
    /// reported a disagreement that does not exist and then handed the answer
    /// to whichever spelling sorted first.
    #[test]
    fn spelling_variants_are_one_consensus_group_not_a_disagreement() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        add_member(&conn, set_id, "f2", "2026-06-28T21:00:00Z");
        set_member_bayer(
            &conn,
            set_id,
            &[
                (Some("rggb"), None, None, Some("top-down")),
                (Some(" RGGB "), None, None, Some("TOP-DOWN")),
                (Some("RGGB"), None, None, Some(" top-down ")),
            ],
        );
        let bayer = load_bayer_consensus(&conn, set_id).unwrap();
        assert_eq!(bayer.bayerpat.as_deref(), Some("RGGB"), "one claim, canonically spelled");
        assert_eq!(bayer.roworder.as_deref(), Some("TOP-DOWN"));
        assert!(
            bayer.disagreements.is_empty(),
            "one claim spelled three ways is not a disagreement, got {:?}",
            bayer.disagreements
        );
        let cards = cards_for(&conn, set_id);
        assert_eq!(card_str(&cards, "BAYERPAT"), Some("RGGB"));
        assert_eq!(card_str(&cards, "ROWORDER"), Some("TOP-DOWN"));
    }

    /// The same folding decides the majority: two members spelling RGGB
    /// differently outvote one genuine BGGR. Ungrouped they were 1-1-1, and
    /// the tie-break handed the set to the minority pattern.
    #[test]
    fn spelling_variants_count_together_toward_the_majority() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        add_member(&conn, set_id, "f2", "2026-06-28T21:00:00Z");
        set_member_bayer(
            &conn,
            set_id,
            &[
                (Some("rggb"), None, None, None),
                (Some("RGGB"), None, None, None),
                (Some("BGGR"), None, None, None),
            ],
        );
        let bayer = load_bayer_consensus(&conn, set_id).unwrap();
        assert_eq!(bayer.bayerpat.as_deref(), Some("RGGB"), "2 spellings of one claim beat 1 of another");
        assert_eq!(bayer.disagreements, vec!["bayerpat"], "BGGR is a real disagreement");
    }

    /// A blank column is a SILENCE, not a vote. Two members carrying an empty
    /// BAYERPAT used to outvote the one that actually declared RGGB; the
    /// winning `""` was then dropped on the way out, so a genuinely declared
    /// mosaic shipped as a master with no BAYERPAT at all — and warned about a
    /// "disagreement" nobody had.
    #[test]
    fn blank_values_never_vote_against_a_real_declaration() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        add_member(&conn, set_id, "f2", "2026-06-28T21:00:00Z");
        set_member_bayer(
            &conn,
            set_id,
            &[
                (Some(""), None, None, Some("")),
                (Some("   "), None, None, Some("  ")),
                (Some("RGGB"), None, None, Some("TOP-DOWN")),
            ],
        );
        let bayer = load_bayer_consensus(&conn, set_id).unwrap();
        assert_eq!(bayer.bayerpat.as_deref(), Some("RGGB"), "2 blanks must not outvote 1 declaration");
        assert_eq!(bayer.roworder.as_deref(), Some("TOP-DOWN"));
        assert!(!bayer.bayerpat_from_blob, "the column answered — no blob fallback");
        assert!(
            bayer.disagreements.is_empty(),
            "a blank contradicts nothing, got {:?}",
            bayer.disagreements
        );
        let cards = cards_for(&conn, set_id);
        assert_eq!(card_str(&cards, "BAYERPAT"), Some("RGGB"));
        assert_eq!(card_str(&cards, "ROWORDER"), Some("TOP-DOWN"));
    }

    /// A rescanned catalog has both the column and the old blob. The column
    /// wins — it is the consensus of every member, the blob is one arbitrary
    /// member's raw text.
    #[test]
    fn column_consensus_beats_stored_header_blob() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        let header = format!(
            "{:<80}\n{:<80}",
            "BAYERPAT= 'GBRG    '           / Bayer color pattern", "END",
        );
        seed_stored_headers(&conn, set_id, "FITS", &header);
        set_member_bayer(
            &conn,
            set_id,
            &[(Some("RGGB"), Some(1), Some(1), None), (Some("RGGB"), Some(1), Some(1), None)],
        );
        let bayer = load_bayer_consensus(&conn, set_id).unwrap();
        assert!(!bayer.bayerpat_from_blob, "column present — no fallback");
        assert_eq!(bayer.bayerpat.as_deref(), Some("RGGB"));
        assert_eq!(card_str(&cards_for(&conn, set_id), "BAYERPAT"), Some("RGGB"));
    }

    /// Values we cannot vouch for are omitted, not guessed at — and the rest
    /// of the header still lands.
    #[test]
    fn unknown_pattern_and_row_order_are_omitted_not_guessed() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        set_member_bayer(
            &conn,
            set_id,
            &[
                (Some("XYZW"), Some(1), Some(0), Some("SIDEWAYS")),
                (Some("XYZW"), Some(1), Some(0), Some("SIDEWAYS")),
            ],
        );
        let cards = cards_for(&conn, set_id);
        for kw in ["BAYERPAT", "XBAYROFF", "YBAYROFF", "ROWORDER"] {
            assert!(!has_card(&cards, kw), "{kw} must be omitted for an unvouchable value");
        }
        assert!(has_card(&cards, "IMAGETYP"), "the rest of the header still builds");
    }

    /// An out-of-domain phase (CFA phase is mod 2) is reported but still
    /// emitted verbatim — silently rewriting the camera's statement would be
    /// its own fabrication.
    #[test]
    fn out_of_domain_offset_is_emitted_verbatim() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        set_member_bayer(
            &conn,
            set_id,
            &[(Some("RGGB"), Some(3), Some(0), None), (Some("RGGB"), Some(3), Some(0), None)],
        );
        assert_eq!(card_int(&cards_for(&conn, set_id), "XBAYROFF"), Some(3));
    }

    /// The parsers strip quotes, but the column stores whatever they yielded —
    /// normalize on the way out so a quoted/lowercase spelling still produces
    /// a canonical card rather than being dropped as unrecognized.
    #[test]
    fn row_order_spelling_is_normalized() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        set_member_bayer(
            &conn,
            set_id,
            &[(None, None, None, Some("'top-down' ")), (None, None, None, Some("'top-down' "))],
        );
        assert_eq!(card_str(&cards_for(&conn, set_id), "ROWORDER"), Some("TOP-DOWN"));
    }

    /// Nothing declared by any member → no Bayer cards at all. A master that
    /// says nothing is honest; one that says XBAYROFF=0 is a lie a debayer
    /// will act on.
    #[test]
    fn missing_bayer_data_emits_no_bayer_cards_and_no_zero_fabrication() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        let cards = cards_for(&conn, set_id);
        for kw in ["BAYERPAT", "XBAYROFF", "YBAYROFF", "ROWORDER"] {
            assert!(!has_card(&cards, kw), "{kw} must be absent when no member declares it");
        }
    }

    /// The exact shape the old `.unwrap_or(0)` got wrong: the pattern is
    /// known but the phase is not. Emit BAYERPAT alone — absent beats
    /// fabricated.
    #[test]
    fn pattern_without_offsets_emits_no_fabricated_zeros() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        set_member_bayer(
            &conn,
            set_id,
            &[(Some("RGGB"), None, None, None), (Some("RGGB"), None, None, None)],
        );
        let cards = cards_for(&conn, set_id);
        assert_eq!(card_str(&cards, "BAYERPAT"), Some("RGGB"));
        assert!(!has_card(&cards, "XBAYROFF"), "no offset consensus — no XBAYROFF card");
        assert!(!has_card(&cards, "YBAYROFF"), "no offset consensus — no YBAYROFF card");
    }

    /// A 6x6 RGGB mosaic painted from the pattern definition itself — even
    /// column/even row is R, odd/odd is B, the diagonal is G — one constant
    /// per colour. Painted here rather than via `cfa_channel_at` so the
    /// assertions below test the mapping instead of restating it.
    #[cfg(feature = "render")]
    fn mosaic_rggb_6x6(r: f32, g: f32, blue: f32) -> Vec<f32> {
        let mut data = vec![0f32; 36];
        for y in 0..6usize {
            for x in 0..6usize {
                data[y * 6 + x] = match (x % 2 == 0, y % 2 == 0) {
                    (true, true) => r,
                    (false, false) => blue,
                    _ => g,
                };
            }
        }
        data
    }

    /// Turn the seeded set into a flat and stamp one Bayer tuple on every
    /// member. Returns the loaded header inputs.
    #[cfg(feature = "render")]
    fn cfa_flat_inputs(
        conn: &Connection,
        set_id: i64,
        bayerpat: Option<&str>,
        xoff: Option<i64>,
        yoff: Option<i64>,
    ) -> MasterHeaderInputs {
        conn.execute("UPDATE calibration_set SET imagetyp='Flat' WHERE id=?1", [set_id]).unwrap();
        let row = (bayerpat, xoff, yoff, None);
        set_member_bayer(conn, set_id, &[row, row]);
        load_header_inputs(conn, set_id).unwrap()
    }

    /// The point of Package 2: a CFA master flat carries the level of each
    /// colour, not only the blend of all three. A consumer dividing a mosaic
    /// by the blend leaves the flat's own colour response in the light.
    #[test]
    #[cfg(feature = "render")]
    fn cfa_master_flat_stamps_per_channel_constants_beside_the_global_one() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        let inputs = cfa_flat_inputs(&conn, set_id, Some("RGGB"), Some(0), Some(0));
        let data = mosaic_rggb_6x6(2000.0, 4000.0, 1000.0);
        let channels =
            measure_flat_channel_norms(&inputs, &data, 6, 6).expect("an RGGB flat has channel constants");
        let global = crate::integration::engine::central_third_mean(&data, 6, 6);
        let cards =
            build_master_cards(&inputs, "0.2.5", "percentile(0.2,0.02) n=2", "cafe", Some(global), Some(channels))
                .unwrap();
        // The central-third window is x,y in 2..4: one R, two G, one B — so
        // the global constant is their blend and each channel keeps its own.
        assert_eq!(card_real(&cards, "ATH_FNRM"), Some(2750.0));
        assert_eq!(card_real(&cards, "ATH_FNR"), Some(2000.0));
        assert_eq!(card_real(&cards, "ATH_FNG"), Some(4000.0));
        assert_eq!(card_real(&cards, "ATH_FNB"), Some(1000.0));
        // Keyword + comment must fit one 80-column card: a CommentTooLong
        // here would fail the write AFTER the whole integration ran.
        for c in &cards {
            crate::fits_writer::card::format_card(c)
                .unwrap_or_else(|e| panic!("card {} failed to format: {e}", c.keyword));
        }
    }

    /// Mono flats are untouched: no pattern, no per-channel claim, and the
    /// global constant every existing consumer reads is still there.
    #[test]
    #[cfg(feature = "render")]
    fn mono_master_flat_keeps_the_global_constant_alone() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        // `seed` leaves every Bayer column NULL — that IS the mono shape.
        let inputs = cfa_flat_inputs(&conn, set_id, None, None, None);
        let data = vec![1000.0f32; 36];
        assert!(
            measure_flat_channel_norms(&inputs, &data, 6, 6).is_none(),
            "no pattern — nothing to measure per channel"
        );
        let cards = build_master_cards(&inputs, "0.2.5", "median n=2", "cafe", Some(1000.0), None).unwrap();
        assert_eq!(card_real(&cards, "ATH_FNRM"), Some(1000.0));
        for kw in ["ATH_FNR", "ATH_FNG", "ATH_FNB"] {
            assert!(!has_card(&cards, kw), "{kw} must be absent on a mono flat");
        }
    }

    /// The declared phase reaches the MATH, not just the header: read one
    /// column out of phase, the same pixels are labelled differently and the
    /// constants change accordingly.
    #[test]
    #[cfg(feature = "render")]
    fn per_channel_constants_follow_the_declared_cfa_phase() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        let inputs = cfa_flat_inputs(&conn, set_id, Some("RGGB"), Some(1), Some(0));
        let data = mosaic_rggb_6x6(2000.0, 4000.0, 1000.0);
        let channels = measure_flat_channel_norms(&inputs, &data, 6, 6).expect("channel constants");
        // Window pixels (2,2)/(3,2)/(2,3)/(3,3) are painted R/G/G/B; at xoff=1
        // they are labelled G/R/B/G, so R reads the G paint and G averages the
        // R and B paints.
        assert_eq!(channels, [4000.0, 1500.0, 4000.0]);
        let cards = build_master_cards(&inputs, "0.2.5", "median n=2", "cafe", Some(2750.0), Some(channels)).unwrap();
        assert_eq!(card_int(&cards, "XBAYROFF"), Some(1), "the same phase is what the header declares");
    }

    /// The one place a missing CFA phase may be assumed: the measurement
    /// picks pixels at phase 0 and says so in the log, while the header still
    /// refuses to invent XBAYROFF/YBAYROFF.
    #[test]
    #[cfg(feature = "render")]
    fn absent_cfa_phase_is_assumed_for_the_math_but_never_for_the_header() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        let inputs = cfa_flat_inputs(&conn, set_id, Some("RGGB"), None, None);
        let data = mosaic_rggb_6x6(2000.0, 4000.0, 1000.0);
        let channels = measure_flat_channel_norms(&inputs, &data, 6, 6).expect("phase 0 is assumed for the math");
        assert_eq!(channels, [2000.0, 4000.0, 1000.0]);
        let cards = build_master_cards(&inputs, "0.2.5", "median n=2", "cafe", Some(2750.0), Some(channels)).unwrap();
        assert_eq!(card_str(&cards, "BAYERPAT"), Some("RGGB"));
        assert!(!has_card(&cards, "XBAYROFF"), "an assumed phase must not become a claim");
        assert!(!has_card(&cards, "YBAYROFF"), "an assumed phase must not become a claim");
    }

    /// A channel constant is a DIVISOR downstream. A zero (or negative, or
    /// non-finite) one is worse than absent, so the three cards are dropped
    /// together and the consumer falls back to the global constant.
    #[test]
    #[cfg(feature = "render")]
    fn degenerate_channel_constants_are_omitted_and_the_global_one_survives() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        let inputs = cfa_flat_inputs(&conn, set_id, Some("RGGB"), Some(0), Some(0));
        // Blue never got light — the whole channel reads 0.
        let data = mosaic_rggb_6x6(2000.0, 4000.0, 0.0);
        assert!(
            measure_flat_channel_norms(&inputs, &data, 6, 6).is_none(),
            "a zero channel must not be stamped as a divisor"
        );
        let cards = build_master_cards(&inputs, "0.2.5", "median n=2", "cafe", Some(2500.0), None).unwrap();
        assert_eq!(card_real(&cards, "ATH_FNRM"), Some(2500.0));
        for kw in ["ATH_FNR", "ATH_FNG", "ATH_FNB"] {
            assert!(!has_card(&cards, kw), "{kw} must be dropped with its siblings");
        }
    }

    /// A master that is not a flat has no flat constant, so the per-channel
    /// ones have nothing to qualify — they ride with ATH_FNRM or not at all.
    #[test]
    fn per_channel_constants_never_appear_without_the_global_one() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        let inputs = load_header_inputs(&conn, set_id).unwrap();
        let cards =
            build_master_cards(&inputs, "0.2.5", "median n=2", "cafe", None, Some([2000.0, 4000.0, 1000.0])).unwrap();
        for kw in ["ATH_FNRM", "ATH_FNR", "ATH_FNG", "ATH_FNB"] {
            assert!(!has_card(&cards, kw), "{kw} must be absent on a dark");
        }
    }

    /// Attach the same stored-header blob (and format) to every member file
    /// of the set — the BAYERPAT blob fallback uses `LIMIT 1` with no ORDER
    /// BY, so seeding all members keeps the test deterministic regardless of
    /// which row SQLite returns first.
    fn seed_stored_headers(conn: &Connection, set_id: i64, format: &str, header: &str) {
        conn.execute(
            &format!(
                "UPDATE files SET format = ?1 WHERE id IN (
                     SELECT f.file_id FROM calibration_set_frames csf
                     JOIN frames f ON f.id = csf.frame_id WHERE csf.set_id = {set_id})"
            ),
            [format],
        ).unwrap();
        conn.execute(
            &format!(
                "INSERT INTO fits_header (file_id, header)
                 SELECT f.file_id, ?1 FROM calibration_set_frames csf
                 JOIN frames f ON f.id = csf.frame_id WHERE csf.set_id = {set_id}"
            ),
            [header],
        ).unwrap();
    }

    /// Blob-fallback shape: `frames.bayerpat` is NULL (a pre-Task-1 catalog
    /// that has not rescanned), so BAYERPAT comes off the stored header — and
    /// nothing else does. The offsets were never parsed into those blobs'
    /// era of the catalog, so they must stay ABSENT rather than become zeros.
    fn assert_bayer_consolidated(conn: &Connection, set_id: i64) {
        let bayer = load_bayer_consensus(conn, set_id).unwrap();
        assert!(bayer.bayerpat_from_blob, "column is NULL — this is the fallback path");
        let inputs = load_header_inputs(conn, set_id).unwrap();
        assert_eq!(inputs.bayerpat.as_deref(), Some("RGGB"));
        let cards = build_master_cards(&inputs, "0.2.5", "winsorized(3.0,3.0) n=2", "cafe", None, None).unwrap();
        assert_eq!(card_str(&cards, "BAYERPAT"), Some("RGGB"));
        assert!(!has_card(&cards, "XBAYROFF"), "no offset in the blob era — no XBAYROFF card");
        assert!(!has_card(&cards, "YBAYROFF"), "no offset in the blob era — no YBAYROFF card");
    }

    #[test]
    fn bayerpat_from_fits_card_header() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        // FITS shape: 80-col cards joined by \n (FitsHeader::to_header_text).
        let header = format!(
            "{:<80}\n{:<80}\n{:<80}",
            "BAYERPAT= 'RGGB    '           / Bayer color pattern",
            "EXPTIME =                300.0 / Exposure time in seconds",
            "END",
        );
        seed_stored_headers(&conn, set_id, "FITS", &header);
        assert_bayer_consolidated(&conn, set_id);
    }

    #[test]
    fn bayerpat_from_asiair_plain_dump() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        // ASIAIR shape: plain "KEY = value" dump. parse_fits_card_text must
        // yield NOTHING for this blob so the dispatcher falls through to
        // parse_keyword_eq_text — that requires every keyword be 8 chars
        // (a <=7-char key like CREATOR puts "= " at cols 8-10 and would
        // accidentally parse as a FITS card, masking the fallback path).
        let dump = "Captured FITS Keywords:\n=======================\n\nBAYERPAT = RGGB\nXBINNING = 1\n";
        seed_stored_headers(&conn, set_id, "FITS", dump);
        assert_bayer_consolidated(&conn, set_id);
    }

    /// The blob fallback reads ONE member's raw text. Which one must not be
    /// left to SQLite's row order — the lowest frame id wins, every run, so a
    /// set whose members' blobs disagree still consolidates the same way twice.
    #[test]
    fn blob_fallback_reads_the_lowest_frame_id_member() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        let members: Vec<(i64, i64)> = conn
            .prepare(
                "SELECT csf.frame_id, f.file_id FROM calibration_set_frames csf
                 JOIN frames f ON f.id = csf.frame_id WHERE csf.set_id = ?1 ORDER BY csf.frame_id",
            )
            .unwrap()
            .query_map([set_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(members.len(), 2);
        for ((_, file_id), pat) in members.iter().zip(["RGGB", "BGGR"]) {
            conn.execute(
                "INSERT INTO fits_header (file_id, header) VALUES (?1, ?2)",
                rusqlite::params![
                    file_id,
                    format!("{:<80}\n{:<80}", format!("BAYERPAT= '{pat}    '"), "END")
                ],
            )
            .unwrap();
        }
        let bayer = load_bayer_consensus(&conn, set_id).unwrap();
        assert!(bayer.bayerpat_from_blob, "columns are NULL — this is the fallback path");
        assert_eq!(bayer.bayerpat.as_deref(), Some("RGGB"), "the lowest frame id decides, not row order");
    }

    #[test]
    fn bayerpat_from_xisf_xml_header() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        // XISF shape: raw XML blob with FITSKeyword elements (adapted from
        // stored_header.rs's parses_xisf_xml_fitskeyword_elements fixture).
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<xisf>
  <FITSKeyword name="BAYERPAT" value="'RGGB'" comment="Bayer color pattern" />
  <FITSKeyword name="EXPTIME" value="300.0" />
  <FITSKeyword name="XBINNING" value="1" />
</xisf>"#;
        seed_stored_headers(&conn, set_id, "XISF", xml);
        assert_bayer_consolidated(&conn, set_id);
    }
}
