// AUTO-GENERATED from Rust by athenaeum-core/src/ts_export.rs — do not edit.
// Regenerate: TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract

import type { CanonicalGrid, ExportReadiness, FlatNormMode, LightCalParams, PathSetting, ProposalRotation } from './models';

export type Interpolation = "nearest" | "bilinear" | "bicubicSpline" | "bicubicBSpline" | "lanczos3" | "lanczos4" | "mitchellNetravali";

export type SeedPrefilter = "none" | "median3";

export type SeedDetector = "peak" | "structure";

export type SeedSource = "matchedFilter" | "peak" | "structure";

export type PsfModel = "auto" | "moffat4";

export type WeightMode = "psfSignalWeight" | "psfSnr" | "noise" | "formula" | "exposure" | "keyword" | "none";

export type FormulaWeights = { 
/**
 * Points for the sharpest FWHM in the group (the term scales down to 0 at the widest FWHM).
 */
fwhm: number, 
/**
 * Points for the roundest stars in the group (0 at the most eccentric).
 */
eccentricity: number, 
/**
 * Points for the highest SNR weight in the group (0 at the lowest).
 */
snr: number, 
/**
 * Points for the most fitted stars in the group (0 at the fewest).
 */
stars: number, 
/**
 * Points every frame earns regardless of its measurements.
 */
pedestal: number, };

export type SelectionConfig = { 
/**
 * Frames below this fraction of the group's maximum weight are excluded.
 */
minWeightFraction: number, 
/**
 * Exclude a frame whose mean FWHM over its channels exceeds this, in pixels; `None` = no limit.
 */
maxFwhmPx: number | null, 
/**
 * Exclude a frame whose mean eccentricity over its channels exceeds this (0 = round, towards 1 = elongated);
 * `None` = no limit.
 */
maxEccentricity: number | null, 
/**
 * Exclude a frame whose lowest per-channel fitted-star count is below this; `None` = no limit.
 */
minStars: number | null, 
/**
 * A frame whose registration failed is dropped from its group (spec
 * §9.2). Read by the run orchestration, not by [`select_frames`] —
 * this stage has no registration outcome to check, so the field is
 * carried here only so a stored config round-trips.
 */
excludeOnRegistrationFailure: boolean, };

export type RegistrationGeometry = "coRegistered" | "native";

export type ModelChoice = "auto" | "similarity" | "affine" | "homography";

export type DistortionChoice = "off" | "polynomial2" | "polynomial3" | "polynomial4" | "tps" | "auto";

export type DetectionConfig = { 
/**
 * Smallest peak signal-to-noise ratio (unitless) a detected star must reach to be kept.
 */
minSnr: number, 
/**
 * Largest eccentricity a detected star may have to be kept: 0 is round, values toward 1 are elongated
 * (unitless).
 */
maxEccentricity: number, };

export type RegistrationConfig = { 
/**
 * Whether the whole set shares one reference geometry (co-registered, the default) or each group gets
 * its own (native).
 *
 * M4b (ruling R-M4b-4): co-registered (the default — one reference and
 * one geometry for the whole set) or native (a reference and a
 * geometry per group). `#[serde(default)]` rides the struct-level
 * `default`, so every stored config written before M4b decodes as
 * co-registered and no `STACKING_CONFIG_VERSION` bump is needed.
 */
geometry: RegistrationGeometry, 
/**
 * The linear model fitted between a frame and the reference; auto picks it from the number of
 * correspondences.
 */
model: ModelChoice, 
/**
 * The non-linear distortion fitted on top of the linear model.
 */
distortion: DistortionChoice, 
/**
 * The thin-plate spline's regularization weight `λ` (M4c, ruling
 * R-M4c-5) — `0.0` is the interpolating spline, which lands on every
 * inlier exactly, and a larger value trades node fidelity for a
 * smoother surface that generalizes between them.
 *
 * Unit: px² of the NORMALIZED frame. The kernel is evaluated on
 * coordinates divided by the node cloud's own diagonal, so `|φ|` is
 * bounded by 0.184 whatever the sensor, and `λ` is the weight of the
 * `λ · wᵀw` penalty against displacements measured in pixels. The
 * useful range therefore depends on the node count (the kernel
 * block's eigenvalues grow with it): around 0.01 for a few dozen
 * nodes, roughly an order of magnitude higher at the 600-node cap.
 * Read only by [`DistortionChoice::Tps`]; ignored by every other
 * choice.
 *
 * **The shipped default is `0.5`** (ruling R-T7-1), measured rather
 * than guessed: at the 600-node cap on the acceptance set's real
 * 26 Mpx frames the hold-out rms was 0.145 / 0.203 px (mono / OSC)
 * at `λ = 0`, **0.099 / 0.156 px at `λ = 0.5`** and 0.102 / 0.165 px
 * at `λ = 2`. `λ = 0` interpolates the star-position noise — it
 * lands every inlier exactly and generalizes worst of the three — so
 * the interpolating spline is a deliberate choice now, not what a
 * default run gets.
 */
tpsSmoothing: number, 
/**
 * The local distortion loop (M4c, ruling R-M4c-7): after the first
 * map, up to `align::LOCAL_DISTORTION_ROUNDS` rounds of re-pairing
 * every subject star THROUGH the current map at a widening
 * tolerance, fitting a corrector homography on what is left and
 * refitting the distortion around it. Needs a distortion model to
 * refit, so it is a no-op with `distortion: off`.
 */
localDistortion: boolean, 
/**
 * The resampling kernel used when a frame is warped into the reference geometry.
 */
interpolation: Interpolation, 
/**
 * Deringing threshold of the sharpening kernels, unitless: when the outer taps' negative contribution
 * reaches this fraction of the inner taps' contribution, the interpolation falls back to a linear
 * estimate (default 0.30).
 */
clampingThreshold: number, 
/**
 * Most registration stars kept per frame, brightest first.
 */
maxStars: number, 
/**
 * Distance in pixels within which a star pair counts as an inlier of the RANSAC fit (the pairing radii
 * of the later rounds scale from it).
 */
ransacTolerancePx: number, 
/**
 * Most RANSAC iterations per fit.
 */
ransacMaxIterations: number, 
/**
 * Residual RMS in pixels above which a frame's registration is flagged; it fails the frame only when
 * `fail_on_max_rms` is set.
 */
maxRmsPx: number, 
/**
 * Whether a frame whose residual RMS exceeds `max_rms_px` fails registration (`true`) or only carries a
 * warning (`false`).
 */
failOnMaxRms: boolean, 
/**
 * The star detection cuts.
 */
detection: DetectionConfig, 
/**
 * Vestigial since perf tier A Task 6a: the registered artifact became
 * the run's REQUIRED per-frame output (Normalize/Integrate read it in
 * place of re-warping) and is now written unconditionally — this
 * field no longer gates that write. Kept in the schema/UI (the
 * `RegisterPanel` "Write registered frames" toggle) only because
 * removing it is a command-surface change out of that task's scope; it
 * has no observable effect any more.
 */
writeRegisteredFrames: boolean, };

