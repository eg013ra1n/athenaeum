//! Dev probe (star measurement part 1, spec
//! `docs/superpowers/specs/2026-10-06-star-truth-lab-design.md` §5): runs one image
//! through the production star-measurement entry points and writes every seed and
//! every measured star as JSON lines for the lab's scorecard (athenaeum-lab
//! `star-lab/`). Nothing here re-implements a measurement.
//!
//!   star_probe measure <image.fits> --out <file.jsonl> [--config <json>]
//!       [--plane all|N] [--debayer RGGB|BGGR|GRBG|GBRG] [--rect x,y,w,h]
//!       [--paths measure,betas,register,analysis,legacy-analysis,population]   (default: measure,betas,register)
//!       (population = probe / lab only; the stacking Measure runs Mode::All - Plan A Task 13b: the frame-statistic population
//!        and the star list only, lines `path: "population"`, the frame line adds `population`, `apertureSe` and `fittedTotals`
//!        (TFlux / TMeanFlux over the fitted set alone; NOT the stacking weight, R13b-11); replaces measure)
//!       [--betas 2.5,4,6,10]   (legacy-analysis = the analysis path, `path: "analysis"`; any other name exits 1)
//!       [--domain auto|unit|adu]   (default auto: the P-4 rule; unit never divides, adu always divides by 65535)
//!       [--diagnostics]   (one `refused` line per seed the measure path did not accept)
//!       [--seeds matched|peak|structure]   (default: the config's `seedSource`, the matched filter without `--config`)
//!       [--seed-threshold K]   (the matched filter's threshold in σ_f; default: the config's `seedThreshold`)
//!       [--pixel-noise white|mixed|unknown]   (the caller's statement of the pixels' noise correlation, ruling C-10;
//!                              default: what the Measure stage states for a calibrated frame, 1 plane white, 3 mixed)
//!       [--psf-beta B]   (the measure path fitted at `PsfModel::Fixed(B)` from its own verdict pass: the stacking
//!                         Measure stage's second pass, so the lab replays a group's fits at its group β frame by frame)
//!       [--fit-stars K]   (fast measurement, FM-2: the measure path fits the first K seeds in detection order only, the
//!                          rest keep their detection record as `notSelected`; unclamped, the lab's K may sit below the product's floor;
//!                          a `--config` document is replayed as stored, its `fitStars` too, never clamped by `resolve_config`)
//!       [--dump-ratios]   (Plan B B4: the measure frame line adds `apertureRatios`, the growth / model ratios `c` is the
//!                          median over, brightest by model flux first - the 1 to 4 of a plane at floor (i) too; null without
//!                          the flag. A diagnostics field, never the product). Every measure and analysis-mode frame line also
//!                          carries `pointSourceScreened` (the `c` candidates the point-source screen refused), `apertureBelowSnr`
//!                          (those under SNR 20 after it) and `apertureTerciles` (the median ratio per model-flux third of
//!                          the <= 64 `c` sample, brightest first; reported, never read)
//!   star_probe debayer <mosaic.fits> --pattern RGGB --out <rgb.fits>
//!   star_probe warp <image.fits> --shift dx,dy --rotate deg --out <image.fits>
//!   star_probe dump-fits <fits artifact> --out <file.jsonl>
//!
//! Energies (background, amplitude, signal, peak5; seed peak and flux; a
//! refused line's backgroundStart) are written ÷ 65535, the lab's image units;
//! production carries them ×65535.
//!
//! F-0 (Plan A Task 4): every star line carries `flags` (the named
//! `StarFlags`; the measure path sets `saturated` from Task 8); the measure and betas frame
//! lines carry `refused: {total, byReason, notSelected, trimmed}`, so
//! `fits + refused.total + notSelected + trimmed = seeds`; with
//! `--diagnostics` the measure path also writes one line per refused seed,
//! `{kind: refused, path, plane, seed (the seed line's i), reason,
//! regionRadius, backgroundStart, widthStart, growthSteps, iterations,
//! converged}` — `iterations` is the bounded verdict fit's LM iterations
//! (null where no verdict fit succeeded), `converged` the fit at β's flag.
//!
//! F-1 seeds (Plan A Task 6a, ruling F3): the measure path's seeds come from
//! `--seeds` (`matched` = `SeedSource::MatchedFilter` at `--seed-threshold`;
//! `peak` = the config's peak detector, forced even under a structure config;
//! `structure` = the structure map). Every frame line's `options.measure`
//! carries the EFFECTIVE `seedSource` (`matchedFilter` | `peak` |
//! `structure`) and `seedThreshold` (the matched filter's K, else null), so
//! the lab reads the threshold the probe actually ran.
//!
//! F-3 (Plan A Task 8, ruling R-OSP-5): a 3-plane input is measured as a
//! debayered colour image (`colour`: the saturation test's plateau needs all 8
//! neighbours). The measure frame line carries `starsUnsaturated` (the frame
//! statistic's population), `mostlySaturated` (the seeds refused
//! `mostlySaturated`, also in `refused.byReason`), `verdictFitFailed` (the
//! seeds whose verdict fit failed otherwise: no flag, no refusal) and
//! `colour`. With `--diagnostics` every measure star line carries its `seed`
//! and `verdict`, and every refused line its `verdict` (null when the seed had
//! none: refused before its region, a donut, or `mostlySaturated`):
//! `{plateau, level, excluded, amplitude, beta, fwhm, saturated, fit,
//! flagSource}`, the level and the amplitude ÷ 65535 like every energy here;
//! `byAmplitude` / `byPlateauClip` / `byNearClip` name the clause that flagged it (R2; `byNearClip` = Task 10c; `top3x3` = the 3x3 maximum it reads, ÷ 65535);
//! the frame line's `clipFloor` (the near-clip floor in range units, null = no clipped evidence) and `clipEvidence`
//! (the tie and certified cores that set it);
//! `flagSource` is `own`, `cross` (the cross-plane rule, Task 8 fix wave: a
//! star flagged on another plane at its position) or null (not flagged).
//! Every plane goes through the multi-plane entry the run uses
//! (`measure_planes_diagnosed`), so on a multi-plane input every plane is
//! measured even when `--plane` selects one (the cross-plane rule needs every
//! plane's verdicts); the measure frame line also carries `crossFlags` and
//! its `wallMs` is the plane's own share of the call.
//!
//! F-4 (Plan A Task 9, ruling R-OSP-6, revised by the owner's E5): the
//! measure path's β is continuous (`Auto`, the median of the pixel-integrated
//! bounded free-β fits of the unflagged bright seeds, each on its verdict
//! fit's own pixels); its frame line carries `betaSource` (`fitted` |
//! `fallback` | `fixed`) and `betaSample` (the fits the median is over, null
//! unless `fitted`), and every star line's `flags` may name `betaAtCap`. With
//! `--diagnostics` a sample star's line (star or refused) also carries
//! `sampleBeta`, its own integrated fit's β; no other line has the key. The
//! betas arm keeps the discrete ladder (`--betas`, default 2.5,4,6,10) as
//! row 3's β-sensitivity diagnostic.
//!
//! F-4b (Plan A Task 11b, owner decision R11b-12): the stacking Measure stage
//! fits every member of a group at the group β (the lower median of the
//! members' own βs). The measure frame line's `betaOwn` is the plane's own β
//! (F-4's estimator; null under a fixed model); `--psf-beta B` runs the
//! measure path the way that stage's second pass does — the verdict pass and
//! the own β first, then every fit at `Fixed(B)` (`betaSource` `fixed`,
//! `beta` B) — and the options record `psfModel: "fixed"`, `psfBeta: B`.
//!
//! F-5 (Plan A Task 10, ruling R-OSP-7): every star's `signal` is the fitted
//! model's flux in its FWTM ellipse times the plane's aperture correction;
//! the measure and betas frame lines carry `apertureCorrection` (that `c`,
//! null when the plane had fewer than 5 qualifying stars: `c = 1`) and
//! `apertureStars` (the qualifying stars it is the median over).
//!
//! F-10 (Plan A Task 10b, phase A1): the matched filter's measure frame line
//! carries `sigmaF` (the propagated detection noise its second pass
//! thresholded at, native units), `sigmaFMeasured` (the filtered plane's
//! measured σ, the rule through Task 10), `noiseGain` (the noise's
//! filtered-to-pixel ratio, exactly 1 when `pixelNoise` is `white`), `structureDropped` (seeds the knot test
//! dropped) and `extendedSeeds` (seeds it flagged extended; each seed line
//! carries `extended`); all `null` for the other detectors. A star whose
//! seed was flagged carries the flag `extended`. Each seed line (and, with
//! `--diagnostics`, each measure star line) carries `filterSnr`, the seed's
//! matched-filter response over the propagated σ_f it was thresholded at;
//! absent when not computed (other detectors, a star line without its seed).
//!
//! F-10 (Plan A Task 10b, phase A2b): the measure frame line also carries
//! `fieldWidth` (the plane's field width, px, the fit's and the knot kernels'),
//! `fieldWidthSpread` (`s`, px: the stars' width spread about it) and
//! `fieldWidthEnvelope` (F_env = fieldWidth + 3.09·s, px: the FWHM the knot
//! bounds sit at); the last two `null` when the matched filter did not run.
//!
//! R-OSP-15 (Plan A Task 14b, route E): the measure and analysis-mode frame lines always carry `fieldWidthAtFloor`
//! (`FieldWidth::at_floor`: the field-width pool's unclamped half-sample mode lay below the narrowest read a
//! representable star gives, 1.607042 px). On a plane stated white the field-width pass searches the 3x3 median of
//! the detection plane; every other read is unchanged.
//!
//! F-10 (Plan A Task 10b, phase A4): the measure frame line carries `trailed`
//! (the plane-level trailed verdict: no star kept, every would-be accepted fit
//! refused `trailed`) and `trailShare` (its input: the share of the judged seeds
//! refused `centreMoved`, `regionTooSmall` or `centreAtRim`, A4-2), on every detector.

//! Task 12 (spec D-3, R12-0…R12-7): `--paths analysis-mode` is the Analysis mode — `star_measure::measure_plane` under
//! `Mode::Budget` on the RAW file as the Analysis tab reads it (`Domain::RawAdu`: no `normalise`, the plane in ADU).
//! It runs alone (no other path in the same invocation), refuses `--rect`, `--debayer`, `--config`, `--psf-beta`,
//! `--domain unit`, a file whose maximum is not above 1.5 and any `--seeds` but `matched` — each an error before any
//! output file exists. `--budget N` (default `budget::DEFAULT_BUDGET`, 500; at least 1), `--psf moffat4|auto` (default
//! `budget::ANALYSIS_PSF_MODEL`, Moffat 4: no β sample, no `betaOwn`), `--clip C` (the sensor's clip level; default the
//! header's DATAMAX, else the stored integer type's full scale; a float file needs it). The matched filter runs at
//! `--seed-threshold` (default 6.0). NO seed lines are written (the lab merges every file's seed lines into the measure
//! path's); star lines carry `path: "analysis-mode"` plus `apSum` / `apN`, and with `--diagnostics` `seed`,
//! `filterSnr`, `verdict` (and, under `--psf auto`, `sampleBeta`) with one `refused` line per seed that is not a kept
//! star (reasons include `notSelected` and `trimmed`). One frame line per plane carries the counts, the `budget` report
//! (`cellsWithStars`, `cellsCovered`, `candidates`, `kept`, `notSelected`, `trimmed`), the module's phase `timings`,
//! and `options.measure` with the effective seed source, threshold, PSF model, budget and clip.

#![recursion_limit = "256"] // the frame line's `json!` literal

