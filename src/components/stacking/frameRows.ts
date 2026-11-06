import type {
  PlanProjectFrame,
  RunSummary,
  StackingRunDetail,
  StackingRunFrameRow,
  StackingRunRow,
  SummaryFrame,
} from '../../types/stacking';

/** The set's LIGHT frames, as `FrameSetDetail.tsx` derives them from its own
 *  `detail.nights` tree — plan 5b Task 4, Decisions item 3. This is the row
 *  source before any run has been recorded, and the filename fallback for a
 *  run row a summary hasn't reached yet (`StackingRunFrameRow` itself carries
 *  no filename — only `SummaryFrame`, which doesn't exist until the run's
 *  summary is written). */
export interface LightFrameRef {
  frameId: number;
  filename: string;
  /** The imaging night the frame belongs to, labelled as the Analysis tab
   *  labels it (`formatNightLabel`, e.g. `September 13–14, 2025`); `null` when
   *  the caller has no night. */
  night: string | null;
  /** The frame's exposure in seconds (`frame.exptime`); `null` when unknown. */
  exptimeSec: number | null;
}

/** The run-scoped slice the builders need — `groups`/`frames` and the run's
 *  id and reference mode, deliberately NOT the whole `StackingRunDetail`
 *  (whose `summary` is taken as its own, separately-nullable parameter
 *  below). Splitting it this way lets a caller exercise "run active, not yet
 *  finished" (`detail` present, `summary` still `null`) independently of "no
 *  run at all" (`detail` itself `null`) — the two states
 *  `StackingRunDetail.frames` being "present throughout the run" and
 *  `.summary` being "`null` until the run finishes" actually produce. */
export type RunFrameData = Pick<StackingRunDetail, 'groups' | 'frames'> & {
  run: Pick<StackingRunRow, 'id' | 'referenceMode'>;
};

/** What the selected run did with a frame (spec §6.4): one structured
 *  outcome, decided from the run row's own fields — never by parsing
 *  `exclusionReason`. Independent of the next run's exclusion list, which a
 *  row carries as `outOfNextRun`. */
export type OutcomeKind = 'reference' | 'included' | 'regFailed' | 'excluded' | 'notStackable';

export interface Outcome {
  kind: OutcomeKind;
  label: string;
  tone: 'accent' | 'success' | 'warning' | 'error';
  /** The muted text beside the chip: why, or which run, or `null`. */
  reason: string | null;
}

/** What a builder needs beyond the run: where a frame with no run row is
 *  planned to go, and which frame the set's next run is pinned to. */
export interface RowContext {
  /** The plan's group of a frame, by the frame id of the row (the plan
   *  handle for a project). Consulted only for a row with no run row. */
  planGroupByFrameId: Map<number, string>;
  /** The frame-set table's manual reference in effect; `null` for none. */
  pinnedFrameId: number | null;
}

export interface FrameRow {
  /** The row's exclusion key and React key: `String(frameId)` for a frame
   *  set, the frame uuid for a project. A project run row that carries no
   *  uuid is keyed `h:<handle>`, which no uuid can spell. */
  key: string;
  /** The frame id in the run's namespace (a catalog id for a set, a
   *  project-frame handle for a project); `null` only for a project frame
   *  that has no handle yet and no run row. */
  frameId: number | null;
  filename: string;
  /** Project table only: who published the frame; `null` for a set row and
   *  for a run row that left the plan (the run does not record it). */
  publisher: string | null;
  /** The run group's key when the selected run has a row for the frame, else
   *  the plan's group (`RowContext.planGroupByFrameId`), else `null`. */
  groupKey: string | null;
  /** Frame-set row: the label of the frame's imaging night
   *  (`LightFrameRef.night`). `null` for a project row and for a run row the
   *  set no longer lists. */
  night: string | null;
  exptimeSec: number | null;
  weight: number | null;
  fwhmPx: number | null;
  eccentricity: number | null;
  stars: number | null;
  regRmsPx: number | null;
  /** Stage 6 (local normalization, M2): the frame's own relative scale
   *  (`SummaryFrame.lnScale`) — `null` when LN never ran for this group's
   *  frame (disabled, a ruling-R3 fallback to global normalization, or the
   *  run's summary isn't loaded yet), same convention as every other
   *  summary-sourced metric column here. */
  lnScale: number | null;
  regInliers: number | null;
  /** Stage 5's alignment model string (`SummaryFrame.regModel` /
   *  `StackingRunFrameRow.regModel`), e.g. `homography` or
   *  `homography+polynomial3+wcs` — the trailing `+wcs` (M4b) marks a
   *  frame whose correspondence search was seeded from the stored plate
   *  solves instead of the quad matcher alone. `null` before the frame has
   *  registered in the selected run. */
  regModel: string | null;
  /** What the selected run did with the frame; `null` with no run row (and,
   *  for a project row, no plan reason either). */
  outcome: Outcome | null;
  /** The frame is in the exclusion list: the next run leaves it out,
   *  whatever the selected run did (Ruling 7's checkbox reflects this and
   *  nothing else). */
  outOfNextRun: boolean;
  /** False only for a project run row whose frame left the plan (excluded by
   *  the coordinator, remapped, no longer landed): it renders from the run's
   *  own data and carries neither the include checkbox nor the pin. */
  editable: boolean;
  /** The plan's `reason` the frame cannot be stacked on this device
   *  (`PlanProjectFrame.reason`), whatever `onDisk` says: a landed file can be
   *  unsupported or unopenable and has `onDisk` true AND a reason. `null` for
   *  a set row and a usable frame. */
  unavailable: string | null;
  /** Project row: usable on this device (`onDisk` and no reason). A frame
   *  without a manifest WCS is pinnable too (D2): it is plate-solved locally
   *  first. Set row: the frame is one of the set's current lights. */
  pinnable: boolean;
  /** This frame is the manual reference in effect. */
  pinned: boolean;
  /** What the pin's tooltip says beyond its default: the reason a frame
   *  cannot be pinned, or the note on a frame without a manifest WCS. */
  pinNote: string | null;
}