export type OutputNormalization = "none" | "additive" | "additiveWithScaling" | "multiplicative" | "multiplicativeWithScaling";

export type RejectionNormalization = "none" | "scaleZeroOffset" | "equalizeFluxes" | "local";

export type ScaleEstimator = "bwmv" | "mad" | "avgDev";

export type LnScaleMode = "auto" | "fixed";

export type LnStarSelection = { 
/**
 * The least (peak − background) / σ_local a star must reach in both frames to be used (dimensionless, 5–200).
 */
minPeakSnr: number, 
/**
 * The most matched star pairs used, brightest first (a count of pairs, 20–5000).
 */
maxStars: number, 
/**
 * The fraction of a plane's maximum above which a star pair is left out: a pixel within max(2, FWHM) pixels of
 * either centroid above it removes the pair (dimensionless, 0.5–1.0).
 */
saturation: number, };

export type LnFluxEstimator = "psfSignal" | "commonAperture";

export type LnAperture = "fwhm2" | "fwhm3" | "growthCurve";

export type LocalNormalizationConfig = { 
/**
 * Whether local normalization is the OUTPUT normalization of the group (default off).
 */
enabled: boolean, 
/**
 * The local-normalization scale, in pixels, used when `scaleMode` is `fixed`; snapped to the supported list by
 * `resolve_config`. Read only by `ln::resolve_scale`.
 */
scale: number, 
/**
 * How the scale is chosen: derived from the geometry or taken from `scale`. `None` only on a document stored
 * before 2026-10 (field-level default, so the struct's own default cannot fill it): `resolve_config` turns it
 * into `Some` by the legacy rule L-9.
 */
scaleMode: LnScaleMode | null, 
/**
 * The automatic scale is the short side of the normalization geometry divided by this (2-8), snapped to the
 * supported list.
 */
gridSize: number, 
/**
 * How many of the group's best frames build the local-normalization reference (a count of frames).
 */
referenceFrames: number, 
/**
 * The star profile the local-normalization fits use.
 */
psfModel: PsfModel, 
/**
 * Whether the scale is a spline over the stride grid instead of one frame-global value (default off; only read
 * while `enabled`).
 */
localScale: boolean, 
/**
 * Which matched stars set the scale (spec §3).
 */
starSelection: LnStarSelection, 
/**
 * Which flux the scale ratios use (spec 2026-10-04 §4.1).
 */
fluxEstimator: LnFluxEstimator, 
/**
 * The common aperture's circle; read only under `commonAperture` (§4.1).
 */
aperture: LnAperture, };

export type NormalizationConfig = { 
/**
 * How each frame is matched to the reference for the final combine.
 */
output: OutputNormalization, 
/**
 * How each frame is matched to the reference for the rejection tests (default `local`).
 */
rejection: RejectionNormalization, 
/**
 * Informational at this layer: the estimator that produced the
 * stage-3 location/scale is `MeasureOptions::scale_estimator` — the
 * orchestrator (Plan 5) keeps the two equal.
 */
scaleEstimator: ScaleEstimator, 
/**
 * Local (small-scale) normalization settings (spec §5.2, M2).
 */
local: LocalNormalizationConfig, };

export type Combination = "average" | "median";

export type RejectionChoice = { "method": "auto" } | { "method": "none" } | { "method": "percentileClip", 
/**
 * Reject a sample this fraction of the median's magnitude below the median (dimensionless, e.g. 0.2 = 20 %).
 */
low: number, 
/**
 * Reject a sample this fraction of the median's magnitude above the median (dimensionless).
 */
high: number, } | { "method": "sigmaClip", 
/**
 * Standard deviations below the centre at which a sample is rejected.
 */
sigmaLow: number, 
/**
 * Standard deviations above the centre at which a sample is rejected.
 */
sigmaHigh: number, } | { "method": "winsorizedSigma", 
/**
 * Winsorized standard deviations below the centre at which a sample is rejected.
 */
sigmaLow: number, 
/**
 * Winsorized standard deviations above the centre at which a sample is rejected.
 */
sigmaHigh: number, } | { "method": "linearFitClip", 
/**
 * Deviation units below the fitted line at which a sample is rejected.
 */
sigmaLow: number, 
/**
 * Deviation units above the fitted line at which a sample is rejected.
 */
sigmaHigh: number, } | { "method": "minMax", 
/**
 * How many of the smallest samples to drop (a count).
 */
low: number, 
/**
 * How many of the largest samples to drop (a count).
 */
high: number, } | { "method": "esd", 
/**
 * The largest share of the samples that may be removed as outliers, in [0, 1].
 */
outliersFraction: number, 
/**
 * The significance level of each test (dimensionless, e.g. 0.05).
 */
alpha: number, 
/**
 * The factor that inflates the scale below the centre, so faint samples are rejected less eagerly.
 */
lowRelaxation: number, } | { "method": "rcr", 
/**
 * The expected number of samples at least as extreme as the candidate below which it is rejected (0.5 is
 * Chauvenet's criterion).
 */
limit: number, };

export type LargeScaleRejection = { 
/**
 * Whether large-scale rejection runs (default off); integration then runs twice.
 */
enabled: boolean, 
/**
 * The scale selector: structures thinner than roughly `2^layers / 2`
 * pixels are erased (see [`process_large_scale`]'s own doc for the
 * exact thresholds — 3 px at `2`, 5 px at `3`, 9 px at `4`). 1–6.
 */
protectedLayers: number, 
/**
 * Radius in pixels of the disc every surviving structure is grown by,
 * so the structure's own faint edges — which the per-pixel test never
 * reached — are covered too. 0–4.
 */
growth: number, };

export type IntegrationConfig = { 
/**
 * The master builder's `Combination` — one enum, one spelling
 * (`"average"`/`"median"`, snake_case per `combine.rs`'s own attribute).
 */
combination: Combination, 
/**
 * Which pixel-rejection method the master uses; `Auto` picks one by the group's frame count.
 */
rejection: RejectionChoice, 
/**
 * The weight floor (spec §6.2): a frame whose lowest per-channel
 * normalized weight falls below this is dropped from the group.
 */
minWeight: number, 
/**
 * Range rejection on the raw pixel value: reject `raw <= range_low`.
 */
rangeLow: number | null, 
/**
 * Reject `raw >= range_high`; `None` until the user turns it on.
 */
rangeHigh: number | null, 
/**
 * Also return per-pixel rejection maps (counts of rejected samples) with the master (default off).
 */
writeRejectionMaps: boolean, 
/**
 * Large-scale (structure-aware) pixel rejection, M4c (spec §6.2,
 * ruling R-M4c-4).
 */
largeScale: LargeScaleRejection, };

