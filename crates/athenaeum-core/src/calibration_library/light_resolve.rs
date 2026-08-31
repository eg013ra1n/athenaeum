//! Per-frame master resolution for light calibration (moved out of the
//! `api` layer in Task 5 of the 2026-08-31 calibrated-export-v2 plan so the
//! export-side generator can reuse it without an export→api dependency).
//! Pure, connection-level, no pixel I/O — resolves ONE light frame's
//! calibration links (Dark/Flat/Bias) against the current catalog into
//! everything the light-calibration engine and the header builder need. There
//! is no tracking row: a calibrated artifact is a product of an export or a
//! transfer, never a catalogued file.
//!
//! Its consumer is `export::calibrated_generator::resolve_generation`. Errors
//! are `anyhow::Error` (never `api::ApiError` — this module must not depend on
//! `crate::api`, the whole point of the move), which is what the generator
//! propagates and what an export turns into one frame's skip reason.

use std::path::PathBuf;

use rusqlite::{params, Connection, OptionalExtension};

use crate::db::calibration_links::get_links_for_frame;
use crate::fits_parser::stored_header::parse_stored_header_keys;
use crate::fits_writer::keywords::Bayer;
use crate::fits_writer::{Card, CardValue};
use crate::integration::cfa::CfaGeometry;
use crate::models::{CalibrationLink, FileFormat};

// ── Per-frame resolution (pure, connection-level, unit-tested) ───────────────

/// One resolved master link for a light frame: the master calibration set, the
/// on-disk file, and the frame uuid stamped into the `ATH_C*` provenance card.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedMaster {
    /// The MASTER calibration_set id (post-supersede the link points here); goes
    /// into the tracking row's `dark_set_id`/`flat_set_id`/`bias_set_id`.
    pub set_id: i64,
    /// Master frame uuid → `ATH_CDRK`/`ATH_CFLT`/`ATH_CBIA` value.
    pub uuid: String,
    /// Master file path (the engine subtrahend/divisor input).
    pub path: String,
}

/// Everything the engine + header builder + tracking row need for ONE light
/// frame, resolved against the current catalog in a single pooled connection.
/// No pixel I/O — so it is unit-testable against a seeded in-memory conn.
pub struct ResolvedFrameInputs {
    pub frame_id: i64,
    pub light_path: PathBuf,
    pub source_filename: String,
    pub source_uuid: Option<String>,
    /// OBJECT / INSTRUME / DATE-OBS date → the `<object>/<cam>/<date>/` output
    /// folder (spec §3). Empty strings fall back to `Unknown*` in the path.
    pub object: String,
    pub instrume: String,
    pub date_obs_date: String,
    /// Still-valid header cards copied from the source (WCS/optics/session);
    /// [`crate::calibration_library::light_headers::build_light_cal_cards`]
    /// filters these to its whitelist.
    pub source_cards: Vec<Card>,
    /// The light's own mosaic phase when it declares a CFA pattern the parser
    /// can vouch for — what makes per-channel flat scaling applicable.
    pub cfa_geometry: Option<CfaGeometry>,
    pub dark: Option<ResolvedMaster>,
    pub flat: Option<ResolvedMaster>,
    pub bias: Option<ResolvedMaster>,
}

/// Current calibration link of `cal_type` for a frame, if any.
pub fn link_set_id(links: &[CalibrationLink], cal_type: &str) -> Option<i64> {
    links
        .iter()
        .find(|l| l.calibration_type == cal_type)
        .map(|l| l.calibration_set_id)
}

