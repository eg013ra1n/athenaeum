// AUTO-GENERATED from Rust by athenaeum-core/src/ts_export.rs — do not edit.
// Regenerate: TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract

import type { ExportReadiness, FlatNormMode, LightCalParams, PathSetting } from './models';

export type Interpolation = "nearest" | "bilinear" | "bicubicSpline" | "bicubicBSpline" | "lanczos3" | "lanczos4" | "mitchellNetravali";

export type SeedPrefilter = "none" | "median3";

export type PsfModel = "auto" | "moffat4";

export type WeightMode = "psfSignalWeight" | "psfSnr" | "noise" | "formula" | "exposure" | "keyword" | "none";

export type FormulaWeights = { fwhm: number, eccentricity: number, snr: number, stars: number, pedestal: number, };

export type SelectionConfig = { 
/**
 * Frames below this fraction of the group's maximum weight are excluded.
 */
minWeightFraction: number, maxFwhmPx: number | null, maxEccentricity: number | null, minStars: number | null, 
/**
 * A frame whose registration failed is dropped from its group (spec
 * §9.2). Read by the run orchestration, not by [`select_frames`] —
 * this stage has no registration outcome to check, so the field is
 * carried here only so a stored config round-trips.
 */
excludeOnRegistrationFailure: boolean, };

export type RegistrationGeometry = "coRegistered" | "native";

export type ModelChoice = "auto" | "similarity" | "affine" | "homography";

export type DistortionChoice = "off" | "polynomial2" | "polynomial3" | "polynomial4" | "auto";

export type DetectionConfig = { minSnr: number, maxEccentricity: number, };

export type RegistrationConfig = { 
/**
 * M4b (ruling R-M4b-4): co-registered (the default — one reference and
 * one geometry for the whole set) or native (a reference and a
 * geometry per group). `#[serde(default)]` rides the struct-level
 * `default`, so every stored config written before M4b decodes as
 * co-registered and no `STACKING_CONFIG_VERSION` bump is needed.
 */
geometry: RegistrationGeometry, model: ModelChoice, distortion: DistortionChoice, interpolation: Interpolation, clampingThreshold: number, maxStars: number, ransacTolerancePx: number, ransacMaxIterations: number, maxRmsPx: number, failOnMaxRms: boolean, detection: DetectionConfig, writeRegisteredFrames: boolean, };

export type OutputNormalization = "none" | "additive" | "additiveWithScaling" | "multiplicative" | "multiplicativeWithScaling";

export type RejectionNormalization = "none" | "scaleZeroOffset" | "equalizeFluxes" | "local";

export type ScaleEstimator = "bwmv" | "mad" | "avgDev";

export type LocalNormalizationConfig = { enabled: boolean, 
/**
 * Tile size in pixels for the local-normalization grid.
 */
scale: number, referenceFrames: number, psfModel: PsfModel, localScale: boolean, };

export type NormalizationConfig = { output: OutputNormalization, rejection: RejectionNormalization, 
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

export type RejectionChoice = { "method": "auto" } | { "method": "none" } | { "method": "percentileClip", low: number, high: number, } | { "method": "sigmaClip", sigmaLow: number, sigmaHigh: number, } | { "method": "winsorizedSigma", sigmaLow: number, sigmaHigh: number, } | { "method": "linearFitClip", sigmaLow: number, sigmaHigh: number, };

export type IntegrationConfig = { 
/**
 * The master builder's `Combination` — one enum, one spelling
 * (`"average"`/`"median"`, snake_case per `combine.rs`'s own attribute).
 */
combination: Combination, rejection: RejectionChoice, 
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
rangeHigh: number | null, writeRejectionMaps: boolean, };

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
debayerOsc: boolean, };

export type GroupingConfig = { 
/**
 * Clamped to a floor of [`MIN_EXPOSURE_TOLERANCE_SEC`] by
 * [`resolve_config`] — never trusted raw from a stored document.
 */
exposureToleranceSec: number, };

export type MeasurementConfig = { weightMode: WeightMode, psfModel: PsfModel, 
/**
 * Detection cap (spec §9.2 `maxStars`).
 */
maxStars: number, formula: FormulaWeights, 
/**
 * FITS keyword `WeightMode::Keyword` reads its value from.
 */
keyword: string, 
/**
 * Star-detection threshold for the quality measurement, in sigma above
 * the local background (spec §9.2). The measurement detector's two
 * levels sit at `background + k*noise` and `background + (k/2)*noise`,
 * so the seed population follows THIS frame's sky instead of a fixed
 * bright-pixel budget (M4a Task 2, ruling R-M4a-1).
 */
detectionSigma: number, 
/**
 * What the seed detection runs on (math reference §5.1): the plane as
 * it is, or its 3×3 median. Everything downstream of detection always
 * measures the untouched plane.
 */
seedPrefilter: SeedPrefilter, };

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

