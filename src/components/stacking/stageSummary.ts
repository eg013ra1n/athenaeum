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

/**
 * The subset of `STAGES` that actually get their own `StageTiming` entry in
 * `RunSummary.stages` (`run.rs`'s `rc.timings.push(…)` call sites) — used by
 * `rowState`'s `finishedStages` derivation (A5). `normalize` (final fix
 * wave, ruling R5 — reverses the earlier fix-round-1 call: the acceptance
 * run measured local normalization as the run's single most expensive
 * stage, so it cannot stay invisible) gets a real entry ONLY when LN was
 * active for the run — a `normalize` timing simply being absent from
 * `finishedStages` is already exactly what "a stage without an entry counts
 * as done when a later stage has one" (see the `outcome` branch below)
 * handles correctly, the same way `masters` having no timing when there is
 * no masters work already does. `drizzle` (M3 Task 6) joins the same way —
 * off for most runs, its own timing simply absent from `finishedStages`
 * when the stage didn't run. This list is now every member of `STAGES`, so
 * `TIMED_STAGES.indexOf` below never returns -1 for a real `Stage`.
 */
const TIMED_STAGES: readonly Stage[] = [
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

export type RowState =
  | 'ready'
  | 'blocked'
  | 'stale'
  | 'queued'
  | 'running'
  | 'done'
  | 'skipped'
  | 'failed'
  | 'cancelled'
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

/**
 * Up to 3 decimal places, trailing zeros trimmed — `0.005` stays `0.005`
 * instead of rounding away to `0.01` (Plan 5b final fix wave, review item
 * A1). Only `minWeight` needed this; every other number in `stageSummary`
 * keeps its own `toFixed`.
 */
function formatUpTo3Decimals(v: number): string {
  return Number(v.toFixed(3)).toString();
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
      return `${weightModeLabel(config.measurement.weightMode)} weight · ${psfModelLabel(config.measurement.psfModel)} PSF · max ${config.measurement.maxStars} stars · σ ${config.measurement.detectionSigma}${
        config.measurement.seedPrefilter === 'median3' ? ' · 3×3 median seeds' : ''
      }`;
    case 'reference':
      if (config.reference.mode !== 'auto') return 'Manual selection';
      return config.reference.twoPass
        ? 'Auto (highest-weight frame · two-pass)'
        : 'Auto (highest-weight frame)';
    case 'register': {
      // M4b (ruling R-M4b-4): native mode changes what the whole stage
      // delivers — a reference and a geometry per group — so it leads the
      // row; co-registered is the default and says nothing.
      const geometry = config.registration.geometry === 'native' ? 'native · ' : '';
      return `${geometry}${modelLabel(config.registration.model)} model · distortion ${distortionLabel(config.registration.distortion)} · ${interpolationLabel(config.registration.interpolation)} · clamp ${config.registration.clampingThreshold.toFixed(2)} · ${config.registration.maxStars} stars`;
    }
    case 'normalize': {
      const local = config.normalization.local;
      if (local.enabled) {
        return `Local · scale ${local.scale} · ref ${local.referenceFrames} frames`;
      }
      return `Global · ${outputNormLabel(config.normalization.output)} · ${config.normalization.scaleEstimator.toUpperCase()}`;
    }
    case 'integrate': {
      const lnRejection = config.normalization.rejection === 'local' ? ' · LN rejection' : '';
      return `${combinationLabel(config.integration.combination)} · ${rejectionLabel(config.integration.rejection)} · min weight ${formatUpTo3Decimals(config.integration.minWeight)}${lnRejection}`;
    }
    case 'drizzle':
      if (!config.drizzle.enabled) return 'Off';
      return `${config.drizzle.scale}× · ${kernelLabel(config.drizzle.kernel)} kernel · drop ${config.drizzle.dropShrink.toFixed(2)}`;
    case 'output':
      return `${config.output.format.toUpperCase()} · ${cleanupLabel(config.output.cleanup)}`;
  }
}

// ── drizzleEstimate ─────────────────────────────────────────────────────

/**
 * Provisional per-plane drizzle throughput at 2x scale, seconds/plane/frame
 * — measured nowhere yet, a placeholder pending the M3 acceptance run
 * (Task 7 re-fits this from real numbers). Time for a 1x/3x run scales by
 * `(scale/2)²` relative to this, since work scales with the output pixel
 * count.
 */
