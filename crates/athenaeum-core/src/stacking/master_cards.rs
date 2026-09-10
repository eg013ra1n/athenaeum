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
use crate::stacking::drizzle::{DrizzleKernel, DrizzleOutput};
use crate::stacking::groups::ColorMode;
use crate::stacking::integrate::GroupOutput;
use crate::stacking::register::writer::REGISTERED_COPY_THROUGH;

/// `ATH_STKV`: the master-light header format version.
pub const ATH_STK_VERSION: i64 = 1;

/// Drizzle provenance cards (spec §7, `build_drizzle_cards`).
pub const ATH_DRZ: &str = "ATH_DRZ"; // integer scale
pub const ATH_DRZP: &str = "ATH_DRZP"; // drop shrink (real)
pub const ATH_DRZK: &str = "ATH_DRZK"; // kernel serde name

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
    pub reference_id: &'a str, // ATH_STKF — the reference frame's identity (Plan 5 decides the string)
    pub group_key: &'a str,    // ATH_STKG
    /// Every distinct camera in the group (`IntegrationGroup.cameras`,
    /// already trimmed/sorted/deduped) — `ATH_STKC` (owner decision
    /// 2026-09-10: groups are camera-agnostic, so a master's own header
    /// must say which camera(s) actually contributed).
    pub cameras: &'a [String],
    pub run_id: &'a str, // ATH_STKI
    pub app_version: &'a str,
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

    Ok(cards)
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
/// `None`), then the `ATH_DRZ`/`ATH_DRZP`/`ATH_DRZK` provenance cards.
/// `NAXIS*` are the writer's business, never cards here.
pub fn build_drizzle_cards(
    master_cards: &[Card],
    scaled: Option<&PlateSolveRecord>,
    scale: u32,
    drop_shrink: f64,
    kernel: DrizzleKernel,
) -> Result<Vec<Card>, FitsWriteError> {
    let mut cards: Vec<Card> = master_cards
        .iter()
        .filter(|c| !is_wcs_keyword(&c.keyword))
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

pub struct WrittenMaster {
    pub master: PathBuf,
    pub rejection_low: Option<PathBuf>,
    pub rejection_high: Option<PathBuf>,
}

/// Header cards for one rejection map: `IMAGETYP` `Rejection Map Low`/`High`
/// (a plain card, not `FrameKind` — these are Athenaeum-only artifacts, not
/// one of the frame kinds the catalog knows), `ATH_STK`/`ATH_STKV`, `BUNIT`,
/// and the master's own `ATH_STKI`/`ATH_STKG` cards copied through so a map
/// can be traced back to its run and group without opening the master too.
fn rejection_map_cards(label: &str, master_cards: &[Card]) -> Result<Vec<Card>, FitsWriteError> {
    let mut cards = vec![
        Card::new("IMAGETYP", CardValue::Str(format!("Rejection Map {label}")))?,
        Card::new("ATH_STK", CardValue::Logical(true))?
            .with_comment("stacked by Athenaeum; never cataloged"),
        Card::new("ATH_STKV", CardValue::Integer(ATH_STK_VERSION))?
            .with_comment("stacking header version"),
        Card::new("BUNIT", CardValue::Str("count".into()))?.with_comment("rejected-sample count"),
    ];
    for kw in ["ATH_STKI", "ATH_STKG"] {
        if let Some(c) = master_cards.iter().find(|c| c.keyword == kw) {
            cards.push(c.clone());
        }
    }
    Ok(cards)
}

/// Header cards for the drizzle weight map (`IMAGETYP = 'Drizzle Weight'`,
/// `ATH_STK`/`ATH_STKV`, `BUNIT = 'relative weight'`, and the drizzle
/// output's own `ATH_STKI`/`ATH_STKG`/`ATH_DRZ` cards copied through so the
/// map can be traced back to its run/group/scale without opening the
/// drizzled master too) — the [`rejection_map_cards`] pattern.
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
    for kw in ["ATH_STKI", "ATH_STKG", ATH_DRZ] {
        if let Some(c) = drizzle_cards.iter().find(|c| c.keyword == kw) {
            cards.push(c.clone());
        }
    }
    Ok(cards)
}