export type SortKey =
  | 'filename'
  | 'publisher'
  | 'groupKey'
  | 'night'
  | 'exptimeSec'
  | 'weight'
  | 'fwhmPx'
  | 'eccentricity'
  | 'stars'
  | 'regRmsPx'
  | 'lnScale'
  | 'regInliers'
  | 'regModel'
  | 'status';

/** D2: a frame without a manifest WCS is pinnable; the run plate-solves it. */
const NO_WCS_NOTE = 'No WCS in the manifest — plate-solved locally first';

/** The run's own `exclusionReason` of a failed registration starts with this;
 *  the chip already says "Reg failed", so the text beside it drops the echo. */
const REG_FAILED_PREFIX = 'registration failed: ';

/** The outcome of a run row, decided in this order (spec §6.4): a registration
 *  failure first (a failed-registration frame is always also `included =
 *  false` — `crates/athenaeum-core/src/stacking/run.rs`'s registration-failure
 *  branch sets both together — so checking `included` first would relabel it a
 *  generic exclusion and lose the more specific reason), then the run's
 *  reference, then any other exclusion, else included. */
function runOutcome(runRow: StackingRunFrameRow, run: RunFrameData['run']): Outcome {
  if (runRow.regStatus === 'failed') {
    const reason = runRow.exclusionReason;
    return {
      kind: 'regFailed',
      label: 'Reg failed',
      tone: 'error',
      reason: reason != null && reason.startsWith(REG_FAILED_PREFIX) ? reason.slice(REG_FAILED_PREFIX.length) : reason,
    };
  }
  if (runRow.regStatus === 'reference') {
    return { kind: 'reference', label: 'Reference', tone: 'accent', reason: `run #${run.id} · ${run.referenceMode}` };
  }
  if (!runRow.included) {
    return { kind: 'excluded', label: 'Excluded', tone: 'warning', reason: runRow.exclusionReason };
  }
  return { kind: 'included', label: 'Included', tone: 'success', reason: null };
}

/** The Frames table's row source (fix round 1, Important #2): the UNION of
 *  `lightFrames` (the set's current LIGHT frames) and `detail.frames` (the
 *  selected run's own frame rows), keyed by `frameId` — not `detail.frames`
 *  alone. A light added to the set after the selected run (or present under
 *  an older run than the one currently selected) has no row in
 *  `detail.frames`; without the union it silently had no row at all in the
 *  table, and therefore no include checkbox — the tab's one frame-level
 *  write (Ruling 7) simply couldn't reach it. Such a frame gets a
 *  synthetic "no run data yet" row (no group from the run, every metric
 *  empty, no outcome; the exclusion list alone says `outOfNextRun`) — the
 *  same shape every row had before any run existed.
 *
 *  `detail`/`summary` are deliberately separate parameters, not one
 *  `StackingRunDetail` — `detail.frames` is present throughout a run,
 *  `summary` (`RunSummary`, carrying the FWHM/eccentricity/seed metrics
 *  `StackingRunFrameRow` itself has no columns for) is `null` until the run
 *  finishes; keeping them apart lets a caller exercise "run active, not yet
 *  finished" separately from "no run at all" (`detail === null`). Pure — no
 *  React, safe to import and unit-exercise outside a component. */