use std::borrow::Cow;
use std::convert::Infallible;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use astroimage::processing::vng::vng_debayer_f32;
use astroimage::BayerPattern;
use athenaeum_core::analysis::analyzer::{map_measurement, measure_raw_frame_with};
use athenaeum_core::analysis::config::{AnalysisConfig, AnalysisPsfModel};
use athenaeum_core::analysis::input::{read_raw_frame, RawPlanes};
use athenaeum_core::models::{FrameAnalysis, StarMetric};
use athenaeum_core::fits_writer::write_fits_f32;
use athenaeum_core::geometry::{Linear, LinearKind};
use athenaeum_core::integration::plane_reader::PlaneReader;
use athenaeum_core::resample::{warp_rows, Plane};
use athenaeum_core::stacking::config::{SeedSource, StackingConfig};
use athenaeum_core::stacking::fits_artifact;
use athenaeum_core::stacking::measure::{
    gain_from_header, measure_planes_diagnosed, measure_planes_pass1, measure_planes_pass2,
    MeasureOptions, SeedOverride, StarPopulation, ADU_SCALE,
};
use athenaeum_core::stacking::psf_signal::{self, StarFit};
use athenaeum_core::stacking::register::detect::{detect_stars, luminance, stars_from_fits};
use athenaeum_core::stacking::register::{DetectionConfig, RegistrationConfig};
use athenaeum_core::star_measure::{
    self as sm, fit_stars_at, star_snr, ApertureNoise, BetaSource, FitDiagnostics, FitStarsOptions,
    Outcome, PixelNoise, PsfModel, RefusalCounts, RefusalReason, SaturationVerdict,
};
use serde_json::{json, Value};

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("star_probe: {msg}");
    std::process::exit(1)
}

fn read_planes(path: &Path) -> (Vec<Vec<f32>>, usize, usize) {
    let r = PlaneReader::open(path).unwrap_or_else(|e| die(format!("{}: {e}", path.display())));
    let planes: Vec<Vec<f32>> = (0..r.channels())
        .map(|p| {
            r.read_plane(p)
                .unwrap_or_else(|e| die(format!("{}: plane {p}: {e}", path.display())))
        })
        .collect();
    (planes, r.width(), r.height())
}

/// P-28: the input's domain, declared by the caller. `Auto` is the P-4 rule; `Unit`
/// never divides (a calibrated frame exceeds 1.5 legitimately where flat division
/// lifts a saturated core); `Adu` always divides by 65535.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Domain {
    Auto,
    Unit,
    Adu,
}

impl Domain {
    fn parse(s: &str) -> Domain {
        match s {
            "auto" => Domain::Auto,
            "unit" => Domain::Unit,
            "adu" => Domain::Adu,
            o => die(format!("--domain auto|unit|adu, not {o}")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Domain::Auto => "auto",
            Domain::Unit => "unit",
            Domain::Adu => "adu",
        }
    }
}

/// P-4: the harness rule — a plane above 1.5 is in the ADU domain (`Domain::Auto`);
/// P-28: `Unit` and `Adu` replace the guess with the caller's declaration.
fn normalise(planes: &mut [Vec<f32>], domain: Domain) -> &'static str {
    let adu = match domain {
        Domain::Auto => planes.iter().flatten().any(|v| v.is_finite() && *v > 1.5),
        Domain::Unit => false,
        Domain::Adu => true,
    };
    if adu {
        planes.iter_mut().flatten().for_each(|v| *v /= ADU_SCALE);
        "adu/65535"
    } else {
        "none"
    }
}

fn pattern(s: &str) -> BayerPattern {
    match s {
        "RGGB" => BayerPattern::Rggb,
        "BGGR" => BayerPattern::Bggr,
        "GRBG" => BayerPattern::Grbg,
        "GBRG" => BayerPattern::Gbrg,
        o => die(format!("unknown Bayer pattern {o}")),
    }
}

fn debayer(plane: &[f32], w: usize, h: usize, pat: BayerPattern) -> Vec<Vec<f32>> {
    let rgb = vng_debayer_f32(plane, w, h, pat);
    let n = w * h;
    (0..3).map(|c| rgb[c * n..(c + 1) * n].to_vec()).collect()
}

fn crop(planes: &[Vec<f32>], w: usize, r: (usize, usize, usize, usize)) -> Vec<Vec<f32>> {
    let (x0, y0, cw, ch) = r;
    planes
        .iter()
        .map(|p| {
            (0..ch)
                .flat_map(|y| p[(y0 + y) * w + x0..(y0 + y) * w + x0 + cw].iter().copied())
                .collect()
        })
        .collect()
}

fn n_or_null(v: f64) -> Value {
    if v.is_finite() {
        json!(v)
    } else {
        Value::Null
    }
}

/// `apertureTerciles` (Plan B B4, PB-13'): the terciles of the <= 64 `c` sample - the median growth / model ratio of its
/// brightest, middle and faintest third by model flux (`ratios` is brightest first); null under three ratios. Reported,
/// never read.
fn aperture_terciles(ratios: &[sm::ApertureRatio]) -> Value {
    let n = ratios.len();
    if n < 3 {
        return Value::Null;
    }
    let third = |lo: usize, hi: usize| -> Value {
        let mut v: Vec<f64> = ratios[lo..hi].iter().map(|r| r.ratio).collect();
        v.sort_by(f64::total_cmp);
        let m = v.len();
        n_or_null(if m % 2 == 1 {
            v[m / 2]
        } else {
            0.5 * (v[m / 2 - 1] + v[m / 2])
        })
    };
    json!([
        third(0, n / 3),
        third(n / 3, 2 * n / 3),
        third(2 * n / 3, n)
    ])
}

fn fit_line(path: &str, plane: usize, beta: Option<f64>, f: &StarFit) -> Value {
    let s = ADU_SCALE as f64;
    json!({
        "kind": "star", "path": path, "plane": plane, "beta": beta.unwrap_or(f.beta),
        "x": f.x, "y": f.y,
        "background": f.background / s, "amplitude": f.amplitude / s, "signal": f.signal / s,
        "peak5": n_or_null(f.peak5 / s),
        "fwhmX": f.fwhm_x, "fwhmY": f.fwhm_y, "fwhm": f.fwhm(), "ecc": f.eccentricity(),
        "thetaDeg": f.theta.to_degrees(), "residual": f.residual, "flags": f.flags.names(),
        "skyRing": n_or_null(f.sky_ring / s), "skyRingSigma": n_or_null(f.sky_ring_sigma / s),
        "skyRingN": f.sky_ring_n, "skyOffset": n_or_null(f.sky_offset() / s),
        "snr": n_or_null(f.snr), "hfr": n_or_null(f.hfr),
    })
}

/// A frame line's `refused` object (F-0): the refused seeds by reason and the
/// two budget outcomes, so `fits + total + notSelected + trimmed = seeds`.
fn refused_counts(c: &RefusalCounts) -> Value {
    json!({"total": c.total(), "byReason": c.by_reason, "notSelected": c.not_selected, "trimmed": c.trimmed})
}

/// A seed's saturation verdict (F-3), the level and the amplitude ÷ 65535.
fn verdict_json(v: Option<SaturationVerdict>) -> Value {
    let s = ADU_SCALE as f64;
    match v {
        None => Value::Null,
        Some(v) => json!({
            "plateau": v.plateau, "level": n_or_null(v.level / s), "excluded": v.excluded,
            "amplitude": n_or_null(v.amplitude / s), "beta": n_or_null(v.beta), "fwhm": n_or_null(v.fwhm),
            "saturated": v.saturated, "fit": v.fit, "centred": v.centred,
            "byAmplitude": v.by_amplitude, "byPlateauClip": v.by_plateau_clip, "byNearClip": v.by_near_clip, "top3x3": n_or_null(v.top3x3 / s),
            "flagSource": match (v.saturated, v.cross) {
                (false, _) => Value::Null,
                (true, false) => json!("own"),
                (true, true) => json!("cross"),
            },
        }),
    }
}

/// One `refused` line per seed the path did not accept (`--diagnostics`);
/// `None` for an accepted one.
fn refused_line(path: &str, plane: usize, d: &FitDiagnostics) -> Option<Value> {
    let reason = match d.outcome {
        Outcome::Accepted { .. } => return None,
        Outcome::Refused(r) => r.name(),
        Outcome::NotSelected => "notSelected",
        Outcome::Trimmed => "trimmed",
    };
    let mut line = json!({
        "kind": "refused", "path": path, "plane": plane, "seed": d.seed, "x": n_or_null(d.x), "y": n_or_null(d.y), "reason": reason,
        "regionRadius": d.region_radius, "backgroundStart": n_or_null(d.background_start / ADU_SCALE as f64),
        "widthStart": n_or_null(d.width_start), "growthSteps": d.growth_steps, "iterations": d.iterations,
        "converged": d.converged, "verdict": verdict_json(d.verdict),
    });
    insert_sample_beta(&mut line, d);
    Some(line)
}

/// A frame β sample star's own integrated fit β (F-4, E5): `sampleBeta` on its line, only where the record has one.
fn insert_sample_beta(line: &mut Value, d: &FitDiagnostics) {
    if let (Some(b), Some(obj)) = (d.sample_beta, line.as_object_mut()) {
        obj.insert("sampleBeta".into(), n_or_null(b));
    }
}

/// `--seeds` (Plan A Task 6a): which detector seeds the measure path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeedChoice {
    Matched,
    Peak,
    Structure,
}

impl SeedChoice {
    fn parse(s: &str) -> SeedChoice {
        match s {
            "matched" => SeedChoice::Matched,
            "peak" => SeedChoice::Peak,
            "structure" => SeedChoice::Structure,
            o => die(format!("--seeds matched|peak|structure, not {o}")),
        }
    }

    fn source(self) -> SeedSource {
        match self {
            SeedChoice::Matched => SeedSource::MatchedFilter,
            SeedChoice::Peak => SeedSource::Peak,
            SeedChoice::Structure => SeedSource::Structure,
        }
    }
}

/// The adapter options the measure path runs with (Task 7): `--seeds` / `--seed-threshold` override what the config
/// says (no `--config` = the stacking default, the matched filter at its default threshold), and the override is
/// always `Fast`, so the options ARE the effective source. Ruling F3: the frame line prints them.
fn effective_seeds(
    mut opts: MeasureOptions,
    choice: Option<SeedChoice>,
    threshold: Option<f64>,
) -> (MeasureOptions, SeedOverride) {
    if let Some(c) = choice {
        opts.seed_source = c.source();
    }
    if let Some(k) = threshold {
        opts.seed_threshold = k;
    }
    (opts, SeedOverride::Fast)
}

struct MeasureArgs {
    image: PathBuf,
    out: PathBuf,
    config: Option<PathBuf>,
    plane: Option<usize>,
    debayer: Option<String>,
    rect: Option<(usize, usize, usize, usize)>,
    paths: Vec<String>,
    betas: Vec<f64>,
    domain: Domain,
    diagnostics: bool,
    seeds: Option<SeedChoice>,
    seed_threshold: Option<f64>,
    pixel_noise: Option<PixelNoise>,
    psf_beta: Option<f64>,
    /// `--clip` (the analysis-mode path): the sensor's clip level in the file's own ADU.
    clip: Option<f64>,
    /// `--budget` (the analysis-mode path): N good unsaturated stars.
    budget: Option<usize>,
    /// `--psf` (the analysis-mode path): the PSF model; default the Analysis mode's.
    psf: Option<PsfModel>,
    /// `--fit-stars` (the measure path): fit the first K seeds only (`Mode::Brightest`).
    fit_stars: Option<usize>,
    /// `--dump-ratios` (the measure path, Plan B B4): the frame line carries `apertureRatios`.
    dump_ratios: bool,
}