export type CalibratedLightOptions = { 
/**
 * Normalize the master flat by its own level before dividing (spec §2).
 */
flatNorm: boolean, 
/**
 * Which statistic computes that normalization constant. Plain
 * `#[serde(default)]` resolves through [`FlatNormMode::default`]
 * (`CentralThird`), so this tracks the enum's own default instead of
 * restating it here.
 */
flatNormMode: FlatNormMode, 
/**
 * Advanced per-run parameters (pedestal, trim fraction, bias fallback,
 * per-CFA-channel flat scaling). Omitting it wholesale is the same as
 * sending `{}` — every one of ITS fields defaults too.
 */
params: LightCalParams, 
/**
 * Replace the master dark's hot pixels with a neighbourhood median.
 */
hotPixelCorrection: boolean, 
/**
 * Debayer a CFA light to full-resolution planar RGB. Ignored for mono
 * frames and for a `BAYERPAT` the catalog cannot vouch for.
 */
debayerOsc: boolean, 
/**
 * The container a calibrated light is written in on export and send.
 * The stacking run ignores it for its own intermediates (they stay
 * FITS — `PlaneReader` reads nothing else), and says so in stage 1.
 */
format: OutputFormat, };

export type ExposureMode = "seconds" | "stops" | "equivalent" | "none";

export type GroupingConfig = { 
/**
 * Clamped to a floor of [`MIN_EXPOSURE_TOLERANCE_SEC`] by
 * [`resolve_config`] — never trusted raw from a stored document.
 */
exposureToleranceSec: number, exposureMode: ExposureMode, stopsWidth: number, };

export type MeasurementConfig = { 
/**
 * How a frame's measurement becomes its stacking weight.
 */
weightMode: WeightMode, 
/**
 * The star profile fitted to each detected star.
 */
psfModel: PsfModel, 
/**
 * Detection cap: the most stars kept per frame (spec §9.2 `maxStars`; stars, not pixels).
 */
maxStars: number, 
/**
 * The coefficients of the user formula weight; read only by `WeightMode::Formula`.
 */
formula: FormulaWeights, 
/**
 * FITS keyword `WeightMode::Keyword` reads its value from.
 */
keyword: string, 
/**
 * Star-detection threshold for the quality measurement, in sigma above
 * the local background (spec §9.2). The measurement detector's two
 * levels sit at `background + k*noise` and `background + (k/2)*noise`,
 * so the seed population follows THIS frame's sky instead of a fixed
 * bright-pixel budget (M4a Task 2, ruling R-M4a-1). Peak only: the
 * matched filter and the structure map do not read it.
 */
detectionSigma: number, 
/**
 * What the seed detection runs on (math reference §5.1): the plane as
 * it is, or its 3×3 median. Everything downstream of detection always
 * measures the untouched plane. Read by the peak detector only.
 */
seedPrefilter: SeedPrefilter, 
/**
 * Legacy mirror of [`Self::seed_source`] (peak vs structure, R-M4c-11),
 * kept so older readers of the stored document still find the field. It
 * steers nothing: the measurement stage reads `seed_source` only.
 * A stored config written before M4c Task 0 has no such field and
 * decodes to the shipped default.
 */
seedDetector: SeedDetector, 
/**
 * The seed source (spec §2.2): the star-measurement module's matched filter (default), the peak threshold
 * (reads `detectionSigma` and `seedPrefilter`), or the structure map. A document without it is decoded by
 * `legacy_seed_source`; one that carries it uses it as is.
 */
seedSource: SeedSource, 
/**
 * The matched filter's threshold in units of the filtered plane's noise (ruling R-OSP-2). Clamped to
 * `MIN_SEED_THRESHOLD`..=`MAX_SEED_THRESHOLD` by `resolve_config`.
 */
seedThreshold: number, 
/**
 * Fast measurement (FM-2, FM-3): the first K seeds in detection order (filter flux descending) get the PSF
 * fits, the rest keep their detection record and are not fitted; `None` = all (the shipped behaviour). No
 * imputation: the frame aggregates, `c` and the F-6 medians are over the fitted set. Clamped up to
 * `max(``MIN_FIT_STARS``, registration.maxStars)` and canonicalized to `None` at or above
 * `measurement.maxStars` by `resolve_config`. Absent from the document at `None`, so every All-mode hash is
 * the one before the field existed.
 */
fitStars?: number, };

export type ReferenceMode = "auto" | "manual";

export type ReferenceConfig = { mode: ReferenceMode, 
/**
 * The two-pass registration reference (spec §4.4, ruling R-M4a-5):
 * with `mode = Auto`, stage 5 registers the reference's OWN group once
 * without persisting anything, then re-picks the reference among the
 * top-weighted frames closest to that group's median transform before
 * the real, persisting pass runs. Ignored in `Manual` mode — a pinned
 * reference never moves. Default ON: a run whose reference happens to
 * be the one frame the mount was nudged on rotates every master and
 * loses its corners, and the dry pass costs one extra registration of
 * one group.
 *
 * `#[serde(default = "default_two_pass")]` (ruling R-M4a-9): a config
 * stored before M4a has no such field and must decode to `true`, not
 * to `false`.
 */
twoPass: boolean, };

export type DrizzleKernel = "square" | "circle" | "gaussian";