/// Resolve a calibration set to its single master member file — `Some` only
/// when the set is a built master (`is_master_library = 1`). A raw, unbuilt set
/// (or one with no member file) yields `None`, which the caller treats as
/// "skip this term" per the best-effort policy.
pub fn resolve_master(conn: &Connection, set_id: i64) -> anyhow::Result<Option<ResolvedMaster>> {
    let row: Option<(Option<String>, String)> = conn
        .query_row(
            "SELECT fr.uuid, fi.path
             FROM calibration_set cs
             JOIN calibration_set_frames csf ON csf.set_id = cs.id
             JOIN frames fr ON fr.id = csf.frame_id
             JOIN files fi ON fi.id = fr.file_id
             WHERE cs.id = ?1 AND cs.is_master_library = 1
             LIMIT 1",
            params![set_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.map(|(uuid, path)| ResolvedMaster {
        set_id,
        uuid: uuid.unwrap_or_default(),
        path,
    }))
}

/// Resolve one calibration type's link to a built master, warning (best-effort
/// policy, spec §6) when the link points at a raw set the preflight did not or
/// could not build.
fn resolve_type(
    conn: &Connection,
    links: &[CalibrationLink],
    frame_id: i64,
    cal_type: &str,
) -> anyhow::Result<Option<ResolvedMaster>> {
    match link_set_id(links, cal_type) {
        Some(set_id) => {
            let resolved = resolve_master(conn, set_id)?;
            if resolved.is_none() {
                tracing::warn!(
                    frame_id,
                    set_id,
                    cal_type,
                    "linked calibration set is not a built master — skipping this term (best-effort)"
                );
            }
            Ok(resolved)
        }
        None => Ok(None),
    }
}

/// Typed FITS card from a `KEYWORD -> value-string` pair. Numeric strings are
/// preserved as `Integer`/`Real` so copied-through WCS/optics cards keep their
/// type; everything else becomes a string. A keyword `fits_writer` rejects
/// (>8 chars, reserved) is dropped rather than erroring the whole frame.
fn card_from_kv(keyword: &str, value: &str) -> Option<Card> {
    let cv = if let Ok(i) = value.parse::<i64>() {
        CardValue::Integer(i)
    } else if let Ok(f) = value.parse::<f64>() {
        CardValue::Real(f)
    } else {
        CardValue::Str(value.to_string())
    };
    Card::new(keyword, cv).ok()
}

/// Rebuild the source frame's header cards from the scanner-stored
/// `fits_header` blob (format-aware, so an XISF source works too). Pure DB — no
/// disk re-read of the (possibly huge) light file. Missing blob → a warn (an
/// output stripped of its source metadata is a real, visible loss, never a
/// silent one) and the catalog-derived Bayer cards only.
///
/// The Bayer fallback runs on BOTH paths deliberately: sync-ingest inserts an
/// EMPTY `fits_header` row while three scanner branches insert no row at all —
/// the same information state (no header keywords, populated `frames` columns)
/// reached two ways. Skipping the fallback on the row-less path would give
/// those two identical states opposite CFA outcomes.
pub fn source_cards_for_file(
    conn: &Connection,
    frame_id: i64,
    file_id: i64,
    format: FileFormat,
) -> anyhow::Result<Vec<Card>> {
    let header_text: Option<String> = conn
        .query_row(
            "SELECT header FROM fits_header WHERE file_id = ?1 LIMIT 1",
            params![file_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(header_text) = header_text else {
        tracing::warn!(
            frame_id,
            file_id,
            "no stored header for light — calibrated output will carry only catalog-derived cards"
        );
        let mut cards = Vec::new();
        append_bayer_cards_from_columns(conn, frame_id, file_id, &mut cards)?;
        return Ok(cards);
    };
    let keys = parse_stored_header_keys(format, &header_text);
    let mut cards: Vec<Card> = keys
        .iter()
        .filter_map(|(k, v)| card_from_kv(k, v))
        .collect();
    append_bayer_cards_from_columns(conn, frame_id, file_id, &mut cards)?;
    Ok(cards)
}

/// Bayer/CFA cards the stored blob can legitimately lack while the catalog
/// columns hold the truth: an XISF that declares its CFA only through the
/// `<ColorFilterArray>` element populates `frames.bayerpat` but has no
/// BAYERPAT line anywhere in its raw XML blob — and the blob is what
/// copy-through reads. Without this fallback the calibrated output of such a
/// source would ship with no CFA geometry at all.
///
/// Rules, in order:
/// - a card parsed from the blob wins — but only if it actually carries a
///   value. A BLANK card does not: the XISF stored-header parser has no
///   empty-value check, so `<FITSKeyword name="BAYERPAT" value=""/>` lands as
///   `Str("")`, which says nothing and must not out-rank a real column value.
///   Such a card is REPLACED in place (never left beside the derived one — two
///   cards for one keyword would contradict each other in the output header);
/// - a NULL/blank column adds nothing — absent beats fabricated, exactly as in
///   `headers.rs::load_bayer_consensus`;
/// - the blob itself is never touched. `fits_header` stays the raw
///   scan-time record; only the OUTPUT header gains the derived card.
///
/// The columns are read directly rather than through a `Frame`: the index-based
/// list readers hardcode `None` for xbayroff/ybayroff/roworder regardless of
/// what is stored (see `models.rs`), so a `Frame` round-trip would erase them.
fn append_bayer_cards_from_columns(
    conn: &Connection,
    frame_id: i64,
    file_id: i64,
    cards: &mut Vec<Card>,
) -> anyhow::Result<()> {
    #[allow(clippy::type_complexity)]
    let row: Option<(Option<String>, Option<i64>, Option<i64>, Option<String>)> = conn
        .query_row(
            "SELECT bayerpat, xbayroff, ybayroff, roworder FROM frames WHERE id = ?1",
            params![frame_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((bayerpat, xbayroff, ybayroff, roworder)) = row else {
        tracing::debug!(
            frame_id,
            file_id,
            "no frames row for light — no catalog-derived bayer cards"
        );
        return Ok(());
    };

    let text = |v: Option<String>| -> Option<String> {
        v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    };
    let candidates: Vec<(&str, CardValue)> = [
        text(bayerpat).map(|v| ("BAYERPAT", CardValue::Str(v))),
        xbayroff.map(|v| ("XBAYROFF", CardValue::Integer(v))),
        ybayroff.map(|v| ("YBAYROFF", CardValue::Integer(v))),
        text(roworder).map(|v| ("ROWORDER", CardValue::Str(v))),
    ]
    .into_iter()
    .flatten()
    .collect();

    // A card carrying no usable value: either valueless, or a `Str` that is
    // empty/whitespace (the XISF `value=""` case). Such a card conveys nothing,
    // so it must not out-rank a real catalog column.
    let is_blank = |c: &Card| match &c.value {
        Some(CardValue::Str(s)) => s.trim().is_empty(),
        Some(_) => false,
        None => true, // valueless card (COMMENT/HISTORY shape) — no CFA info
    };

    for (keyword, value) in candidates {
        let existing = cards.iter().position(|c| c.keyword == keyword);
        let blank_at = match existing {
            Some(i) if !is_blank(&cards[i]) => continue, // the file's own card wins
            Some(i) => Some(i),
            None => None,
        };
        // These four keywords are all writer-legal by construction, so a
        // rejection here means something changed underneath us — log it rather
        // than dropping the CFA geometry silently.
        let card = match Card::new(keyword, value) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    frame_id,
                    file_id,
                    field = keyword,
                    error = %e,
                    "derived bayer card rejected by the writer — omitted"
                );
                continue;
            }
        };
        match blank_at {
            Some(i) => {
                tracing::debug!(
                    frame_id,
                    file_id,
                    field = keyword,
                    "bayer card derived from catalog column (stored header value blank)"
                );
                cards[i] = card;
            }
            None => {
                tracing::debug!(
                    frame_id,
                    file_id,
                    field = keyword,
                    "bayer card derived from catalog column (absent from stored header)"
                );
                cards.push(card);
            }
        }
    }
    Ok(())
}

/// The LIGHT frame's mosaic phase for per-channel flat scaling, or `None` when
/// it declares no CFA pattern (mono) or one [`Bayer::parse`] cannot vouch for.
///
/// Read straight from the `frames` columns, NOT through a `Frame`: the
/// index-based list readers hardcode `None` for `xbayroff`/`ybayroff` whatever
/// is stored, so a `Frame` round-trip would silently erase the phase and put
/// every offset light one row/column out — swapping R and B.
///
/// A missing offset defaults to 0 with a `debug!`. That guess is allowed here
/// for the same reason `measure_flat_channel_norms` allows it and
/// `build_master_cards` does not: it only decides which pixels are grouped for
/// a divisor — a wrong guess costs a colour cast the operator can see — whereas
/// writing the guess into `XBAYROFF` would be a fabricated claim every future
/// debayer acts on.
///
/// `ROWORDER` is deliberately not folded in: `BAYERPAT` describes the mosaic in
/// FILE row order (see [`CfaGeometry`]'s own rustdoc, which ratifies this).
fn resolve_cfa_geometry(conn: &Connection, frame_id: i64) -> anyhow::Result<Option<CfaGeometry>> {
    let row: Option<(Option<String>, Option<i64>, Option<i64>)> = conn
        .query_row(
            "SELECT bayerpat, xbayroff, ybayroff FROM frames WHERE id = ?1",
            params![frame_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((bayerpat, xbayroff, ybayroff)) = row else {
        return Ok(None);
    };
    let Some(raw) = bayerpat.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None); // mono — the common case, not worth a log line
    };
    let Some(pattern) = Bayer::parse(raw) else {
        tracing::warn!(
            frame_id,
            bayerpat = %raw,
            "unrecognized bayer pattern — per-channel flat scaling not applied to this light"
        );
        return Ok(None);
    };
    let assumed = match (xbayroff, ybayroff) {
        (Some(_), Some(_)) => None,
        (None, Some(_)) => Some("xbayroff"),
        (Some(_), None) => Some("ybayroff"),
        (None, None) => Some("xbayroff,ybayroff"),
    };
    if let Some(field) = assumed {
        tracing::debug!(
            frame_id,
            field,
            "cfa phase not declared — per-channel flat scaling assumes 0"
        );
    }
    Ok(Some(CfaGeometry {
        pattern,
        xoff: xbayroff.unwrap_or(0),
        yoff: ybayroff.unwrap_or(0),
    }))
}

/// YYYY-MM-DD from a DATE-OBS string (`2026-07-05T20:30:00Z` → `2026-07-05`).
/// The result becomes a path segment, so it goes through the shared
/// sanitizer — a malformed non-ISO DATE-OBS must not nest directories ('/')
/// or hit Windows-illegal chars (':'). Missing/empty/unsalvageable →
/// `"UnknownDate"` so the layout never gets an empty segment.
fn date_part(date_obs: Option<&str>) -> String {
    let raw: String = date_obs
        .and_then(|d| d.split('T').next())
        .map(|d| d.chars().take(10).collect())
        .unwrap_or_default();
    let sanitized = crate::archive::path_layout::sanitize_for_filename(&raw);
    if sanitized.is_empty() {
        "UnknownDate".to_string()
    } else {
        sanitized
    }
}

pub fn resolve_frame_inputs(
    conn: &Connection,
    frame_id: i64,
    _flat_norm: bool,
) -> anyhow::Result<ResolvedFrameInputs> {
    #[allow(clippy::type_complexity)]
    let row: Option<(
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        String,
        String,
        String,
    )> = conn
        .query_row(
            "SELECT fr.uuid, fr.object, fr.instrume, fr.date_obs, fi.id, fi.path, fi.filename, fi.format
             FROM frames fr JOIN files fi ON fi.id = fr.file_id
             WHERE fr.id = ?1",
            params![frame_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .optional()?;
    let Some((uuid, object, instrume, date_obs, file_id, path, filename, format_str)) = row else {
        anyhow::bail!("light frame {frame_id} not found");
    };

    let format = if format_str.eq_ignore_ascii_case("XISF") {
        FileFormat::XISF
    } else {
        FileFormat::FITS
    };
    let source_cards = source_cards_for_file(conn, frame_id, file_id, format)?;

    let links = get_links_for_frame(conn, frame_id)?;
    let dark = resolve_type(conn, &links, frame_id, "Dark")?;
    let flat = resolve_type(conn, &links, frame_id, "Flat")?;
    let bias = resolve_type(conn, &links, frame_id, "Bias")?;

    Ok(ResolvedFrameInputs {
        frame_id,
        light_path: PathBuf::from(path),
        source_filename: filename,
        source_uuid: uuid,
        object: object.unwrap_or_default(),
        instrume: instrume.unwrap_or_default(),
        date_obs_date: date_part(date_obs.as_deref()),
        source_cards,
        cfa_geometry: resolve_cfa_geometry(conn, frame_id)?,
        dark,
        flat,
        bias,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Moved with [`date_part`] from `api/lights.rs` (Task 5): self-contained,
    /// no DB fixture, so it needed no rewiring to follow the function.
    #[test]
    fn date_part_sanitizes_non_iso_values() {
        assert_eq!(date_part(Some("2026-07-05T20:30:00Z")), "2026-07-05");
        // Malformed locale date: '/' must not become directory nesting, ':' is
        // Windows-illegal — both map to '_' (audit F6).
        assert_eq!(date_part(Some("05/07/2026")), "05_07_2026");
        assert_eq!(date_part(None), "UnknownDate");
        assert_eq!(date_part(Some("")), "UnknownDate");
    }
}