fn parse_measure(mut it: impl Iterator<Item = String>) -> MeasureArgs {
    let image = PathBuf::from(it.next().unwrap_or_else(|| die("measure: <image> missing")));
    let mut a = MeasureArgs {
        image,
        out: PathBuf::new(),
        config: None,
        plane: None,
        debayer: None,
        rect: None,
        paths: vec!["measure".into(), "betas".into(), "register".into()],
        betas: vec![2.5, 4.0, 6.0, 10.0],
        domain: Domain::Auto,
        diagnostics: false,
        seeds: None,
        seed_threshold: None,
        pixel_noise: None,
        psf_beta: None,
        clip: None,
        budget: None,
        psf: None,
        fit_stars: None,
        dump_ratios: false,
    };
    while let Some(k) = it.next() {
        if k == "--diagnostics" {
            a.diagnostics = true;
            continue;
        }
        if k == "--dump-ratios" {
            a.dump_ratios = true;
            continue;
        }
        let mut v = || {
            it.next()
                .unwrap_or_else(|| die(format!("{k}: value missing")))
        };
        match k.as_str() {
            "--out" => a.out = PathBuf::from(v()),
            "--config" => a.config = Some(PathBuf::from(v())),
            "--plane" => {
                let s = v();
                a.plane = if s == "all" {
                    None
                } else {
                    Some(s.parse().unwrap_or_else(|_| die("--plane: all or N")))
                }
            }
            "--debayer" => a.debayer = Some(v()),
            "--rect" => {
                let p: Vec<usize> = v()
                    .split(',')
                    .map(|t| t.parse().unwrap_or_else(|_| die("--rect x,y,w,h")))
                    .collect();
                if p.len() != 4 {
                    die("--rect x,y,w,h")
                }
                a.rect = Some((p[0], p[1], p[2], p[3]));
            }
            "--paths" => a.paths = v().split(',').map(String::from).collect(),
            "--domain" => a.domain = Domain::parse(&v()),
            "--seeds" => a.seeds = Some(SeedChoice::parse(&v())),
            "--pixel-noise" => {
                a.pixel_noise = Some(
                    PixelNoise::parse(&v())
                        .unwrap_or_else(|| die("--pixel-noise: white | mixed | unknown")),
                )
            }
            "--seed-threshold" => {
                a.seed_threshold = Some(
                    v().parse()
                        .ok()
                        .filter(|k: &f64| k.is_finite() && *k > 0.0)
                        .unwrap_or_else(|| die("--seed-threshold: a positive number")),
                )
            }
            "--psf-beta" => {
                a.psf_beta = Some(
                    v().parse()
                        .ok()
                        .filter(|b: &f64| b.is_finite() && *b > 1.0)
                        .unwrap_or_else(|| die("--psf-beta: a number above 1")),
                )
            }
            "--clip" => {
                a.clip = Some(
                    v().parse()
                        .ok()
                        .filter(|c: &f64| c.is_finite() && *c > 0.0)
                        .unwrap_or_else(|| die("--clip: a positive number")),
                )
            }
            "--budget" => {
                a.budget = Some(
                    v().parse()
                        .ok()
                        .filter(|n: &usize| *n >= 1)
                        .unwrap_or_else(|| die("--budget: an integer of at least 1")),
                )
            }
            "--fit-stars" => {
                a.fit_stars = Some(
                    v().parse()
                        .ok()
                        .filter(|n: &usize| *n >= 1)
                        .unwrap_or_else(|| die("--fit-stars: an integer of at least 1")),
                )
            }
            "--psf" => {
                a.psf = Some(match v().as_str() {
                    "moffat4" => PsfModel::Moffat4,
                    "auto" => PsfModel::Auto,
                    o => die(format!("--psf moffat4|auto, not {o}")),
                })
            }
            "--betas" => {
                a.betas = v()
                    .split(',')
                    .map(|t| t.parse().unwrap_or_else(|_| die("--betas")))
                    .collect()
            }
            o => die(format!("unknown flag {o}")),
        }
    }
    if a.out.as_os_str().is_empty() {
        die("--out is required")
    }
    a
}

fn load_config(path: &Option<PathBuf>) -> (MeasureOptions, DetectionConfig, usize, bool) {
    match path {
        None => (
            MeasureOptions::default(),
            DetectionConfig::default(),
            RegistrationConfig::default().max_stars,
            false,
        ),
        Some(p) => {
            let text =
                std::fs::read_to_string(p).unwrap_or_else(|e| die(format!("{}: {e}", p.display())));
            let v: Value = serde_json::from_str(&text)
                .unwrap_or_else(|e| die(format!("{}: {e}", p.display())));
            let doc = v.get("config").cloned().unwrap_or(v);
            let cfg: StackingConfig =
                serde_json::from_value(doc).unwrap_or_else(|e| die(format!("config: {e}")));
            let opts = cfg
                .measurement
                .measure_options(cfg.normalization.scale_estimator);
            (
                opts,
                cfg.registration.detection,
                cfg.registration.max_stars,
                true,
            )
        }
    }
}

/// P-20: the Analysis tab reads raw lights; its detector bins the background in integer ADU.
fn analysis_domain_ok(raw_max: f32) -> bool {
    raw_max > 1.5
}

/// Ruling F2 (Plan A): the `--paths` names. `legacy-analysis` is the `analysis` path
/// under the name it keeps once the Analysis tab measures through the shared module;
/// its JSONL `path` stays `"analysis"`. Any other name is an error.
fn resolve_paths(paths: &[String]) -> Result<Vec<String>, String> {
    paths
        .iter()
        .map(|p| match p.as_str() {
            "measure" | "betas" | "register" | "analysis" | "analysis-mode" | "population" => {
                Ok(p.clone())
            }
            "legacy-analysis" => Ok("analysis".to_string()),
            o => Err(format!(
                "unknown path: {o} (known: measure, betas, register, analysis, legacy-analysis, analysis-mode, population)"
            )),
        })
        .collect()
}

