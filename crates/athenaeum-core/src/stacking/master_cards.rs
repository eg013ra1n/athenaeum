//! Master-light header (spec §6.4), file naming (spec §9.5) and the writers
//! for the master and its two optional rejection maps. Consumes a group's
//! `GroupOutput` (Plan 4's `integrate::integrate_group`) plus the reference
//! frame's own copy-through cards (`register::writer::source_cards_from_file`)
//! and, when the reference has one, its plate solve — the WCS card writer
//! (`fits_writer::wcs::wcs_cards`) reproduces that solve unchanged since the
//! master shares the reference's pixel grid.

use std::path::{Path, PathBuf};

use anyhow::Context;
use tracing::info;

use crate::archive::path_layout::sanitize_for_filename;
use crate::calibration_library::paths::{fmt_num, resolve_collision};
use crate::fits_writer::keywords::{FrameKind, HeaderBuilder};
use crate::fits_writer::wcs::wcs_cards;
use crate::fits_writer::{write_fits_f32, Card, CardValue, FitsWriteError};
use crate::plate_solve::storage::PlateSolveRecord;
use crate::stacking::config::OutputFormat;
use crate::stacking::crop::{crop_file, CropRect};
use crate::stacking::drizzle::{DrizzleKernel, DrizzleOutput};
use crate::stacking::groups::ColorMode;
use crate::stacking::integrate::GroupOutput;
use crate::stacking::register::writer::REGISTERED_COPY_THROUGH;
use crate::stacking::register::RegistrationGeometry;

/// `ATH_STKV`: the master-light header format version.
pub const ATH_STK_VERSION: i64 = 1;

/// Drizzle provenance cards (spec §7, `build_drizzle_cards`).
pub const ATH_DRZ: &str = "ATH_DRZ"; // integer scale
pub const ATH_DRZP: &str = "ATH_DRZP"; // drop shrink (real)
pub const ATH_DRZK: &str = "ATH_DRZK"; // kernel serde name

/// `ATH_RGEO` (M4b ruling R-M4b-8): which geometry mode delivered this
/// master — the [`RegistrationGeometry`] serde name, `coRegistered` or
/// `native`. Stamped on every master light AND every drizzled master (and
/// copied onto a drizzle weight map), because the two modes answer
/// different questions about what the pixel grid MEANS: a co-registered
/// master shares the whole run's grid, a native one carries its own
/// group's.
pub const ATH_RGEO: &str = "ATH_RGEO";

/// `ATH_SPRJ`: the collaboration project a master was stacked for (spec
/// §5.9). Never `ATH_PRJ`, which the scanner reads as "a landed replica".
pub const ATH_SPRJ: &str = "ATH_SPRJ";
/// `ATH_GRID`: the canonical grid's hash, 16 hex (spec §6.1).
pub const ATH_GRID: &str = "ATH_GRID";
/// `ATH_RGEO`'s value for a master on a project's canonical grid (spec §5.9).
pub const RGEO_GRID: &str = "grid";

/// What a master on a project's canonical grid states (spec §5.9, plan
/// 6b-3 D3, D11).
pub struct GridStamp<'a> {
    pub project_id: &'a str,
    pub grid_hash: &'a str,
    /// The group's canonical filter; `None` writes no `FILTER` card.
    pub filter: Option<&'a str>,
}

/// A string card that keeps `comment` only when the whole card fits one
/// 80-column record; the value is never shortened (a long one continues).
pub fn card_with_optional_comment(
    keyword: &str,
    value: &str,
    comment: &str,
) -> Result<Card, FitsWriteError> {
    let with = Card::new(keyword, CardValue::Str(value.to_string()))?.with_comment(comment);
    match crate::fits_writer::card::format_card(&with) {
        // A value that continues over several records keeps no comment.
        Ok(records) if records.len() == 1 => Ok(with),
        Ok(_) | Err(FitsWriteError::CommentTooLong(_)) => {
            Card::new(keyword, CardValue::Str(value.to_string()))
        }
        Err(e) => Err(e),
    }
}

/// Re-states a grid master's (or drizzled master's) cards (spec §5.9, plan
/// 6b-3 D3, D11): the canonical `FILTER` (the reference's copy-through card
/// carries a publisher's raw name), `ROWORDER = 'BOTTOM-UP'` (the grid's own;
/// never the reference's), `ATH_RGEO = 'grid'`, `ATH_SPRJ`, `ATH_GRID`, and
/// no `PLTSOLVD` (a grid TAN is not a plate solve). Every earlier card of
/// those keys is dropped first, and `ATH_PRJ` defensively too, so each
/// appears once. Variable-length values carry no comment unless it fits.
/// An empty canonical filter writes no `FILTER` at all (review M4): the
/// reference's card is a publisher's raw name and must not leak; the run
/// warns once for such a group.
pub fn stamp_grid_cards(
    cards: &mut Vec<Card>,
    stamp: &GridStamp<'_>,
) -> Result<(), FitsWriteError> {
    cards.retain(|c| {
        !matches!(
            c.keyword.as_str(),
            "FILTER" | "ROWORDER" | "PLTSOLVD" | "ATH_PRJ" | ATH_SPRJ | ATH_GRID | ATH_RGEO
        )
    });
    if let Some(filter) = stamp.filter.map(str::trim).filter(|f| !f.is_empty()) {
        cards.push(Card::new("FILTER", CardValue::Str(filter.to_string()))?);
    }
    cards.push(
        Card::new("ROWORDER", CardValue::Str("BOTTOM-UP".into()))?
            .with_comment("row order of the canonical grid"),
    );
    cards.push(card_with_optional_comment(
        ATH_SPRJ,
        stamp.project_id,
        "collaboration project",
    )?);
    cards.push(
        Card::new(ATH_GRID, CardValue::Str(stamp.grid_hash.to_string()))?
            .with_comment("canonical grid hash"),
    );
    cards.push(
        Card::new(ATH_RGEO, CardValue::Str(RGEO_GRID.into()))?
            .with_comment("registration geometry mode"),
    );
    Ok(())
}

pub struct MasterCardInputs<'a> {
    /// Copy-through cards of the REFERENCE frame (`source_cards_from_file`).
    pub reference_cards: &'a [Card],
    /// The reference's plate solve, when it has one.
    pub wcs: Option<&'a PlateSolveRecord>,
    pub frames: usize,
    pub weighted_exposure_s: f64,
    /// Earliest DATE-OBS among the included frames; when None the
    /// reference's own DATE-OBS card is kept.
    pub date_obs_first: Option<&'a str>,
    /// Latest DATE-OBS among the included frames (ISO text as stored);
    /// `DATE-END` is written only when this is Some.
    pub date_obs_last: Option<&'a str>,
    pub recipe: &'a str,        // IntegrationRecipe::describe()
    pub weight_mode: &'a str,   // the WeightMode serde name
    pub normalization: &'a str, // "<output>/<rejection>" serde names, e.g. "additiveWithScaling/scaleZeroOffset"
    // ATH_STKF — intentionally the group's NORMALIZATION ANCHOR's identity
    // (ruling R-M3-17 v2, spec §4.4/§6.4), not the run's registration
    // reference: it has always named whichever frame the group's own
    // copy-through header/WCS was built from (the M1 Checkpoint B OSC
    // master already read `ATH_STKF = '…_0019'`, that group's own
    // best-weighted member, never the mono set's registration reference).
    // The registration reference is a RUN-level fact, not a per-master
    // header card — it lives in `stacking_runs.reference_frame_id` /
    // `RunSummary.reference`, unaffected by which frame any one group
    // normalizes to.
    pub reference_id: &'a str,
    pub group_key: &'a str, // ATH_STKG
    /// Every distinct camera in the group (`IntegrationGroup.cameras`,
    /// already trimmed/sorted/deduped) — `ATH_STKC` (owner decision
    /// 2026-09-10: groups are camera-agnostic, so a master's own header
    /// must say which camera(s) actually contributed).
    pub cameras: &'a [String],
    pub run_id: &'a str, // ATH_STKI
    pub app_version: &'a str,
    /// The run's geometry mode — `ATH_RGEO` (M4b ruling R-M4b-8).
    pub geometry: RegistrationGeometry,
}

/// `ATH_STKC`'s FITS string-value budget: `MAX_STR_CONTENT`
/// (`fits_writer::card`) — the printable characters inside the quotes of
/// ONE 80-byte record with no comment, the same convention `ATH_STKG`/
/// `ATH_STKR`/`ATH_STKO`/`ATH_STKF` already rely on below (no
/// `.with_comment()` — a comment would need to fit in the same record as a
/// caller string that can already be this long). `format_card` would
/// happily chain a longer string across `CONTINUE` cards, but a camera list
/// is auxiliary metadata, not worth that — truncated with `...` (three
/// ASCII dots, fix round 1: `…` is non-ASCII — `fits_writer::card::
/// sanitize_text` silently rewrites a non-ASCII byte to `?` and logs
/// `warn!("non-ASCII characters in header value sanitized")` on every long
/// camera list, which would have fired on every truncated `ATH_STKC`).
/// M10 (final fix wave): a direct alias of
/// [`crate::fits_writer::card::MAX_STR_CONTENT`] (`pub(crate)` precisely so
/// this can reference it) — this used to be its own hard-coded `68` with a
/// comment merely NAMING that constant, so the two could silently drift if
/// it ever changed.
const ATH_STKC_MAX_CHARS: usize = crate::fits_writer::card::MAX_STR_CONTENT;
const ATH_STKC_MARKER: &str = "...";

/// Sorted, comma-joined camera list for `ATH_STKC`, truncated to
/// [`ATH_STKC_MAX_CHARS`] TOTAL (including the marker itself) with a
/// trailing [`ATH_STKC_MARKER`] when it would otherwise overflow one FITS
/// card. Re-sorts/dedupes defensively rather than trusting the caller's own
/// `IntegrationGroup.cameras` invariant.
fn format_cameras_card(cameras: &[String]) -> String {
    let mut sorted: Vec<&str> = cameras.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let joined = sorted.join(",");
    if joined.chars().count() <= ATH_STKC_MAX_CHARS {
        joined
    } else {
        let keep = ATH_STKC_MAX_CHARS.saturating_sub(ATH_STKC_MARKER.len());
        let truncated: String = joined.chars().take(keep).collect();
        format!("{truncated}{ATH_STKC_MARKER}")
    }
}

/// Build a master-light header: `IMAGETYP`/`SWCREATE`, the reference's
/// copy-through cards (minus `EXPTIME`/`DATE-OBS`, replaced below), the
/// group-level `NCOMBINE`/`EXPTIME`/`DATE-OBS`/`DATE-END`, the reference's
/// WCS (when it has one, unchanged — the master is in reference geometry),
/// and the `ATH_STK*` provenance cards.
pub fn build_master_light_cards(
    inputs: &MasterCardInputs<'_>,
) -> Result<Vec<Card>, FitsWriteError> {
    let mut cards = HeaderBuilder::new(FrameKind::MasterLight)
        .swcreate(inputs.app_version)
        .build()?;

    cards.extend(
        inputs
            .reference_cards
            .iter()
            .filter(|c| REGISTERED_COPY_THROUGH.contains(&c.keyword.as_str()))
            // EXPTIME is always replaced below (the weighted total is always
            // known); DATE-OBS is replaced only when the group has one of
            // its own — otherwise the reference's own card is kept.
            .filter(|c| c.keyword != "EXPTIME")
            .filter(|c| inputs.date_obs_first.is_none() || c.keyword != "DATE-OBS")
            .cloned(),
    );

    cards.push(
        Card::new("NCOMBINE", CardValue::Integer(inputs.frames as i64))?
            .with_comment("frames combined"),
    );
    cards.push(
        Card::new("EXPTIME", CardValue::Real(inputs.weighted_exposure_s))?
            .with_comment("weighted total exposure, s"),
    );
    if let Some(first) = inputs.date_obs_first {
        cards.push(Card::new("DATE-OBS", CardValue::Str(first.to_string()))?);
    }
    if let Some(last) = inputs.date_obs_last {
        cards.push(Card::new("DATE-END", CardValue::Str(last.to_string()))?);
    }

    if let Some(solve) = inputs.wcs {
        cards.extend(wcs_cards(solve)?);
    }

    cards.push(
        Card::new("ATH_STK", CardValue::Logical(true))?
            .with_comment("stacked by Athenaeum; never cataloged"),
    );
    cards.push(
        Card::new("ATH_STKV", CardValue::Integer(ATH_STK_VERSION))?
            .with_comment("stacking header version"),
    );
    cards.push(
        Card::new("ATH_STKN", CardValue::Integer(inputs.frames as i64))?
            .with_comment("frames combined"),
    );
    cards.push(
        Card::new("ATH_STKW", CardValue::Str(inputs.weight_mode.to_string()))?
            .with_comment("weight mode"),
    );
    // Variable-length values: no card comment — value + comment must fit one
    // 80-byte record and format_card errors otherwise (the same rule
    // register/writer.rs applies to ATH_REGT). ATH_STKG looks bounded
    // (`<mono|osc>__<filter>__bin<n>__<exposure cluster>`, owner decision
    // 2026-09-10 — camera/geometry are no longer part of it) but the
    // sanitized FILTER string inside it is caller text, not fixed width, so
    // it belongs here too. ATH_STKC (below) is truncated instead — see
    // `format_cameras_card`.
    cards.push(Card::new(
        "ATH_STKR",
        CardValue::Str(inputs.recipe.to_string()),
    )?);
    cards.push(Card::new(
        "ATH_STKO",
        CardValue::Str(inputs.normalization.to_string()),
    )?);
    cards.push(Card::new(
        "ATH_STKF",
        CardValue::Str(inputs.reference_id.to_string()),
    )?);
    cards.push(Card::new(
        "ATH_STKG",
        CardValue::Str(inputs.group_key.to_string()),
    )?);
    cards.push(Card::new(
        "ATH_STKC",
        CardValue::Str(format_cameras_card(inputs.cameras)),
    )?);
    cards.push(
        Card::new("ATH_STKI", CardValue::Str(inputs.run_id.to_string()))?
            .with_comment("stacking run"),
    );
    cards.push(
        Card::new(
            ATH_RGEO,
            CardValue::Str(registration_geometry_serde_name(inputs.geometry)?),
        )?
        .with_comment("registration geometry mode"),
    );

    Ok(cards)
}