pub struct WrittenDrizzle {
    pub drizzle: PathBuf,
    pub weight_map: Option<PathBuf>,
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
/// Not atomic as a pair (fix round 1, review M4 — the same property
/// [`write_master_light`] documents for its own master/map pair): the
/// drizzle output lands before the weight map, so a failed weight-map write
/// leaves the drizzle output on disk with no weight map (re-running never
/// overwrites it — `resolve_collision` suffixes the retry). Two runs
/// writing into one folder concurrently can resolve the same free name
/// (`resolve_collision` is check-then-write); the orchestrator serializes
/// group writes.
///
/// `cards` must be built for `output.stats.scale` (`build_drizzle_cards`'s
/// own `scale` argument) — a `debug_assert!` (fix round 1, review M3) below
/// catches a caller that names the file for one scale while stamping
/// `ATH_DRZ` for another, which would otherwise ship a drizzled master
/// whose file name and header silently disagree.
pub fn write_drizzled_master(
    dir: &Path,
    master_stem: &str,
    output: &DrizzleOutput,
    cards: &[Card],
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

    let (drizzle_name, _) = drizzle_file_names(master_stem, output.stats.scale);
    let drizzle_path = resolve_collision(&dir.join(&drizzle_name));
    write_fits_f32(
        &drizzle_path,
        output.width,
        output.height,
        output.channels,
        &output.data,
        cards,
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
            let weight_path = resolve_collision(&dir.join(format!("{resolved_stem}_weight.fits")));
            let weight_cards = weight_map_cards(cards)?;
            write_fits_f32(
                &weight_path,
                output.width,
                output.height,
                output.channels,
                weight_data,
                &weight_cards,
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

    Ok(WrittenDrizzle {
        drizzle: drizzle_path,
        weight_map,
    })
}

/// Writes the master (planar `channels × w × h`) and, when present, the two
/// maps as `<stem>_rejlow.fits` / `<stem>_rejhigh.fits`; returns the paths
/// written. Never overwrites: `resolve_collision` on the master path, the
/// map names derived from the resolved stem (so a collision-suffixed master
/// still names its own maps, not a sibling's).
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
) -> anyhow::Result<WrittenMaster> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let master_path = resolve_collision(&dir.join(file_name));
    write_fits_f32(
        &master_path,
        output.width,
        output.height,
        output.channels,
        &output.data,
        cards,
    )
    .with_context(|| format!("writing {}", master_path.display()))?;

    let stem = master_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("master")
        .to_string();

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

    Ok(WrittenMaster {
        master: master_path,
        rejection_low,
        rejection_high,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_parser::FitsHeader;
    use crate::stacking::integrate::GroupStats;

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

        let output = GroupOutput {
            width: w,
            height: h,
            channels: ch,
            data: vec![0.5f32; plane * ch],
            rejection_low: Some(vec![1.0f32; plane * ch]),
            rejection_high: Some(vec![2.0f32; plane * ch]),
            included: vec![0, 1, 2],
            output_pairs: vec![Vec::new(); ch],
            stats: dummy_stats(),
        };

        let first = write_master_light(dir.path(), "master.fits", &output, &cards).unwrap();
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

        let high_header = FitsHeader::from_path(&rejhigh).unwrap();
        assert_eq!(
            high_header.get_str("IMAGETYP").as_deref(),
            Some("Rejection Map High")
        );
        assert_eq!(high_header.get_str("ATH_STK").as_deref(), Some("T"));

        // Write again with the same file name → collision-suffixed names, no overwrite.
        let second = write_master_light(dir.path(), "master.fits", &output, &cards).unwrap();
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
        ];
        master_cards.extend(wcs_cards(&solve).unwrap());

        let scaled = crate::fits_writer::wcs::scale_plate_solve(&solve, 2).unwrap();
        let cards =
            build_drizzle_cards(&master_cards, Some(&scaled), 2, 0.9, DrizzleKernel::Square)
                .unwrap();

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

        let cards =
            build_drizzle_cards(&master_cards, None, 3, 0.7, DrizzleKernel::Circle).unwrap();
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
        let drizzle_cards =
            build_drizzle_cards(&master_cards, Some(&scaled), 2, 0.9, DrizzleKernel::Square)
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

        // The provenance trio still survives too.
        for kw in ["ATH_STKI", "ATH_STKG", ATH_DRZ] {
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
            stats: dummy_drizzle_stats(2, ch, w, h),
        };

        let first = write_drizzled_master(dir.path(), "master", &output, &cards).unwrap();
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
        let second = write_drizzled_master(dir.path(), "master", &output, &cards).unwrap();
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
}