export function buildFrameRows(
  lightFrames: LightFrameRef[],
  detail: RunFrameData | null,
  summary: RunSummary | null,
  excludedFrameIds: number[],
  ctx: RowContext,
): FrameRow[] {
  const excludedSet = new Set(excludedFrameIds);
  const lightByFrameId = new Map(lightFrames.map((f) => [f.frameId, f]));
  const groupKeyById = new Map((detail?.groups ?? []).map((g) => [g.id, g.groupKey]));
  const runRowByFrameId = new Map<number, StackingRunFrameRow>(
    (detail?.frames ?? []).map((r) => [r.frameId, r]),
  );
  const summaryByFrameId = summaryFramesByFrameId(summary);

  // Union of frame ids, lightFrames first (their own order) then any
  // run-row frame id lightFrames doesn't have (e.g. a frame removed from
  // the set since the selected run — still worth showing as part of that
  // run's own record).
  const frameIds: number[] = [];
  const seen = new Set<number>();
  for (const f of lightFrames) {
    if (!seen.has(f.frameId)) {
      seen.add(f.frameId);
      frameIds.push(f.frameId);
    }
  }
  for (const r of detail?.frames ?? []) {
    if (!seen.has(r.frameId)) {
      seen.add(r.frameId);
      frameIds.push(r.frameId);
    }
  }

  return frameIds.map((frameId) => {
    const runRow = runRowByFrameId.get(frameId);
    const lightRef = lightByFrameId.get(frameId);
    const s = summaryByFrameId.get(frameId);

    return {
      key: String(frameId),
      frameId,
      filename: (runRow ? s?.filename : undefined) ?? lightRef?.filename ?? `frame ${frameId}`,
      publisher: null,
      // A frame with no run row has no outcome; its group is the plan's, when
      // the context knows it.
      groupKey: runRow
        ? (groupKeyById.get(runRow.groupId) ?? null)
        : (ctx.planGroupByFrameId.get(frameId) ?? null),
      night: lightRef?.night ?? null,
      exptimeSec: lightRef?.exptimeSec ?? null,
      ...runMetrics(runRow, s),
      outcome: runRow && detail ? runOutcome(runRow, detail.run) : null,
      outOfNextRun: excludedSet.has(frameId),
      editable: true,
      unavailable: null,
      pinnable: lightRef !== undefined,
      pinned: frameId === ctx.pinnedFrameId,
      pinNote: null,
    };
  });
}

/** The prefix of a project run row's key when the row carries no frame uuid. */
export const HANDLE_KEY_PREFIX = 'h:';

/** A project run row's key: its frame uuid, or `h:<handle>` when the run row
 *  carries none (its project-frame row is gone). `h:` cannot collide with a
 *  uuid, and such a row is never joined to a plan row. */
function runRowKey(r: StackingRunFrameRow): string {
  return r.frameUuid ?? `${HANDLE_KEY_PREFIX}${r.frameId}`;
}

/** The project Frames table's row source (spec §12): every frame of the
 *  plan (`PlanProjectFrame`, stackable on this device or not), then the
 *  selected run's own rows for frames the plan no longer lists.
 *
 *  Rows are keyed by frame uuid, never by handle: a run row joins its plan
 *  row on `StackingRunFrameRow.frameUuid` (the documented join key) and its
 *  summary frame on the run row's own `frameId` (the run's handle namespace;
 *  a `SummaryFrame` carries the handle and no uuid).
 *
 *  A run row whose uuid has no plan row (the frame was excluded by the
 *  coordinator, remapped, or is no longer on disk) keeps the uuid as its key
 *  and renders from the run alone — the summary's file name, or `frame
 *  <handle>` — without the checkbox and the pin (`editable: false`): it
 *  never takes another row's uuid, and no write is possible for a frame the
 *  project no longer has. `excluded` holds the exclusion keys (uuids);
 *  `referenceFrameUuid` is the manual reference in effect, `null` for none.
 *  Pure, like `buildFrameRows`. */
