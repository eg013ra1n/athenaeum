// AUTO-GENERATED from Rust by athenaeum-core/src/ts_export.rs — do not edit.
// Regenerate: TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract

import type { ExportReadiness, FlatNormMode, LightCalParams, PathSetting } from './models';

export type Interpolation = "nearest" | "bilinear" | "bicubicSpline" | "bicubicBSpline" | "lanczos3" | "lanczos4" | "mitchellNetravali";

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

export type ModelChoice = "auto" | "similarity" | "affine" | "homography";

export type DistortionChoice = "off" | "polynomial2" | "polynomial3" | "polynomial4" | "auto";

export type DetectionConfig = { minSnr: number, maxEccentricity: number, };

export type RegistrationConfig = { model: ModelChoice, distortion: DistortionChoice, interpolation: Interpolation, clampingThreshold: number, maxStars: number, ransacTolerancePx: number, ransacMaxIterations: number, maxRmsPx: number, failOnMaxRms: boolean, detection: DetectionConfig, writeRegisteredFrames: boolean, };

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
 * Local (small-scale) normalization settings (spec §5.2, M2). Carried
 * here as an opaque, defaulted block so a stored config round-trips
 * before M2 lands — `integrate_group` never reads it.
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

export type GroupingConfig = { exposureToleranceSec: number, };

export type MeasurementConfig = { weightMode: WeightMode, psfModel: PsfModel, 
/**
 * Detection cap (spec §9.2 `maxStars`).
 */
maxStars: number, formula: FormulaWeights, 
/**
 * FITS keyword `WeightMode::Keyword` reads its value from.
 */
keyword: string, };

export type ReferenceMode = "auto" | "manual";

export type ReferenceConfig = { mode: ReferenceMode, };

export type DrizzleKernel = "square" | "circle" | "gaussian";

export type DrizzleConfig = { enabled: boolean, scale: number, dropShrink: number, kernel: DrizzleKernel, useRejection: boolean, useWeights: boolean, useLocalNormalization: boolean, writeWeightMap: boolean, };

export type OutputFormat = "fits";

export type CleanupPolicy = "keepAll" | "deleteRegistered" | "deleteIntermediates";

export type OutputConfig = { format: OutputFormat, cleanup: CleanupPolicy, };

export type PathsConfig = { workingDir: string | null, outputDir: string | null, };

export type StackingConfig = { version: number, grouping: GroupingConfig, calibration: CalibratedLightOptions, measurement: MeasurementConfig, selection: SelectionConfig, reference: ReferenceConfig, registration: RegistrationConfig, normalization: NormalizationConfig, integration: IntegrationConfig, drizzle: DrizzleConfig, output: OutputConfig, paths: PathsConfig, };

export type StackingPreset = "default" | "fastPreview" | "maximumQuality";

export type StackingPresets = { default: StackingConfig, fastPreview: StackingConfig, maximumQuality: StackingConfig, };

export type ColorMode = "mono" | "osc";

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
lnCached: number, };

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

export type StackingRunGroupRow = { id: number, runId: number, groupKey: string, instrume: string | null, colorMode: string, filter: string | null, binning: number | null, width: number | null, height: number | null, exposure: number | null, frameCount: number, includedCount: number, masterPath: string | null, drizzlePath: string | null, rejectionLowPath: string | null, rejectionHighPath: string | null, statsJson: string | null, status: string, error: string | null, };

export type StackingRunFrameRow = { id: number, runId: number, groupId: number, frameId: number, included: boolean, exclusionReason: string | null, weight: number | null, weightChannelsJson: string | null, metricsJson: string | null, regStatus: string | null, regModel: string | null, regRmsPx: number | null, regInliers: number | null, regInlierRatio: number | null, regFlipped: boolean | null, rejectedFraction: number | null, };

export type StackingRunSummary = { run: StackingRunRow, groupCount: number, masterPaths: Array<string>, };

export type SummaryReference = { frameId: number | null, filename: string | null, mode: ReferenceMode, weight: number | null, };

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
 * Stage 6 (local normalization, M2): the group's LN reference file
 * (`ln/<group>/reference.fits`, spec §9.5), when local normalization
 * ran for this group at all — `None` when it is disabled, the group
 * never reached Output (skipped/failed), or a ruling-R3 fallback to
 * global normalization applied. `#[serde(default)]`, see
 * `SummaryFrame::ln_scale`'s own doc.
 */
lnReferencePath: string | null, frames: Array<SummaryFrame>, };

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

export type WorkUsage = { calibratedBytes: number, registeredBytes: number, lnBytes: number, runsBytes: number, totalBytes: number, };

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