export type DrizzleConfig = { 
/**
 * Whether the run also produces a drizzled master (default off).
 */
enabled: boolean, 
/**
 * Output pixels per input pixel along each axis: 2 gives an image twice as wide and as tall (default 2).
 */
scale: number, 
/**
 * The drop's side as a fraction of an input pixel, the drizzle `pixfrac`: 1 deposits the whole pixel, smaller
 * values a smaller drop (default 0.9; dimensionless).
 */
dropShrink: number, 
/**
 * The shape each drop deposits its flux with.
 */
kernel: DrizzleKernel, 
/**
 * Leave out the pixels the rejection stage flagged in each frame's rejection bitmap (default on).
 */
useRejection: boolean, 
/**
 * Weight each frame's deposit by its stacking weight (default on).
 */
useWeights: boolean, 
/**
 * Whether the drizzle follows local output normalization (default on).
 *
 * B8 (M3 final fix wave, M5 ruling): the drizzle follows the plain
 * master's OUTPUT normalization, so this toggle only has an effect when
 * local normalization drives output normalization
 * (`normalization.local.enabled == true`). With it off, the drizzle
 * falls back to each included frame's own global output-normalization
 * pair, exactly as [`super::integrate`]'s own engine does for the plain
 * master - including when LN was computed for rejection alone
 * (`normalization.rejection == "local"`, the default): those grids never
 * drive the drizzle's output (pcores review r1, I1).
 */
useLocalNormalization: boolean, 
/**
 * Also keep the drizzle's weight map, the accumulated deposit weight per output pixel divided by its maximum, so
 * it lies in [0, 1] (default off).
 */
writeWeightMap: boolean, 
/**
 * Bayer drizzle: whether each colour is deposited from its own samples of the calibrated mosaic instead of the
 * debayered planes (default off).
 *
 * M4d Task 1 (ruling R-M4d-2): deposit each colour's OWN samples from
 * the calibrated CFA mosaic instead of the debayered planes'
 * interpolated ones. Ignored for a mono group (silently — there is no
 * mosaic to deposit), and it makes stage 1 keep one extra artifact per
 * OSC frame.
 *
 * `#[serde(default)]` rides the struct-level `default`, so every stored
 * config written before M4d decodes as `false` and no
 * `STACKING_CONFIG_VERSION` bump is needed. It deliberately does NOT
 * enter `calibration_subtree`: the debayered artifact is
 * byte-identical either way, so flipping this must not invalidate a
 * set's whole calibrated cache — the missing `calibrated_mosaic`
 * artifact is what makes the run regenerate the pair (`stacking::run`).
 */
bayer: boolean, };

export type OutputFormat = "fits" | "xisf";

export type CleanupPolicy = "keepAll" | "deleteRegistered" | "deleteIntermediates";

export type OutputConfig = { format: OutputFormat, cleanup: CleanupPolicy, coverageMap: boolean, cropToCoverage: number, };

export type PathsConfig = { workingDir: string | null, outputDir: string | null, };

export type StackingConfig = { version: number, grouping: GroupingConfig, calibration: CalibratedLightOptions, measurement: MeasurementConfig, selection: SelectionConfig, reference: ReferenceConfig, registration: RegistrationConfig, normalization: NormalizationConfig, integration: IntegrationConfig, drizzle: DrizzleConfig, output: OutputConfig, paths: PathsConfig, };

export type StackingPreset = "default" | "fastPreview" | "maximumQuality";

export type StackingPresets = { default: StackingConfig, fastPreview: StackingConfig, maximumQuality: StackingConfig, };

export type NamedPreset = { name: string, config: StackingConfig, };

export type ColorMode = "mono" | "osc";

export type ScaleSource = "solve" | "header";

export type Stage = "masters" | "calibrate" | "measure" | "reference" | "register" | "normalize" | "integrate" | "drizzle" | "output";

export type MasterWork = "build" | "rebuild";

export type PlanMaster = { setId: number, kind: MasterWork, imagetyp: string, frameCount: number, label: string, };

export type PlanBlocker = { code: string, message: string, };

export type PlanGroup = { key: string, instrume: string | null, colorMode: ColorMode, filter: string | null, binning: number, cameras: Array<string>, exposureS: number | null, exposureClass: string, frameCount: number, 
/**
 * The frames this group holds, in group order, every member (a manually excluded one included, so
 * `member_frame_ids.len() == frame_count`): the same grouping code that builds the group fills it, so the
 * plan and the run never disagree about membership. The ids are in the run's namespace — a catalog frame
 * id for a set, a project-frame handle for a project. The Frames table groups its rows by it before the
 * first run (Frames block spec §7.1).
 */
memberFrameIds: Array<number>, includedCount: number, totalExposureS: number, calibratedCached: number, metricsCached: number, 
/**
 * Tier C Task 1: frames (of `frame_count`) whose `metrics` row AND
 * every one of its planes' `fits` rows are all fresh — a subset of
 * `metrics_cached` in practice (see [`build_plan`]'s own doc on the
 * per-frame loop), never counted when `metrics` itself is stale.
 */
fitsCached: number, 
/**
 * Frames (of `included_count`) whose `.athln` sidecar already exists on
 * disk (spec §9.3, M2) — `0` when local normalization is off. A
 * PRESENCE check, not a hash-verified freshness one: see the doc on
 * this field's computation in [`build_plan`] for why (the LN reference
 * member list is a stage-3 weight quantity, unavailable at plan time).
 */
lnCached: number, lnScalePx: number | null, lnScaleEstimated: boolean, lnWidth: number | null, lnHeight: number | null, 
/**
 * M3 Task 5: the group's reference-anchor member's own native
 * `NAXIS1`/`NAXIS2` — the SAME member and the SAME convention
 * `stacking::run::group_anchor_geometry` uses to stamp
 * `stacking_run_groups.width`/`height` at insert time (a plan and the
 * run it precedes must never disagree about which member anchors a
 * group's geometry). `None` only for an (unreachable in practice)
 * empty group. Frontend's Task 6 drizzle estimate line reads these —
 * the run's own actual reference geometry is not known this early.
 */
anchorWidth: number | null, anchorHeight: number | null, 
/**
 * M4b: this group's own pixel scale — the median of whichever members
 * have one (`IntegrationGroup::pixel_scale_arcsec`, propagated
 * verbatim). `None` when no member has a usable scale.
 */
pixelScaleArcsec: number | null, 
/**
 * M4b: `pixel_scale_arcsec / reference_scale`, where `reference_scale`
 * is the plan's resolved reference frame's OWN pixel scale. `None`
 * when either side is unavailable — the reference isn't resolved yet
 * (`Auto` mode with no prior run) or carries no scale, or this group
 * has none of its own. A ratio outside `[1 / SCALE_TOLERANCE,
 * SCALE_TOLERANCE]` is what the plan's "far from reference" warning
 * (never a blocker) is about.
 */
scaleRatioToReference: number | null, 
/**
 * M4b: whether `pixel_scale_arcsec` rests entirely on measured plate
 * solves (`Solve`) or includes at least one member whose scale came
 * from the header's focal length/pixel size instead (`Header`) —
 * the UI's `~` prefix on a header-implied number. `None` alongside
 * `pixel_scale_arcsec == None` (no member has a scale at all).
 */
scaleSource: ScaleSource | null, };

export type PlanReference = { mode: ReferenceMode, frameId: number | null, filename: string | null, onDisk: boolean, };