/// Returns `Err` instead of exiting once the output file exists, so the
/// `BufWriter` drops (and flushes) first: a failed run leaves complete lines.
/// An unknown `--paths` name is an `Err` before the image is read or any output
/// file exists (ruling F2; `main` exits 1 on it).
fn cmd_measure(a: MeasureArgs) -> Result<(), String> {
    let paths = resolve_paths(&a.paths)?;
    if paths.iter().any(|p| p == "analysis-mode") {
        return cmd_analysis_mode(a, &paths);
    }
    let is_xisf = a
        .image
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("xisf"));
    if is_xisf && paths.iter().all(|p| p == "analysis") {
        return cmd_legacy_xisf(a);
    }
    let (mut planes, mut w, mut h) = read_planes(&a.image);
    let raw_max = planes
        .iter()
        .flatten()
        .filter(|v| v.is_finite())
        .fold(f32::MIN, |m, &v| m.max(v));
    let want = |p: &str| paths.iter().any(|q| q == p);
    // Every `analysis` precondition is checked before any output file exists.
    if want("analysis") {
        if a.rect.is_some() || a.debayer.is_some() {
            die("the analysis path takes the file as the Analysis tab does: no --rect, no --debayer")
        }
        if !analysis_domain_ok(raw_max) {
            die(format!(
                "the analysis path takes a raw-domain file (16-bit or ADU float), as the Analysis tab does; \
                 {} is in [0, 1] (use its .u16.fits sibling)",
                a.image.display()
            ))
        }
    }
    let conversion = normalise(&mut planes, a.domain);
    let domain = a.domain.as_str();
    if let Some(p) = &a.debayer {
        if planes.len() != 1 {
            die("--debayer needs a one-plane mosaic")
        }
        planes = debayer(&planes[0], w, h, pattern(p));
    }
    if let Some(r) = a.rect {
        if r.0 + r.2 > w || r.1 + r.3 > h {
            die("--rect outside the image")
        }
        planes = crop(&planes, w, r);
        w = r.2;
        h = r.3;
    }
    let (opts, det, reg_max, from_config) = load_config(&a.config);
    let (mut opts, seed_source) = effective_seeds(opts, a.seeds, a.seed_threshold);
    // `population` (Plan A Task 13b): a probe / lab path (the stacking Measure runs `Mode::All`), fitting the frame-statistic
    // population and the star list only; its lines carry `path: "population"`. It replaces `measure`.
    if want("population") {
        if want("measure") {
            die("the population path replaces the measure path: name one of them")
        }
        opts.population = Some(StarPopulation::probe_default(reg_max));
    }
    // `--fit-stars K` (fast measurement): unclamped, straight to the module's `Mode::Brightest`.
    if let Some(k) = a.fit_stars {
        opts.fit_stars = Some(k);
    }
    // A `--config` document may carry `fitStars` itself; population wins in the module, so refuse both.
    if opts.population.is_some() && opts.fit_stars.is_some() {
        die("fitStars (--fit-stars or the --config document) and the population path both choose the fitted seeds: name one")
    }
    let measure_path = if want("population") {
        "population"
    } else {
        "measure"
    };
    let non_finite: usize = planes.iter().flatten().filter(|v| !v.is_finite()).count();
    let selected: Vec<usize> = match a.plane {
        Some(p) if p < planes.len() => vec![p],
        Some(p) => die(format!("plane {p} of {}", planes.len())),
        None => (0..planes.len()).collect(),
    };
    if want("analysis") && a.image.to_str().is_none() {
        die("non-UTF-8 path")
    }
    let file =
        std::fs::File::create(&a.out).unwrap_or_else(|e| die(format!("{}: {e}", a.out.display())));
    let mut out = BufWriter::new(file);
    let mut emit = |v: Value| writeln!(out, "{v}").unwrap_or_else(|e| die(format!("write: {e}")));
    // Ruling F3: the EFFECTIVE seed source and threshold, override included.
    let mut measure_options =
        serde_json::to_value(opts).unwrap_or_else(|e| die(format!("options: {e}")));
    if let Some(m) = measure_options.as_object_mut() {
        m.insert("seedSource".into(), json!(opts.seed_source.as_str()));
        let k = (opts.seed_source == SeedSource::MatchedFilter).then_some(opts.seed_threshold);
        m.insert("seedThreshold".into(), json!(k));
        if let Some(b) = a.psf_beta {
            m.insert("psfModel".into(), json!("fixed"));
            m.insert("psfBeta".into(), json!(b));
        }
    }
    let options = json!({ "measure": measure_options,
                          "detection": { "minSnr": det.min_snr, "maxEccentricity": det.max_eccentricity },
                          "registerMaxStars": reg_max, "fromConfig": from_config, "rawMax": raw_max });

    let mut fits0: Option<Vec<StarFit>> = None;
    let need_measure = want("measure")
        || want("population")
        || want("betas")
        || (want("register") && planes.len() == 1);
    // F-3 (Q3): a 3-plane input is a debayered colour image. Every plane goes through the run's multi-plane entry
    // (Task 8 fix wave: the cross-plane rule reads every plane's verdicts), whichever planes are selected.
    let colour = planes.len() == 3;
    // Ruling C-10: what the Measure stage states for a calibrated frame unless the flag says otherwise.
    let pixel_noise = a.pixel_noise.unwrap_or(match planes.len() {
        1 => PixelNoise::White,
        3 => PixelNoise::Mixed,
        _ => PixelNoise::Unknown,
    });
    let mut measured: Vec<_> = if need_measure && !selected.is_empty() && a.psf_beta.is_some() {
        // `--psf-beta`: the stacking Measure stage's two passes (Task 11b), the second at the given β on every plane.
        let read = |p: usize| Ok::<_, Infallible>(Cow::Borrowed(planes[p].as_slice()));
        let pass1 = measure_planes_pass1(
            planes.len(),
            w,
            h,
            &opts,
            None,
            seed_source,
            a.diagnostics,
            pixel_noise,
            gain_from_header(&a.image),
            read,
        )
        .unwrap_or_else(|never| match never {});
        let betas = vec![a.psf_beta.unwrap_or(4.0); pass1.planes()];
        measure_planes_pass2(pass1, &betas, None, read)
            .unwrap_or_else(|never| match never {})
            .into_iter()
            .map(Some)
            .collect()
    } else if need_measure && !selected.is_empty() {
        measure_planes_diagnosed(
            planes.len(),
            w,
            h,
            &opts,
            None,
            seed_source,
            a.diagnostics,
            pixel_noise,
            // F-6 (Task 11): e- per calibrated unit, `EGAIN x ATH_CSCL` from the file's header (`null` on the frame line
            // when the header has no gain: the SNR then omits its shot term).
            gain_from_header(&a.image),
            |p| Ok::<_, Infallible>(Cow::Borrowed(planes[p].as_slice())),
        )
        .unwrap_or_else(|never| match never {})
        .into_iter()
        .map(Some)
        .collect()
    } else {
        Vec::new()
    };
    for &p in &selected {
        let Some(mp) = measured.get_mut(p).and_then(Option::take) else {
            continue;
        };
        let (pm, seeds, diagnostics, wall) = (mp.measurement, mp.seeds, mp.diagnostics, mp.wall_ms);
        let s = ADU_SCALE as f64;
        if want("measure") || want("population") {
            // F-10 (Task 10b): the matched filter's knot flag rides on its seed's line (`null` for other detectors), and
            // its filtered SNR (the response over the propagated σ_f) as `filterSnr`, absent when not computed.
            let extended = |i: usize| {
                pm.detection
                    .as_ref()
                    .map(|d| d.extended.get(i).copied().unwrap_or(false))
            };
            let filter_snr = |i: usize| {
                pm.detection
                    .as_ref()
                    .and_then(|d| d.filter_snr.get(i).copied())
            };
            for (i, sd) in seeds.iter().enumerate() {
                let mut line = json!({"kind": "seed", "plane": p, "i": i, "x": sd.x, "y": sd.y, "peak": sd.peak / s,
                                      "flux": sd.flux / s, "extended": extended(i)});
                if let (Some(v), Some(obj)) = (filter_snr(i), line.as_object_mut()) {
                    obj.insert("filterSnr".into(), n_or_null(v));
                }
                emit(line);
            }
            // `--diagnostics`: each accepted star's seed record (its seed index and saturation verdict).
            let mut star_seed: Vec<Option<&FitDiagnostics>> = vec![None; pm.fits.len()];
            for d in &diagnostics {
                if let Outcome::Accepted { star } = d.outcome {
                    star_seed[star] = Some(d);
                }
            }
            // F-6 (Task 11): the aperture sum S and its pixel count, from the plane the fits were measured on.
            let working: Vec<f32> = planes[p].iter().map(|v| v * ADU_SCALE).collect();
            for (k, f) in pm.fits.iter().enumerate() {
                let mut line = fit_line(measure_path, p, None, f);
                let (_, ap) = star_snr(&working, w, h, f, None, None, &ApertureNoise::WHITE);
                if let Some(obj) = line.as_object_mut() {
                    obj.insert("apSum".into(), n_or_null(ap.s / s));
                    obj.insert("apN".into(), json!(ap.n_ap));
                }
                if let (Some(d), Some(obj)) = (star_seed[k], line.as_object_mut()) {
                    obj.insert("seed".into(), json!(d.seed));
                    if let Some(v) = filter_snr(d.seed) {
                        obj.insert("filterSnr".into(), n_or_null(v));
                    }
                    obj.insert("verdict".into(), verdict_json(d.verdict));
                    if let Some(b) = d.sample_beta {
                        obj.insert("sampleBeta".into(), n_or_null(b));
                    }
                }
                emit(line)
            }
            for d in &diagnostics {
                if let Some(line) = refused_line(measure_path, p, d) {
                    emit(line)
                }
            }
            // The adapter runs every seed (no budget) unless the populations choose: then `notSelected` is the detected
            // seeds they left out (their detection record stays), `trimmed` is 0.
            let not_selected = match (pm.population.as_ref(), opts.fit_stars) {
                (Some(r), _) => seeds.len() - r.fitted,
                (None, Some(k)) => seeds.len().saturating_sub(k),
                (None, None) => 0,
            };
            let refused = json!({"total": pm.channel.stars_refused, "byReason": pm.channel.refused_by_reason,
                                 "notSelected": not_selected, "trimmed": 0});
            // Task 13b: what the populations did - the selection counts, (a)'s accepted fits, the share of them that
            // are merged pairs and `c`'s standard error as the median of the 64 brightest ratios.
            let population = pm.population.as_ref().map(|r| {
                json!({"statistic": r.statistic, "list": r.list, "fitted": r.fitted,
                       "statisticAccepted": r.statistic_accepted, "mergedShare": n_or_null(r.merged_share),
                       "apertureSe": r.aperture_se.map(n_or_null)})
            });
            let mostly_saturated = pm
                .channel
                .refused_by_reason
                .get(RefusalReason::MostlySaturated.name())
                .copied()
                .unwrap_or(0);
            emit(
                json!({"kind": "frame", "path": measure_path, "plane": p, "beta": pm.channel.beta,
                        "betaOwn": pm.channel.beta_own.or((pm.beta_source != BetaSource::Fixed).then_some(pm.channel.beta)),
                        "betaOwnAtCap": pm.channel.beta_own_at_cap,
                        "betaSource": pm.beta_source.name(), "betaSample": pm.beta_source.sample(),
                        "seeds": seeds.len(), "fits": pm.fits.len(), "refused": refused,
                        "statistic": if pm.population.is_some() {
                            "residual-weighted mean over population (a)'s accepted fits not flagged saturated"
                        } else {
                            "residual-weighted mean over the accepted fits not flagged saturated"
                        },
                        "population": population, "apertureSe": pm.aperture_se.map(n_or_null),
                        "fittedTotals": pm.population.as_ref().map(|_| {
                            // Task 13b Step 3b: the totals over the accepted fits alone, beside the complete-detection ones (the fallback R13b-1 names)
                            let t = athenaeum_core::star_measure::signal_totals(&pm.fits);
                            let u = athenaeum_core::star_measure::ADU_SCALE as f64;
                            json!({"tflux": n_or_null(t.tflux / u), "tmeanFlux": n_or_null(t.tmean_flux / u), "meanFluxRejected": t.rejected})
                        }),
                        "starsUnsaturated": pm.channel.stars_unsaturated, "mostlySaturated": mostly_saturated,
                        "verdictFitFailed": pm.verdict_fit_failed, "crossFlags": pm.cross_flags, "colour": colour,
                        "apertureCorrection": pm.aperture_correction, "apertureStars": pm.aperture_stars,
                        "pointSourceScreened": pm.point_source_screened, "apertureBelowSnr": pm.aperture_below_snr,
                        "apertureTerciles": aperture_terciles(&pm.aperture_ratios),
                        "apertureRatios": a.dump_ratios.then(|| pm.aperture_ratios.iter().map(|r| n_or_null(r.ratio)).collect::<Vec<_>>()),
                        "ringSkyMissing": pm.ring_sky_missing,
                        "clipFloor": pm.clip_floor.map(n_or_null), "clipEvidence": pm.clip_evidence,
                        "trailed": pm.trailed, "trailShare": n_or_null(pm.trail_share),
                        "frameFwhm": pm.channel.fwhm_px, "frameEcc": pm.channel.eccentricity,
                        "sigmaF": pm.detection.as_ref().map(|d| n_or_null(d.sigma_f / s)),
                        "sigmaFMeasured": pm.detection.as_ref().map(|d| n_or_null(d.sigma_f_measured / s)),
                        "noiseGain": pm.detection.as_ref().map(|d| n_or_null(d.noise_gain)),
                        "pixelNoise": pixel_noise.as_str(),
                        "snrGain": pm.snr_gain.map(|g| n_or_null(g * s)), "snrMissing": pm.snr_missing,
                        "snrMedian": pm.snr_median.map(n_or_null), "hfrMedian": pm.hfr_median.map(n_or_null),
                        "snrNoise": pm.snr_noise.as_str(), "shotSpread": pm.shot_spread,
                        "structureDropped": pm.detection.as_ref().map(|d| d.structure_dropped),
                        "extendedSeeds": pm.detection.as_ref().map(|d| d.extended.iter().filter(|&&e| e).count()),
                        "fieldWidth": n_or_null(pm.field_width.fwhm_px),
                        "fieldWidthAtFloor": pm.field_width.at_floor(),
                        "fieldWidthSpread": pm.detection.as_ref().map(|d| n_or_null(d.envelope.spread_px)),
                        "fieldWidthEnvelope": pm.detection.as_ref().map(|d| n_or_null(d.envelope.fwhm_px)),
                        "channel": pm.channel, "options": options, "conversion": conversion,
                        "domain": domain, "nonFinite": non_finite, "wallMs": wall}),
            );
        }
        if want("betas") {
            let scaled: Vec<f32> = planes[p].iter().map(|v| v * ADU_SCALE).collect();
            // Ruling R3 (Plan A Task 6b round 1): the betas arm starts from the measure path's own field width (the
            // matched filter's, for its seeds), so at the measure's β it fits the measure's stars exactly.
            // F-5 round 4: the ADU-scaled plane's full scale caps the aperture correction's clip screen.
            let betas_opts = FitStarsOptions {
                field_width: Some(pm.field_width.fwhm_px),
                full_scale: Some(ADU_SCALE as f64),
                ..Default::default()
            };
            for &b in &a.betas {
                let t = Instant::now();
                let o = fit_stars_at(&scaled, w, h, &seeds, b, &betas_opts, None);
                let (ff, fe) = psf_signal::frame_shape(&o.fits).unwrap_or((0.0, 0.0));
                for f in &o.fits {
                    emit(fit_line("betas", p, Some(b), f))
                }
                emit(
                    json!({"kind": "frame", "path": "betas", "plane": p, "beta": b, "seeds": seeds.len(),
                            "fits": o.fits.len(), "refused": refused_counts(&o.refused), "statistic": "residual-weighted mean over all accepted fits",
                            "apertureCorrection": o.aperture_correction, "apertureStars": o.aperture_stars,
                            "ringSkyMissing": o.ring_sky_missing,
                            "frameFwhm": ff, "frameEcc": fe, "options": options, "conversion": conversion,
                            "domain": domain, "nonFinite": non_finite, "wallMs": t.elapsed().as_millis() as u64}),
                );
            }
        }
        if p == selected[0] {
            fits0 = Some(pm.fits.clone())
        }
    }

    if want("register") {
        let t = Instant::now();
        // P-17, as `register/frame.rs::reference_stars`: one plane reuses the
        // Measure fits, any other count detects on the luminance of all planes.
        let (stars, how) = if planes.len() == 1 {
            let fits = fits0
                .clone()
                .ok_or_else(|| "register on a mono plane needs the measure fits".to_string())?;
            (
                stars_from_fits(&fits, &det, reg_max),
                "stars_from_fits on the Measure fits",
            )
        } else {
            let refs: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
            let lum = luminance(&refs);
            (
                detect_stars(&lum, w, h, &det, reg_max, None),
                "detect_stars on the luminance of all planes",
            )
        };
        for s in &stars {
            let (sx, sy) = s
                .sigma
                .map(|(a, b)| (json!(a), json!(b)))
                .unwrap_or((Value::Null, Value::Null));
            emit(
                json!({"kind": "star", "path": "register", "plane": selected[0], "beta": null,
                        "x": s.x, "y": s.y, "flux": s.flux, "sigmaX": sx, "sigmaY": sy}),
            );
        }
        emit(
            json!({"kind": "frame", "path": "register", "plane": selected[0], "beta": null, "seeds": null,
                    "fits": stars.len(), "statistic": how, "frameFwhm": null, "frameEcc": null,
                    "options": options, "conversion": conversion, "domain": domain, "nonFinite": non_finite,
                    "wallMs": t.elapsed().as_millis() as u64}),
        );
    }

    if want("analysis") {
        let img = a.image.to_str().expect("checked before File::create");
        run_legacy_analysis(img, domain, non_finite, &mut emit)?;
    }
    // Debug-only (feature `lm-counters`): the Moffat LM solves this process ran, by free-parameter count.
    #[cfg(feature = "lm-counters")]
    emit(lm_counters_line());
    out.flush().map_err(|e| format!("flush: {e}"))
}

/// The process-wide Moffat LM solve counters as one `lmCounters` line: `fixedBeta` (7 free parameters: EVERY fixed-β Moffat
/// solve in the process, whichever stage ran it) and `freeBeta` (8: every free-β solve, the saturation verdict and the β
/// sample, point and integrated, all planes merged), each with solves, iterations, accepted and
/// refused steps (by the step guard / by the cost), Jacobian passes computed and reused, cost passes, the summed window
/// pixels and the solves by window size (≤ 32, 64, 128, 256, 512, 1024, 2048 px, more).
#[cfg(feature = "lm-counters")]
fn lm_counters_line() -> Value {
    let c = astroimage::analysis::fitting::lm_counters::lm_counters();
    let one = |k: &astroimage::analysis::fitting::lm_counters::LmSolveCounters| {
        json!({"solves": k.solves, "iterations": k.iterations, "acceptedSteps": k.accepted_steps,
               "refusedGuard": k.refused_guard, "refusedCost": k.refused_cost, "jacobianPasses": k.jacobian_passes,
               "jacobianReused": k.jacobian_reused, "costPasses": k.cost_passes, "pixels": k.pixels,
               "pixelHistogram": k.pixel_histogram})
    };
    json!({"kind": "lmCounters", "fixedBeta": one(&c[0]), "freeBeta": one(&c[1])})
}

/// The `legacy-analysis` arm's configuration as the Analysis tab had it before Plan C (the legacy builder chain's
/// inputs), kept here so its output cannot change when the product's `AnalysisConfig` does. `batch_concurrency` is the
/// old config's machine-dependent default, as its frame line has always carried it.
fn legacy_config_json() -> Value {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    json!({
        "detection_sigma": 5.0, "min_star_area": 5, "max_star_area": 2000, "saturation_fraction": 0.95,
        "max_stars": 500, "trail_threshold": 0.5, "mrs_layers": 0, "measure_cap": 2000, "fit_max_iter": 25,
        "fit_tolerance": 1e-4, "fit_max_rejects": 5,
        "batch_concurrency": (cores / 3).max(2).min(16) as u32,
    })
}