/// The serde wire name of a [`RegistrationGeometry`] variant — the value
/// [`ATH_RGEO`] carries. Goes through `serde_json` for the same reason
/// [`drizzle_kernel_serde_name`] does: the card can then never drift from
/// the config's own wire representation, and a future non-unit variant
/// surfaces as a caller-visible error rather than a panic inside the
/// header writer.
fn registration_geometry_serde_name(
    geometry: RegistrationGeometry,
) -> Result<String, FitsWriteError> {
    match serde_json::to_value(geometry) {
        Ok(serde_json::Value::String(s)) => Ok(s),
        other => Err(FitsWriteError::Malformed(format!(
            "RegistrationGeometry did not serialize to a JSON string: {other:?}"
        ))),
    }
}

/// Whether `keyword` is one [`wcs_cards`] can emit — used by
/// [`build_drizzle_cards`] to strip a master's original (un-scaled) WCS
/// block before appending the scaled one, so nothing duplicates. Beyond the
/// fixed keywords (every card `wcs_cards` writes outside the SIP tables,
/// plus `CDELT*`/`CROTA*` in case a copy-through header ever carries the
/// legacy scale/rotation pair alongside the CD matrix), matches the SIP
/// coefficient pattern `(A|B|AP|BP)_<i>_<j>` — `wcs_cards` writes one card
/// per non-zero term, so no fixed list covers every possible table.
pub fn is_wcs_keyword(keyword: &str) -> bool {
    const FIXED: &[&str] = &[
        "WCSAXES", "CTYPE1", "CTYPE2", "CRVAL1", "CRVAL2", "CRPIX1", "CRPIX2", "CD1_1", "CD1_2",
        "CD2_1", "CD2_2", "CDELT1", "CDELT2", "CROTA1", "CROTA2", "CUNIT1", "CUNIT2", "RADESYS",
        "EQUINOX", "PLTSOLVD", "A_ORDER", "B_ORDER", "AP_ORDER", "BP_ORDER",
    ];
    FIXED.contains(&keyword) || is_sip_term_keyword(keyword)
}

/// `^(A|B|AP|BP)_\d+_\d+$` — one SIP coefficient card, any of the four
/// tables. `AP_`/`BP_` are checked before `A_`/`B_` only for readability;
/// `strip_prefix` already requires the full literal prefix, so the order
/// can't cause a false match either way (`"AP_2_0"` never starts with
/// `"A_"`).
fn is_sip_term_keyword(keyword: &str) -> bool {
    for prefix in ["AP_", "BP_", "A_", "B_"] {
        let Some(rest) = keyword.strip_prefix(prefix) else {
            continue;
        };
        let mut parts = rest.split('_');
        let (Some(i), Some(j), None) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let is_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        if is_digits(i) && is_digits(j) {
            return true;
        }
    }
    false
}

/// The serde wire name of a [`DrizzleKernel`] variant — the value
/// [`build_drizzle_cards`] stamps into `ATH_DRZK`. Goes through
/// `serde_json` rather than a hand-written match so the card can never
/// drift from the config's own wire representation.
///
/// Fix round 1 (review M6): returns `Result` rather than panicking — sound
/// today (`DrizzleKernel` is a unit-only enum, so serde always yields a
/// string), but a future struct-variant addition would otherwise turn into
/// a runtime panic inside the header writer instead of a caller-visible
/// error.
fn drizzle_kernel_serde_name(kernel: DrizzleKernel) -> Result<String, FitsWriteError> {
    match serde_json::to_value(kernel) {
        Ok(serde_json::Value::String(s)) => Ok(s),
        other => Err(FitsWriteError::Malformed(format!(
            "DrizzleKernel did not serialize to a JSON string: {other:?}"
        ))),
    }
}

/// The master's cards with its original (un-scaled) WCS block — every card
/// [`is_wcs_keyword`] recognizes — stripped, `wcs_cards(scaled)` appended
/// when `scaled` is `Some` (nothing appended, i.e. no WCS at all, when
/// `None`), then the `ATH_DRZ`/`ATH_DRZP`/`ATH_DRZK` provenance cards and
/// M4b's [`ATH_RGEO`]. `NAXIS*` are the writer's business, never cards here.
///
/// `geometry` is stamped from the ARGUMENT, and the master's own
/// [`ATH_RGEO`] card is stripped alongside the WCS block first: the master
/// header already carries one, and a drizzled header must end up with
/// exactly one card, not two (ruling R-M4b-8).
pub fn build_drizzle_cards(
    master_cards: &[Card],
    scaled: Option<&PlateSolveRecord>,
    scale: u32,
    drop_shrink: f64,
    kernel: DrizzleKernel,
    geometry: RegistrationGeometry,
) -> Result<Vec<Card>, FitsWriteError> {
    let mut cards: Vec<Card> = master_cards
        .iter()
        .filter(|c| !is_wcs_keyword(&c.keyword) && c.keyword != ATH_RGEO)
        .cloned()
        .collect();

    if let Some(solve) = scaled {
        cards.extend(wcs_cards(solve)?);
    }

    cards.push(
        Card::new(ATH_DRZ, CardValue::Integer(scale as i64))?.with_comment("drizzle output scale"),
    );
    cards.push(
        Card::new(ATH_DRZP, CardValue::Real(drop_shrink))?.with_comment("drizzle drop shrink"),
    );
    cards.push(
        Card::new(ATH_DRZK, CardValue::Str(drizzle_kernel_serde_name(kernel)?))?
            .with_comment("drizzle kernel"),
    );
    cards.push(
        Card::new(
            ATH_RGEO,
            CardValue::Str(registration_geometry_serde_name(geometry)?),
        )?
        .with_comment("registration geometry mode"),
    );

    Ok(cards)
}

/// Trim, then treat a blank result as absent — a `Some("  ")` filter must
/// fall back to the default just like `None` does.
fn blank(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// §9.5 (owner decision 2026-09-10 — groups are camera-agnostic, so the
/// camera token is gone; fix round 1, ruling — the colour-mode token stays
/// ALWAYS in the name: with camera gone, a mono and an OSC group of the
/// same filter and exposure would otherwise differ only by a `_2` collision
/// suffix): `<set slug>_<filter>_<mono|osc>_<exp>s_<n>x.fits`, or
/// `<set slug>_<filter>_<mono|osc>_unknown_<n>x.fits` for the group of
/// frames with no `EXPTIME`; every part sanitized. `exp` reuses [`fmt_num`]
/// — the SAME sub-second-exposure formatter the calibration library's own
/// naming and `plan.rs`'s calibration-set label already use (`180s`,
/// `0.39s`), so a calibration-set label, a group key and a master filename
/// never format an exposure three different ways. There is no "equal vs
/// mixed exposure" branch any more: the grouping rule itself already
/// clustered every frame in `n` to within the group's own exposure
/// tolerance, so one exposure value (the cluster's label) is always the
/// honest answer.
/// M11 (final fix wave, ruling): `binning` gains a `bin<n>` token ONLY when
/// `>= 2` — bin-1 names are unchanged (existing masters, including the
/// acceptance run's, keep their names). Without this token, a bin-1 and a
/// bin-2 group of the same filter/colour-mode/exposure would differ only by
/// the opaque `_2` collision suffix — the same reasoning Task 10's ruling
/// used to keep the colour-mode token always present, just for the fourth
/// group-key axis instead of the second.
pub fn master_file_name(
    set_name: &str,
    filter: Option<&str>,
    color_mode: ColorMode,
    binning: i64,
    exposure_s: Option<f64>,
    n: usize,
) -> String {
    let mut slug = sanitize_for_filename(set_name);
    if slug.is_empty() {
        slug = "set".to_string();
    }
    let filter = sanitize_for_filename(blank(filter).unwrap_or("NoFilter"));
    let color_tok = match color_mode {
        ColorMode::Mono => "mono",
        ColorMode::Osc => "osc",
    };
    let exp = match exposure_s {
        Some(e) => format!("{}s", fmt_num(e)),
        None => "unknown".to_string(),
    };
    if binning >= 2 {
        format!("{slug}_{filter}_{color_tok}_bin{binning}_{exp}_{n}x.fits")
    } else {
        format!("{slug}_{filter}_{color_tok}_{exp}_{n}x.fits")
    }
}

/// `<master stem>_drizzle<s>x.fits` and `<master stem>_drizzle<s>x_weight.fits`
/// (spec §9.5) — `master_stem` is the ALREADY collision-resolved master file
/// stem (no extension); [`write_drizzled_master`] re-derives the weight
/// name from its own resolved drizzle stem rather than calling this twice,
/// so a `_2`-suffixed drizzle output still names its own weight map.
pub fn drizzle_file_names(master_stem: &str, scale: u32) -> (String, String) {
    (
        format!("{master_stem}_drizzle{scale}x.fits"),
        format!("{master_stem}_drizzle{scale}x_weight.fits"),
    )
}

/// The file extension the run's [`OutputFormat`] writes (M4d Task 2, ruling
/// R-M4d-3): the master, the drizzled master and the drizzle weight map all
/// take it; the rejection maps never do — they stay FITS diagnostics.
///
/// [`master_file_name`] and [`drizzle_file_names`] keep naming `.fits`: they
/// answer "what is this output called", and the writers below swap in the
/// chosen container's extension. That is also what lets a FITS master and an
/// XISF master of the SAME run stem coexist — `resolve_collision` only ever
/// probes the exact candidate path, so `x.fits` never occupies `x.xisf`'s
/// name and neither gets a `_2` suffix because of the other.
pub fn output_extension(format: OutputFormat) -> &'static str {
    format.extension()
}

/// Whether a master's own card list says its stored rows run bottom-up
/// (M4d Task 2 fix round 1, ruling R-T2-1): the `ROWORDER` card's value
/// through [`crate::orientation::row_order_is_bottom_up`], the one encoding
/// of the astronomical convention — a missing card included, which is the
/// common case for a set whose frames never carried one.
///
/// Why the run needs this: XISF has no row-order concept (row 0 is the top
/// row for every XISF reader) while this master's array is simply its
/// source frames' order, carried through every stage unflipped. So a
/// bottom-up set's XISF master displays mirrored relative to its FITS
/// master, and the run says so once rather than shipping a silent flip.
pub fn cards_row_order_is_bottom_up(cards: &[Card]) -> bool {
    let value = cards
        .iter()
        .find(|c| c.keyword == "ROWORDER")
        .and_then(|c| match c.value.as_ref() {
            Some(CardValue::Str(s)) => Some(s.as_str()),
            _ => None,
        });
    crate::orientation::row_order_is_bottom_up(value)
}

/// One image in the run's chosen container — the ONE place the two writers
/// below branch on format, so a master and its drizzled sibling can never
/// disagree about which container they were written in.
fn write_output(
    path: &Path,
    width: usize,
    height: usize,
    channels: usize,
    data: &[f32],
    cards: &[Card],
    format: OutputFormat,
) -> Result<(), FitsWriteError> {
    crate::fits_writer::write_image_f32(
        path,
        width,
        height,
        channels,
        data,
        cards,
        format,
        crate::fits_writer::XisfBounds::Unit,
    )
}

#[derive(Debug, Clone)]
pub struct WrittenMaster {
    pub master: PathBuf,
    pub rejection_low: Option<PathBuf>,
    pub rejection_high: Option<PathBuf>,
    /// The coverage map beside the master (plan 6b-3 D9); `None` unless the
    /// group's output carried one.
    pub coverage: Option<PathBuf>,
}