export const DRIZZLE_SECONDS_PER_PLANE_AT_2X = 1.2;

export interface DrizzleEstimate {
  /** Bitmap-temporary bytes (`0` when `useRejection` is off). */
  bitmapBytes: number;
  /** Output-file bytes (drizzled master, plus the weight map when on). */
  outputBytes: number;
  /** Estimated wall-clock seconds for the whole set's drizzle pass. */
  seconds: number;
  /** `true` when at least one group in `plan.groups` has no anchor
   *  geometry (`anchorWidth`/`anchorHeight` both `null` — an empty group,
   *  unreachable in practice) and was skipped rather than counted. */
  incomplete: boolean;
}

/**
 * Pure arithmetic for the Drizzle panel's estimate line (M3 Task 6): per
 * group, with `W`/`H` the group's anchor-member geometry, `planes` 3 for an
 * OSC group / 1 for mono, `n` the group's included-frame count and `s` the
 * configured drizzle scale —
 *
 * - bitmap bytes: `n·planes·ceil(W/64)·8·H`, only when `config.drizzle.
 *   useRejection` (the rejection-bitmap temporaries the M3 spec §6.2
 *   allocates during integration; skipped devices for it are 0 when
 *   rejection is off for the drizzle pass).
 * - output bytes: `planes·(W·s)·(H·s)·4·(writeWeightMap ? 2 : 1)` (a
 *   float32 drizzled master, doubled when the weight map is also written).
 * - seconds: `n·planes·DRIZZLE_SECONDS_PER_PLANE_AT_2X·(s/2)²`.
 *
 * Never reads run/progress state — same purity contract as `stageSummary`/
 * `rowState` above (Ruling 8) — so it is safe to call from both the
 * frame-set tab's inspector and the Settings global-defaults one (which has
 * no plan at all: `plan` is `null` there, and the caller is expected to
 * render its own "no plan" fallback rather than calling this with an empty
 * one — an empty/null plan here simply yields all-zero, `incomplete: false`,
 * which reads as "nothing to estimate" rather than "no plan loaded").
 */
export function drizzleEstimate(config: StackingConfig, plan: StackingPlan | null): DrizzleEstimate {
  const d = config.drizzle;
  const s = d.scale;
  let bitmapBytes = 0;
  let outputBytes = 0;
  let seconds = 0;
  let incomplete = false;

  for (const g of plan?.groups ?? []) {
    const { anchorWidth: w, anchorHeight: h, includedCount: n, colorMode } = g;
    if (w == null || h == null) {
      incomplete = true;
      continue;
    }
    const planes = colorMode === 'osc' ? 3 : 1;
    if (d.useRejection) {
      bitmapBytes += n * planes * Math.ceil(w / 64) * 8 * h;
    }
    outputBytes += planes * (w * s) * (h * s) * 4 * (d.writeWeightMap ? 2 : 1);
    seconds += n * planes * DRIZZLE_SECONDS_PER_PLANE_AT_2X * (s / 2) ** 2;
  }

  return { bitmapBytes, outputBytes, seconds, incomplete };
}

// ── rowState ────────────────────────────────────────────────────────────