/// The pre-Plan-C Analysis tab: `astroimage::ImageAnalyzer` with the legacy defaults, moved verbatim from
/// `analysis/analyzer.rs`. Reads FITS and XISF alike (rustafits on the path).
fn legacy_analyze(path: &str) -> Result<(FrameAnalysis, Vec<StarMetric>, bool), String> {
    use astroimage::{ImageAnalyzer, ImageConverter};
    let analyzer = ImageAnalyzer::new()
        .with_detection_sigma(5.0f64 as f32)
        .with_min_star_area(5usize)
        .with_max_star_area(2000usize)
        .with_saturation_fraction(0.95f64 as f32)
        .with_max_stars(500usize)
        .with_trail_threshold(0.5f64 as f32)
        .with_mrs_layers(0usize)
        .with_measure_cap(2000usize)
        .with_fit_max_iter(25usize)
        .with_fit_tolerance(1e-4)
        .with_fit_max_rejects(5usize);
    let (meta, pixels) = ImageConverter::read_raw(path).map_err(|e| format!("{e:#}"))?;
    let flip_vertical = meta.flip_vertical;
    let result = analyzer
        .analyze_raw(&meta, &pixels)
        .map_err(|e| format!("{e:#}"))?;
    let stars: Vec<StarMetric> = result
        .stars
        .iter()
        .map(|s| StarMetric {
            id: None,
            frame_analysis_id: 0,
            x: s.x as f64,
            y: s.y as f64,
            peak: s.peak as f64,
            flux: s.flux as f64,
            fwhm: s.fwhm as f64,
            fwhm_x: s.fwhm_x as f64,
            fwhm_y: s.fwhm_y as f64,
            eccentricity: s.eccentricity as f64,
            snr: Some(s.snr as f64),
            hfr: Some(s.hfr as f64),
            theta: s.theta as f64,
            beta: s.beta.map(|v| v as f64),
            fit_method: format!("{:?}", s.fit_method),
            fit_residual: s.fit_residual as f64,
        })
        .collect();
    let analysis = FrameAnalysis {
        id: None,
        frame_id: 0,
        file_id: 0,
        stars_detected: result.stars_detected as i64,
        median_fwhm: result.median_fwhm as f64,
        median_eccentricity: result.median_eccentricity as f64,
        median_snr: result.median_snr as f64,
        median_hfr: result.median_hfr as f64,
        frame_snr: result.frame_snr as f64,
        snr_weight: result.snr_weight as f64,
        psf_signal: result.psf_signal as f64,
        background: result.background as f64,
        noise: result.noise as f64,
        detection_threshold: result.detection_threshold as f64,
        width: result.width as i64,
        height: result.height as i64,
        source_channels: result.source_channels as i64,
        trail_r_squared: result.trail_r_squared as f64,
        possibly_trailed: result.possibly_trailed,
        median_beta: result.median_beta.map(|v| v as f64),
        quality_score: None,
        config_hash: Some("star_probe".to_string()),
        analyzed_at: chrono::Utc::now().to_rfc3339(),
        measure_version: None,
        stars_refused: None,
        frame_verdict: None,
    };
    Ok((analysis, stars, flip_vertical))
}

/// The `legacy-analysis` arm's star and frame lines (JSONL `path: "analysis"`).
fn run_legacy_analysis(
    img: &str,
    domain: &str,
    non_finite: usize,
    emit: &mut dyn FnMut(Value),
) -> Result<(), String> {
    let t = Instant::now();
    match legacy_analyze(img) {
        Ok((fa, stars, flip)) => {
            for s in &stars {
                emit(
                    json!({"kind": "star", "path": "analysis", "plane": 0, "beta": s.beta,
                            "x": s.x, "y": s.y, "peak": s.peak, "flux": s.flux, "fwhm": s.fwhm,
                            "fwhmX": s.fwhm_x, "fwhmY": s.fwhm_y, "ecc": s.eccentricity,
                            "thetaDeg": s.theta.to_degrees(), "snr": s.snr, "hfr": s.hfr,
                            "fitMethod": s.fit_method, "residual": s.fit_residual}),
                );
            }
            emit(
                json!({"kind": "frame", "path": "analysis", "plane": 0, "beta": null, "seeds": null,
                        "fits": stars.len(), "statistic": "clipped weighted median over e <= 0.8",
                        "frameFwhm": fa.median_fwhm, "frameEcc": fa.median_eccentricity,
                        "frame": fa, "flipVertical": flip,
                        "options": { "analysis": legacy_config_json() }, "conversion": "none", "domain": domain,
                        "nonFinite": non_finite, "wallMs": t.elapsed().as_millis() as u64}),
            );
            Ok(())
        }
        Err(e) => Err(format!("analysis: {e}")),
    }
}

/// `legacy-analysis` on an XISF: the FITS front-end (`PlaneReader`) cannot open it, so the file is read through
/// rustafits and only the finite count and the raw-domain check are taken from the pixels. Runs alone.
fn cmd_legacy_xisf(a: MeasureArgs) -> Result<(), String> {
    if a.rect.is_some() || a.debayer.is_some() {
        return Err(
            "the analysis path takes the file as the Analysis tab does: no --rect, no --debayer".into(),
        );
    }
    let img = a
        .image
        .to_str()
        .ok_or_else(|| "non-UTF-8 path".to_string())?;
    let (_, pixels) =
        astroimage::ImageConverter::read_raw(img).map_err(|e| format!("{img}: {e:#}"))?;
    let (non_finite, raw_max) = match &pixels {
        astroimage::PixelData::Uint16(v) => (0, v.iter().copied().max().unwrap_or(0) as f32),
        astroimage::PixelData::Float32(v) => (
            v.iter().filter(|x| !x.is_finite()).count(),
            v.iter().copied().filter(|x| x.is_finite()).fold(f32::MIN, f32::max),
        ),
    };
    if !analysis_domain_ok(raw_max) {
        return Err(format!(
            "the analysis path takes a raw-domain file (16-bit or ADU float), as the Analysis tab does; \
             {} is in [0, 1] (use its .u16.fits sibling)",
            a.image.display()
        ));
    }
    let file = std::fs::File::create(&a.out).map_err(|e| format!("{}: {e}", a.out.display()))?;
    let mut out = BufWriter::new(file);
    let mut emit = |v: Value| writeln!(out, "{v}").unwrap_or_else(|e| die(format!("write: {e}")));
    run_legacy_analysis(img, a.domain.as_str(), non_finite, &mut emit)?;
    out.flush().map_err(|e| format!("flush: {e}"))
}

/// The analysis-mode path (Task 12, R12-7; Plan C C5): the product itself. The file is read with `read_raw_frame` (the
/// Analysis tab's raw input rules), measured with `measure_raw_frame` (mono, a Bayer mosaic's green sites or a
/// debayered file's G plane) and its star / refusal / frame lines come from the `PlaneMeasurement`; the frame line adds
/// `"product"`, the `FrameAnalysis` row. Every refusal is an `Err` before the output file is created.
fn cmd_analysis_mode(a: MeasureArgs, paths: &[String]) -> Result<(), String> {
    if paths.len() != 1 {
        return Err("the analysis-mode path runs alone: no other --paths name beside it".into());
    }
    if a.rect.is_some() || a.debayer.is_some() || a.config.is_some() || a.psf_beta.is_some() {
        return Err(
            "the analysis-mode path reads the file as the Analysis tab does: no --rect, --debayer, --config or --psf-beta"
                .into(),
        );
    }
    if a.domain == Domain::Unit {
        return Err(
            "the analysis-mode path reads the raw file: --domain adu or auto, not unit".into(),
        );
    }
    if a.seeds.is_some_and(|c| c != SeedChoice::Matched) {
        return Err(
            "the analysis-mode path detects with the matched filter: --seeds matched or none"
                .into(),
        );
    }
    let mut frame = read_raw_frame(&a.image).map_err(|e| format!("{e:#}"))?;
    if let (Some(c), sm::Domain::RawAdu { clip, .. }) = (a.clip, &mut frame.domain) {
        *clip = c;
    }
    let defaults = AnalysisConfig::default();
    let budget = a.budget.unwrap_or(defaults.max_stars as usize);
    let psf_model = match a.psf {
        None => match defaults.psf_model {
            AnalysisPsfModel::Moffat4 => PsfModel::Moffat4,
            AnalysisPsfModel::Auto => PsfModel::Auto,
        },
        Some(m @ (PsfModel::Moffat4 | PsfModel::Auto)) => m,
        Some(_) => {
            return Err(
                "the analysis-mode path fits moffat4 or auto: --psf moffat4|auto".into(),
            )
        }
    };
    let threshold = a.seed_threshold.unwrap_or(defaults.detection_threshold);
    let config = AnalysisConfig {
        detection_threshold: threshold,
        psf_model: match psf_model {
            PsfModel::Moffat4 => AnalysisPsfModel::Moffat4,
            _ => AnalysisPsfModel::Auto,
        },
        max_stars: u32::try_from(budget).unwrap_or(u32::MAX),
        batch_concurrency: 1,
    };
    let (plane, p, colour): (&[f32], usize, bool) = match &frame.planes {
        RawPlanes::Mono(m) => (m, 0, false),
        RawPlanes::Cfa { mosaic, .. } => (mosaic, 0, false),
        RawPlanes::Rgb(r) => (&r[1], 1, true),
    };
    if a.plane.is_some_and(|q| q != p) {
        return Err(format!(
            "plane {}: the analysis-mode path measures plane {p} of this file only",
            a.plane.unwrap_or(0)
        ));
    }
    let (clip, full_scale, gain) = match frame.domain {
        sm::Domain::RawAdu {
            clip,
            full_scale,
            gain_e_per_adu,
        } => (clip, full_scale, gain_e_per_adu),
        sm::Domain::Calibrated { .. } => die("read_raw_frame returned a calibrated domain"),
    };
    let pixel_noise = if colour {
        PixelNoise::Mixed
    } else {
        PixelNoise::White
    };
    let non_finite: usize = plane.iter().filter(|v| !v.is_finite()).count();
    let options = json!({
        "measure": {
            "seedSource": "matchedFilter", "seedThreshold": threshold,
            "psfModel": match psf_model { PsfModel::Moffat4 => "moffat4", _ => "auto" },
            "budget": budget, "clip": clip, "fullScale": full_scale, "gainEPerAdu": gain,
            "maxStars": sm::MeasureOptions::default().max_stars,
        },
        "channels": frame.channels,
        "cfa": matches!(frame.planes, RawPlanes::Cfa { .. }),
    });
    let file = std::fs::File::create(&a.out).map_err(|e| format!("{}: {e}", a.out.display()))?;
    let mut out = BufWriter::new(file);
    let mut emit = |v: Value| writeln!(out, "{v}").unwrap_or_else(|e| die(format!("write: {e}")));
    let s = ADU_SCALE as f64;
    let domain = a.domain.as_str();
    let t = Instant::now();
    let pm = measure_raw_frame_with(&frame, &config, None, a.diagnostics);
    let wall = t.elapsed().as_millis() as u64;
    let (product, _) = map_measurement(&pm, &frame, &config, "star_probe");
    let mut star_seed: Vec<Option<&FitDiagnostics>> = vec![None; pm.stars.len()];
    for d in &pm.diagnostics {
        if let Outcome::Accepted { star } = d.outcome {
            star_seed[star] = Some(d);
        }
    }
    let filter_snr = |i: usize| {
        pm.detection
            .as_ref()
            .and_then(|d| d.filter_snr.get(i).copied())
    };
    let is_cfa = matches!(frame.planes, RawPlanes::Cfa { .. });
    for (k, f) in pm.stars.iter().enumerate() {
        let mut line = fit_line("analysis-mode", p, None, f);
        // The plane is already in ADU: the aperture sum reads it as it is (not on a mosaic: its sites are not one colour).
        if !is_cfa {
            let (_, ap) = star_snr(plane, frame.w, frame.h, f, None, None, &ApertureNoise::WHITE);
            if let Some(obj) = line.as_object_mut() {
                obj.insert("apSum".into(), n_or_null(ap.s / s));
                obj.insert("apN".into(), json!(ap.n_ap));
            }
        }
        if let (Some(d), Some(obj)) = (star_seed[k], line.as_object_mut()) {
            obj.insert("seed".into(), json!(d.seed));
            if let Some(v) = filter_snr(d.seed) {
                obj.insert("filterSnr".into(), n_or_null(v));
            }
            obj.insert("verdict".into(), verdict_json(d.verdict));
            if let Some(b) = d.sample_beta {
                obj.insert("sampleBeta".into(), n_or_null(b));
            }
        }
        emit(line)
    }
    for d in &pm.diagnostics {
        if let Some(line) = refused_line("analysis-mode", p, d) {
            emit(line)
        }
    }
    let mostly_saturated = pm
        .refused
        .by_reason
        .get(&RefusalReason::MostlySaturated)
        .copied()
        .unwrap_or(0);
    emit(json!({
        "kind": "frame", "path": "analysis-mode", "plane": p,
        "beta": pm.beta, "betaSource": pm.beta_source.name(), "betaSample": pm.beta_source.sample(),
        "seeds": pm.seeds, "fits": pm.stars.len(), "refused": refused_counts(&pm.refused),
        "budget": pm.budget,
        "statistic": "residual-weighted mean over the kept fits neither saturated nor extended",
        "starsUnsaturated": pm.frame.stars_unsaturated, "mostlySaturated": mostly_saturated,
        "verdictFitFailed": pm.verdict_fit_failed, "colour": colour,
        "apertureCorrection": pm.aperture_correction, "apertureStars": pm.aperture_stars,
        "pointSourceScreened": pm.point_source_screened, "apertureBelowSnr": pm.aperture_below_snr,
        "apertureTerciles": aperture_terciles(&pm.aperture_ratios),
        "ringSkyMissing": pm.ring_sky_missing,
        "clipFloor": pm.clip_floor.map(n_or_null), "clipEvidence": pm.clip_evidence,
        "trailed": pm.trailed, "trailShare": n_or_null(pm.trail_share), "defocused": pm.defocused,
        "frameFwhm": pm.frame.fwhm_px.unwrap_or(0.0), "frameEcc": pm.frame.eccentricity.unwrap_or(0.0),
        "sigmaF": pm.detection.as_ref().map(|d| n_or_null(d.sigma_f / s)),
        "sigmaFMeasured": pm.detection.as_ref().map(|d| n_or_null(d.sigma_f_measured / s)),
        "noiseGain": pm.detection.as_ref().map(|d| n_or_null(d.noise_gain)),
        "pixelNoise": pixel_noise.as_str(),
        "structureDropped": pm.detection.as_ref().map(|d| d.structure_dropped),
        "extendedSeeds": pm.detection.as_ref().map(|d| d.extended.iter().filter(|&&e| e).count()),
        "fieldWidth": n_or_null(pm.field_width.fwhm_px),
        "fieldWidthAtFloor": pm.field_width.at_floor(),
        "fieldWidthSpread": pm.detection.as_ref().map(|d| n_or_null(d.envelope.spread_px)),
        "fieldWidthEnvelope": pm.detection.as_ref().map(|d| n_or_null(d.envelope.fwhm_px)),
        "snrGain": pm.snr_gain.map(|g| n_or_null(g * s)), "snrMissing": pm.snr_missing,
        "snrMedian": pm.frame.snr_median.map(n_or_null), "snrNoise": pm.snr_noise.as_str(),
        "shotSpread": pm.shot_spread, "hfrMedian": pm.frame.hfr_median.map(n_or_null),
        "timings": {"backgroundMs": pm.timings.background_ms, "noiseMs": pm.timings.noise_ms,
                    "detectMs": pm.timings.detect_ms, "fitMs": pm.timings.fit_ms},
        "detected": pm.detected.len(),
        "product": product,
        "options": options, "conversion": "none", "domain": domain,
        "nonFinite": non_finite, "wallMs": wall,
    }));
    out.flush().map_err(|e| format!("flush: {e}"))
}

