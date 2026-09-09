// Pure presentation helpers for the Stacking pipeline board (Plan 5b Task 2).
// `stageSummary` and `rowState` must never read run state beyond the
// `progress`/`outcome` arguments handed to them — no imports of hooks,
// no side effects — so the board and (later) the inspector headers never
// disagree (plan Ruling 8).

import type {
  Combination,
  DistortionChoice,
  DrizzleKernel,
  Interpolation,
  ModelChoice,
  OutputNormalization,
  PlanBlocker,
  PsfModel,
  RejectionChoice,
  RejectionNormalization,
  Stage,
  StackingConfig,
  StackingPlan,
  WeightMode,
} from '../../types/stacking';
import type { FlatNormMode } from '../../types/models';
import type { RunOutcome, RunProgress } from '../../hooks/useStackingRuns';

/** Backend pipeline stages, in execution order. */
export const STAGES: readonly Stage[] = [
  'calibrate',
  'measure',
  'reference',
  'register',
  'normalize',
  'integrate',
  'drizzle',
  'output',
];

/** Board rows: the backend stages plus the display-only Debayer row, which
 *  never appears as a `stacking-progress` `stage` value — it mirrors
 *  `calibrate`'s own state (see `rowState` below). */
export type BoardStage = Stage | 'debayer';

export type RowState =
  | 'ready'
  | 'blocked'
  | 'stale'
  | 'queued'
  | 'running'
  | 'done'
  | 'skipped'
  | 'failed'
  | 'off';

// ── Label maps ──────────────────────────────────────────────────────────

export function modelLabel(v: ModelChoice): string {
  switch (v) {
    case 'auto': return 'Auto';
    case 'similarity': return 'Similarity';
    case 'affine': return 'Affine';
    case 'homography': return 'Homography';
  }
}

export function distortionLabel(v: DistortionChoice): string {
  switch (v) {
    case 'off': return 'off';
    case 'polynomial2': return 'polynomial-2';
    case 'polynomial3': return 'polynomial-3';
    case 'polynomial4': return 'polynomial-4';
    case 'auto': return 'auto';
  }
}

export function interpolationLabel(v: Interpolation): string {
  switch (v) {
    case 'nearest': return 'nearest';
    case 'bilinear': return 'bilinear';
    case 'bicubicSpline': return 'bicubic spline';
    case 'bicubicBSpline': return 'bicubic B-spline';
    case 'lanczos3': return 'Lanczos-3';
    case 'lanczos4': return 'Lanczos-4';
    case 'mitchellNetravali': return 'Mitchell-Netravali';
  }
}

export function weightModeLabel(v: WeightMode): string {
  switch (v) {
    case 'psfSignalWeight': return 'PSF signal';
    case 'psfSnr': return 'PSF SNR';
    case 'noise': return 'noise';
    case 'formula': return 'formula';
    case 'exposure': return 'exposure';
    case 'keyword': return 'keyword';
    case 'none': return 'none';
  }
}

export function psfModelLabel(v: PsfModel): string {
  switch (v) {
    case 'auto': return 'Auto';
    case 'moffat4': return 'Moffat-4';
  }
}

export function flatNormModeLabel(v: FlatNormMode): string {
  switch (v) {
    case 'centralThird': return 'central third';
    case 'pixinsightTrimmed': return 'trimmed';
  }
}

export function outputNormLabel(v: OutputNormalization): string {
  switch (v) {
    case 'none': return 'none';
    case 'additive': return 'additive';
    case 'additiveWithScaling': return 'additive+scaling';
    case 'multiplicative': return 'multiplicative';
    case 'multiplicativeWithScaling': return 'multiplicative+scaling';
  }
}

export function rejectionNormLabel(v: RejectionNormalization): string {
  switch (v) {
    case 'none': return 'none';
    case 'scaleZeroOffset': return 'scale-zero-offset';
    case 'equalizeFluxes': return 'equalize-fluxes';
    case 'local': return 'local';
  }
}

export function combinationLabel(v: Combination): string {
  switch (v) {
    case 'average': return 'Average';
    case 'median': return 'Median';
  }
}

export function rejectionLabel(v: RejectionChoice): string {
  switch (v.method) {
    case 'auto': return 'auto rejection';
    case 'none': return 'no rejection';
    case 'percentileClip': return `percentile clip ${v.low}/${v.high}`;
    case 'sigmaClip': return `sigma clip ${v.sigmaLow}/${v.sigmaHigh}`;
    case 'winsorizedSigma': return `winsorized sigma ${v.sigmaLow}/${v.sigmaHigh}`;
    case 'linearFitClip': return `linear-fit clip ${v.sigmaLow}/${v.sigmaHigh}`;
  }
}

export function kernelLabel(v: DrizzleKernel): string {
  switch (v) {
    case 'square': return 'square';
    case 'circle': return 'circle';
    case 'gaussian': return 'gaussian';
  }
}

export function cleanupLabel(v: StackingConfig['output']['cleanup']): string {
  switch (v) {
    case 'keepAll': return 'keep all';
    case 'deleteRegistered': return 'delete registered';
    case 'deleteIntermediates': return 'delete intermediates';
  }
}

// ── stageSummary ────────────────────────────────────────────────────────

/**
 * One-line description of a stage's current configuration. Pure function of
 * `config` alone — never reads plan/progress/outcome (Ruling 8), so this is
 * safe to call from both the board row and (Task 3) the inspector header.
 */