/** Which board stage a plan blocker's `code` belongs to. `unsupported` is
 *  deliberately absent — historically the backend reused that one code for
 *  both the LN and drizzle "arrives in a later milestone" blockers
 *  (`plan.rs` Gate 6), disambiguated below by which optional stage is
 *  actually turned on rather than by parsing the blocker's message text.
 *  As of M2/M3, `plan.rs`'s own Gate 6 comment records that LN's blocker was
 *  lifted first (M2 fix round 1) and drizzle's outright-refusal blocker
 *  after it (M3 Task 5) — the ONLY `unsupported` blocker left is an
 *  out-of-range drizzle `scale`, still surfaced on the drizzle row. The
 *  `normalize` branch below is kept as a harmless no-op (no blocker source
 *  names it any more) rather than assumed dead and removed.
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
 * | 4 | outcome    | no live run; the last finished run's per-row status, from |
 * |   |            | `finishedStages` when the caller has it (A5), else the    |
 * |   |            | old coarse pass/fail/cancel.                               |
 * | 5 | ready      | nothing else applies.                                     |
 *
 * `progress` must be checked BEFORE `blockers`/`stale`: the backend's
 * `staleStages` only ever names the per-frame stages (calibrate/measure/
 * register — `plan.rs`'s Gate 6 note), and the tab never re-plans mid-run,
 * so with the old order those three rows rendered "Stale" — and suppressed
 * their own live progress bar — for the whole run.
 *
 * `finishedStages` (Plan 5b final fix wave, click-through item A5) is the
 * SAME run's `RunSummary.stages` list (just the `stage` field of each
 * `StageTiming`), when the caller has it loaded — the caller's job to make
 * sure it is the same run `outcome` describes, never a different one (an
 * older run the user picked in the Results panel, say); pass `null`/
 * `undefined` otherwise and step 4 falls back to the old coarse read.
 * Typed example (no test runner for this file — see the skill's own note):
 * ```ts
 * // A run that got through Calibrate (no masters work, so no `masters`
 * // timing — that row is 'off') and was cancelled inside Measure:
 * const finished: Stage[] = ['calibrate'];
 * rowState('masters', plan, undefined, cancelledOutcome, config, finished);   // 'off'
 * rowState('calibrate', plan, undefined, cancelledOutcome, config, finished); // 'done'
 * rowState('measure', plan, undefined, cancelledOutcome, config, finished);   // 'cancelled'
 * rowState('reference', plan, undefined, cancelledOutcome, config, finished); // 'skipped'
 * ```
 */
export function rowState(
  stage: BoardStage,
  plan: StackingPlan | null,
  progress: RunProgress | undefined,
  outcome: RunOutcome | undefined,
  config: StackingConfig,
  finishedStages?: readonly Stage[] | null,
): RowState {
  // 0. Debayer is display-only — it mirrors `calibrate`'s own state for
  // sets that actually have an OSC group, and is `off` otherwise.
  if (stage === 'debayer') {
    const hasOsc = plan?.groups.some((g) => g.colorMode === 'osc') ?? false;
    if (!hasOsc) return 'off';
    return rowState('calibrate', plan, progress, outcome, config, finishedStages);
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

  // 4. The last finished run's per-row status.
  if (outcome) {
    if (outcome.success) return 'done';
    // A5: the run stopped in the first timed stage AFTER the last one that
    // has a timing entry — every stage up to and including the last timed
    // one completed (a stage without an entry of its own counts as done
    // when a later stage has one: `masters` pushes no timing when there is
    // no masters work, which is most runs, and — final fix wave, ruling R5
    // — `normalize` pushes no timing when LN was off for the run, the same
    // way), the stop row reads 'cancelled'/'failed', everything after it
    // never ran. `integrate`'s timing is pushed once after the whole group
    // loop, so a cancel inside any group's integration reads 'cancelled' on
    // the Integrate row. When nothing finished at all and the plan has no
    // masters work, the stop row is `calibrate` — the Masters row is 'off'
    // (returned above) and must not absorb the stop.
    if (finishedStages) {
      const mine = TIMED_STAGES.indexOf(stage);
      if (mine === -1) {
        // M3 Task 6: `TIMED_STAGES` now covers every member of `STAGES`
        // (drizzle joined it), so this branch is unreachable for any real
        // `Stage` today — kept as a defensive fallback (never guess a row's
        // state) rather than assumed permanently impossible.
      } else {
        let lastFinished = -1;
        for (const s of finishedStages) {
          lastFinished = Math.max(lastFinished, TIMED_STAGES.indexOf(s));
        }
        let stopAt = lastFinished + 1;
        const mastersHasWork = (plan?.mastersToBuild.length ?? 0) > 0;
        if (stopAt === 0 && !mastersHasWork) stopAt = 1;
        if (stopAt >= TIMED_STAGES.length || mine < stopAt) return 'done';
        if (mine === stopAt) return outcome.cancelled ? 'cancelled' : 'failed';
        return 'skipped';
      }
    }
    // No matching run detail loaded yet (or a non-timeable stage) — old
    // coarse read (every row the same state) rather than a wrong guess.
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