fn cmd_debayer(mut it: impl Iterator<Item = String>) {
    let input = PathBuf::from(
        it.next()
            .unwrap_or_else(|| die("debayer: <mosaic> missing")),
    );
    let (mut pat, mut out) = (String::from("RGGB"), PathBuf::new());
    while let Some(k) = it.next() {
        let v = it
            .next()
            .unwrap_or_else(|| die(format!("{k}: value missing")));
        match k.as_str() {
            "--pattern" => pat = v,
            "--out" => out = PathBuf::from(v),
            o => die(format!("unknown flag {o}")),
        }
    }
    let (mut planes, w, h) = read_planes(&input);
    normalise(&mut planes, Domain::Auto);
    if planes.len() != 1 {
        die("debayer needs a one-plane mosaic")
    }
    let rgb: Vec<f32> = debayer(&planes[0], w, h, pattern(&pat)).concat();
    write_fits_f32(&out, w, h, 3, &rgb, &[])
        .unwrap_or_else(|e| die(format!("{}: {e}", out.display())));
}

fn cmd_dump_fits(mut it: impl Iterator<Item = String>) {
    let input = PathBuf::from(
        it.next()
            .unwrap_or_else(|| die("dump-fits: <artifact> missing")),
    );
    let out = match (it.next().as_deref(), it.next()) {
        (Some("--out"), Some(p)) => PathBuf::from(p),
        _ => die("dump-fits <artifact> --out <file.jsonl>"),
    };
    let fits = fits_artifact::read_fits(&input)
        .unwrap_or_else(|e| die(format!("{}: {e}", input.display())));
    let mut w = BufWriter::new(
        std::fs::File::create(&out).unwrap_or_else(|e| die(format!("{}: {e}", out.display()))),
    );
    for f in &fits {
        writeln!(w, "{}", fit_line("artifact", 0, None, f)).unwrap_or_else(|e| die(e))
    }
    w.flush().unwrap_or_else(|e| die(e));
}

fn cmd_warp(mut it: impl Iterator<Item = String>) {
    let input = PathBuf::from(it.next().unwrap_or_else(|| die("warp: <image> missing")));
    let (mut dx, mut dy, mut rot, mut out) = (0.0f64, 0.0f64, 0.0f64, PathBuf::new());
    while let Some(k) = it.next() {
        let v = it
            .next()
            .unwrap_or_else(|| die(format!("{k}: value missing")));
        match k.as_str() {
            "--shift" => {
                let p: Vec<f64> = v
                    .split(',')
                    .map(|t| t.parse().unwrap_or_else(|_| die("--shift dx,dy")))
                    .collect();
                if p.len() != 2 {
                    die("--shift dx,dy")
                }
                dx = p[0];
                dy = p[1];
            }
            "--rotate" => rot = v.parse().unwrap_or_else(|_| die("--rotate deg")),
            "--out" => out = PathBuf::from(v),
            o => die(format!("unknown flag {o}")),
        }
    }
    if out.as_os_str().is_empty() {
        die("warp: --out missing")
    }
    let (planes, w, h) = read_planes(&input);
    let warped = warp_affine(&planes, w, h, dx, dy, rot);
    write_fits_f32(&out, w, h, warped.len(), &warped.concat(), &[])
        .unwrap_or_else(|e| die(format!("{}: {e}", out.display())));
}

/// Output pixel p reads the source at R(θ)(p − c) + c + (dx, dy), through the
/// production resampler with the registration defaults (B-spline, clamping 0.30).
/// A bare `Linear` is an inverse map applied as-is (`geometry::pixel_map`), pixel
/// centres sit on integer coordinates (`resample::sample_at` covers `[0, w−1]`).
fn warp_affine(
    planes: &[Vec<f32>],
    w: usize,
    h: usize,
    dx: f64,
    dy: f64,
    rot_deg: f64,
) -> Vec<Vec<f32>> {
    let (c, s) = (rot_deg.to_radians().cos(), rot_deg.to_radians().sin());
    let (cx, cy) = ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
    let m = [
        [c, -s, cx - c * cx + s * cy + dx],
        [s, c, cy - s * cx - c * cy + dy],
        [0.0, 0.0, 1.0],
    ];
    let map = Linear {
        kind: LinearKind::Affine,
        m,
    };
    let reg = RegistrationConfig::default();
    planes
        .iter()
        .map(|p| {
            let mut o = vec![0.0f32; w * h];
            warp_rows(
                &Plane::full(p, w, h),
                &map,
                w,
                0,
                h,
                reg.interpolation,
                reg.clamping_threshold,
                &mut o,
            );
            o
        })
        .collect()
}