export function buildProjectFrameRows(
  projectFrames: PlanProjectFrame[],
  detail: RunFrameData | null,
  summary: RunSummary | null,
  excluded: Set<string>,
  referenceFrameUuid: string | null,
  ctx: RowContext,
): FrameRow[] {
  const groupKeyById = new Map((detail?.groups ?? []).map((g) => [g.id, g.groupKey]));
  const runRowByKey = new Map<string, StackingRunFrameRow>(
    (detail?.frames ?? []).map((r) => [runRowKey(r), r]),
  );
  const summaryByHandle = summaryFramesByFrameId(summary);
  const planKeys = new Set<string>();

  const planRows = projectFrames.map((p): FrameRow => {
    planKeys.add(p.frameUuid);
    const runRow = runRowByKey.get(p.frameUuid);
    const s = runRow ? summaryByHandle.get(runRow.frameId) : undefined;
    const pinnable = p.onDisk && p.reason == null;
    const frameId = runRow?.frameId ?? p.handle;
    // The run's outcome is the record of that run; a frame that cannot be
    // stacked now still says so beside its name (`unavailable`). With no run
    // row there is no outcome unless the plan itself says the frame cannot be
    // stacked here.
    const outcome: Outcome | null =
      runRow && detail
        ? runOutcome(runRow, detail.run)
        : p.reason != null
          ? { kind: 'notStackable', label: 'Not stackable here', tone: 'warning', reason: p.reason }
          : null;
    return {
      key: p.frameUuid,
      frameId,
      filename: p.fileName,
      publisher: p.publisher,
      groupKey: runRow
        ? (groupKeyById.get(runRow.groupId) ?? null)
        : frameId != null
          ? (ctx.planGroupByFrameId.get(frameId) ?? null)
          : null,
      night: null,
      exptimeSec: p.exptimeSec,
      ...runMetrics(runRow, s),
      outcome,
      outOfNextRun: excluded.has(p.frameUuid),
      editable: true,
      unavailable: p.reason,
      pinnable,
      pinned: referenceFrameUuid === p.frameUuid,
      pinNote: !pinnable ? p.reason : p.hasWcs ? null : NO_WCS_NOTE,
    };
  });

  const runOnlyRows = [...runRowByKey.entries()]
    .filter(([key]) => !planKeys.has(key))
    .map(([key, runRow]): FrameRow => {
      const s = summaryByHandle.get(runRow.frameId);
      return {
        key,
        frameId: runRow.frameId,
        filename: s?.filename ?? `frame ${runRow.frameId}`,
        publisher: null,
        groupKey: groupKeyById.get(runRow.groupId) ?? null,
        night: null,
        exptimeSec: null,
        ...runMetrics(runRow, s),
        outcome: detail ? runOutcome(runRow, detail.run) : null,
        outOfNextRun: excluded.has(key),
        editable: false,
        unavailable: null,
        pinnable: false,
        pinned: false,
        pinNote: null,
      };
    });

  return [...planRows, ...runOnlyRows];
}

/** The summary's per-frame records by the frame id they carry (the run's
 *  namespace: a catalog id for a set, a project-frame handle for a project). */
function summaryFramesByFrameId(summary: RunSummary | null): Map<number, SummaryFrame> {
  const byFrameId = new Map<number, SummaryFrame>();
  if (summary) {
    for (const g of summary.groups) {
      for (const f of g.frames) byFrameId.set(f.frameId, f);
    }
  }
  return byFrameId;
}

type RunMetrics = Pick<
  FrameRow,
  'weight' | 'fwhmPx' | 'eccentricity' | 'stars' | 'regRmsPx' | 'lnScale' | 'regInliers' | 'regModel'
>;

/** The measurement/registration columns of a row: the summary's value where it
 *  has one, else the run row's own, `—` (null) before the run reached the
 *  frame. Without a run row every column is empty, summary or not. */
function runMetrics(runRow: StackingRunFrameRow | undefined, s: SummaryFrame | undefined): RunMetrics {
  if (!runRow) {
    return {
      weight: null,
      fwhmPx: null,
      eccentricity: null,
      stars: null,
      regRmsPx: null,
      lnScale: null,
      regInliers: null,
      regModel: null,
    };
  }
  return {
    weight: s?.weight ?? runRow.weight,
    fwhmPx: s?.fwhmPx ?? null,
    eccentricity: s?.eccentricity ?? null,
    stars: s?.stars ?? null,
    regRmsPx: s?.regRmsPx ?? runRow.regRmsPx,
    lnScale: s?.lnScale ?? null,
    regInliers: s?.regInliers ?? runRow.regInliers,
    regModel: s?.regModel ?? runRow.regModel,
  };
}

/** What a column sorts by: the row's own value, or for `status` the outcome's
 *  label (a row with no outcome has none). */
function sortValue(row: FrameRow, column: SortKey): string | number | null {
  return column === 'status' ? (row.outcome?.label ?? null) : row[column];
}

/** Client-side sort, any column, numeric-aware, nulls always last regardless
 *  of direction. `Array.prototype.sort` is spec-stable since ES2019, so no
 *  extra work is needed for "stable" beyond not mutating `rows` in place
 *  (returns a new array). Pure, exported for the same reason
 *  `buildFrameRows` is. */
export function sortRows(rows: FrameRow[], column: SortKey, dir: 'asc' | 'desc'): FrameRow[] {
  const copy = [...rows];
  copy.sort((a, b) => {
    const av = sortValue(a, column);
    const bv = sortValue(b, column);
    const aNull = av == null;
    const bNull = bv == null;
    if (aNull && bNull) return 0;
    if (aNull) return 1; // nulls sort last regardless of direction
    if (bNull) return -1;
    const cmp =
      typeof av === 'number' && typeof bv === 'number'
        ? av - bv
        : String(av).localeCompare(String(bv));
    return dir === 'asc' ? cmp : -cmp;
  });
  return copy;
}