export type GridState = "unset" | "proposed" | "confirmed";

export type PlanGridTarget = { raDeg: number, decDeg: number, radiusDeg: number, };

export type PlanGridFootprint = { frameUuid: string, publisherAccountId: string, publisher: string, filterCanonical: string, outline: Array<[number, number]>, };

export type PlanGrid = { state: GridState, current: CanonicalGrid | null, proposal: CanonicalGrid | null, proposalRotation: ProposalRotation | null, framesInside: number, canEdit: boolean, target: PlanGridTarget, footprints: Array<PlanGridFootprint>, framesNotDrawn: number, };

export type PlanPrepare = { inPlace: number, debayered: number, converted: number, unsupported: number, atRun: number, };

export type PlanProjectFrame = { handle: number | null, frameUuid: string, fileName: string, publisher: string, filterCanonical: string, exptimeSec: number, colorMode: ColorMode, focalLenMm: number | null, hasWcs: boolean, onDisk: boolean, reason: string | null, transparencyLoss: number | null, };

export type StackingPlan = { setId: number | null, projectId: string | null, setName: string, config: StackingConfig, configHash: string, groups: Array<PlanGroup>, blockers: Array<PlanBlocker>, warnings: Array<string>, readiness: ExportReadiness, 
/**
 * Stage 0.5's work list (spec §2 row 0.5, owner requirement 2026-09-09):
 * every buildable raw set and rebuildable missing master, sorted by
 * [`crate::api::masters::type_build_rank`] then id — bias/darkflat
 * before dark before flat, the same dependency order
 * `start_master_builds_batch` submits a manual batch in, so a flat
 * built by stage 0.5 sees its own precal master already on disk.
 */
mastersToBuild: Array<PlanMaster>, reference: PlanReference, frameCount: number, includedCount: number, excludedFrameIds: Array<number>, estimateBytes: number, freeBytes: number | null, workingDir: string | null, outputDir: string | null, staleStages: Array<Stage>, activeRunId: number | null, grid: PlanGrid | null, projectFrames: Array<PlanProjectFrame>, prepare: PlanPrepare | null, };

export type StackingRunRow = { id: number, framesSetId: number | null, projectId: string | null, status: string, startedAt: string, finishedAt: string | null, configJson: string, configHash: string, referenceFrameId: number | null, referenceMode: string, workingDir: string, outputDir: string, summaryJson: string | null, error: string | null, };

export type StackingRunGroupRow = { id: number, runId: number, groupKey: string, instrume: string | null, colorMode: string, filter: string | null, binning: number | null, 
/**
 * Owner decision 2026-09-10 (groups are camera-agnostic): the group's
 * reference-ANCHOR member's own native `NAXIS1`/`NAXIS2`
 * (`stacking::run::group_anchor_geometry`), recorded once at
 * `insert_group` time — NOT the run's actual output/reference geometry
 * (`RunContext::reference_width`/`height`, resolved later, in stage 5
 * Register). There is no post-Register write-back today (`GroupUpdate`
 * carries no width/height field), so a group whose reference frame
 * ends up being a DIFFERENT camera than its anchor member will show
 * this row's `width`/`height` as that anchor's size, not the actual
 * master's.
 */
width: number | null, height: number | null, exposure: number | null, frameCount: number, includedCount: number, masterPath: string | null, drizzlePath: string | null, rejectionLowPath: string | null, rejectionHighPath: string | null, statsJson: string | null, status: string, error: string | null, 
/**
 * Perf tier C Task 1 (ruling C-1/C-1a): the group β Measure resolved
 * once from its members' own `Auto` picks — `psf_signal::group_beta`,
 * always one of their own βs (continuous from Plan A Task 9, F-4; one of
 * `psf_signal::AUTO_BETAS` before it). `None` for a run predating
 * this task, or a group that measured no frame at all.
 *
 * Read by the PLAN GATE since M1 (final fix wave, ruling C-30): the β
 * is an LN input (stage 6 fits the reference and re-fits every
 * member's seeds at it), so `stacking::plan::normalization_hash_for`
 * folds it in, and the gate — which never runs a measurement of its
 * own — recovers it from this column on the LAST run for the group.
 * The run itself uses `RunContext::measured_groups`, the in-memory
 * value stage 3 just resolved; the two must agree, which is what
 * `local_normalization_sidecars_are_cached_on_the_second_run` pins.
 */
beta: number | null, exposureClass: string | null, };

export type StackingRunFrameRow = { id: number, runId: number, groupId: number, frameId: number, included: boolean, exclusionReason: string | null, weight: number | null, weightChannelsJson: string | null, metricsJson: string | null, regStatus: string | null, regModel: string | null, regRmsPx: number | null, regInliers: number | null, regInlierRatio: number | null, regFlipped: boolean | null, rejectedFraction: number | null, frameUuid: string | null, };

export type StackingRunSummary = { run: StackingRunRow, groupCount: number, masterPaths: Array<string>, };

export type SummaryReference = { frameId: number | null, filename: string | null, mode: ReferenceMode, weight: number | null, 
/**
 * The frame stage 4 originally picked, when stage 5's two-pass pick
 * then moved the reference somewhere else (spec §4.4, ruling
 * R-M4a-5) — `None` on every run that kept its first choice, which is
 * most of them. `#[serde(default)]` so a `runs/run-<id>.json` written
 * before M4a still deserializes.
 */
switchedFrom: number | null, };

export type SummaryMeasurement = { seedSource: string, scaleEstimator: ScaleEstimator, };