fn main() {
    if std::env::var("RUST_LOG").is_ok() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .init();
    }
    let mut it = std::env::args().skip(1);
    match it.next().as_deref() {
        Some("measure") => cmd_measure(parse_measure(it)).unwrap_or_else(|e| die(e)),
        Some("debayer") => cmd_debayer(it),
        Some("dump-fits") => cmd_dump_fits(it),
        Some("warp") => cmd_warp(it),
        _ => die("usage: star_probe measure|debayer|warp|dump-fits …"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gaussian_field(w: usize, h: usize, nan_border: usize, stars: bool) -> Vec<f32> {
        let mut d = vec![0.05f32; w * h];
        if stars {
            for k in 0..40 {
                let (cx, cy) = (30.0 + (k % 8) as f32 * 25.0, 30.0 + (k / 8) as f32 * 25.0);
                for y in 0..h {
                    for x in 0..w {
                        let r2 = (x as f32 - cx).powi(2) + (y as f32 - cy).powi(2);
                        d[y * w + x] += 0.4 * (-r2 / (2.0 * 1.5f32.powi(2))).exp();
                    }
                }
            }
        }
        // deterministic pseudo-noise (the MRS noise estimate needs some)
        for (i, v) in d.iter_mut().enumerate() {
            *v += 0.002 * (((i as u64).wrapping_mul(2_654_435_761) % 1000) as f32 / 1000.0 - 0.5);
        }
        for y in 0..h {
            for x in 0..w {
                if x < nan_border || y < nan_border {
                    d[y * w + x] = f32::NAN
                }
            }
        }
        d
    }

    fn args(img: PathBuf, out: PathBuf, paths: &[&str]) -> MeasureArgs {
        MeasureArgs {
            image: img,
            out,
            config: None,
            plane: None,
            debayer: None,
            rect: None,
            paths: paths.iter().map(|p| p.to_string()).collect(),
            betas: vec![4.0],
            domain: Domain::Auto,
            diagnostics: false,
            seeds: None,
            seed_threshold: None,
            pixel_noise: None,
            psf_beta: None,
            clip: None,
            budget: None,
            psf: None,
            fit_stars: None,
            dump_ratios: false,
        }
    }

    fn read_lines(out: &Path) -> Vec<Value> {
        std::fs::read_to_string(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn run_paths(dir: &Path, data: Vec<f32>, w: usize, h: usize, paths: &[&str]) -> Vec<Value> {
        let img = dir.join("p.fits");
        write_fits_f32(&img, w, h, 1, &data, &[]).unwrap();
        let out = dir.join("p.jsonl");
        cmd_measure(args(img, out.clone(), paths)).unwrap();
        read_lines(&out)
    }

    fn run(dir: &Path, data: Vec<f32>, w: usize, h: usize) -> Vec<Value> {
        run_paths(dir, data, w, h, &["measure", "betas", "register"])
    }

    #[test]
    fn nan_border_runs_every_path() {
        let d = tempfile::tempdir().unwrap();
        let lines = run(d.path(), gaussian_field(240, 240, 6, true), 240, 240);
        let frames: Vec<&Value> = lines.iter().filter(|v| v["kind"] == "frame").collect();
        assert_eq!(frames.len(), 3);
        // 6 full rows of 240 + 6 columns of the remaining 234 rows.
        assert!(frames.iter().all(|f| f["nonFinite"] == 6 * 240 + 6 * 234));
        for path in ["measure", "betas", "register"] {
            let f = frames.iter().find(|f| f["path"] == path).unwrap();
            assert!(f["fits"].as_u64().unwrap() > 0, "{f}");
            assert!(
                lines
                    .iter()
                    .any(|v| v["kind"] == "star" && v["path"] == path),
                "no star line for {path}"
            );
        }
    }

    /// Plan A Task 13b: `--paths population` is a probe / lab path (the stacking Measure runs `Mode::All`) - its lines carry
    /// `path: "population"`, the frame line reports the populations, `notSelected` counts the seeds they left out and
    /// the F-0 identity holds. It replaces `measure`.
    #[test]
    fn the_population_path_reports_the_populations_and_replaces_measure() {
        let d = tempfile::tempdir().unwrap();
        let lines = run_paths(
            d.path(),
            gaussian_field(240, 240, 6, true),
            240,
            240,
            &["population"],
        );
        let frame = lines
            .iter()
            .find(|v| v["kind"] == "frame" && v["path"] == "population")
            .expect("a population frame line");
        assert!(lines.iter().all(|v| v["path"] != "measure"));
        let p = &frame["population"];
        let fitted = p["fitted"].as_u64().unwrap();
        assert!(
            fitted > 0 && fitted <= frame["seeds"].as_u64().unwrap(),
            "{frame}"
        );
        assert!(p["statisticAccepted"].as_u64().unwrap() <= p["statistic"].as_u64().unwrap());
        assert_eq!(
            frame["fits"].as_u64().unwrap()
                + frame["refused"]["total"].as_u64().unwrap()
                + frame["refused"]["notSelected"].as_u64().unwrap(),
            frame["seeds"].as_u64().unwrap()
        );
        assert!(lines
            .iter()
            .any(|v| v["kind"] == "star" && v["path"] == "population"));
    }

    /// Plan A Task 4 (F-0, ruling F13): with `--diagnostics` every refused seed has a `refused` line, the frame line
    /// counts them, and fits + refused = seeds.
    #[test]
    fn refused_lines_plus_fits_equal_seeds() {
        let d = tempfile::tempdir().unwrap();
        let mut a = args(
            d.path().join("p.fits"),
            d.path().join("p.jsonl"),
            &["measure"],
        );
        a.diagnostics = true;
        // The NaN border alone refuses no seed (the detector seeds nothing there), so one more star sits 7 px from the
        // unmasked right edge: it is seeded, and its width-start box (radius ⌈2·F_field⌉ = 8 px for the field's FWHM
        // ≈ 3.7; the nominal stamp before Plan A Task 6b) leaves the image (outsideImage).
        let mut data = gaussian_field(240, 240, 6, true);
        for y in 0..240 {
            for x in 0..240 {
                let r2 = (x as f32 - 232.0).powi(2) + (y as f32 - 160.0).powi(2);
                data[y * 240 + x] += 0.4 * (-r2 / (2.0 * 1.5f32.powi(2))).exp();
            }
        }
        write_fits_f32(&a.image, 240, 240, 1, &data, &[]).unwrap();
        cmd_measure(a).unwrap();
        let lines = read_lines(&d.path().join("p.jsonl"));
        let frame = lines
            .iter()
            .find(|v| v["kind"] == "frame" && v["path"] == "measure")
            .unwrap();
        let refused = lines.iter().filter(|v| v["kind"] == "refused").count();
        assert!(
            lines
                .iter()
                .any(|v| v["kind"] == "refused" && v["reason"] == "outsideImage"),
            "the edge star is refused outsideImage: {frame}"
        );
        assert_eq!(refused as u64, frame["refused"]["total"].as_u64().unwrap());
        assert_eq!(
            frame["fits"].as_u64().unwrap() + refused as u64,
            frame["seeds"].as_u64().unwrap()
        );
        assert!(lines
            .iter()
            .filter(|v| v["kind"] == "refused")
            .all(|v| v["reason"].is_string()
                && v["regionRadius"].is_u64()
                && v["x"].is_number()
                && v["y"].is_number()));
        let edge = lines
            .iter()
            .find(|v| v["kind"] == "refused" && v["reason"] == "outsideImage")
            .unwrap();
        assert!(
            (edge["x"].as_f64().unwrap() - 232.0).abs() < 2.0
                && (edge["y"].as_f64().unwrap() - 160.0).abs() < 2.0,
            "the refused line names the seed's native position: {edge}"
        );
    }

    /// Plan A Task 6b round 1 (ruling R3): the betas arm starts from the measure path's field width, so at the measure's
    /// own β it fits exactly the measure's stars. The image holds 60 narrow (FWHM 3, amplitude 0.4) and 60 broad (FWHM
    /// 7, amplitude 0.3) Moffat-4 stars: the matched filter's width (its first pass ranks by 3-px flux, where a broad
    /// star leads) and `field_width_of` the seeds (ranked by peak, where a narrow one leads) differ by ~4 px.
    #[test]
    fn the_betas_arm_starts_from_the_measure_width() {
        let (w, h) = (640usize, 480usize);
        let mut data = vec![0.05f32; w * h];
        for i in 0..120usize {
            let narrow = (i + i / 12) % 2 == 0;
            let (alpha, amp) = if narrow {
                (3.449f32, 0.4f32)
            } else {
                (8.048, 0.3)
            };
            let (cx, cy) = (30.0 + (i % 12) as f32 * 50.0, 30.0 + (i / 12) as f32 * 45.0);
            for y in 0..h {
                for x in 0..w {
                    let q = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)) / (alpha * alpha);
                    data[y * w + x] += amp * (1.0 + q).powi(-4);
                }
            }
        }
        for (i, v) in data.iter_mut().enumerate() {
            *v += 0.002 * (((i as u64).wrapping_mul(2_654_435_761) % 1000) as f32 / 1000.0 - 0.5);
        }
        let d = tempfile::tempdir().unwrap();
        let img = d.path().join("p.fits");
        write_fits_f32(&img, w, h, 1, &data, &[]).unwrap();
        // F-4 (Plan A Task 9): the measure's β is continuous, so it is no longer one of the ladder's — the measure
        // runs first, and the betas arm then runs at exactly the β it read (the JSON number round-trips the f64).
        let first = d.path().join("m.jsonl");
        cmd_measure(args(img.clone(), first.clone(), &["measure"])).unwrap();
        let measure_beta = |lines: &[Value]| -> f64 {
            lines
                .iter()
                .find(|v| v["kind"] == "frame" && v["path"] == "measure")
                .unwrap()["beta"]
                .as_f64()
                .unwrap()
        };
        let beta = measure_beta(&read_lines(&first));
        let out = d.path().join("p.jsonl");
        let mut a = args(img, out.clone(), &["measure", "betas"]);
        a.betas = vec![2.5, beta, 10.0];
        cmd_measure(a).unwrap();
        let lines = read_lines(&out);
        assert_eq!(measure_beta(&lines), beta, "the measure is deterministic");
        let stars = |path: &str| -> Vec<String> {
            let mut s: Vec<String> = lines
                .iter()
                .filter(|v| {
                    v["kind"] == "star" && v["path"] == path && v["beta"].as_f64() == Some(beta)
                })
                .map(|v| format!("{} {} {} {}", v["x"], v["y"], v["fwhm"], v["background"]))
                .collect();
            s.sort();
            s
        };
        let (m, b) = (stars("measure"), stars("betas"));
        assert!(m.len() > 100, "{}", m.len());
        assert_eq!(
            m, b,
            "the betas arm at the measure's β fits the measure's stars exactly"
        );
    }

    /// Plan A Task 6a (ruling F3): every frame line names the EFFECTIVE seed source and threshold the measure path ran,
    /// so the lab's expected set reads the threshold the probe actually used; `--seeds peak` writes no seedThreshold.
    #[test]
    fn the_frame_line_names_the_effective_seed_source_and_threshold() {
        assert_eq!(SeedChoice::parse("matched"), SeedChoice::Matched);
        assert_eq!(SeedChoice::parse("peak"), SeedChoice::Peak);
        assert_eq!(SeedChoice::parse("structure"), SeedChoice::Structure);
        let d = tempfile::tempdir().unwrap();
        let img = d.path().join("p.fits");
        write_fits_f32(&img, 240, 240, 1, &gaussian_field(240, 240, 0, true), &[]).unwrap();
        for (choice, source, k) in [
            (SeedChoice::Matched, "matchedFilter", json!(5.5)),
            (SeedChoice::Peak, "peak", Value::Null),
            (SeedChoice::Structure, "structure", Value::Null),
        ] {
            let out = d.path().join(format!("{source}.jsonl"));
            let mut a = args(img.clone(), out.clone(), &["measure", "betas", "register"]);
            a.seeds = Some(choice);
            a.seed_threshold = Some(5.5);
            cmd_measure(a).unwrap();
            let lines = read_lines(&out);
            let frames: Vec<&Value> = lines.iter().filter(|v| v["kind"] == "frame").collect();
            assert_eq!(frames.len(), 3);
            for f in &frames {
                assert_eq!(f["options"]["measure"]["seedSource"], source, "{f}");
                assert_eq!(f["options"]["measure"]["seedThreshold"], k, "{f}");
            }
            let m = frames.iter().find(|f| f["path"] == "measure").unwrap();
            assert!(m["fits"].as_u64().unwrap() > 0, "{source}: {m}");
        }
        // No flags: the config's own source stands; `--seeds` overrides it.
        let structure = MeasureOptions {
            seed_source: SeedSource::Structure,
            ..MeasureOptions::default()
        };
        let (o, s) = effective_seeds(structure, None, None);
        assert_eq!(
            (o.seed_source, s),
            (SeedSource::Structure, SeedOverride::Fast),
            "the config's source stands"
        );
        let (o, _) = effective_seeds(structure, Some(SeedChoice::Peak), None);
        assert_eq!(
            o.seed_source,
            SeedSource::Peak,
            "peak is forced over a structure config"
        );
        assert_eq!(
            MeasureOptions::default().seed_source,
            SeedSource::MatchedFilter
        );
        assert_eq!(
            MeasureOptions::default().seed_threshold,
            athenaeum_core::star_measure::seeds::DEFAULT_THRESHOLD
        );
    }

    #[test]
    fn domain_auto_is_the_p4_rule_on_both_sides_of_1_5() {
        let mut p = vec![vec![0.2f32, 1.5]];
        assert_eq!(normalise(&mut p, Domain::Auto), "none");
        assert_eq!(p[0], vec![0.2f32, 1.5]);
        let mut p = vec![vec![0.2f32, 1.5001]];
        assert_eq!(normalise(&mut p, Domain::Auto), "adu/65535");
        assert_eq!(p[0], vec![0.2f32 / ADU_SCALE, 1.5001f32 / ADU_SCALE]);
    }

    #[test]
    fn domain_unit_keeps_a_calibrated_plane_above_1_5() {
        let mut p = vec![vec![0.05f32, 1.645], vec![0.04f32, 0.9]];
        assert_eq!(normalise(&mut p, Domain::Unit), "none");
        assert_eq!(p, vec![vec![0.05f32, 1.645], vec![0.04f32, 0.9]]);
    }

    #[test]
    fn domain_adu_always_divides() {
        let mut p = vec![vec![0.5f32, 1.0]];
        assert_eq!(normalise(&mut p, Domain::Adu), "adu/65535");
        assert_eq!(p[0], vec![0.5f32 / ADU_SCALE, 1.0f32 / ADU_SCALE]);
    }

    #[test]
    fn domain_parses_and_the_frame_lines_record_it() {
        assert_eq!(Domain::parse("auto"), Domain::Auto);
        assert_eq!(Domain::parse("unit"), Domain::Unit);
        assert_eq!(Domain::parse("adu"), Domain::Adu);
        let d = tempfile::tempdir().unwrap();
        // a unit-domain field with one pixel at 1.645 (a flat-lifted saturated core)
        let mut data = gaussian_field(240, 240, 0, true);
        data[5 * 240 + 5] = 1.645;
        let img = d.path().join("u.fits");
        write_fits_f32(&img, 240, 240, 1, &data, &[]).unwrap();
        let out = d.path().join("u.jsonl");
        let mut a = args(img, out.clone(), &["measure", "betas", "register"]);
        a.domain = Domain::Unit;
        cmd_measure(a).unwrap();
        let lines = read_lines(&out);
        let frames: Vec<&Value> = lines.iter().filter(|v| v["kind"] == "frame").collect();
        assert_eq!(frames.len(), 3);
        for f in &frames {
            assert_eq!(f["domain"], "unit", "{f}");
            assert_eq!(f["conversion"], "none", "{f}");
        }
        let m = frames.iter().find(|f| f["path"] == "measure").unwrap();
        assert!(m["fits"].as_u64().unwrap() > 0, "{m}");
    }

    #[test]
    fn analysis_refuses_a_unit_domain_file() {
        // the 1.5 boundary, both sides
        assert!(!analysis_domain_ok(1.5));
        assert!(analysis_domain_ok(1.5001));
        assert!(!analysis_domain_ok(1.0));
        assert!(!analysis_domain_ok(f32::MIN));
        assert!(analysis_domain_ok(40000.0));
    }

    #[test]
    fn starless_plane_writes_frame_lines_only() {
        let d = tempfile::tempdir().unwrap();
        let lines = run(d.path(), gaussian_field(128, 128, 0, false), 128, 128);
        assert!(lines.iter().all(|v| v["kind"] == "frame"));
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn starless_raw_domain_plane_writes_the_analysis_frame_line() {
        let d = tempfile::tempdir().unwrap();
        let data: Vec<f32> = gaussian_field(128, 128, 0, false)
            .iter()
            .map(|v| (v * 65535.0).round())
            .collect();
        let lines = run_paths(d.path(), data, 128, 128, &["analysis"]);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0]["path"], "analysis");
        assert_eq!(lines[0]["fits"], 0);
        assert_eq!(lines[0]["conversion"], "none");
    }

    /// The lines of a probe output without the analysis path's two clock fields: the
    /// frame line's `wallMs` and the production `FrameAnalysis.analyzed_at` stamp it
    /// carries in `frame`. Two runs of the same path differ in those two only.
    fn without_clocks(out: &Path) -> Vec<String> {
        read_lines(out)
            .into_iter()
            .map(|mut v| {
                if let Some(o) = v.as_object_mut() {
                    o.remove("wallMs");
                    if let Some(f) = o.get_mut("frame").and_then(Value::as_object_mut) {
                        f.remove("analyzed_at");
                    }
                }
                v.to_string()
            })
            .collect()
    }

    /// Ruling F2 (Plan A, Task 1b): `legacy-analysis` is the `analysis` path under its
    /// lasting name (the same lines, `path: "analysis"`), and an unknown `--paths` name
    /// is refused before any output file exists. The outputs are compared without their
    /// clock fields (`without_clocks`): a byte compare fails on two runs of one path.
    #[test]
    fn legacy_analysis_is_an_alias_of_analysis_and_an_unknown_path_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let data: Vec<f32> = gaussian_field(128, 128, 0, true)
            .iter()
            .map(|v| (v * 65535.0).round())
            .collect();
        let img = d.path().join("p.fits");
        write_fits_f32(&img, 128, 128, 1, &data, &[]).unwrap();
        let a = d.path().join("a.jsonl");
        cmd_measure(args(img.clone(), a.clone(), &["analysis"])).unwrap();
        let b = d.path().join("b.jsonl");
        cmd_measure(args(img.clone(), b.clone(), &["legacy-analysis"])).unwrap();
        assert_eq!(without_clocks(&a), without_clocks(&b));
        let lines = read_lines(&b);
        assert!(
            lines.iter().filter(|v| v["kind"] == "star").count() > 0,
            "the control is not empty"
        );
        assert!(lines.iter().all(|v| v["path"] == "analysis"));
        let c = d.path().join("c.jsonl");
        let err = cmd_measure(args(img, c.clone(), &["bogus"])).unwrap_err();
        assert!(err.contains("unknown path: bogus"), "{err}");
        assert!(!c.exists(), "no output file for a refused path");
    }

    #[test]
    fn a_half_pixel_shift_moves_a_star_and_keeps_its_flux() {
        let (w, h) = (96usize, 96usize);
        let mut d = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let r2 = (x as f32 - 48.0).powi(2) + (y as f32 - 48.0).powi(2);
                d[y * w + x] = (-r2 / (2.0 * 2.0f32.powi(2))).exp();
            }
        }
        let o = warp_affine(&[d.clone()], w, h, 0.5, 0.0, 0.0).remove(0);
        // the resampler writes NaN where the source is out of range: here the last column only
        assert_eq!(o.iter().filter(|v| !v.is_finite()).count(), h);
        let s0: f64 = d.iter().map(|&v| v as f64).sum();
        let s1: f64 = o.iter().filter(|v| v.is_finite()).map(|&v| v as f64).sum();
        assert!((s1 / s0 - 1.0).abs() < 1e-3, "flux {s1} vs {s0}");
        let cx: f64 = o
            .iter()
            .enumerate()
            .filter(|(_, v)| v.is_finite())
            .map(|(i, &v)| (i % w) as f64 * v as f64)
            .sum::<f64>()
            / s1;
        assert!((cx - 47.5).abs() < 0.02, "centroid {cx}");
    }

    fn amode_args(img: PathBuf, out: PathBuf, budget: usize) -> MeasureArgs {
        MeasureArgs {
            domain: Domain::Adu,
            diagnostics: true,
            clip: Some(65535.0),
            budget: Some(budget),
            ..args(img, out, &["analysis-mode"])
        }
    }

    fn amode_frame(
        dir: &Path,
        a: impl FnOnce(PathBuf, PathBuf) -> MeasureArgs,
    ) -> (Vec<Value>, Value) {
        let data: Vec<f32> = gaussian_field(256, 256, 0, true)
            .iter()
            .map(|v| (v * 65535.0).round())
            .collect();
        let img = dir.join("p.fits");
        write_fits_f32(&img, 256, 256, 1, &data, &[]).unwrap();
        let out = dir.join("p.amode.jsonl");
        cmd_measure(a(img, out.clone())).unwrap();
        let lines = read_lines(&out);
        let f = lines
            .iter()
            .find(|v| v["kind"] == "frame")
            .cloned()
            .unwrap();
        (lines, f)
    }

    /// Task 12 (R12-7, R12-1). On 256² with 40 stars in 12 cells and budget 16 (quota 2, target 32), there are 8 seeds not
    /// selected and about 16 trimmed. No seed line is written (the lab merges every file's seed lines into the measure
    /// path's). The frame line carries the counts, the report, the effective seed source and Moffat 4.
    #[test]
    fn analysis_mode_writes_the_budget_report_and_no_seed_line() {
        let d = tempfile::tempdir().unwrap();
        let (lines, f) = amode_frame(d.path(), |i, o| amode_args(i, o, 16));
        let n = |v: &Value| v.as_u64().unwrap();
        let (b, r) = (&f["budget"], &f["refused"]);
        assert!(lines
            .iter()
            .all(|v| v["kind"] != "seed" && v["path"] == "analysis-mode"));
        assert_eq!(lines.iter().filter(|v| v["kind"] == "frame").count(), 1);
        assert_eq!(
            n(&f["fits"]) + n(&r["total"]) + n(&r["notSelected"]) + n(&r["trimmed"]),
            n(&f["seeds"])
        );
        assert_eq!(
            (n(&r["notSelected"]), n(&r["trimmed"])),
            (n(&b["notSelected"]), n(&b["trimmed"]))
        );
        assert_eq!(n(&b["candidates"]) + n(&b["notSelected"]), n(&f["seeds"]));
        assert_eq!(n(&b["kept"]), n(&f["fits"]));
        assert!(
            n(&f["fits"]) <= 16 && n(&b["notSelected"]) > 0 && n(&b["trimmed"]) > 0,
            "{f}"
        );
        assert_eq!(b["cellsCovered"], b["cellsWithStars"]);
        assert_eq!(
            lines.iter().filter(|v| v["kind"] == "refused").count() as u64,
            n(&f["seeds"]) - n(&f["fits"])
        );
        assert_eq!(
            lines
                .iter()
                .filter(|v| v["reason"] == "notSelected")
                .count() as u64,
            n(&b["notSelected"])
        );
        let m = &f["options"]["measure"];
        assert_eq!(
            (m["seedSource"].as_str(), m["seedThreshold"].as_f64()),
            (Some("matchedFilter"), Some(6.0))
        );
        assert_eq!(
            (m["psfModel"].as_str(), m["budget"].as_u64()),
            (Some("moffat4"), Some(16))
        );
        assert_eq!(
            (f["beta"].as_f64(), f["betaSource"].as_str()),
            (Some(4.0), Some("fixed"))
        );
        assert!(
            f.get("betaOwn").is_none(),
            "R12-1: no own beta under Moffat 4"
        );
        assert_eq!(
            (f["domain"].as_str(), f["conversion"].as_str()),
            (Some("adu"), Some("none"))
        );
    }

    #[test]
    fn analysis_mode_with_psf_auto_reports_the_fitted_beta() {
        let d = tempfile::tempdir().unwrap();
        let (_, f) = amode_frame(d.path(), |i, o| MeasureArgs {
            psf: Some(PsfModel::Auto),
            ..amode_args(i, o, 16)
        });
        assert_eq!(f["options"]["measure"]["psfModel"], "auto");
        assert!(
            matches!(f["betaSource"].as_str(), Some("fitted" | "fallback")),
            "{f}"
        );
        assert!(f["beta"].as_f64().unwrap().is_finite());
    }

    /// R12-7: the analysis-mode path runs alone, on the raw domain, with the matched filter. Each refusal is an `Err`
    /// before any output file exists (the input's scale is `read_raw_frame`'s, C2).
    #[test]
    fn analysis_mode_is_refused_before_any_output_when_its_input_is_not_the_analysis_tabs() {
        let d = tempfile::tempdir().unwrap();
        let field = gaussian_field(128, 128, 0, true);
        let raw: Vec<f32> = field.iter().map(|v| (v * 65535.0).round()).collect();
        let (ri, out) = (d.path().join("r.fits"), d.path().join("o.jsonl"));
        write_fits_f32(&ri, 128, 128, 1, &raw, &[]).unwrap();
        let a = || amode_args(ri.clone(), out.clone(), 16);
        for (args, want) in [
            (
                MeasureArgs {
                    paths: vec!["analysis-mode".into(), "measure".into()],
                    ..a()
                },
                "runs alone",
            ),
            (
                MeasureArgs {
                    domain: Domain::Unit,
                    ..a()
                },
                "raw",
            ),
            (
                MeasureArgs {
                    seeds: Some(SeedChoice::Peak),
                    ..a()
                },
                "matched filter",
            ),
        ] {
            let err = cmd_measure(args).unwrap_err();
            assert!(err.contains(want), "{err}");
            assert!(!out.exists(), "no output file for a refused invocation");
        }
    }

    /// I-1 (Task 12 review): the analysis-mode frame line's `snrGain` is in the measure path's unit, e- per lab unit
    /// (`gain * 65535`), not e- per ADU. New test (red at a7ac7659: the line carried the per-ADU gain).
    #[test]
    fn analysis_mode_snr_gain_is_per_lab_unit_like_the_measure_path() {
        let d = tempfile::tempdir().unwrap();
        let data: Vec<f32> = gaussian_field(256, 256, 0, true)
            .iter()
            .map(|v| (v * 65535.0).round())
            .collect();
        let img = d.path().join("g.fits");
        let cards = [athenaeum_core::fits_writer::Card::new(
            "EGAIN",
            athenaeum_core::fits_writer::CardValue::Real(0.5),
        )
        .unwrap()];
        write_fits_f32(&img, 256, 256, 1, &data, &cards).unwrap();
        let out = d.path().join("g.jsonl");
        cmd_measure(amode_args(img, out.clone(), 16)).unwrap();
        let lines = read_lines(&out);
        let f = lines.iter().find(|v| v["kind"] == "frame").unwrap();
        assert_eq!(
            f["options"]["measure"]["gainEPerAdu"].as_f64(),
            Some(0.5),
            "{f}"
        );
        let g = f["snrGain"].as_f64().expect("snrGain with an EGAIN header");
        assert!((g - 0.5 * 65535.0).abs() < 1e-6, "snrGain {g}");
    }

    /// M-3: the other four analysis-mode refusals, each an `Err` before any output file. New tests over existing
    /// behaviour (green at a7ac7659).
    #[test]
    fn analysis_mode_refuses_rect_debayer_config_and_psf_beta() {
        let d = tempfile::tempdir().unwrap();
        let raw: Vec<f32> = gaussian_field(128, 128, 0, true)
            .iter()
            .map(|v| (v * 65535.0).round())
            .collect();
        let (img, out) = (d.path().join("r.fits"), d.path().join("o.jsonl"));
        write_fits_f32(&img, 128, 128, 1, &raw, &[]).unwrap();
        let a = || amode_args(img.clone(), out.clone(), 16);
        for args in [
            MeasureArgs {
                rect: Some((0, 0, 64, 64)),
                ..a()
            },
            MeasureArgs {
                debayer: Some("RGGB".into()),
                ..a()
            },
            MeasureArgs {
                config: Some(d.path().join("c.json")),
                ..a()
            },
            MeasureArgs {
                psf_beta: Some(3.0),
                ..a()
            },
        ] {
            let err = cmd_measure(args).unwrap_err();
            assert!(
                err.contains("no --rect, --debayer, --config or --psf-beta"),
                "{err}"
            );
            assert!(!out.exists(), "no output file for a refused invocation");
        }
    }
}
