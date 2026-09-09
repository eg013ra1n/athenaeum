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

/** Backend pipeline stages, in execution order. `'masters'` (stage 0.5, owner
 *  requirement 2026-09-09 — the run builds its own missing calibration
 *  masters) runs first. */
export const STAGES: readonly Stage[] = [
  'masters',
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
 *
 * `'masters'` is the ONE exception (Plan 5b Task 8, owner requirement
 * 2026-09-09): stage 0.5 has no config knobs of its own to summarize — its
 * "configuration" IS the plan's own work list — so this row's summary reads
 * `plan.mastersToBuild` instead. `plan` is optional and defaults to `null`
 * so every other stage's call site (and the inspector header, which may not
 * always have a plan handy) keeps working unchanged.
 */
export function stageSummary(
  stage: BoardStage,
  config: StackingConfig,
  plan?: StackingPlan | null,
): string {
  switch (stage) {
    case 'masters': {
      const items = plan?.mastersToBuild ?? [];
      const toBuild = items.filter((m) => m.kind === 'build').length;
      const toRebuild = items.filter((m) => m.kind === 'rebuild').length;
      if (toBuild === 0 && toRebuild === 0) return 'Nothing to build';
      const parts: string[] = [];
      if (toBuild > 0) parts.push(`${toBuild} to build`);
      if (toRebuild > 0) parts.push(`${toRebuild} to rebuild`);
      return parts.join(', ');
    }
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
 *  on, not by parsing the blocker's message text.
 *
 *  `masters`/`masterFiles` moved from `calibrate` to `masters` (Plan 5b Task
 *  8, owner requirement 2026-09-09): the gate reinterpretation means these
 *  two codes now name a raw set/master stage 0.5 CANNOT build/rebuild —
 *  still the reason calibrate has nothing to work with, but the row the
 *  operator needs to look at is the new Masters one. `links` stays on
 *  `calibrate` — an unlinked light is a calibrate-stage input problem no
 *  master build can fix. */
const BLOCKER_STAGE: Partial<Record<string, BoardStage>> = {
  masters: 'masters',
  masterFiles: 'masters',
  links: 'calibrate',
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
 * The board row's current state.
 *
 * Precedence (fix round 1, Critical #1 — `progress` must outrank `stale`):
 *
 * | # | check      | when it applies                                        |
 * | - | ---------- | ------------------------------------------------------- |
 * | 0 | off        | `debayer` with no OSC group / `drizzle` disabled — an    |
 * |   |            | unconditional early return; neither is a real member of |
 * |   |            | `STAGES`/never runs, so nothing below may override it.   |
 * | 1 | progress   | a run is live right now — `running`/`done`/`queued` from |
 * |   |            | its own stage index is the only authoritative signal.    |
 * | 2 | blockers   | no live run; the plan says this stage can't proceed.     |
 * | 3 | stale      | no live run, not blocked; a cached artifact is out of    |
 * |   |            | date and a re-run would redo this stage.                 |
 * | 4 | outcome    | no live run; the last finished run's coarse pass/fail/   |
 * |   |            | cancel (Task 4 refines this with per-group status).       |
 * | 5 | ready      | nothing else applies.                                     |
 *
 * `progress` must be checked BEFORE `blockers`/`stale`: the backend's
 * `staleStages` only ever names the per-frame stages (calibrate/measure/
 * register — `plan.rs`'s Gate 6 note), and the tab never re-plans mid-run,
 * so with the old order those three rows rendered "Stale" — and suppressed
 * their own live progress bar — for the whole run.
 */
export function rowState(
  stage: BoardStage,
  plan: StackingPlan | null,
  progress: RunProgress | undefined,
  outcome: RunOutcome | undefined,
  config: StackingConfig,
): RowState {
  // 0. Debayer is display-only — it mirrors `calibrate`'s own state for
  // sets that actually have an OSC group, and is `off` otherwise.
  if (stage === 'debayer') {
    const hasOsc = plan?.groups.some((g) => g.colorMode === 'osc') ?? false;
    if (!hasOsc) return 'off';
    return rowState('calibrate', plan, progress, outcome, config);
  }

  // 0. Drizzle is the only stage whose toggle turns the row fully off. Local
  // normalization stays `ready` (labelled "global") when its toggle is off
  // — LN is the optional PART of the `normalize` stage, not the whole row.
  if (stage === 'drizzle' && !config.drizzle.enabled) return 'off';

  // 0. Masters (stage 0.5, Plan 5b Task 8) is off when there is nothing to
  // build/rebuild AND no masters/masterFiles blocker either — most runs
  // never touch this stage at all. When either is true, fall through to the
  // normal precedence chain below (progress/blockers/outcome/ready) exactly
  // like any other stage — `masters` is a real member of `STAGES` now, so
  // step 1's progress-index comparison already handles it correctly.
  if (stage === 'masters') {
    const hasWork = (plan?.mastersToBuild.length ?? 0) > 0;
    const hasBlocker = plan ? isBlockedBy('masters', plan.blockers, config) : false;
    if (!hasWork && !hasBlocker) return 'off';
  }

  // 1. Live progress outranks everything below it.
  if (progress) {
    const mine = STAGES.indexOf(stage);
    const cur = STAGES.indexOf(progress.stage);
    if (mine === cur) return 'running';
    return mine < cur ? 'done' : 'queued';
  }

  // 2. Blockers.
  if (plan && isBlockedBy(stage, plan.blockers, config)) return 'blocked';

  // 3. Staleness.
  if (plan?.staleStages.includes(stage)) return 'stale';

  // 4. Coarse, run-wide fallback — Task 4's per-group/per-frame status (from
  // `get_stacking_run`) will replace this with a real per-stage read.
  if (outcome) {
    if (outcome.success) return 'done';
    if (outcome.cancelled) return 'skipped';
    return 'failed';
  }

  // 5. Default.
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

/**
 * Every field except `paths` — the preset comparison (`StackingTab`'s
 * toolbar selector and, Plan 5b Task 5, `StackingSection`'s global-defaults
 * selector) ignores the folder override, which is never part of what makes
 * a config "Default"/"Fast preview"/"Maximum quality" (Task 3 "Decisions"
 * item 3). Exported here (moved out of `StackingTab.tsx`, which now imports
 * it) so the two preset selectors share one implementation instead of a
 * second hand copy.
 */
export function withoutPaths(config: StackingConfig): Omit<StackingConfig, 'paths'> {
  const { paths: _paths, ...rest } = config;
  return rest;
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