/// Header cards for a coverage map (spec §5.7, plan 6b-3 D9): `IMAGETYP =
/// 'Coverage Map'`, `ATH_STK`/`ATH_STKV` (the scanner's skip marker),
/// `BUNIT = 'frames'`, the master's own WCS block (the map IS the master's
/// grid, pixel for pixel), and the trace cards a reader needs without the
/// master: `ROWORDER`, `ATH_STKI`, `ATH_STKG`, `ATH_RGEO`, `ATH_GRID`,
/// `ATH_SPRJ`, each only when the master carries it (a frame-set master
/// may carry no `ROWORDER`: the orientation default, `orientation.rs`).
/// Never `FILTER` and never `ATH_PRJ`. The cards are copied as they stand,
/// so the master's own comment-length rule for caller text carries over.
pub fn coverage_map_cards(master_cards: &[Card]) -> Result<Vec<Card>, FitsWriteError> {
    let mut cards = vec![
        Card::new("IMAGETYP", CardValue::Str("Coverage Map".into()))?,
        Card::new("ATH_STK", CardValue::Logical(true))?
            .with_comment("stacked by Athenaeum; never cataloged"),
        Card::new("ATH_STKV", CardValue::Integer(ATH_STK_VERSION))?
            .with_comment("stacking header version"),
        Card::new("BUNIT", CardValue::Str("frames".into()))?
            .with_comment("frames with a sample here"),
    ];
    // `PLTSOLVD` stays behind: a count map is not a solved image (final
    // review B6); its WCS block still lets a viewer place it.
    for c in master_cards
        .iter()
        .filter(|c| is_wcs_keyword(&c.keyword) && c.keyword != "PLTSOLVD")
    {
        cards.push(c.clone());
    }
    for kw in [
        "ROWORDER", "ATH_STKI", "ATH_STKG", ATH_RGEO, ATH_GRID, ATH_SPRJ,
    ] {
        if let Some(c) = master_cards.iter().find(|c| c.keyword == kw) {
            cards.push(c.clone());
        }
    }
    Ok(cards)
}

/// Header cards for one rejection map: `IMAGETYP` `Rejection Map Low`/`High`
/// (a plain card, not `FrameKind` — these are Athenaeum-only artifacts, not
/// one of the frame kinds the catalog knows), `ATH_STK`/`ATH_STKV`, `BUNIT`,
/// and the master's own `ROWORDER`/`ATH_STKI`/`ATH_STKG` cards copied
/// through so a map can be traced back to its run and group — and read back
/// in the master's own row order — without opening the master too.
///
/// `ROWORDER` (M4d final review, Minor #3, matching the `weight_map_cards`
/// precedent from Task 2 fix round 2): a rejection map IS the master's
/// grid, row for row, so it must declare the same row order — otherwise an
/// XISF rejection map would claim the synthesized `'BOTTOM-UP'` default
/// beside a master that says `'TOP-DOWN'` (`fits_writer::xisf_writer`
/// states the effective order explicitly).
fn rejection_map_cards(label: &str, master_cards: &[Card]) -> Result<Vec<Card>, FitsWriteError> {
    let mut cards = vec![
        Card::new("IMAGETYP", CardValue::Str(format!("Rejection Map {label}")))?,
        Card::new("ATH_STK", CardValue::Logical(true))?
            .with_comment("stacked by Athenaeum; never cataloged"),
        Card::new("ATH_STKV", CardValue::Integer(ATH_STK_VERSION))?
            .with_comment("stacking header version"),
        Card::new("BUNIT", CardValue::Str("count".into()))?.with_comment("rejected-sample count"),
    ];
    for kw in ["ROWORDER", "ATH_STKI", "ATH_STKG"] {
        if let Some(c) = master_cards.iter().find(|c| c.keyword == kw) {
            cards.push(c.clone());
        }
    }
    Ok(cards)
}

/// Header cards for the drizzle weight map (`IMAGETYP = 'Drizzle Weight'`,
/// `ATH_STK`/`ATH_STKV`, `BUNIT = 'relative weight'`, and the drizzle
/// output's own `ATH_STKI`/`ATH_STKG`/`ATH_DRZ`/[`ATH_RGEO`] cards copied
/// through so the map can be traced back to its run/group/scale/geometry
/// mode without opening the drizzled master too) — the
/// [`rejection_map_cards`] pattern.
pub fn weight_map_cards(drizzle_cards: &[Card]) -> Result<Vec<Card>, FitsWriteError> {
    let mut cards = vec![
        Card::new("IMAGETYP", CardValue::Str("Drizzle Weight".into()))?,
        Card::new("ATH_STK", CardValue::Logical(true))?
            .with_comment("stacked by Athenaeum; never cataloged"),
        Card::new("ATH_STKV", CardValue::Integer(ATH_STK_VERSION))?
            .with_comment("stacking header version"),
        Card::new("BUNIT", CardValue::Str("relative weight".into()))?,
    ];
    // B11 (M3 final fix wave, M8): the weight map shares the drizzled
    // master's output grid exactly (same `output.data`/`output.weight`
    // dimensions, same pixel scale) — copy the SCALED WCS block
    // `build_drizzle_cards` already stamped onto `drizzle_cards` through
    // too, in the same order `wcs_cards` emitted it, not just the
    // `ATH_STKI`/`ATH_STKG`/`ATH_DRZ` provenance cards below. Consistent
    // with the rejection-map precedent (no WCS there either) is deliberate
    // for THAT artifact; a weight map is far more likely to be loaded next
    // to the image it belongs to.
    for c in drizzle_cards.iter().filter(|c| is_wcs_keyword(&c.keyword)) {
        cards.push(c.clone());
    }
    // `ROWORDER` (M4d Task 2 fix round 2): the weight map IS the drizzled
    // master's grid, row for row, so it must declare the same row order.
    // Load-bearing for an XISF weight map specifically —
    // `fits_writer::xisf_writer` states the effective order explicitly and
    // synthesizes the astronomical default when the cards carry none, so
    // without this copy-through a weight map would claim `'BOTTOM-UP'`
    // beside a master that says `'TOP-DOWN'`.
    //
    // Plan 6b-3 (ruling a): `ATH_SPRJ`/`ATH_GRID` too, so the weight map, the
    // drizzle coverage and the master coverage agree; a frame-set drizzle's
    // cards carry neither, so its bytes do not move.
    for kw in [
        "ROWORDER", "ATH_STKI", "ATH_STKG", ATH_DRZ, ATH_RGEO, ATH_SPRJ, ATH_GRID,
    ] {
        if let Some(c) = drizzle_cards.iter().find(|c| c.keyword == kw) {
            cards.push(c.clone());
        }
    }
    Ok(cards)
}

/// Header cards for drizzle's own coverage map (plan 6b-3 D9, spec §5.8):
/// [`coverage_map_cards`] of the drizzle's cards, plus `ATH_DRZ` (so the map
/// can be told apart from a master's coverage map) and a `BUNIT` comment
/// that says what is counted: drop contributions that survived rejection,
/// per output pixel, not frames.
pub fn drizzle_coverage_cards(drizzle_cards: &[Card]) -> Result<Vec<Card>, FitsWriteError> {
    let mut cards = coverage_map_cards(drizzle_cards)?;
    for c in cards.iter_mut().filter(|c| c.keyword == "BUNIT") {
        *c = Card::new("BUNIT", CardValue::Str("frames".into()))?
            .with_comment("frames whose drops reached the pixel");
    }
    if let Some(c) = drizzle_cards.iter().find(|c| c.keyword == ATH_DRZ) {
        cards.push(c.clone());
    }
    Ok(cards)
}

#[derive(Debug, Clone)]
pub struct WrittenDrizzle {
    pub drizzle: PathBuf,
    pub weight_map: Option<PathBuf>,
    /// Drizzle's own coverage beside the weight map (plan 6b-3 D9); `None`
    /// unless the output carried one.
    pub coverage: Option<PathBuf>,
}

/// Writes `output.data` (planar `channels × w × h`) as `<dir>/<drizzle
/// name>` and, when `output.weight` is `Some`, the weight map alongside it.
/// `resolve_collision` runs on BOTH paths: the drizzle path from
/// [`drizzle_file_names`]'s own name, the weight-map path from a name
/// derived off the drizzle path's RESOLVED stem (so a `_2`-suffixed
/// drizzle output still names its own weight map, not a sibling's) — the
/// same two-step pattern [`write_master_light`] uses for its rejection
/// maps.
///
/// Not atomic as a set (fix round 1, review M4 — the same property
/// [`write_master_light`] documents for its own master/map set): the
/// drizzle output lands first, then the weight map, then the coverage map
/// (plan 6b-3 D9), so a failed later write leaves the earlier files on disk
/// without it (re-running never overwrites them — `resolve_collision`
/// suffixes the retry). Two runs
/// writing into one folder concurrently can resolve the same free name
/// (`resolve_collision` is check-then-write); the orchestrator serializes
/// group writes.
///
/// `cards` must be built for `output.stats.scale` (`build_drizzle_cards`'s
/// own `scale` argument) — a `debug_assert!` (fix round 1, review M3) below
/// catches a caller that names the file for one scale while stamping
/// `ATH_DRZ` for another, which would otherwise ship a drizzled master
/// whose file name and header silently disagree.
///
/// M4d Task 2: `format` picks the container for the drizzled master and its
/// weight map — they always share the master's own format (ruling R-M4d-3),
/// so a run never leaves one of the pair behind in the other container. The
/// coverage map (plan 6b-3 D9) is always FITS, like the rejection maps.
pub fn write_drizzled_master(
    dir: &Path,
    master_stem: &str,
    output: &DrizzleOutput,
    cards: &[Card],
    format: OutputFormat,
) -> anyhow::Result<WrittenDrizzle> {
    debug_assert!(
        cards
            .iter()
            .find(|c| c.keyword == ATH_DRZ)
            .and_then(|c| c.value.as_ref())
            .is_none_or(|v| *v == CardValue::Integer(output.stats.scale as i64)),
        "write_drizzled_master: cards' ATH_DRZ must match output.stats.scale ({}) \
         — the drizzle file name and its header would otherwise disagree on scale",
        output.stats.scale
    );

    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let ext = output_extension(format);
    let (drizzle_name, _) = drizzle_file_names(master_stem, output.stats.scale);
    let drizzle_path = resolve_collision(&dir.join(&drizzle_name).with_extension(ext));
    write_output(
        &drizzle_path,
        output.width,
        output.height,
        output.channels,
        &output.data,
        cards,
        format,
    )
    .with_context(|| format!("writing {}", drizzle_path.display()))?;
    info!(
        path = %drizzle_path.display(),
        bytes = (output.data.len() * std::mem::size_of::<f32>()) as u64,
        "wrote drizzled master"
    );

    let weight_map = match output.weight.as_deref() {
        Some(weight_data) => {
            let resolved_stem = drizzle_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("drizzle")
                .to_string();
            let weight_path = resolve_collision(&dir.join(format!("{resolved_stem}_weight.{ext}")));
            let weight_cards = weight_map_cards(cards)?;
            write_output(
                &weight_path,
                output.width,
                output.height,
                output.channels,
                weight_data,
                &weight_cards,
                format,
            )
            .with_context(|| format!("writing {}", weight_path.display()))?;
            info!(
                path = %weight_path.display(),
                bytes = (weight_data.len() * std::mem::size_of::<f32>()) as u64,
                "wrote drizzle weight map"
            );
            Some(weight_path)
        }
        None => None,
    };

    // Plan 6b-3 D9 (spec §5.8): drizzle's own coverage — always FITS, one
    // plane per channel, the drizzled master's WCS.
    let coverage = match output.coverage.as_deref() {
        Some(data) => {
            let resolved_stem = drizzle_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("drizzle")
                .to_string();
            let path = resolve_collision(&dir.join(format!("{resolved_stem}_coverage.fits")));
            let map_cards = drizzle_coverage_cards(cards)?;
            write_fits_f32(
                &path,
                output.width,
                output.height,
                output.channels,
                data,
                &map_cards,
            )
            .with_context(|| format!("writing {}", path.display()))?;
            info!(
                path = %path.display(),
                bytes = (data.len() * std::mem::size_of::<f32>()) as u64,
                "wrote drizzle coverage map"
            );
            Some(path)
        }
        None => None,
    };

    Ok(WrittenDrizzle {
        drizzle: drizzle_path,
        weight_map,
        coverage,
    })
}

/// Writes the master (planar `channels × w × h`) and, when present, the two
/// maps as `<stem>_rejlow.fits` / `<stem>_rejhigh.fits`; returns the paths
/// written. Never overwrites: `resolve_collision` on the master path, the
/// map names derived from the resolved stem (so a collision-suffixed master
/// still names its own maps, not a sibling's).
///
/// M4d Task 2: `format` picks the master's container —
/// [`output_extension`] replaces `file_name`'s extension with the chosen
/// one, so a caller keeps passing [`master_file_name`]'s `.fits` name. The
/// rejection MAPS stay FITS in both cases (ruling R-M4d-3): they are
/// diagnostics, read by the same tools either way, and the master's own
/// `_2` collision suffix still carries into their names.
/// Not atomic as a set: the master lands before the maps, so a failed map
/// write leaves the master on disk with no map (re-running never overwrites
/// it — resolve_collision suffixes the retry). Two runs writing into one
/// folder concurrently can resolve the same free name (resolve_collision is
/// check-then-write); the orchestrator serializes group writes.
pub fn write_master_light(
    dir: &Path,
    file_name: &str,
    output: &GroupOutput,
    cards: &[Card],
    format: OutputFormat,
) -> anyhow::Result<WrittenMaster> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let master_path =
        resolve_collision(&dir.join(file_name).with_extension(output_extension(format)));
    write_output(
        &master_path,
        output.width,
        output.height,
        output.channels,
        &output.data,
        cards,
        format,
    )
    .with_context(|| format!("writing {}", master_path.display()))?;

    let stem = master_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("master")
        .to_string();

    // Always FITS, whatever the master's own container is (ruling R-M4d-3).
    let write_map = |suffix: &str, label: &str, data: &[f32]| -> anyhow::Result<PathBuf> {
        let path = resolve_collision(&dir.join(format!("{stem}_{suffix}.fits")));
        let map_cards = rejection_map_cards(label, cards)?;
        write_fits_f32(
            &path,
            output.width,
            output.height,
            output.channels,
            data,
            &map_cards,
        )
        .with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    };

    let rejection_low = output
        .rejection_low
        .as_deref()
        .map(|data| write_map("rejlow", "Low", data))
        .transpose()?;
    let rejection_high = output
        .rejection_high
        .as_deref()
        .map(|data| write_map("rejhigh", "High", data))
        .transpose()?;

    // Plan 6b-3 D9 (spec §5.7): the coverage map — always FITS, one plane,
    // the master's WCS — named from the master's resolved stem.
    let coverage = output
        .coverage
        .as_deref()
        .map(|data| -> anyhow::Result<PathBuf> {
            let path = resolve_collision(&dir.join(format!("{stem}_coverage.fits")));
            let map_cards = coverage_map_cards(cards)?;
            write_fits_f32(&path, output.width, output.height, 1, data, &map_cards)
                .with_context(|| format!("writing {}", path.display()))?;
            Ok(path)
        })
        .transpose()?;

    Ok(WrittenMaster {
        master: master_path,
        rejection_low,
        rejection_high,
        coverage,
    })
}