export type DrizzleConfig = { enabled: boolean, scale: number, dropShrink: number, kernel: DrizzleKernel, useRejection: boolean, useWeights: boolean, 
/**
 * B8 (M3 final fix wave, M5 ruling): a no-op whenever local
 * normalization is off for the run (`normalization.local.enabled ==
 * false` and `normalization.rejection != "local"`) — the drizzle
 * driver already falls back to each included frame's own global
 * output-normalization pair per frame in that case, exactly as
 * [`super::integrate`]'s own engine does; this toggle only has an
 * effect when local normalization is actually driving output
 * normalization for the group.
 */
useLocalNormalization: boolean, writeWeightMap: boolean, };

export type OutputFormat = "fits";

export type CleanupPolicy = "keepAll" | "deleteRegistered" | "deleteIntermediates";

export type OutputConfig = { format: OutputFormat, cleanup: CleanupPolicy, };

export type PathsConfig = { workingDir: string | null, outputDir: string | null, };

export type StackingConfig = { version: number, grouping: GroupingConfig, calibration: CalibratedLightOptions, measurement: MeasurementConfig, selection: SelectionConfig, reference: ReferenceConfig, registration: RegistrationConfig, normalization: NormalizationConfig, integration: IntegrationConfig, drizzle: DrizzleConfig, output: OutputConfig, paths: PathsConfig, };

export type StackingPreset = "default" | "fastPreview" | "maximumQuality";

export type StackingPresets = { default: StackingConfig, fastPreview: StackingConfig, maximumQuality: StackingConfig, };

export type ColorMode = "mono" | "osc";

export type ScaleSource = "solve" | "header";

export type Stage = "masters" | "calibrate" | "measure" | "reference" | "register" | "normalize" | "integrate" | "drizzle" | "output";

export type MasterWork = "build" | "rebuild";

export type PlanMaster = { setId: number, kind: MasterWork, imagetyp: string, frameCount: number, label: string, };

export type PlanBlocker = { code: string, message: string, };

export type PlanGroup = { key: string, instrume: string | null, colorMode: ColorMode, filter: string | null, binning: number, cameras: Array<string>, exposureS: number | null, frameCount: number, includedCount: number, totalExposureS: number, calibratedCached: number, metricsCached: number, 
/**
 * Frames (of `included_count`) whose `.athln` sidecar already exists on
 * disk (spec §9.3, M2) — `0` when local normalization is off. A
 * PRESENCE check, not a hash-verified freshness one: see the doc on
 * this field's computation in [`build_plan`] for why (the LN reference
 * member list is a stage-3 weight quantity, unavailable at plan time).
 */
lnCached: number, 
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

export type StackingPlan = { setId: number, setName: string, config: StackingConfig, configHash: string, groups: Array<PlanGroup>, blockers: Array<PlanBlocker>, warnings: Array<string>, readiness: ExportReadiness, 
/**
 * Stage 0.5's work list (spec §2 row 0.5, owner requirement 2026-09-09):
 * every buildable raw set and rebuildable missing master, sorted by
 * [`crate::api::masters::type_build_rank`] then id — bias/darkflat
 * before dark before flat, the same dependency order
 * `start_master_builds_batch` submits a manual batch in, so a flat
 * built by stage 0.5 sees its own precal master already on disk.
 */
mastersToBuild: Array<PlanMaster>, reference: PlanReference, frameCount: number, includedCount: number, excludedFrameIds: Array<number>, estimateBytes: number, freeBytes: number | null, workingDir: string | null, outputDir: string | null, staleStages: Array<Stage>, activeRunId: number | null, };

export type StackingRunRow = { id: number, framesSetId: number, status: string, startedAt: string, finishedAt: string | null, configJson: string, configHash: string, referenceFrameId: number | null, referenceMode: string, workingDir: string, outputDir: string, summaryJson: string | null, error: string | null, };

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
width: number | null, height: number | null, exposure: number | null, frameCount: number, includedCount: number, masterPath: string | null, drizzlePath: string | null, rejectionLowPath: string | null, rejectionHighPath: string | null, statsJson: string | null, status: string, error: string | null, };

export type StackingRunFrameRow = { id: number, runId: number, groupId: number, frameId: number, included: boolean, exclusionReason: string | null, weight: number | null, weightChannelsJson: string | null, metricsJson: string | null, regStatus: string | null, regModel: string | null, regRmsPx: number | null, regInliers: number | null, regInlierRatio: number | null, regFlipped: boolean | null, rejectedFraction: number | null, };

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
cachedLn: boolean, };

export type SummaryGroup = { key: string, frameCount: number, includedCount: number, masterPath: string | null, rejectionLowPath: string | null, rejectionHighPath: string | null, stats: GroupStats | null, normalizationReferenceFrameId: number | null, 
/**
 * M4b (ruling R-M4b-4): the frame every member of THIS group was
 * registered onto — the run-wide reference in `coRegistered` mode
 * (the same id `RunSummary::reference` carries), the group's own in
 * `native` mode. `None` on a summary written before M4b, and for a
 * group that never reached stage 5 (fewer than 3 included frames):
 * such a group has a resolved reference in the run's own bookkeeping,
 * but it never warped anything onto it and never wrote a master, so
 * reporting one here would put a `reference #N` on a Results card
 * with no master behind it. `#[serde(default)]`, see
 * [`SummaryFrame::ln_scale`]'s own doc.
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
weightMapPath: string | null, 
/**
 * The drizzle stage's own per-group stats, `Some` exactly when
 * `drizzle_path` is.
 */
drizzle: DrizzleStats | null, frames: Array<SummaryFrame>, };