export type SummaryFrame = { frameId: number, filename: string, included: boolean, exclusionReason: string | null, weight: number | null, weightChannels: Array<number>, fwhmPx: number | null, eccentricity: number | null, stars: number | null, psfSignalWeight: number | null, psfSnr: number | null, noise: number | null, regStatus: string | null, regModel: string | null, regRmsPx: number | null, regInliers: number | null, regInlierRatio: number | null, regFlipped: boolean | null, rejectedFraction: number | null, calibratedPath: string | null, cachedCalibrated: boolean, cachedMetrics: boolean, cachedRegistration: boolean, 
/**
 * Stage 6 (local normalization, M2): the frame's own relative scale
 * (mean across channels — [`crate::stacking::ln::LnFrameOutcome::scale`]),
 * `None` when local normalization never ran for this group (disabled,
 * or a ruling-R3 fallback to global normalization) or this frame was
 * excluded before reaching it. `#[serde(default)]` so a `runs/run-<id>.json`
 * written before M2 still deserializes.
 */
lnScale: number | null, 
/**
 * Whether stage 6 REUSED an existing `ln` artifact for this frame
 * rather than normalizing it fresh — same convention as
 * `cached_calibrated`/`cached_metrics`. `#[serde(default)]`, see
 * `ln_scale`'s own doc.
 */
cachedLn: boolean, 
/**
 * Plan B B0 (PB-2): F1's growth-curve factor for this frame (`LnFrameOutcome::growth_factor`, the mean over its
 * planes). `Some` only for a frame LN computed in THIS run under the growth-curve aperture; `None` for a frame LN
 * did not normalize, on an LN cache hit (the cached row carries only the scale), under another estimator, and in a
 * summary written before B0. A plain `Option` (`null` in the payload; a skipped `ts_rs` `Option` would arrive
 * `undefined`), `#[serde(default)]`, see `ln_scale`'s own doc.
 */
lnGrowthFactor: number | null, 
/**
 * F1's pair count for this frame (`LnFrameOutcome::growth_pairs`), the MINIMUM over its planes so a plane at
 * floor (ii) (`Some(0)`, the 2 x FWHM ratio kept) shows. `Some` exactly when `ln_growth_factor` is.
 */
lnGrowthPairs: number | null, 
/**
 * The TARGET side's aperture correction `c` LN's scale call used (`LnFrameOutcome::target_aperture_correction`,
 * the mean over its planes, like `fwhm_px`, and only when EVERY plane measured one, R-PB0-5). `Some` for a frame
 * LN computed in this run whose planes all measured a `c`; `None` when any plane is at floor (i), for a frame LN
 * did not normalize, on an LN cache hit, and in a summary written before B0.
 */
lnApertureCorrection: number | null, 
/**
 * The frame's own β (Plan A Task 11b, owner decision R11b-12; mean across channels like `fwhm_px`): F-4's
 * per-frame estimate, a diagnostic beside the group β every member was fitted at (`SummaryGroup::beta`). `None`
 * when the frame was not measured, under `Moffat4`, and in a summary written before Task 11b. `#[serde(default)]`,
 * see `ln_scale`'s own doc.
 */
betaOwn: number | null, 
/**
 * Whether the frame's own β sat at a bound of the β range on any channel (`ChannelMeasurement::beta_own_at_cap`):
 * then `beta_own` is a clamp, not an estimate. `false` when not measured, under `Moffat4`, and in a summary
 * written before this field. `#[serde(default)]`, see `ln_scale`'s own doc.
 */
betaOwnAtCap: boolean, 
/**
 * Whether the frame's field width sat at the clamp floor on any channel
 * (`ChannelMeasurement::field_width_at_floor`). `false` when not measured and in a summary written before this
 * field. `#[serde(default)]`, see `ln_scale`'s own doc.
 */
fieldWidthAtFloor: boolean, };

export type SummaryGroup = { key: string, frameCount: number, includedCount: number, masterPath: string | null, rejectionLowPath: string | null, rejectionHighPath: string | null, stats: GroupStats | null, normalizationReferenceFrameId: number | null, 
/**
 * M4b (ruling R-M4b-4): the frame every member of THIS group was
 * registered onto — the run-wide reference in `coRegistered` mode
 * (the same id `RunSummary::reference` carries), the group's own in
 * `native` mode.
 *
 * `Some` ONLY when the group's master was written (ruling R-T3-3);
 * `None` for a group skipped, failed, or dropped below the member
 * floor at any stage — and on a summary written before M4b. Such a
 * group does have a resolved reference in the run's own bookkeeping,
 * but nothing was ever delivered in that geometry, so reporting one
 * here would put a `reference #N` on a Results card with no master
 * behind it. `#[serde(default)]`, see [`SummaryFrame::ln_scale`]'s
 * own doc.
 */
referenceFrameId: number | null, 
/**
 * Stage 6 (local normalization, M2): the group's LN reference file
 * (`ln/<group>/reference.fits`, spec §9.5), when local normalization
 * ran for this group at all — `None` when it is disabled, the group
 * never reached Output (skipped/failed), or a ruling-R3 fallback to
 * global normalization applied. `#[serde(default)]`, see
 * `SummaryFrame::ln_scale`'s own doc.
 */
lnReferencePath: string | null, 
/**
 * M3 Task 5 (spec §7, rulings R-M3-7/R-M3-9): the group's drizzled
 * master, when the run's drizzle stage wrote one for it — `None` when
 * drizzle is off, this group's drizzle failed (`warnings` carries the
 * reason; the master above is unaffected either way), or the group
 * never reached Output. `#[serde(default)]`, see `SummaryFrame::ln_scale`'s
 * own doc for why every M3 field here follows that convention.
 */
drizzlePath: string | null, 
/**
 * The drizzle weight map alongside `drizzle_path`, when
 * `DrizzleConfig::write_weight_map` was on for this run — `None`
 * whenever `drizzle_path` is `None` too, or the toggle was off.
 */
weightMapPath: string | null, coveragePath: string | null, drizzleCoveragePath: string | null, 
/**
 * The drizzle stage's own per-group stats, `Some` exactly when
 * `drizzle_path` is.
 */
drizzle: DrizzleStats | null, 
/**
 * Perf tier C Task 1 (ruling C-1/C-1a): the group β Measure resolved
 * from its members' own `Auto` picks — always one of their own βs
 * (continuous from Plan A Task 9, F-4; one of `psf_signal::AUTO_BETAS`
 * before it). From Plan A Task 11b (owner decision R11b-12) every
 * member was fitted at it under `Auto` (each frame's own β is
 * `SummaryFrame::beta_own`). `None` when the group measured no frame at
 * all, or on a summary written before this task. `#[serde(default)]`,
 * see `SummaryFrame::ln_scale`'s own doc for the convention.
 */
beta: number | null, 
/**
 * Perf tier C Task 2 fix round 4 (ruling C-14 item 2): the group's own
 * per-CHANNEL seeds calibration `k` — the factor stage 6 measured once
 * for the whole group and multiplied into every seeds-path channel's
 * `A`. One entry per plane of the group's LN reference, `1.0` for a
 * channel that could not be calibrated (too few usable ratios, or a
 * median outside its band). `None` when local normalization did not
 * run for this group at all, when the calibration could not be
 * measured, and on a summary written before this field existed.
 * `#[serde(default)]`, see [`SummaryFrame::ln_scale`]'s own doc for
 * the convention.
 */
seedsCalibration: Array<number> | null, lnScalePx: number | null, lnFluxEstimator: LnFluxEstimator | null, lnAperture: LnAperture | null, 
/**
 * Plan B B0 (PB-2, R-PB0-7): the LN reference's aperture correction `c` per plane (`PreparedReferenceChannel::
 * aperture_correction`), the reference side of `SummaryFrame::ln_aperture_correction`. A plane at floor (i) (no
 * `c`; `c = 1` was applied) reads `null`, never the applied `1.0`, so a reader of `c_ref / c_tgt` sees the floor.
 * `None` when LN did not run for this group and in a summary written before B0; a row written as `[1.0, ...]`
 * still decodes. A plain `Option`, `#[serde(default)]`, see `SummaryFrame::ln_scale`'s own doc.
 */
lnReferenceApertureCorrection: Array<number | null> | null, frames: Array<SummaryFrame>, };