/// Plan 6b-3 D10 (spec §5.9): one group's master family — the master, its
/// rejection maps and its coverage map, written uncropped as FITS into the
/// run's scratch folder by [`write_master_light`] — re-read, cropped to
/// `rect` and written into `dir` in `format`, named as
/// [`write_master_light`] names them (the maps stay FITS). `cards` are the
/// master's with the cropped `CRPIX` ([`crate::stacking::crop::restate_crpix`]);
/// the maps' cards derive from them exactly as at the first write. On an
/// error every file this call wrote is removed; the scratch is never
/// touched.
pub fn write_cropped_master(
    dir: &Path,
    scratch: &WrittenMaster,
    rect: CropRect,
    cards: &[Card],
    format: OutputFormat,
) -> anyhow::Result<WrittenMaster> {
    let mut made = Vec::new();
    let result = crop_master_family(dir, scratch, rect, cards, format, &mut made);
    if result.is_err() {
        remove_partial_outputs(&made);
    }
    result
}

fn crop_master_family(
    dir: &Path,
    scratch: &WrittenMaster,
    rect: CropRect,
    cards: &[Card],
    format: OutputFormat,
    made: &mut Vec<PathBuf>,
) -> anyhow::Result<WrittenMaster> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    // The scratch's FULL file name: a stem would lose everything after a dot
    // inside it (`..._0.39s_3x`) once `with_extension` swaps the extension.
    let scratch_name = scratch
        .master
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("master.fits");
    let master = resolve_collision(
        &dir.join(scratch_name)
            .with_extension(output_extension(format)),
    );
    crop_file(&scratch.master, &master, rect, cards, format)?;
    made.push(master.clone());
    log_cropped("cropped master", &master);
    let stem = master
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("master")
        .to_string();
    // The maps stay FITS, named from the final master's stem, their cards
    // derived from the master's exactly as `write_master_light` derives them.
    let maps = [
        (
            &scratch.rejection_low,
            "rejlow",
            rejection_map_cards("Low", cards)?,
        ),
        (
            &scratch.rejection_high,
            "rejhigh",
            rejection_map_cards("High", cards)?,
        ),
        (&scratch.coverage, "coverage", coverage_map_cards(cards)?),
    ];
    let mut written: [Option<PathBuf>; 3] = [None, None, None];
    for (slot, (src, suffix, map_cards)) in written.iter_mut().zip(maps.iter()) {
        let Some(src) = src else { continue };
        let path = resolve_collision(&dir.join(format!("{stem}_{suffix}.fits")));
        crop_file(src, &path, rect, map_cards, OutputFormat::Fits)?;
        made.push(path.clone());
        log_cropped("cropped master map", &path);
        *slot = Some(path);
    }
    let [rejection_low, rejection_high, coverage] = written;
    Ok(WrittenMaster {
        master,
        rejection_low,
        rejection_high,
        coverage,
    })
}