export type StageTiming = { stage: Stage, durationMs: number, };

export type MasterBuilt = { setId: number, kind: MasterWork, masterSetId: number, path: string, durationMs: number, };

export type RunSummary = { runId: number, setId: number, setName: string, appVersion: string, startedAt: string, finishedAt: string | null, status: string, config: StackingConfig, configHash: string, reference: SummaryReference, measurement: SummaryMeasurement, groups: Array<SummaryGroup>, 
/**
 * Stage 0.5's own result list (spec §2 row 0.5, owner requirement
 * 2026-09-09) — every master the run built or rebuilt before
 * calibrating. Empty when `masters_to_build` was empty (nothing to do)
 * or the run never reached stage 0.5 (a blocker or an earlier failure).
 */
mastersBuilt: Array<MasterBuilt>, stages: Array<StageTiming>, warnings: Array<string>, error: string | null, };

export type StackingProgressEvent = { runId: number, setId: number, stage: Stage, groupKey: string | null, current: number, total: number, percent: number, bytesDone: number, bytesTotal: number, frameId: number | null, message: string | null, };

export type StackingMasterRef = { groupKey: string, path: string, drizzlePath: string | null, };

export type StackingCompleteEvent = { runId: number, setId: number, success: boolean, cancelled: boolean, error: string | null, warnings: Array<string>, masters: Array<StackingMasterRef>, };

export type StartedStacking = { runId: number, };

export type StackingRunDetail = { run: StackingRunRow, groups: Array<StackingRunGroupRow>, frames: Array<StackingRunFrameRow>, summary: RunSummary | null, };

export type StackingSetConfig = { config: StackingConfig, excludedFrameIds: Array<number>, 
/**
 * `true` when the frame set has no stored override row at all — the
 * resolved config above is entirely the global default's.
 */
isDefault: boolean, updatedAt: string | null, };

export type StackingPaths = { working: PathSetting, output: PathSetting, };

export type WorkUsage = { calibratedBytes: number, registeredBytes: number, lnBytes: number, runsBytes: number, 
/**
 * M3 Task 2: bytes under `rej/` — per-run rejection-bitmap temporaries
 * (spec §6.2, ruling R-M3-8), never a `stacking_artifacts` row.
 */
rejBytes: number, totalBytes: number, };

export type CleanupWhat = "registered" | "intermediates" | "all";

export type GroupStats = { 
/**
 * The group's total frame count, before the min-weight drop.
 */
frames: number, included: number, droppedBelowMinWeight: number, 
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
rejectedLowFraction: number, rejectedHighFraction: number, 
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
masterNoise: Array<number>, masterLocation: Array<number>, masterScale: Array<number>, 
/**
 * Per plane, of the included frame with the highest `weight.normalized_mean`.
 */
bestSubNoise: Array<number>, masterPsfSnr: Array<number>, bestSubPsfSnr: Array<number>, 
/**
 * `master_psf_snr / best_sub_psf_snr`; `0.0` (with a warn) when the
 * best sub's own PSF SNR is not positive.
 */
snrGain: Array<number>, masterFwhmPx: Array<number>, masterEccentricity: Array<number>, 
/**
 * `Σ exposure_i · weight_i` over included frames, `weight_i` the mean
 * over planes of frame i's normalized weight (`FrameWeight::normalized_mean`).
 */
weightedExposureS: number, totalExposureS: number, readMs: number, combineMs: number, bytesRead: number, 
/**
 * Included frames integrated with an LN grid (M2 Task 7) — `0` when the
 * caller passes no grids at all (`GroupInput.ln: None`: local
 * normalization off for this group, or `stacking::run` never resolved
 * any), otherwise the count of included frames whose own `ln[i]` was
 * `Some`.
 */
lnFrames: number, };

export type DrizzleStats = { scale: number, outWidth: number, outHeight: number, 
/**
 * Included frames this pass drizzled (whether or not every one
 * actually contributed — a frame skipped for zero weight still counts,
 * mirroring `DrizzleInput::frames`'s own length).
 */
frames: number, kernel: DrizzleKernel, dropShrink: number, usedWeights: boolean, usedRejection: boolean, 
/**
 * Of `frames`, how many actually had their LN grids applied (`0` when
 * local normalization was off for the run).
 */
lnFrames: number, 
/**
 * Per plane, measured on the drizzled (output-grid) planes.
 */
fwhmPx: Array<number>, eccentricity: Array<number>, noise: Array<number>, 
/**
 * Per plane, the fraction of output pixels with `W > 0`.
 */
coverage: Array<number>, readMs: number, depositMs: number, bytesRead: number, };