export type StageTiming = { stage: Stage, durationMs: number, };

export type MasterBuilt = { setId: number, kind: MasterWork, masterSetId: number, path: string, durationMs: number, };

export type RunSummary = { runId: number, setId: number | null, projectId: string | null, setName: string, appVersion: string, startedAt: string, finishedAt: string | null, status: string, config: StackingConfig, configHash: string, grid: CanonicalGrid | null, reference: SummaryReference, measurement: SummaryMeasurement, groups: Array<SummaryGroup>, 
/**
 * Stage 0.5's own result list (spec §2 row 0.5, owner requirement
 * 2026-09-09) — every master the run built or rebuilt before
 * calibrating. Empty when `masters_to_build` was empty (nothing to do)
 * or the run never reached stage 0.5 (a blocker or an earlier failure).
 */
mastersBuilt: Array<MasterBuilt>, stages: Array<StageTiming>, warnings: Array<string>, error: string | null, };

export type StackingProgressEvent = { runId: number, setId: number | null, projectId: string | null, stage: Stage, groupKey: string | null, current: number, total: number, percent: number, bytesDone: number, bytesTotal: number, frameId: number | null, message: string | null, };

export type StackingMasterRef = { groupKey: string, path: string, drizzlePath: string | null, };

export type StackingCompleteEvent = { runId: number, setId: number | null, projectId: string | null, success: boolean, cancelled: boolean, error: string | null, warnings: Array<string>, masters: Array<StackingMasterRef>, 
/**
 * Keys of the groups the run summary lists without a master (skipped or
 * failed); wire-only, never stored.
 */
groupsWithoutMaster: Array<string>, };

export type StartedStacking = { runId: number, };

export type StackingRunDetail = { run: StackingRunRow, groups: Array<StackingRunGroupRow>, frames: Array<StackingRunFrameRow>, summary: RunSummary | null, };

export type StackingSetConfig = { config: StackingConfig, excludedFrameIds: Array<number>, 
/**
 * `true` when the frame set has no stored override row at all — the
 * resolved config above is entirely the global default's.
 */
isDefault: boolean, updatedAt: string | null, };

export type StackingProjectConfig = { config: StackingConfig, excludedFrameUuids: Array<string>, referenceFrameUuid: string | null, isDefault: boolean, updatedAt: string | null, };

export type StackingPaths = { working: PathSetting, output: PathSetting, };

export type WorkUsage = { calibratedBytes: number, registeredBytes: number, lnBytes: number, 
/**
 * Tier C Task 1: bytes under `fits/` — the persisted per-plane star
 * fits, cached like `metrics` (no file of its own) but WITH a file
 * behind each row, so it earns a byte counter `metrics` never needed.
 */
fitsBytes: number, 
/**
 * Plan A Task 11c: bytes under `measured/` — the Measure first-pass artifacts (R11c-6), about 2 % of the
 * calibrated footprint.
 */
measuredBytes: number, runsBytes: number, 
/**
 * M3 Task 2: bytes under `rej/` — per-run rejection-bitmap temporaries
 * (spec §6.2, ruling R-M3-8), never a `stacking_artifacts` row.
 */
rejBytes: number, 
/**
 * M4d Task 3: bytes under `previews/` — the cached master-light JPEGs
 * (ruling R-M4d-5). Reported so the working folder's `total_bytes` is
 * the whole tree and not just the subtrees that existed before this
 * cache did; `CleanupWhat::All` frees them.
 */
previewsBytes: number, totalBytes: number, };

export type CleanupWhat = "registered" | "intermediates" | "all";