export function stageSummary(stage: BoardStage, config: StackingConfig): string {
  switch (stage) {
    case 'calibrate': {
      const parts = [
        config.calibration.flatNorm
          ? `flat-norm ${flatNormModeLabel(config.calibration.flatNormMode)}`
          : 'flat-norm off',
        config.calibration.hotPixelCorrection ? 'hot-pixel correction' : 'hot-pixel off',
      ];
      return parts.join(' · ');
    }
    case 'debayer':
      return config.calibration.debayerOsc ? 'VNG debayer' : 'Debayer off';
    case 'measure':
      return `${weightModeLabel(config.measurement.weightMode)} weight · ${psfModelLabel(config.measurement.psfModel)} PSF · max ${config.measurement.maxStars} stars`;
    case 'reference':
      return config.reference.mode === 'auto' ? 'Auto (highest-weight frame)' : 'Manual selection';
    case 'register':
      return `${modelLabel(config.registration.model)} model · distortion ${distortionLabel(config.registration.distortion)} · ${interpolationLabel(config.registration.interpolation)} · clamp ${config.registration.clampingThreshold.toFixed(2)} · ${config.registration.maxStars} stars`;
    case 'normalize': {
      const local = config.normalization.local;
      if (local.enabled) {
        const localScale = local.localScale ? ' · local scale' : '';
        return `Local · tile ${local.scale}px · ${local.referenceFrames} reference frames · ${psfModelLabel(local.psfModel)} PSF${localScale}`;
      }
      return `Global · ${outputNormLabel(config.normalization.output)} output · ${rejectionNormLabel(config.normalization.rejection)} rejection`;
    }
    case 'integrate':
      return `${combinationLabel(config.integration.combination)} · ${rejectionLabel(config.integration.rejection)} · min weight ${config.integration.minWeight.toFixed(2)}`;
    case 'drizzle':
      if (!config.drizzle.enabled) return 'Off';
      return `${config.drizzle.scale}× · ${kernelLabel(config.drizzle.kernel)} kernel · drop ${config.drizzle.dropShrink.toFixed(2)}`;
    case 'output':
      return `${config.output.format.toUpperCase()} · ${cleanupLabel(config.output.cleanup)}`;
  }
}

// ── rowState ────────────────────────────────────────────────────────────

/** Which board stage a plan blocker's `code` belongs to. `unsupported` is
 *  deliberately absent — the backend reuses that one code for both the LN
 *  and drizzle "arrives in a later milestone" blockers (`plan.rs` Gate 6),
 *  so it is disambiguated below by which optional stage is actually turned
 *  on, not by parsing the blocker's message text. */
const BLOCKER_STAGE: Partial<Record<string, BoardStage>> = {
  masters: 'calibrate',
  links: 'calibrate',
  masterFiles: 'calibrate',
  reference: 'reference',
  folders: 'output',
  space: 'output',
  frames: 'measure',
};

function isBlockedBy(stage: BoardStage, blockers: readonly PlanBlocker[], config: StackingConfig): boolean {
  for (const b of blockers) {
    if (b.code === 'unsupported') {
      if (stage === 'normalize' && config.normalization.local.enabled) return true;
      if (stage === 'drizzle' && config.drizzle.enabled) return true;
      continue;
    }
    if (BLOCKER_STAGE[b.code] === stage) return true;
  }
  return false;
}

/**
 * The board row's current state. Order of precedence: the stage's own
 * "off" check, staleness, blockers, live run progress, the last run's
 * outcome, and finally the `ready` default.
 */
export function rowState(
  stage: BoardStage,
  plan: StackingPlan | null,
  progress: RunProgress | undefined,
  outcome: RunOutcome | undefined,
  config: StackingConfig,
): RowState {
  // Debayer is display-only — it mirrors `calibrate`'s own state for sets
  // that actually have an OSC group, and is `off` otherwise.
  if (stage === 'debayer') {
    const hasOsc = plan?.groups.some((g) => g.colorMode === 'osc') ?? false;
    if (!hasOsc) return 'off';
    return rowState('calibrate', plan, progress, outcome, config);
  }

  // Drizzle is the only stage whose toggle turns the row fully off. Local
  // normalization stays `ready` (labelled "global") when its toggle is off
  // — LN is the optional PART of the `normalize` stage, not the whole row.
  if (stage === 'drizzle' && !config.drizzle.enabled) return 'off';

  if (plan?.staleStages.includes(stage)) return 'stale';

  if (plan && isBlockedBy(stage, plan.blockers, config)) return 'blocked';

  if (progress) {
    const mine = STAGES.indexOf(stage);
    const cur = STAGES.indexOf(progress.stage);
    if (mine === cur) return 'running';
    return mine < cur ? 'done' : 'queued';
  }

  // Coarse, run-wide fallback — Task 4's per-group/per-frame status (from
  // `get_stacking_run`) will replace this with a real per-stage read.
  if (outcome) {
    if (outcome.success) return 'done';
    if (outcome.cancelled) return 'skipped';
    return 'failed';
  }

  return 'ready';
}

// ── stableStringify ─────────────────────────────────────────────────────

/**
 * Canonical `JSON.stringify` with every object's keys sorted, at every
 * level (arrays keep their order — order is meaningful there, unlike an
 * object's key order). Used by the inspector's preset selector (Task 3,
 * plan Ruling 1) to compare the draft config against each built-in preset
 * without a field-reorder producing a false "Custom" label.
 */
export function stableStringify(value: unknown): string {
  return JSON.stringify(sortKeysDeep(value));
}

function sortKeysDeep(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sortKeysDeep);
  if (value !== null && typeof value === 'object') {
    const sorted: Record<string, unknown> = {};
    for (const key of Object.keys(value as Record<string, unknown>).sort()) {
      sorted[key] = sortKeysDeep((value as Record<string, unknown>)[key]);
    }
    return sorted;
  }
  return value;
}