/// Plan 6b-3 D10: the drizzle family — the drizzled master, its weight map
/// and its coverage — re-read from the scratch folder, cropped to `rect`
/// (already `scale` times the master's, [`CropRect::scaled`]) and written
/// into `dir` in `format`, named from the FINAL master's stem as
/// [`write_drizzled_master`] names them. `cards` are the drizzle cards with
/// the cropped, scaled `CRPIX`. On an error every file this call wrote is
/// removed.
pub fn write_cropped_drizzle(
    dir: &Path,
    master_stem: &str,
    scale: u32,
    scratch: &WrittenDrizzle,
    rect: CropRect,
    cards: &[Card],
    format: OutputFormat,
) -> anyhow::Result<WrittenDrizzle> {
    let mut made = Vec::new();
    let result = crop_drizzle_family(
        dir,
        master_stem,
        scale,
        scratch,
        rect,
        cards,
        format,
        &mut made,
    );
    if result.is_err() {
        remove_partial_outputs(&made);
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn crop_drizzle_family(
    dir: &Path,
    master_stem: &str,
    scale: u32,
    scratch: &WrittenDrizzle,
    rect: CropRect,
    cards: &[Card],
    format: OutputFormat,
    made: &mut Vec<PathBuf>,
) -> anyhow::Result<WrittenDrizzle> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let ext = output_extension(format);
    let (drizzle_name, _) = drizzle_file_names(master_stem, scale);
    let drizzle = resolve_collision(&dir.join(&drizzle_name).with_extension(ext));
    crop_file(&scratch.drizzle, &drizzle, rect, cards, format)?;
    made.push(drizzle.clone());
    log_cropped("cropped drizzled master", &drizzle);
    let stem = drizzle
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("drizzle")
        .to_string();
    let weight_map = match &scratch.weight_map {
        Some(src) => {
            let path = resolve_collision(&dir.join(format!("{stem}_weight.{ext}")));
            crop_file(src, &path, rect, &weight_map_cards(cards)?, format)?;
            made.push(path.clone());
            log_cropped("cropped drizzle weight map", &path);
            Some(path)
        }
        None => None,
    };
    let coverage = match &scratch.coverage {
        Some(src) => {
            let path = resolve_collision(&dir.join(format!("{stem}_coverage.fits")));
            crop_file(
                src,
                &path,
                rect,
                &drizzle_coverage_cards(cards)?,
                OutputFormat::Fits,
            )?;
            made.push(path.clone());
            log_cropped("cropped drizzle coverage map", &path);
            Some(path)
        }
        None => None,
    };
    Ok(WrittenDrizzle {
        drizzle,
        weight_map,
        coverage,
    })
}

/// One `info!` per final file a crop writer lands, mirroring the K = 0
/// writers' lines; a size that cannot be read is logged, never skipped.
fn log_cropped(message: &'static str, path: &Path) {
    match std::fs::metadata(path) {
        Ok(m) => info!(path = %path.display(), bytes = m.len(), "{}", message),
        Err(error) => {
            tracing::warn!(path = %path.display(), error = %error, "sizing a cropped output failed")
        }
    }
}

/// Removes the files a failed crop wrote, logging each one it cannot.
pub(crate) fn remove_partial_outputs(made: &[PathBuf]) {
    for path in made {
        if let Err(error) = std::fs::remove_file(path) {
            tracing::warn!(path = %path.display(), error = %error, "removing a partial crop output failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_parser::FitsHeader;
    use crate::stacking::integrate::GroupStats;

    /// Plan 6b-3 D3, D11 and the 6b-2 acceptance lesson (an `ATH_REGR`
    /// overflow): grid cards never fail a write — a long project id loses
    /// its comment, a long canonical filter is written whole — each key
    /// appears once, and a grid master claims no plate solve.
    #[test]
    fn grid_cards_survive_long_values() {
        let mut cards = vec![
            Card::new("FILTER", CardValue::Str("raw".into())).unwrap(),
            Card::new("ROWORDER", CardValue::Str("TOP-DOWN".into())).unwrap(),
            Card::new("PLTSOLVD", CardValue::Logical(true)).unwrap(),
            Card::new(ATH_RGEO, CardValue::Str("coRegistered".into())).unwrap(),
            Card::new("ATH_PRJ", CardValue::Str("replica-project".into())).unwrap(),
        ];
        let project_id = "p".repeat(60);
        let filter = "Optolong L-eXtreme dual narrowband, an unusually long canonical filter name";
        stamp_grid_cards(
            &mut cards,
            &GridStamp {
                project_id: &project_id,
                grid_hash: "0123456789abcdef",
                filter: Some(filter),
            },
        )
        .unwrap();
        for c in &cards {
            crate::fits_writer::card::format_card(c).unwrap();
        }
        let get = |k: &str| cards.iter().filter(|c| c.keyword == k).collect::<Vec<_>>();
        assert_eq!(get("FILTER").len(), 1);
        assert_eq!(get("FILTER")[0].value, Some(CardValue::Str(filter.into())));
        assert_eq!(
            get("ROWORDER")[0].value,
            Some(CardValue::Str("BOTTOM-UP".into()))
        );
        assert_eq!(get(ATH_RGEO).len(), 1);
        assert_eq!(
            get(ATH_RGEO)[0].value,
            Some(CardValue::Str(RGEO_GRID.into()))
        );
        assert_eq!(
            get(ATH_SPRJ)[0].value,
            Some(CardValue::Str(project_id.clone()))
        );
        assert!(
            get(ATH_SPRJ)[0].comment.is_none(),
            "a long id drops its comment"
        );
        assert_eq!(
            get(ATH_GRID)[0].value,
            Some(CardValue::Str("0123456789abcdef".into()))
        );
        assert!(
            get("PLTSOLVD").is_empty(),
            "a grid TAN is not a plate solve"
        );
        assert!(
            get("ATH_PRJ").is_empty(),
            "a stacked master is never a replica"
        );
        let tmp = tempfile::tempdir().unwrap();
        crate::fits_writer::write_fits_f32(&tmp.path().join("g.fits"), 2, 2, 1, &[0.0; 4], &cards)
            .unwrap();

        let mut short = Vec::new();
        stamp_grid_cards(
            &mut short,
            &GridStamp {
                project_id: "af136ec9-1885-4087-b684-9136ad655e9b",
                grid_hash: "0123456789abcdef",
                filter: None,
            },
        )
        .unwrap();
        assert!(
            short
                .iter()
                .find(|c| c.keyword == ATH_SPRJ)
                .unwrap()
                .comment
                .is_some(),
            "a UUID keeps its comment"
        );
        assert!(
            short.iter().all(|c| c.keyword != "FILTER"),
            "no canonical filter, no FILTER card"
        );

        // 69 characters: the value alone fills a record and continues, so the
        // comment goes, and the chain still formats.
        let id69 = "q".repeat(69);
        let card = card_with_optional_comment(ATH_SPRJ, &id69, "collaboration project").unwrap();
        assert!(card.comment.is_none(), "a continued card keeps no comment");
        assert!(crate::fits_writer::card::format_card(&card).unwrap().len() > 1);
        assert_eq!(card.value, Some(CardValue::Str(id69)));
    }

    fn dummy_stats() -> GroupStats {
        GroupStats {
            frames: 3,
            included: 3,
            dropped_below_min_weight: 0,
            recipe: "test".into(),
            rejected_low_fraction: 0.0,
            rejected_high_fraction: 0.0,
            rejected_fraction_per_frame: vec![0.0; 3],
            master_noise: vec![0.0; 3],
            master_location: vec![0.0; 3],
            master_scale: vec![0.0; 3],
            best_sub_noise: vec![0.0; 3],
            master_psf_snr: vec![0.0; 3],
            best_sub_psf_snr: vec![0.0; 3],
            snr_gain: vec![0.0; 3],
            master_fwhm_px: vec![0.0; 3],
            master_eccentricity: vec![0.0; 3],
            weighted_exposure_s: 0.0,
            total_exposure_s: 0.0,
            read_ms: 0,
            combine_ms: 0,
            bytes_read: 0,
            ln_frames: 0,
            large_scale_rejected_fraction: None,
        }
    }

    // Solver-shaped SIP tables (see fits_writer::wcs's own tests): square
    // (order+1)x(order+1), non-zero low-order terms.
    const SIP_A: &str = "[[1.0e-6,2.0e-6,1.0e-7],[3.0e-6,2.0e-7,0.0],[3.0e-7,0.0,0.0]]";
    const SIP_B: &str = "[[-1.0e-6,-2.0e-6,-1.0e-7],[-3.0e-6,-2.0e-7,0.0],[-3.0e-7,0.0,0.0]]";

    fn record(sip: bool) -> PlateSolveRecord {
        PlateSolveRecord {
            id: None,
            frame_id: 1,
            crpix1: 3111.5,
            crpix2: 2083.5,
            crval1: 316.25,
            crval2: 70.45,
            cd1_1: -2.5e-4,
            cd1_2: 1.0e-6,
            cd2_1: -1.0e-6,
            cd2_2: -2.5e-4,
            sip_order: sip.then_some(2),
            sip_a_coeffs: sip.then(|| SIP_A.to_string()),
            sip_b_coeffs: sip.then(|| SIP_B.to_string()),
            sip_ap_coeffs: None,
            sip_bp_coeffs: None,
            matched_stars: 100,
            total_detected: 200,
            rms_residual_px: 0.3,
            rms_residual_arcsec: 0.27,
            pixel_scale_arcsec: 0.9,
            field_rotation_deg: 0.0,
            solve_time_ms: 10,
            catalog_used: "test".into(),
            algorithm_used: "test".into(),
            solved_at: "2026-09-09T00:00:00Z".into(),
            expected_catalog_stars_in_fov: None,
            inlier_ratio: None,
        }
    }

    #[test]
    fn master_name_follows_the_layout_rules() {
        // The pin (owner decision 2026-09-10, fix round 1 ruling): no
        // camera token, but the colour-mode token is ALWAYS present —
        // exposure before frame count. `binning = 1` throughout this test:
        // bin-1 names are unchanged by M11 (final fix wave) — no `bin<n>`
        // token.
        assert_eq!(
            master_file_name(
                "LDN 1272",
                Some("NoFilter"),
                ColorMode::Mono,
                1,
                Some(180.0),
                208
            ),
            "LDN_1272_NoFilter_mono_180s_208x.fits"
        );
        assert_eq!(
            master_file_name("M 31", Some("Ha"), ColorMode::Mono, 1, Some(300.0), 3),
            "M_31_Ha_mono_300s_3x.fits"
        );
        // Sub-second exposure, `fmt_num`'s trimmed-decimal form.
        assert_eq!(
            master_file_name("a/b:c", Some("L"), ColorMode::Mono, 1, Some(0.5), 2),
            "a_b_c_L_mono_0.5s_2x.fits"
        );
        // Blank filter falls back to NoFilter; an empty/whitespace set name
        // falls back to "set".
        assert_eq!(
            master_file_name("M 31", Some("  "), ColorMode::Mono, 1, Some(60.0), 2),
            "M_31_NoFilter_mono_60s_2x.fits"
        );
        assert_eq!(
            master_file_name("...", Some("L"), ColorMode::Mono, 1, Some(60.0), 1),
            "set_L_mono_60s_1x.fits"
        );
        // No EXPTIME anywhere in the cluster — the "unknown" token.
        assert_eq!(
            master_file_name("M 31", None, ColorMode::Mono, 1, None, 3),
            "M_31_NoFilter_mono_unknown_3x.fits"
        );
    }

    /// M11 (final fix wave, ruling): binning >= 2 gains a `bin<n>` token;
    /// binning <= 1 (0 = unknown, treated the same as 1) does not.
    #[test]
    fn master_name_carries_a_bin_token_only_at_bin_two_and_above() {
        assert_eq!(
            master_file_name("LDN 1272", Some("Ha"), ColorMode::Mono, 2, Some(180.0), 40),
            "LDN_1272_Ha_mono_bin2_180s_40x.fits"
        );
        assert_eq!(
            master_file_name("LDN 1272", Some("Ha"), ColorMode::Mono, 3, Some(180.0), 40),
            "LDN_1272_Ha_mono_bin3_180s_40x.fits"
        );
        assert_eq!(
            master_file_name("LDN 1272", Some("Ha"), ColorMode::Mono, 1, Some(180.0), 40),
            "LDN_1272_Ha_mono_180s_40x.fits"
        );
        assert_eq!(
            master_file_name("LDN 1272", Some("Ha"), ColorMode::Mono, 0, Some(180.0), 40),
            "LDN_1272_Ha_mono_180s_40x.fits"
        );
    }

    /// Fix round 1, ruling: without a camera token, a mono and an OSC group
    /// sharing the same filter+exposure would otherwise collide (differing
    /// only by an incidental `_2` collision suffix, which hides the actual
    /// distinction instead of naming it). The colour-mode token keeps their
    /// names honestly distinct.
    #[test]
    fn mono_and_osc_names_never_collide_on_the_same_filter_and_exposure() {
        let mono = master_file_name("LDN 1272", None, ColorMode::Mono, 1, Some(180.0), 208);
        let osc = master_file_name("LDN 1272", None, ColorMode::Osc, 1, Some(180.0), 160);
        assert_eq!(mono, "LDN_1272_NoFilter_mono_180s_208x.fits");
        assert_eq!(osc, "LDN_1272_NoFilter_osc_180s_160x.fits");
        assert_ne!(mono, osc);
    }

    #[test]
    fn master_cards_carry_copy_through_wcs_and_provenance() {
        let reference_cards = vec![
            Card::new("EXPTIME", CardValue::Real(180.0)).unwrap(),
            Card::new("INSTRUME", CardValue::Str("cam".into())).unwrap(),
            Card::new("OBJECT", CardValue::Str("LDN 1272".into())).unwrap(),
            Card::new("ROWORDER", CardValue::Str("TOP-DOWN".into())).unwrap(),
            Card::new("DATE-OBS", CardValue::Str("2025-10-18T02:02:02".into())).unwrap(),
            Card::new("BAYERPAT", CardValue::Str("RGGB".into())).unwrap(), // must not survive
        ];
        let solve = record(true);
        let cards = build_master_light_cards(&MasterCardInputs {
            reference_cards: &reference_cards,
            wcs: Some(&solve),
            frames: 208,
            weighted_exposure_s: 36000.0,
            date_obs_first: Some("2025-09-14T00:56:00"),
            date_obs_last: Some("2025-10-19T05:25:52"),
            recipe: "Average | Linear fit clip (5.0/3.5)",
            weight_mode: "psfSignalWeight",
            normalization: "additiveWithScaling/scaleZeroOffset",
            reference_id: "frame:73",
            group_key: "mono__NoFilter__bin1__180s",
            cameras: &["ATR2600M".to_string(), "ZWO ASI2600MC Duo".to_string()],
            run_id: "run-7",
            app_version: "0.5.7",
            geometry: RegistrationGeometry::CoRegistered,
        })
        .unwrap();
        let kw = |k: &str| {
            cards
                .iter()
                .find(|c| c.keyword == k)
                .map(|c| c.value.clone().unwrap())
        };
        assert_eq!(kw("IMAGETYP"), Some(CardValue::Str("Master Light".into())));
        assert_eq!(kw("NCOMBINE"), Some(CardValue::Integer(208)));
        assert_eq!(kw("EXPTIME"), Some(CardValue::Real(36000.0)));
        assert_eq!(
            kw("DATE-OBS"),
            Some(CardValue::Str("2025-09-14T00:56:00".into()))
        );
        assert_eq!(
            kw("DATE-END"),
            Some(CardValue::Str("2025-10-19T05:25:52".into()))
        );
        assert_eq!(kw("ROWORDER"), Some(CardValue::Str("TOP-DOWN".into())));
        assert_eq!(kw("OBJECT"), Some(CardValue::Str("LDN 1272".into())));
        assert_eq!(kw("INSTRUME"), Some(CardValue::Str("cam".into())));
        assert_eq!(kw("BAYERPAT"), None);
        assert_eq!(kw("CTYPE1"), Some(CardValue::Str("RA---TAN-SIP".into())));
        assert_eq!(kw("ATH_STK"), Some(CardValue::Logical(true)));
        assert_eq!(kw("ATH_STKV"), Some(CardValue::Integer(1)));
        assert_eq!(kw("ATH_STKN"), Some(CardValue::Integer(208)));
        assert_eq!(
            kw("ATH_STKR"),
            Some(CardValue::Str("Average | Linear fit clip (5.0/3.5)".into()))
        );
        assert_eq!(
            kw("ATH_STKW"),
            Some(CardValue::Str("psfSignalWeight".into()))
        );
        assert_eq!(
            kw("ATH_STKO"),
            Some(CardValue::Str("additiveWithScaling/scaleZeroOffset".into()))
        );
        assert_eq!(kw("ATH_STKF"), Some(CardValue::Str("frame:73".into())));
        assert_eq!(
            kw("ATH_STKG"),
            Some(CardValue::Str("mono__NoFilter__bin1__180s".into()))
        );
        assert_eq!(
            kw("ATH_STKC"),
            Some(CardValue::Str("ATR2600M,ZWO ASI2600MC Duo".into()))
        );
        assert_eq!(kw("ATH_STKI"), Some(CardValue::Str("run-7".into())));
        // M4b ruling R-M4b-8: every master says which geometry mode built
        // it, in the config's own serde spelling.
        assert_eq!(kw(ATH_RGEO), Some(CardValue::Str("coRegistered".into())));
        assert_eq!(cards.iter().filter(|c| c.keyword == ATH_RGEO).count(), 1);
        assert!(kw("SWCREATE").is_some());
        // exactly one of each — the reference's EXPTIME/DATE-OBS were replaced, not duplicated
        assert_eq!(cards.iter().filter(|c| c.keyword == "EXPTIME").count(), 1);
        assert_eq!(cards.iter().filter(|c| c.keyword == "DATE-OBS").count(), 1);

        // Every card must format into 80-byte records — the provenance
        // values are caller strings and a comment would overflow them.
        for c in &cards {
            crate::fits_writer::card::format_card(c)
                .unwrap_or_else(|e| panic!("{}: {e}", c.keyword));
        }

        // A long, real-world reference id (150 chars: a full source path)
        // and a normalization value that alone would overflow ATH_STKO's
        // old comment (`multiplicativeWithScaling/scaleZeroOffset`, 41
        // chars) must still format — ATH_STKF goes through the CONTINUE
        // chain and reads back whole.
        // M14 (final fix wave): sanitized from a pre-existing test path
        // literal that named the external reference software directly.
        let long_reference_id = "file:/Volumes/BigMac/Users/astrobureau/Pictures/Calibration Test/LDN1272-external/LDN1272-ATH/LDN 1272/camera_atr2600m/lights/c_2025-10-18_02-02-02__-9.90_180.00s_0073.fits";
        let long_cards = build_master_light_cards(&MasterCardInputs {
            reference_cards: &reference_cards,
            wcs: Some(&solve),
            frames: 208,
            weighted_exposure_s: 36000.0,
            date_obs_first: Some("2025-09-14T00:56:00"),
            date_obs_last: Some("2025-10-19T05:25:52"),
            recipe: "Average | Linear fit clip (5.0/3.5)",
            weight_mode: "psfSignalWeight",
            normalization: "multiplicativeWithScaling/scaleZeroOffset",
            reference_id: long_reference_id,
            group_key: "osc__NoFilter__bin1__180s",
            cameras: &["ZWO ASI2600MC Duo".to_string()],
            run_id: "run-7",
            app_version: "0.5.7",
            geometry: RegistrationGeometry::CoRegistered,
        })
        .unwrap();
        for c in &long_cards {
            crate::fits_writer::card::format_card(c)
                .unwrap_or_else(|e| panic!("{}: {e}", c.keyword));
        }
        let long_kw = |k: &str| {
            long_cards
                .iter()
                .find(|c| c.keyword == k)
                .map(|c| c.value.clone().unwrap())
        };
        assert_eq!(
            long_kw("ATH_STKF"),
            Some(CardValue::Str(long_reference_id.to_string()))
        );
    }

    #[test]
    fn date_obs_falls_back_to_the_reference_card_when_the_group_has_none() {
        let reference_cards = vec![
            Card::new("EXPTIME", CardValue::Real(180.0)).unwrap(),
            Card::new("DATE-OBS", CardValue::Str("2025-10-18T02:02:02".into())).unwrap(),
        ];
        let cards = build_master_light_cards(&MasterCardInputs {
            reference_cards: &reference_cards,
            wcs: None,
            frames: 5,
            weighted_exposure_s: 900.0,
            date_obs_first: None,
            date_obs_last: None,
            recipe: "Average | Percentile clip (0.2/0.1)",
            weight_mode: "psfSignalWeight",
            normalization: "additiveWithScaling/scaleZeroOffset",
            reference_id: "frame:1",
            group_key: "mono__NoFilter__bin1__180s",
            cameras: &["cam".to_string()],
            run_id: "run-1",
            app_version: "0.5.7",
            geometry: RegistrationGeometry::Native,
        })
        .unwrap();
        let kw = |k: &str| {
            cards
                .iter()
                .find(|c| c.keyword == k)
                .map(|c| c.value.clone().unwrap())
        };
        assert_eq!(
            kw("DATE-OBS"),
            Some(CardValue::Str("2025-10-18T02:02:02".into()))
        );
        assert_eq!(kw("DATE-END"), None);
        assert_eq!(cards.iter().filter(|c| c.keyword == "DATE-OBS").count(), 1);
        // M4b ruling R-M4b-8: the card follows the caller's mode, whatever
        // the rest of the header says.
        assert_eq!(kw(ATH_RGEO), Some(CardValue::Str("native".into())));
    }

    #[test]
    fn master_cards_without_a_solve_carry_no_wcs() {
        let reference_cards = vec![
            Card::new("EXPTIME", CardValue::Real(180.0)).unwrap(),
            Card::new("INSTRUME", CardValue::Str("cam".into())).unwrap(),
            Card::new("OBJECT", CardValue::Str("LDN 1272".into())).unwrap(),
            Card::new("ROWORDER", CardValue::Str("TOP-DOWN".into())).unwrap(),
            Card::new("DATE-OBS", CardValue::Str("2025-10-18T02:02:02".into())).unwrap(),
            Card::new("BAYERPAT", CardValue::Str("RGGB".into())).unwrap(),
        ];
        let cards = build_master_light_cards(&MasterCardInputs {
            reference_cards: &reference_cards,
            wcs: None,
            frames: 208,
            weighted_exposure_s: 36000.0,
            date_obs_first: Some("2025-09-14T00:56:00"),
            date_obs_last: Some("2025-10-19T05:25:52"),
            recipe: "Average | Linear fit clip (5.0/3.5)",
            weight_mode: "psfSignalWeight",
            normalization: "additiveWithScaling/scaleZeroOffset",
            reference_id: "frame:73",
            group_key: "mono__NoFilter__bin1__180s",
            cameras: &["ATR2600M".to_string()],
            run_id: "run-7",
            app_version: "0.5.7",
            geometry: RegistrationGeometry::CoRegistered,
        })
        .unwrap();
        let forbidden_prefixes = ["CTYPE", "CRPIX", "CRVAL", "CD", "A_", "PLTSOLVD"];
        for c in &cards {
            assert!(
                !forbidden_prefixes.iter().any(|p| c.keyword.starts_with(p)),
                "unexpected WCS card {}",
                c.keyword
            );
        }
        assert_eq!(
            cards
                .iter()
                .find(|c| c.keyword == "ATH_STK")
                .map(|c| c.value.clone().unwrap()),
            Some(CardValue::Logical(true))
        );
    }

    #[test]
    fn ath_stkc_is_sorted_comma_joined_and_truncated() {
        assert_eq!(
            format_cameras_card(&["ZWO ASI2600MC Duo".to_string(), "ATR2600M".to_string()]),
            "ATR2600M,ZWO ASI2600MC Duo",
            "sorted regardless of input order"
        );
        assert_eq!(
            format_cameras_card(&["cam".to_string(), "cam".to_string()]),
            "cam",
            "deduped"
        );

        // Enough distinct long names to blow well past the 68-char budget.
        let many: Vec<String> = (0..10)
            .map(|i| format!("Camera Model Long Name {i:02}"))
            .collect();
        let joined = format_cameras_card(&many);
        assert!(
            joined.chars().count() <= 68,
            "{} chars: {joined}",
            joined.chars().count()
        );
        assert!(joined.ends_with("..."), "{joined}");

        // Fix round 1, item 1: the marker must be pure ASCII — build the
        // actual `Card` and run it through the REAL writer path
        // (`format_card`), which silently rewrites any non-ASCII byte to
        // `?` (`fits_writer::card::sanitize_text`) and would otherwise
        // corrupt every truncated ATH_STKC card without a compile-time or
        // even a panicking runtime signal. Assert on the WRITTEN bytes, not
        // just the pre-write string.
        let card = Card::new("ATH_STKC", CardValue::Str(joined.clone())).unwrap();
        let records = crate::fits_writer::card::format_card(&card).unwrap();
        assert_eq!(records.len(), 1, "68 chars fits in one 80-byte record");
        let written = String::from_utf8_lossy(&records[0]);
        assert!(
            !written.contains('?'),
            "a '?' means sanitize_text silently rewrote a non-ASCII byte: {written}"
        );
        assert!(written.contains("..."), "{written}");
        assert!(
            joined.is_ascii(),
            "format_cameras_card's own output must already be pure ASCII: {joined}"
        );
    }

    #[test]
    fn writer_lands_master_and_maps_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h, ch) = (8usize, 6usize, 3usize);
        let plane = w * h;

        let mut cards = HeaderBuilder::new(FrameKind::MasterLight).build().unwrap();
        cards.push(Card::new("ATH_STKI", CardValue::Str("run-1".into())).unwrap());
        cards.push(Card::new("ATH_STKG", CardValue::Str("group-1".into())).unwrap());
        cards.push(
            Card::new(
                "ROWORDER",
                CardValue::Str(crate::orientation::ROW_ORDER_TOP_DOWN.into()),
            )
            .unwrap(),
        );

        let output = GroupOutput {
            width: w,
            height: h,
            channels: ch,
            data: vec![0.5f32; plane * ch],
            rejection_low: Some(vec![1.0f32; plane * ch]),
            rejection_high: Some(vec![2.0f32; plane * ch]),
            included: vec![0, 1, 2],
            output_pairs: vec![Vec::new(); ch],
            second_pass_rej_ok: false,
            coverage: None,
            measure_rect: None,
            stats: dummy_stats(),
        };

        let first = write_master_light(
            dir.path(),
            "master.fits",
            &output,
            &cards,
            OutputFormat::Fits,
        )
        .unwrap();
        assert_eq!(
            first.master.file_name().and_then(|s| s.to_str()),
            Some("master.fits")
        );
        let rejlow = first.rejection_low.clone().unwrap();
        let rejhigh = first.rejection_high.clone().unwrap();
        assert_eq!(
            rejlow.file_name().and_then(|s| s.to_str()),
            Some("master_rejlow.fits")
        );
        assert_eq!(
            rejhigh.file_name().and_then(|s| s.to_str()),
            Some("master_rejhigh.fits")
        );

        let header = FitsHeader::from_path(&first.master).unwrap();
        assert_eq!(header.get_str("IMAGETYP").as_deref(), Some("Master Light"));
        assert_eq!(header.get_i32("NAXIS3"), Some(3));

        let low_header = FitsHeader::from_path(&rejlow).unwrap();
        assert_eq!(
            low_header.get_str("IMAGETYP").as_deref(),
            Some("Rejection Map Low")
        );
        assert_eq!(low_header.get_str("ATH_STK").as_deref(), Some("T"));
        assert_eq!(low_header.get_str("ATH_STKI").as_deref(), Some("run-1"));
        assert_eq!(low_header.get_str("ATH_STKG").as_deref(), Some("group-1"));
        // M4d final review, Minor #3: a rejection map carries the master's
        // own ROWORDER, matching `weight_map_cards`.
        assert_eq!(
            low_header.get_str("ROWORDER").as_deref(),
            Some(crate::orientation::ROW_ORDER_TOP_DOWN)
        );

        let high_header = FitsHeader::from_path(&rejhigh).unwrap();
        assert_eq!(
            high_header.get_str("IMAGETYP").as_deref(),
            Some("Rejection Map High")
        );
        assert_eq!(
            high_header.get_str("ROWORDER").as_deref(),
            Some(crate::orientation::ROW_ORDER_TOP_DOWN)
        );
        assert_eq!(high_header.get_str("ATH_STK").as_deref(), Some("T"));

        // Write again with the same file name → collision-suffixed names, no overwrite.
        let second = write_master_light(
            dir.path(),
            "master.fits",
            &output,
            &cards,
            OutputFormat::Fits,
        )
        .unwrap();
        assert_eq!(
            second.master.file_name().and_then(|s| s.to_str()),
            Some("master_2.fits")
        );
        assert_eq!(
            second
                .rejection_low
                .unwrap()
                .file_name()
                .and_then(|s| s.to_str()),
            Some("master_2_rejlow.fits")
        );
        assert_eq!(
            second
                .rejection_high
                .unwrap()
                .file_name()
                .and_then(|s| s.to_str()),
            Some("master_2_rejhigh.fits")
        );
        assert!(first.master.exists());
        assert!(second.master.exists());
    }

    /// M4d Task 2 (ruling R-M4d-3): the same call with `OutputFormat::Xisf`
    /// lands `master.xisf`, readable by the reader this project ships, with
    /// its rejection maps still FITS — and the two containers do not
    /// collide: a FITS master of the same stem written afterwards is
    /// `master.fits`, not `master_2.fits`.
    #[test]
    fn xisf_master_lands_beside_a_fits_one_with_fits_rejection_maps() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h, ch) = (8usize, 6usize, 3usize);
        let plane = w * h;

        let mut cards = HeaderBuilder::new(FrameKind::MasterLight).build().unwrap();
        cards.push(Card::new("ATH_STKI", CardValue::Str("run-7".into())).unwrap());
        cards.push(Card::new("ATH_STKG", CardValue::Str("group-7".into())).unwrap());

        let output = GroupOutput {
            width: w,
            height: h,
            channels: ch,
            data: vec![0.5f32; plane * ch],
            rejection_low: Some(vec![1.0f32; plane * ch]),
            rejection_high: Some(vec![2.0f32; plane * ch]),
            included: vec![0, 1, 2],
            output_pairs: vec![Vec::new(); ch],
            second_pass_rej_ok: false,
            coverage: None,
            measure_rect: None,
            stats: dummy_stats(),
        };

        let xisf = write_master_light(
            dir.path(),
            "master.fits",
            &output,
            &cards,
            OutputFormat::Xisf,
        )
        .unwrap();
        assert_eq!(
            xisf.master.file_name().and_then(|s| s.to_str()),
            Some("master.xisf")
        );
        assert_eq!(
            xisf.rejection_low
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str()),
            Some("master_rejlow.fits"),
            "the rejection maps stay FITS"
        );
        assert_eq!(
            xisf.rejection_high
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str()),
            Some("master_rejhigh.fits")
        );

        let (meta, pixels) = astroimage::ImageConverter::read_raw(&xisf.master).unwrap();
        assert_eq!((meta.width, meta.height, meta.channels), (w, h, ch));
        match pixels {
            astroimage::PixelData::Float32(v) => {
                assert_eq!(v.len(), plane * ch);
                // The reader's ADU convention (R-M4a-11): ×65535 on the way
                // out, so divide to compare against what was written.
                assert!(
                    (v[0] / 65535.0 - 0.5).abs() <= 1e-6,
                    "first sample read back as {}",
                    v[0] / 65535.0
                );
            }
            _ => panic!("expected Float32 samples"),
        }

        // A FITS master of the SAME stem: its own name is free, because
        // `resolve_collision` probes the exact candidate path. Its
        // rejection maps DO take the `_2` suffix — those names really are
        // occupied by the XISF run's own FITS maps.
        let fits = write_master_light(
            dir.path(),
            "master.fits",
            &output,
            &cards,
            OutputFormat::Fits,
        )
        .unwrap();
        assert_eq!(
            fits.master.file_name().and_then(|s| s.to_str()),
            Some("master.fits")
        );
        assert_eq!(
            fits.rejection_low
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str()),
            Some("master_rejlow_2.fits")
        );
        assert!(xisf.master.exists() && fits.master.exists());
    }

    #[test]
    fn is_wcs_keyword_matches_every_card_wcs_cards_can_emit_plus_sip_terms() {
        for kw in [
            "WCSAXES", "CTYPE1", "CTYPE2", "CRVAL1", "CRVAL2", "CRPIX1", "CRPIX2", "CD1_1",
            "CD1_2", "CD2_1", "CD2_2", "CDELT1", "CDELT2", "CROTA1", "CROTA2", "CUNIT1", "CUNIT2",
            "RADESYS", "EQUINOX", "PLTSOLVD", "A_ORDER", "B_ORDER", "AP_ORDER", "BP_ORDER",
            "A_0_0", "A_2_0", "B_1_1", "AP_3_0", "BP_0_2",
        ] {
            assert!(is_wcs_keyword(kw), "{kw} should be a WCS keyword");
        }
        for kw in [
            "OBJECT", "EXPTIME", "INSTRUME", "ATH_STK", "ATH_DRZ", "NAXIS1", "A_ORDERX", "A_1",
            "A__1", "A_1_1_1", "AX_1_1", "A_1_x",
        ] {
            assert!(!is_wcs_keyword(kw), "{kw} should NOT be a WCS keyword");
        }
    }

    #[test]
    fn build_drizzle_cards_replaces_the_wcs_block_and_stamps_provenance() {
        let solve = record(true);
        let mut master_cards = vec![
            Card::new("IMAGETYP", CardValue::Str("Master Light".into())).unwrap(),
            Card::new("OBJECT", CardValue::Str("LDN 1272".into())).unwrap(),
            Card::new("ATH_STKI", CardValue::Str("run-1".into())).unwrap(),
            Card::new("ATH_STKG", CardValue::Str("group-1".into())).unwrap(),
            // M4b: the master's own geometry card — the drizzled header
            // must carry ONE, from the argument, never both.
            Card::new(ATH_RGEO, CardValue::Str("coRegistered".into())).unwrap(),
        ];
        master_cards.extend(wcs_cards(&solve).unwrap());

        let scaled = crate::fits_writer::wcs::scale_plate_solve(&solve, 2).unwrap();
        let cards = build_drizzle_cards(
            &master_cards,
            Some(&scaled),
            2,
            0.9,
            DrizzleKernel::Square,
            RegistrationGeometry::Native,
        )
        .unwrap();

        // M4b ruling R-M4b-8: exactly one geometry card, the caller's.
        assert_eq!(cards.iter().filter(|c| c.keyword == ATH_RGEO).count(), 1);
        assert_eq!(
            cards.iter().find(|c| c.keyword == ATH_RGEO).unwrap().value,
            Some(CardValue::Str("native".into()))
        );

        // exactly one CRPIX1 card, equal to the SCALED value
        let crpix1: Vec<&Card> = cards.iter().filter(|c| c.keyword == "CRPIX1").collect();
        assert_eq!(crpix1.len(), 1, "expected exactly one CRPIX1 card");
        assert_eq!(crpix1[0].value, Some(CardValue::Real(scaled.crpix1 + 1.0)));

        // no leftover ORIGINAL SIP cards: A_2_0 must be the scaled value, not the original one
        let original_a20 = wcs_cards(&solve)
            .unwrap()
            .into_iter()
            .find(|c| c.keyword == "A_2_0")
            .unwrap();
        let a20 = cards.iter().find(|c| c.keyword == "A_2_0").unwrap();
        assert_ne!(
            a20.value, original_a20.value,
            "A_2_0 must be the SCALED value, not a leftover of the original"
        );
        assert_eq!(cards.iter().filter(|c| c.keyword == "A_2_0").count(), 1);

        // Fix round 1 (review I1): DERIVED pins, not a second hand-copied
        // keyword list — `is_wcs_keyword`'s whole job is to recognise every
        // keyword `wcs_cards` can emit, so the test must check it against
        // `wcs_cards` itself, not against a list that only agrees with the
        // implementation because both were typed by hand from the same
        // source. A future card added to `wcs_cards` without a matching
        // `is_wcs_keyword` update now fails HERE instead of shipping as a
        // silent duplicate FITS keyword in every drizzled master.
        for c in wcs_cards(&solve).unwrap() {
            assert!(
                is_wcs_keyword(&c.keyword),
                "wcs_cards emits {} but is_wcs_keyword does not know it",
                c.keyword
            );
        }
        // no keyword wcs_cards(&scaled) emits is duplicated in the output
        for c in wcs_cards(&scaled).unwrap() {
            assert_eq!(
                cards.iter().filter(|x| x.keyword == c.keyword).count(),
                1,
                "{} duplicated or missing",
                c.keyword
            );
        }

        // non-WCS cards survive untouched
        assert_eq!(cards.iter().filter(|c| c.keyword == "OBJECT").count(), 1);
        assert_eq!(cards.iter().filter(|c| c.keyword == "ATH_STKI").count(), 1);

        // the three ATH_DRZ* cards, with the right values
        assert_eq!(
            cards.iter().find(|c| c.keyword == ATH_DRZ).unwrap().value,
            Some(CardValue::Integer(2))
        );
        assert_eq!(
            cards.iter().find(|c| c.keyword == ATH_DRZP).unwrap().value,
            Some(CardValue::Real(0.9))
        );
        assert_eq!(
            cards.iter().find(|c| c.keyword == ATH_DRZK).unwrap().value,
            Some(CardValue::Str("square".into()))
        );

        // every card must still format into 80-byte records
        for c in &cards {
            crate::fits_writer::card::format_card(c)
                .unwrap_or_else(|e| panic!("{}: {e}", c.keyword));
        }
    }

    #[test]
    fn build_drizzle_cards_with_no_scaled_solve_carries_no_wcs() {
        let solve = record(true);
        let mut master_cards =
            vec![Card::new("IMAGETYP", CardValue::Str("Master Light".into())).unwrap()];
        master_cards.extend(wcs_cards(&solve).unwrap());

        let cards = build_drizzle_cards(
            &master_cards,
            None,
            3,
            0.7,
            DrizzleKernel::Circle,
            RegistrationGeometry::CoRegistered,
        )
        .unwrap();
        for c in &cards {
            assert!(
                !is_wcs_keyword(&c.keyword),
                "unexpected WCS card {}",
                c.keyword
            );
        }
        assert_eq!(
            cards.iter().find(|c| c.keyword == ATH_DRZK).unwrap().value,
            Some(CardValue::Str("circle".into()))
        );
    }

    /// Plan 6b-3 fix round 1: the drizzle's coverage map carries `ATH_DRZ`
    /// (so it can be told from a master's coverage map) and a `BUNIT`
    /// comment that does not claim samples.
    #[test]
    fn the_drizzle_coverage_map_carries_ath_drz() {
        let drizzle = vec![
            Card::new(ATH_DRZ, CardValue::Integer(2)).unwrap(),
            Card::new("ATH_STKI", CardValue::Str("run-1".into())).unwrap(),
        ];
        let cards = drizzle_coverage_cards(&drizzle).unwrap();
        let drz = cards
            .iter()
            .find(|c| c.keyword == ATH_DRZ)
            .expect("ATH_DRZ");
        assert_eq!(drz.value, Some(CardValue::Integer(2)));
        let bunit = cards.iter().find(|c| c.keyword == "BUNIT").unwrap();
        assert_eq!(
            bunit.comment.as_deref(),
            Some("frames whose drops reached the pixel")
        );
        assert!(coverage_map_cards(&drizzle)
            .unwrap()
            .iter()
            .all(|c| c.keyword != ATH_DRZ));
    }

    /// Plan 6b-3 ruling (a): the drizzle weight map copies `ATH_SPRJ` and
    /// `ATH_GRID` from the drizzle's cards when they carry them, and a
    /// frame-set drizzle's cards (carrying neither) give a weight map with
    /// neither, as before.
    #[test]
    fn weight_map_cards_copy_the_project_cards_and_a_frame_set_has_none() {
        let base = vec![
            Card::new(ATH_DRZ, CardValue::Integer(2)).unwrap(),
            Card::new("ATH_STKI", CardValue::Str("run-1".into())).unwrap(),
        ];
        let frame_set = weight_map_cards(&base).unwrap();
        assert!(frame_set
            .iter()
            .all(|c| c.keyword != ATH_SPRJ && c.keyword != ATH_GRID));
        let mut grid = base.clone();
        grid.push(Card::new(ATH_GRID, CardValue::Str("0123456789abcdef".into())).unwrap());
        grid.push(Card::new(ATH_SPRJ, CardValue::Str("p1".into())).unwrap());
        let cards = weight_map_cards(&grid).unwrap();
        for k in [ATH_SPRJ, ATH_GRID] {
            assert!(cards.iter().any(|c| c.keyword == k), "{k} copied");
        }
    }

    /// B11 (M3 final fix wave, M8): the weight map's own cards must carry
    /// the SAME scaled WCS block the drizzled master's cards do — every
    /// keyword `wcs_cards(&scaled)` emits, with the SAME value, exactly
    /// once — not just the `ATH_STKI`/`ATH_STKG`/`ATH_DRZ` provenance trio.
    #[test]
    fn weight_map_cards_carries_the_same_scaled_wcs_as_the_drizzled_master() {
        let solve = record(true);
        let mut master_cards = vec![
            Card::new("IMAGETYP", CardValue::Str("Master Light".into())).unwrap(),
            Card::new("ATH_STKI", CardValue::Str("run-1".into())).unwrap(),
            Card::new("ATH_STKG", CardValue::Str("group-1".into())).unwrap(),
        ];
        master_cards.extend(wcs_cards(&solve).unwrap());

        let scaled = crate::fits_writer::wcs::scale_plate_solve(&solve, 2).unwrap();
        let drizzle_cards = build_drizzle_cards(
            &master_cards,
            Some(&scaled),
            2,
            0.9,
            DrizzleKernel::Square,
            RegistrationGeometry::Native,
        )
        .unwrap();

        let weight_cards = weight_map_cards(&drizzle_cards).unwrap();

        for c in &drizzle_cards {
            if !is_wcs_keyword(&c.keyword) {
                continue;
            }
            let matches: Vec<&Card> = weight_cards
                .iter()
                .filter(|w| w.keyword == c.keyword)
                .collect();
            assert_eq!(
                matches.len(),
                1,
                "{}: expected exactly one card in the weight map, got {}",
                c.keyword,
                matches.len()
            );
            assert_eq!(
                matches[0].value, c.value,
                "{}: weight map value must match the drizzled master's",
                c.keyword
            );
        }

        // The provenance trio still survives too — plus M4b's geometry
        // card, which a weight map is as entitled to as its own master.
        for kw in ["ATH_STKI", "ATH_STKG", ATH_DRZ, ATH_RGEO] {
            assert_eq!(
                weight_cards.iter().filter(|c| c.keyword == kw).count(),
                1,
                "{kw} missing from the weight map"
            );
        }

        // Every card must still format into 80-byte records.
        for c in &weight_cards {
            crate::fits_writer::card::format_card(c)
                .unwrap_or_else(|e| panic!("{}: {e}", c.keyword));
        }
    }

    #[test]
    fn drizzle_file_names_follow_the_layout_rule() {
        assert_eq!(
            drizzle_file_names("LDN_1272_NoFilter_mono_180s_208x", 2),
            (
                "LDN_1272_NoFilter_mono_180s_208x_drizzle2x.fits".to_string(),
                "LDN_1272_NoFilter_mono_180s_208x_drizzle2x_weight.fits".to_string(),
            )
        );
    }

    fn dummy_drizzle_stats(
        scale: u32,
        channels: usize,
        out_w: usize,
        out_h: usize,
    ) -> crate::stacking::drizzle::DrizzleStats {
        crate::stacking::drizzle::DrizzleStats {
            scale,
            out_width: out_w,
            out_height: out_h,
            frames: 3,
            kernel: DrizzleKernel::Square,
            drop_shrink: 0.9,
            used_weights: true,
            used_rejection: true,
            ln_frames: 0,
            fwhm_px: vec![0.0; channels],
            eccentricity: vec![0.0; channels],
            noise: vec![0.0; channels],
            coverage: vec![0.0; channels],
            read_ms: 0,
            deposit_ms: 0,
            bytes_read: 0,
        }
    }

    #[test]
    fn writer_lands_drizzle_and_weight_map_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        // `write_fits_f32` only accepts 1 or 3 channels (`BadChannels`
        // otherwise) — the same constraint `write_master_light`'s own test
        // above exercises. 3 planes (an OSC drizzle output) stands in for
        // the brief's "2-plane" wording, which does not fit that
        // constraint; see the Task 4 report for this deviation.
        let (w, h, ch) = (8usize, 6usize, 3usize);
        let plane = w * h;

        let cards = vec![
            Card::new("IMAGETYP", CardValue::Str("Master Light".into())).unwrap(),
            Card::new(ATH_DRZ, CardValue::Integer(2)).unwrap(),
            Card::new("ATH_STKI", CardValue::Str("run-1".into())).unwrap(),
            Card::new("ATH_STKG", CardValue::Str("group-1".into())).unwrap(),
        ];

        let output = DrizzleOutput {
            width: w,
            height: h,
            channels: ch,
            data: vec![0.25f32; plane * ch],
            weight: Some(vec![0.75f32; plane * ch]),
            coverage: None,
            stats: dummy_drizzle_stats(2, ch, w, h),
        };

        let first =
            write_drizzled_master(dir.path(), "master", &output, &cards, OutputFormat::Fits)
                .unwrap();
        assert_eq!(
            first.drizzle.file_name().and_then(|s| s.to_str()),
            Some("master_drizzle2x.fits")
        );
        let weight = first.weight_map.clone().unwrap();
        assert_eq!(
            weight.file_name().and_then(|s| s.to_str()),
            Some("master_drizzle2x_weight.fits")
        );

        let reader = crate::integration::plane_reader::PlaneReader::open(&first.drizzle).unwrap();
        assert_eq!(reader.width(), w);
        assert_eq!(reader.height(), h);
        assert_eq!(reader.channels(), ch);

        let weight_reader = crate::integration::plane_reader::PlaneReader::open(&weight).unwrap();
        assert_eq!(weight_reader.width(), w);
        assert_eq!(weight_reader.height(), h);
        assert_eq!(weight_reader.channels(), ch);

        let weight_header = FitsHeader::from_path(&weight).unwrap();
        assert_eq!(
            weight_header.get_str("IMAGETYP").as_deref(),
            Some("Drizzle Weight")
        );
        assert_eq!(
            weight_header.get_str("BUNIT").as_deref(),
            Some("relative weight")
        );
        assert_eq!(weight_header.get_str("ATH_STKI").as_deref(), Some("run-1"));
        assert_eq!(
            weight_header.get_str("ATH_STKG").as_deref(),
            Some("group-1")
        );
        assert_eq!(weight_header.get_i32("ATH_DRZ"), Some(2));

        // Write again with the same master stem → collision-suffixed names,
        // the weight map following the RESOLVED drizzle stem, no overwrite.
        let second =
            write_drizzled_master(dir.path(), "master", &output, &cards, OutputFormat::Fits)
                .unwrap();
        assert_eq!(
            second.drizzle.file_name().and_then(|s| s.to_str()),
            Some("master_drizzle2x_2.fits")
        );
        assert_eq!(
            second
                .weight_map
                .unwrap()
                .file_name()
                .and_then(|s| s.to_str()),
            Some("master_drizzle2x_2_weight.fits")
        );
        assert!(first.drizzle.exists());
        assert!(second.drizzle.exists());
    }

    /// M4d Task 2 (ruling R-M4d-3): a drizzled master and its weight map
    /// always share the master's container — both `.xisf` here, both
    /// readable, and the weight map still derived from the drizzle output's
    /// own resolved stem.
    #[test]
    fn xisf_drizzle_pair_shares_the_masters_container() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h, ch) = (8usize, 6usize, 3usize);
        let plane = w * h;

        let cards = vec![
            Card::new("IMAGETYP", CardValue::Str("Master Light".into())).unwrap(),
            Card::new(ATH_DRZ, CardValue::Integer(2)).unwrap(),
            Card::new("ATH_STKI", CardValue::Str("run-7".into())).unwrap(),
            Card::new(
                "ROWORDER",
                CardValue::Str(crate::orientation::ROW_ORDER_TOP_DOWN.into()),
            )
            .unwrap(),
        ];

        let output = DrizzleOutput {
            width: w,
            height: h,
            channels: ch,
            data: vec![0.25f32; plane * ch],
            weight: Some(vec![0.75f32; plane * ch]),
            coverage: None,
            stats: dummy_drizzle_stats(2, ch, w, h),
        };

        let written =
            write_drizzled_master(dir.path(), "master", &output, &cards, OutputFormat::Xisf)
                .unwrap();
        assert_eq!(
            written.drizzle.file_name().and_then(|s| s.to_str()),
            Some("master_drizzle2x.xisf")
        );
        let weight = written.weight_map.clone().unwrap();
        assert_eq!(
            weight.file_name().and_then(|s| s.to_str()),
            Some("master_drizzle2x_weight.xisf")
        );

        for path in [&written.drizzle, &weight] {
            let (meta, _) = astroimage::ImageConverter::read_raw(path).unwrap();
            assert_eq!(
                (meta.width, meta.height, meta.channels),
                (w, h, ch),
                "{}",
                path.display()
            );
            // Fix round 2: the weight map is the drizzled master's grid row
            // for row, so it declares the master's own row order — not the
            // `'BOTTOM-UP'` the XISF writer synthesizes for a card list
            // that carries none.
            let keys = crate::fits_parser::stored_header::parse_stored_header_keys(
                crate::models::FileFormat::XISF,
                &crate::fits_parser::extract_xisf_header(path).unwrap(),
            );
            assert_eq!(
                keys.get("ROWORDER").map(String::as_str),
                Some(crate::orientation::ROW_ORDER_TOP_DOWN),
                "{}: {keys:?}",
                path.display()
            );
        }
    }

    /// Spec §5.7, plan 6b-3 D9: the coverage map counts frames, carries the
    /// master's WCS and its trace cards (grid ones included), and never a
    /// card that would make it read as a light (`FILTER`) or a replica
    /// (`ATH_PRJ`).
    #[test]
    fn the_coverage_map_carries_the_masters_wcs_and_counts_frames() {
        let master = vec![
            Card::new("FILTER", CardValue::Str("L".into())).unwrap(),
            Card::new("ROWORDER", CardValue::Str("BOTTOM-UP".into())).unwrap(),
            Card::new("CRVAL1", CardValue::Real(10.0)).unwrap(),
            Card::new("CD1_1", CardValue::Real(-1e-4)).unwrap(),
            Card::new("PLTSOLVD", CardValue::Logical(true)).unwrap(),
            Card::new("ATH_STKI", CardValue::Str("7".into())).unwrap(),
            Card::new("ATH_STKG", CardValue::Str("mono__L__st300".into())).unwrap(),
            Card::new(ATH_RGEO, CardValue::Str(RGEO_GRID.into())).unwrap(),
            Card::new(ATH_GRID, CardValue::Str("0123456789abcdef".into())).unwrap(),
            Card::new(ATH_SPRJ, CardValue::Str("p1".into())).unwrap(),
        ];
        let cards = coverage_map_cards(&master).unwrap();
        let get = |k: &str| {
            cards
                .iter()
                .find(|c| c.keyword == k)
                .and_then(|c| c.value.clone())
        };
        assert_eq!(get("IMAGETYP"), Some(CardValue::Str("Coverage Map".into())));
        assert_eq!(get("BUNIT"), Some(CardValue::Str("frames".into())));
        assert_eq!(get("ATH_STK"), Some(CardValue::Logical(true)));
        assert_eq!(get("CRVAL1"), Some(CardValue::Real(10.0)));
        assert_eq!(get("CD1_1"), Some(CardValue::Real(-1e-4)));
        for k in [
            "ROWORDER", "ATH_STKI", "ATH_STKG", ATH_RGEO, ATH_GRID, ATH_SPRJ,
        ] {
            assert!(get(k).is_some(), "{k} is copied");
        }
        assert!(get("FILTER").is_none() && get("ATH_PRJ").is_none());
        assert!(
            get("PLTSOLVD").is_none(),
            "a count map is not a solved image"
        );
    }

    /// Plan 6b-3 D10, review I7: finalizing a scratch family with the FULL
    /// rectangle writes what the K = 0 writer writes — the same name, the
    /// same pixels and keywords (XISF is not byte-reproducible: it stamps its
    /// creation time) and byte-identical FITS maps — and a real crop takes
    /// every member of the family to the rectangle, its `CRPIX` moved.
    #[test]
    fn a_full_rectangle_crop_writes_what_the_master_writer_writes() {
        let (w, h, ch) = (8usize, 6usize, 3usize);
        let plane = w * h;
        let mut cards = HeaderBuilder::new(FrameKind::MasterLight).build().unwrap();
        cards.push(Card::new("ATH_STKI", CardValue::Str("run-7".into())).unwrap());
        cards.extend(wcs_cards(&record(false)).unwrap());
        // The shape `stamp_grid_cards` leaves on a grid master.
        for (k, v) in [
            ("ROWORDER", "BOTTOM-UP"),
            (ATH_RGEO, "grid"),
            (ATH_GRID, "0123456789abcdef"),
            (ATH_SPRJ, "project-9"),
        ] {
            cards.push(Card::new(k, CardValue::Str(v.into())).unwrap());
        }
        let output = GroupOutput {
            width: w,
            height: h,
            channels: ch,
            data: (0..plane * ch).map(|i| i as f32 / 1000.0).collect(),
            rejection_low: Some(vec![1.0f32; plane * ch]),
            rejection_high: Some(vec![2.0f32; plane * ch]),
            coverage: Some((0..plane).map(|i| (i % 4) as f32).collect()),
            included: vec![0, 1, 2],
            output_pairs: vec![Vec::new(); ch],
            second_pass_rej_ok: false,
            stats: dummy_stats(),
            measure_rect: None,
        };
        const DOTTED: &str = "LDN_1272_L_mono_0.39s_3x.fits";
        let (direct, scratch, finals, cut) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let k0 =
            write_master_light(direct.path(), DOTTED, &output, &cards, OutputFormat::Xisf).unwrap();
        let staged =
            write_master_light(scratch.path(), DOTTED, &output, &cards, OutputFormat::Fits)
                .unwrap();
        let full = write_cropped_master(
            finals.path(),
            &staged,
            CropRect::full(w, h),
            &cards,
            OutputFormat::Xisf,
        )
        .unwrap();
        let name = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name(&full.master), name(&k0.master));
        assert_eq!(name(&full.master), "LDN_1272_L_mono_0.39s_3x.xisf");
        let (ma, pa) = astroimage::ImageConverter::read_raw(&k0.master).unwrap();
        let (mb, pb) = astroimage::ImageConverter::read_raw(&full.master).unwrap();
        assert_eq!(
            (ma.width, ma.height, ma.channels),
            (mb.width, mb.height, mb.channels)
        );
        match (pa, pb) {
            (astroimage::PixelData::Float32(a), astroimage::PixelData::Float32(b)) => {
                assert!(
                    a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()),
                    "the same pixels"
                );
            }
            _ => panic!("expected Float32 samples"),
        }
        let keys = |p: &Path| {
            crate::fits_parser::stored_header::parse_stored_header_keys(
                crate::models::FileFormat::XISF,
                &crate::fits_parser::extract_xisf_header(p).unwrap(),
            )
        };
        assert_eq!(keys(&full.master), keys(&k0.master), "the same keywords");
        for (a, b) in [
            (&k0.rejection_low, &full.rejection_low),
            (&k0.rejection_high, &full.rejection_high),
            (&k0.coverage, &full.coverage),
        ] {
            let (a, b) = (a.as_ref().unwrap(), b.as_ref().unwrap());
            assert_eq!(name(a), name(b));
            assert_eq!(
                std::fs::read(a).unwrap(),
                std::fs::read(b).unwrap(),
                "{} differs",
                name(a)
            );
        }

        let rect = CropRect {
            x0: 2,
            y0: 1,
            width: 4,
            height: 3,
        };
        let mut cropped_cards = cards.clone();
        crate::stacking::crop::restate_crpix(
            &mut cropped_cards,
            &crate::stacking::grid_map::crop_plate_solve(&record(false), 2, 1),
        );
        let c = write_cropped_master(
            cut.path(),
            &staged,
            rect,
            &cropped_cards,
            OutputFormat::Fits,
        )
        .unwrap();
        let reader = crate::integration::plane_reader::PlaneReader::open(&c.master).unwrap();
        assert_eq!(
            (reader.width(), reader.height(), reader.channels()),
            (4, 3, 3)
        );
        assert_eq!(
            reader.read_plane(1).unwrap(),
            crate::stacking::crop::crop_plane(&output.data[plane..2 * plane], w, rect)
        );
        let cov = crate::integration::plane_reader::PlaneReader::open(c.coverage.as_ref().unwrap())
            .unwrap();
        assert_eq!((cov.width(), cov.height(), cov.channels()), (4, 3, 1));
        for map in [&c.rejection_low, &c.rejection_high] {
            let r =
                crate::integration::plane_reader::PlaneReader::open(map.as_ref().unwrap()).unwrap();
            assert_eq!((r.width(), r.height(), r.channels()), (4, 3, 3));
        }
        let master_hdr = crate::fits_parser::FitsHeader::from_path(&c.master).unwrap();
        let master_before = crate::fits_parser::FitsHeader::from_path(&staged.master).unwrap();
        assert!(
            (master_hdr.get_f64("CRPIX1").unwrap()
                - (master_before.get_f64("CRPIX1").unwrap() - 2.0))
                .abs()
                < 1e-9
        );
        assert_eq!(
            master_hdr.get_str("ATH_GRID").as_deref(),
            Some("0123456789abcdef")
        );
        let moved =
            crate::fits_parser::FitsHeader::from_path(c.coverage.as_ref().unwrap()).unwrap();
        let before =
            crate::fits_parser::FitsHeader::from_path(k0.coverage.as_ref().unwrap()).unwrap();
        assert!(
            (moved.get_f64("CRPIX1").unwrap() - (before.get_f64("CRPIX1").unwrap() - 2.0)).abs()
                < 1e-9
        );
        assert!(
            (moved.get_f64("CRPIX2").unwrap() - (before.get_f64("CRPIX2").unwrap() - 1.0)).abs()
                < 1e-9
        );
    }

    /// Plan 6b-3 D10, review I7 (drizzle half): a full-rectangle finalization
    /// of a FITS scratch drizzle family writes what `write_drizzled_master`
    /// writes — the same names (dotted stem kept), pixels, a byte-identical
    /// weight map and drizzle coverage (`ATH_DRZ` kept) — and a real crop
    /// takes every member to the rectangle.
    #[test]
    fn a_full_rectangle_crop_writes_what_the_drizzle_writer_writes() {
        let (w, h, ch) = (8usize, 6usize, 3usize);
        let plane = w * h;
        let cards = vec![
            Card::new("IMAGETYP", CardValue::Str("Master Light".into())).unwrap(),
            Card::new(ATH_DRZ, CardValue::Integer(2)).unwrap(),
            Card::new("ATH_STKI", CardValue::Str("run-1".into())).unwrap(),
            Card::new(ATH_GRID, CardValue::Str("0123456789abcdef".into())).unwrap(),
            Card::new(ATH_SPRJ, CardValue::Str("project-9".into())).unwrap(),
        ];
        let output = DrizzleOutput {
            width: w,
            height: h,
            channels: ch,
            data: (0..plane * ch).map(|i| i as f32 / 100.0).collect(),
            weight: Some(vec![0.75f32; plane * ch]),
            coverage: Some((0..plane * ch).map(|i| (i % 5) as f32).collect()),
            stats: dummy_drizzle_stats(2, ch, w, h),
        };
        let stem = "LDN_1272_L_mono_0.39s_3x";
        let (direct, scratch, finals, cut) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let k0 = write_drizzled_master(direct.path(), stem, &output, &cards, OutputFormat::Fits)
            .unwrap();
        let staged =
            write_drizzled_master(scratch.path(), stem, &output, &cards, OutputFormat::Fits)
                .unwrap();
        let full = write_cropped_drizzle(
            finals.path(),
            stem,
            2,
            &staged,
            CropRect::full(w, h),
            &cards,
            OutputFormat::Fits,
        )
        .unwrap();
        let name = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            name(&full.drizzle),
            "LDN_1272_L_mono_0.39s_3x_drizzle2x.fits"
        );
        assert_eq!(name(&full.drizzle), name(&k0.drizzle));
        for (a, b) in [
            (&k0.drizzle, &full.drizzle),
            (
                k0.weight_map.as_ref().unwrap(),
                full.weight_map.as_ref().unwrap(),
            ),
            (
                k0.coverage.as_ref().unwrap(),
                full.coverage.as_ref().unwrap(),
            ),
        ] {
            assert_eq!(name(a), name(b));
            assert_eq!(
                std::fs::read(a).unwrap(),
                std::fs::read(b).unwrap(),
                "{} differs",
                name(a)
            );
        }
        let drz =
            crate::fits_parser::FitsHeader::from_path(full.coverage.as_ref().unwrap()).unwrap();
        assert_eq!(drz.get_i32(ATH_DRZ), Some(2));

        let rect = CropRect {
            x0: 2,
            y0: 2,
            width: 4,
            height: 2,
        };
        let c = write_cropped_drizzle(
            cut.path(),
            stem,
            2,
            &staged,
            rect,
            &cards,
            OutputFormat::Fits,
        )
        .unwrap();
        for p in [
            &c.drizzle,
            c.weight_map.as_ref().unwrap(),
            c.coverage.as_ref().unwrap(),
        ] {
            let r = crate::integration::plane_reader::PlaneReader::open(p).unwrap();
            assert_eq!(
                (r.width(), r.height(), r.channels()),
                (4, 2, 3),
                "{}",
                name(p)
            );
        }
    }
}