export type GroupStats = { 
/**
 * The group's total frame count, before the min-weight drop.
 */
frames: number, 
/**
 * How many frames were integrated, after the min-weight drop.
 */
included: number, 
/**
 * How many frames the min-weight floor removed.
 */
droppedBelowMinWeight: number, 
/**
 * `IntegrationRecipe::describe()` of the resolved recipe.
 */
recipe: string, 
/**
 * `Σ rejected_low / Σ samples_per_frame`, over every included frame and
 * plane (the engine's `rejected_low`/`samples_per_frame` denominator —
 * see `StackOutput`'s doc — NOT `base.rejected_fraction`'s, which
 * counts algorithm rejections only).
 */
rejectedLowFraction: number, 
/**
 * The same as `rejected_low_fraction` for samples rejected on the high side.
 */
rejectedHighFraction: number, 
/**
 * Per included frame (engine order), `Σ rejected / Σ samples` over
 * every plane — the same range-plus-algorithm rejection the group
 * fractions above count, just narrowed to one frame instead of the
 * whole group.
 */
rejectedFractionPerFrame: Array<number>, 
/**
 * MRS noise of the master per plane, native `[0, 1]` units.
 * `measure_plane` already reports `ChannelMeasurement.noise` in native
 * units (it divides the ADU-scaled MRS estimate back down by
 * `measure::ADU_SCALE` internally, see `measure_plane`'s own code) —
 * no further conversion happens here.
 */
masterNoise: Array<number>, 
/**
 * The master's normalization location (median) per plane, in normalized units.
 */
masterLocation: Array<number>, 
/**
 * The master's two-sided scale per plane, in normalized units.
 */
masterScale: Array<number>, 
/**
 * Per plane, of the included frame with the highest `weight.normalized_mean`.
 */
bestSubNoise: Array<number>, 
/**
 * The master's PSF SNR per plane: over the group's frame-common star set when it has one (Plan D D1b, the stars the
 * master matches), else over every star of the master. `best_sub_psf_snr` is the best sub's own `psf_snr` - under the
 * PSF weight family the common-set value after the reweigh (D1w), else its Measure-time one; outside the family
 * (Noise / Formula / Exposure / Keyword / None) always the Measure-time whole-field figure, because no frame is weighed
 * on the set there (PD-19 b).
 *
 * These two are NOT a ratio pair, and nothing divides them to get [`GroupStats::snr_gain`] (that is
 * `paired_snr_gain`, on the stars matched in BOTH crops): `master_psf_snr` is a sum over the stars the master
 * matched in its rectangle on a plane that has a set, and the whole-field figure (unmarked) on a plane that fell
 * back or matched under 20; `best_sub_psf_snr` is the frame's own `psf_snr`, whose flux under the PSF family after D1w
 * is the paired ratio `r_f` times the anchor's whole-set sum (the stars the frame lacks are extrapolated, not summed),
 * or its Measure-time figure on a fallback. Under the PSF family they agree only where one set is whole in both images; outside
 * the family they never agree (the best sub is Measure-time there, the master reads the set).
 */
masterPsfSnr: Array<number>, 
/**
 * The best sub's own PSF SNR per plane (see `master_psf_snr`; dimensionless).
 */
bestSubPsfSnr: Array<number>, 
/**
 * How many times the master's star SNR beats the best sub's, per
 * plane — an SNR (amplitude) factor, `√(PSF SNR master / PSF SNR best
 * sub)`, since PSF SNR is TFlux²/σ² (a power ratio). The two PSF SNRs
 * are a PAIRED measurement (`paired_snr_gain`): the best sub read
 * through the same registration warp the master was built from, both
 * measured with the same options on the same rectangle — a smoothing
 * interpolation lowers a warped image's noise, so a raw sub against
 * the master over-states the gain (2026-10-25: set 109 read 751.98×
 * and 777.33× for 208 and 160 frames; the ratio was a power ratio of a
 * warped master over a raw sub). Since v2 (Plan D D1b) TFlux is summed
 * over the group's frame-common star set only, and only the stars matched
 * and usable in BOTH images: a bright clipped star, or a wider PSF's
 * extra flux, no longer moves the figure. Both sides also read on ONE
 * aperture correction `c` (v2, Plan D D1 fix r4): a side at floor (i), which
 * has none, reads with the other side's (`paired_c_stand_ins`), warned.
 * They are not
 * `master_psf_snr` / `best_sub_psf_snr` above, which stay each image's
 * own measurement. `0.0` (with a warn naming its reason) when the pair
 * cannot be measured: no frame-common star set for the plane, or fewer
 * than [`COMMON_MIN_STARS`] stars paired.
 */
snrGain: Array<number>, 
/**
 * Which formula [`Self::snr_gain`] was measured with:
 * [`SNR_GAIN_VERSION`] for the paired SNR factor above (`1` = over every
 * star of the two images, v0.7.0; `2` = over the frame-common star set).
 * A stats document stored before v0.7.0 has no such key and reads `0`: its
 * `snr_gain` is the old unpaired power ratio (set 109's 751.98×), so the
 * Results panel shows "—" for it instead of passing it off as the same
 * metric; a `1` shows "—" too, "measured with an older formula".
 */
snrGainVersion: number, 
/**
 * The master's star FWHM per plane, in pixels.
 */
masterFwhmPx: Array<number>, 
/**
 * The master's mean star eccentricity per plane (0 is round, towards 1 is elongated).
 */
masterEccentricity: Array<number>, 
/**
 * `Σ exposure_i · weight_i` over included frames, `weight_i` the mean
 * over planes of frame i's normalized weight (`FrameWeight::normalized_mean`).
 */
weightedExposureS: number, 
/**
 * The summed exposure time of the included frames, in seconds.
 */
totalExposureS: number, 
/**
 * Time spent reading the frames, in milliseconds (summed over planes and passes).
 */
readMs: number, 
/**
 * Time spent combining samples, in milliseconds (summed over planes and passes).
 */
combineMs: number, 
/**
 * Bytes read from the frame files (summed over planes and passes).
 */
bytesRead: number, 
/**
 * Included frames integrated with an LN grid (M2 Task 7) — `0` when the
 * caller passes no grids at all (`GroupInput.ln: None`: local
 * normalization off for this group, or `stacking::run` never resolved
 * any), otherwise the count of included frames whose own `ln[i]` was
 * `Some`.
 */
lnFrames: number, 
/**
 * The share of all samples that large-scale rejection forced out before the second pass (a fraction, 0 to 1).
 *
 * M4c Task 3: the fraction of all (frame, plane, pixel) samples the
 * PROCESSED bitmaps forced out before the second pass's algorithm ran
 * — `None` when large-scale rejection did not run for this group at
 * all (off, no bitmap set, or the first pass's bitmaps could not be
 * trusted; `stacking::run` turns the last two into a run warning).
 */
largeScaleRejectedFraction: number | null, };

export type DrizzleStats = { 
/**
 * Output pixels per input pixel along each axis.
 */
scale: number, 
/**
 * Output width in pixels (reference width times `scale`).
 */
outWidth: number, 
/**
 * Output height in pixels (reference height times `scale`).
 */
outHeight: number, 
/**
 * Included frames this pass drizzled (whether or not every one
 * actually contributed — a frame skipped for zero weight still counts,
 * mirroring `DrizzleInput::frames`'s own length).
 */
frames: number, 
/**
 * The drop shape used.
 */
kernel: DrizzleKernel, 
/**
 * The drop's side as a fraction of an input pixel (dimensionless).
 */
dropShrink: number, 
/**
 * Whether per-frame weights were applied.
 */
usedWeights: boolean, 
/**
 * Whether `.rej` rejection bitmaps were applied.
 */
usedRejection: boolean, 
/**
 * Of `frames`, how many actually had their LN grids applied (`0` when
 * local normalization was off for the run).
 */
lnFrames: number, 
/**
 * Per plane, measured on the drizzled (output-grid) planes.
 */
fwhmPx: Array<number>, 
/**
 * Per plane, the eccentricity of the drizzled planes' stars (0 round, toward 1 elongated; unitless).
 */
eccentricity: Array<number>, 
/**
 * Per plane, the noise sigma of the drizzled plane, in normalized units.
 */
noise: Array<number>, 
/**
 * Per plane, the fraction of output pixels with `W > 0`.
 */
coverage: Array<number>, 
/**
 * Time the deposit loop WAITED for a frame's input (source plane and
 * rejection bitmap). Task 13c L2: the next frame is read by a helper
 * thread while the current one deposits, so this is the part of the
 * read the overlap did not hide, not the read's full duration.
 */
readMs: number, 
/**
 * Time spent depositing frames into the output grid, in milliseconds.
 */
depositMs: number, 
/**
 * Source bytes read from disk during the pass.
 */
bytesRead: number, };

export type MasterLightKind = "master" | "drizzle" | "weightMap" | "coverage" | "drizzleCoverage";

export type AlignedState = "available" | "registrationFailed" | "notRegistered" | "missing" | "replaced";

export type AlignedFrameStatus = { frameId: number, state: AlignedState, };

