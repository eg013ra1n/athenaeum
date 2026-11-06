import { useMemo, useState } from 'react';
import { ChevronDown, ChevronRight, ArrowUp, ArrowDown, Pin } from 'lucide-react';
import type {
  PlanProjectFrame,
  StackingRunDetail,
  StackingRunFrameRow,
  RunSummary,
  SummaryFrame,
} from '../../types/stacking';
import type { StackSubject } from './subject';

/** The set's LIGHT frames, as `FrameSetDetail.tsx` derives them from its own
 *  `detail.nights` tree — plan 5b Task 4, Decisions item 3. This is the row
 *  source before any run has been recorded, and the filename fallback for a
 *  run row a summary hasn't reached yet (`StackingRunFrameRow` itself carries
 *  no filename — only `SummaryFrame`, which doesn't exist until the run's
 *  summary is written). */
export interface LightFrameRef {
  frameId: number;
  filename: string;
}

/** The run-scoped slice `buildFrameRows` actually needs — `groups`/`frames`
 *  only, deliberately NOT the whole `StackingRunDetail` (whose `summary` is
 *  taken as its own, separately-nullable parameter below). Splitting it this
 *  way lets a caller exercise "run active, not yet finished" (`detail`
 *  present, `summary` still `null`) independently of "no run at all"
 *  (`detail` itself `null`) — the two states `StackingRunDetail.frames`
 *  being "present throughout the run" and `.summary` being "`null` until the
 *  run finishes" actually produce. */
export type RunFrameData = Pick<StackingRunDetail, 'groups' | 'frames'>;

type StatusTone = 'success' | 'warning' | 'error';

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
  group: string;
  weight: number | null;
  fwhmPx: number | null;
  eccentricity: number | null;
  stars: number | null;
  regRmsPx: number | null;
  regInliers: number | null;
  /** Stage 6 (local normalization, M2): the frame's own relative scale
   *  (`SummaryFrame.lnScale`) — `null` when LN never ran for this group's
   *  frame (disabled, a ruling-R3 fallback to global normalization, or the
   *  run's summary isn't loaded yet), same convention as every other
   *  summary-sourced metric column here. */
  lnScale: number | null;
  /** Stage 5's alignment model string (`SummaryFrame.regModel` /
   *  `StackingRunFrameRow.regModel`), e.g. `homography` or
   *  `homography+polynomial3+wcs` — the trailing `+wcs` (M4b) marks a
   *  frame whose correspondence search was seeded from the stored plate
   *  solves instead of the quad matcher alone. `null` before the frame has
   *  registered in the selected run. Rendered split in two: the model text
   *  and, when present, a `WCS` chip for the suffix — see `splitRegModel`. */
  regModel: string | null;
  statusLabel: string;
  statusTone: StatusTone;
  /** False only for a project run row whose frame left the plan (excluded by
   *  the coordinator, remapped, no longer landed): it renders from the run's
   *  own data and carries neither the include checkbox nor the pin. */
  editable: boolean;
  /** The plan's `reason` the frame cannot be stacked on this device
   *  (`PlanProjectFrame.reason`), whatever `onDisk` says: a landed file can be
   *  unsupported or unopenable and has `onDisk` true AND a reason. `null` for
   *  a set row and a usable frame. */
  unavailable: string | null;
  /** Usable on this device (`onDisk` and no reason): the pin is enabled. A
   *  frame without a manifest WCS is pinnable too (D2): it is plate-solved
   *  locally first. */
  pinnable: boolean;
  /** This frame is the project's manual reference in effect. */
  pinned: boolean;
  /** What the pin's tooltip says beyond its default: the reason a frame
   *  cannot be pinned, or the note on a frame without a manifest WCS. */
  pinNote: string | null;
}

type SortKey =
  | 'filename'
  | 'publisher'
  | 'group'
  | 'weight'
  | 'fwhmPx'
  | 'eccentricity'
  | 'stars'
  | 'regRmsPx'
  | 'lnScale'
  | 'regInliers'
  | 'regModel'
  | 'statusLabel';

const COLUMNS: { key: SortKey; label: string; numeric?: boolean; title?: string }[] = [
  { key: 'filename', label: 'Filename' },
  { key: 'group', label: 'Group' },
  { key: 'weight', label: 'Weight', numeric: true },
  { key: 'fwhmPx', label: 'FWHM', numeric: true },
  { key: 'eccentricity', label: 'Ecc', numeric: true },
  { key: 'stars', label: 'Seeds', numeric: true },
  { key: 'regRmsPx', label: 'Reg RMS', numeric: true },
  { key: 'lnScale', label: 'Scale', numeric: true, title: 'Local-normalization relative scale' },
  { key: 'regInliers', label: 'Inliers', numeric: true },
  { key: 'regModel', label: 'Model', title: 'Registration alignment model' },
  { key: 'statusLabel', label: 'Status' },
];

/** Project table only: the Publisher column follows Filename. */
const PUBLISHER_COLUMN: (typeof COLUMNS)[number] = { key: 'publisher', label: 'Publisher' };
const PROJECT_COLUMNS = COLUMNS.flatMap((c) => (c.key === 'filename' ? [c, PUBLISHER_COLUMN] : [c]));

/** D2: a frame without a manifest WCS is pinnable; the run plate-solves it. */
const NO_WCS_NOTE = 'No WCS in the manifest — plate-solved locally first';

const STATUS_CHIP: Record<StatusTone, string> = {
  success: 'bg-success-muted text-success',
  warning: 'bg-warning-muted text-warning',
  error: 'bg-error-muted text-error',
};

/** Same muted-chip tokens `ProvenanceModal.tsx` uses for its own "off"
 *  state — the neutral pill in this stacking component family. */
const WCS_CHIP = 'bg-surface-hover text-content-muted';

function fmtNum(n: number | null, digits: number, suffix = ''): string {
  return n == null ? '—' : `${n.toFixed(digits)}${suffix}`;
}

function fmtInt(n: number | null): string {
  return n == null ? '—' : String(n);
}

/** Splits a `regModel` string on its optional trailing `+wcs` (M4b) so the
 *  Model column can render the alignment model text and the WCS-seed chip
 *  separately — e.g. `homography+polynomial3+wcs` → base
 *  `homography+polynomial3`, `wcs: true`. */
function splitRegModel(model: string | null): { base: string | null; wcs: boolean } {
  if (model == null) return { base: null, wcs: false };
  if (model.endsWith('+wcs')) return { base: model.slice(0, -'+wcs'.length), wcs: true };
  return { base: model, wcs: false };
}

/** One row's status chip. A registration failure wins over a plain
 *  exclusion (a failed-registration frame is always also `included =
 *  false` — verified in `crates/athenaeum-core/src/stacking/run.rs`'s
 *  registration-failure branch, which sets both together — so checking
 *  `included` first would relabel it a generic exclusion and lose the more
 *  specific reason); the manual-exclusion list is then checked
 *  independently on top of that (unless the row is already the error case)
 *  — Ruling 7's checkbox only ever reflects `excludedFrameIds` membership,
 *  never the run's own outcome, so a frame the run kept but the user has
 *  since flagged for the *next* run still shows that as its own chip. */
function rowStatus(
  included: boolean,
  exclusionReason: string | null,
  regStatus: string | null,
  manuallyExcluded: boolean,
): { statusLabel: string; statusTone: StatusTone } {
  if (regStatus === 'failed') {
    return { statusLabel: 'Registration failed', statusTone: 'error' };
  }
  if (manuallyExcluded) {
    return { statusLabel: 'Excluded (manual)', statusTone: 'warning' };
  }
  if (!included) {
    return { statusLabel: exclusionReason ?? 'Excluded', statusTone: 'warning' };
  }
  return { statusLabel: 'Included', statusTone: 'success' };
}

/** The Frames table's row source (fix round 1, Important #2): the UNION of
 *  `lightFrames` (the set's current LIGHT frames) and `detail.frames` (the
 *  selected run's own frame rows), keyed by `frameId` — not `detail.frames`
 *  alone. A light added to the set after the selected run (or present under
 *  an older run than the one currently selected) has no row in
 *  `detail.frames`; without the union it silently had no row at all in the
 *  table, and therefore no include checkbox — the tab's one frame-level
 *  write (Ruling 7) simply couldn't reach it. Such a frame gets a
 *  synthetic "no run data yet" row (group "—", every metric "—", status
 *  from the manual-exclusion list alone) — the same shape every row had
 *  before any run existed.
 *
 *  `detail`/`summary` are deliberately separate parameters, not one
 *  `StackingRunDetail` — `detail.frames` is present throughout a run,
 *  `summary` (`RunSummary`, carrying the FWHM/eccentricity/seed metrics
 *  `StackingRunFrameRow` itself has no columns for) is `null` until the run
 *  finishes; keeping them apart lets a caller (this file's own smoke
 *  self-check included) exercise "run active, not yet finished" separately
 *  from "no run at all" (`detail === null`). Pure — no React, safe to
 *  import and unit-exercise outside a component. */
export function buildFrameRows(
  lightFrames: LightFrameRef[],
  detail: RunFrameData | null,
  summary: RunSummary | null,
  excludedFrameIds: number[],
): FrameRow[] {
  const excludedSet = new Set(excludedFrameIds);
  const filenameByFrameId = new Map(lightFrames.map((f) => [f.frameId, f.filename]));
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
    const s = summaryByFrameId.get(frameId);
    const manuallyExcluded = excludedSet.has(frameId);

    // No run row for this frame at all (a light added after the selected
    // run, or one that's never survived to stage 1 in any run) — the
    // same "no run data yet" shape a row had before any run existed:
    // status comes from the manual-exclusion list alone.
    //
    // `group` is deliberately display-only here (Plan 5b final fix wave,
    // click-through item A6): the plan's own groups know a frame's group
    // only in aggregate (`PlanGroup.frameCount`, no per-frame membership),
    // and `LightFrameRef` carries no instrument/filter/binning fields to
    // derive it from client-side — the table's own tooltip on this cell
    // ("assigned by the next run") is the fix, not a new backend call.
    const status = runRow
      ? rowStatus(runRow.included, runRow.exclusionReason, runRow.regStatus, manuallyExcluded)
      : {
          statusLabel: manuallyExcluded ? 'Excluded (manual)' : 'Included',
          statusTone: manuallyExcluded ? ('warning' as const) : ('success' as const),
        };

    return {
      key: String(frameId),
      frameId,
      filename: (runRow ? s?.filename : undefined) ?? filenameByFrameId.get(frameId) ?? `frame ${frameId}`,
      publisher: null,
      ...runMetrics(runRow, s, groupKeyById),
      ...status,
      editable: true,
      unavailable: null,
      pinnable: false,
      pinned: false,
      pinNote: null,
    };
  });
}

/** A project run row's key: its frame uuid, or `h:<handle>` when the run row
 *  carries none (its project-frame row is gone). `h:` cannot collide with a
 *  uuid, and such a row is never joined to a plan row. */
function runRowKey(r: StackingRunFrameRow): string {
  return r.frameUuid ?? `h:${r.frameId}`;
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
    const manuallyExcluded = excluded.has(p.frameUuid);
    const pinnable = p.onDisk && p.reason == null;
    // The run's outcome is the record of that run; a frame that cannot be
    // stacked now still says so beside its name (`unavailable`). With no run
    // row there is no outcome, and the status says what the next run does.
    const status = runRow
      ? rowStatus(runRow.included, runRow.exclusionReason, runRow.regStatus, manuallyExcluded)
      : manuallyExcluded
        ? { statusLabel: 'Excluded (manual)', statusTone: 'warning' as const }
        : !pinnable
          ? { statusLabel: 'Not stackable here', statusTone: 'warning' as const }
          : { statusLabel: 'Included', statusTone: 'success' as const };
    return {
      key: p.frameUuid,
      frameId: runRow?.frameId ?? p.handle,
      filename: p.fileName,
      publisher: p.publisher,
      ...runMetrics(runRow, s, groupKeyById),
      ...status,
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
        ...runMetrics(runRow, s, groupKeyById),
        ...rowStatus(runRow.included, runRow.exclusionReason, runRow.regStatus, excluded.has(key)),
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
  'group' | 'weight' | 'fwhmPx' | 'eccentricity' | 'stars' | 'regRmsPx' | 'lnScale' | 'regInliers' | 'regModel'
>;

/** The measurement/registration columns of a row: the summary's value where it
 *  has one, else the run row's own, `—` (null) before the run reached the
 *  frame. Without a run row every column is empty, summary or not. */
function runMetrics(
  runRow: StackingRunFrameRow | undefined,
  s: SummaryFrame | undefined,
  groupKeyById: Map<number, string>,
): RunMetrics {
  if (!runRow) {
    return {
      group: '—',
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
    group: groupKeyById.get(runRow.groupId) ?? '—',
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

/** Client-side sort, any column, numeric-aware, nulls always last regardless
 *  of direction. `Array.prototype.sort` is spec-stable since ES2019, so no
 *  extra work is needed for "stable" beyond not mutating `rows` in place
 *  (returns a new array). Pure, exported for the same reason
 *  `buildFrameRows` is. */
export function sortRows(rows: FrameRow[], column: SortKey, dir: 'asc' | 'desc'): FrameRow[] {
  const copy = [...rows];
  copy.sort((a, b) => {
    const av = a[column];
    const bv = b[column];
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

export interface FramesTableProps {
  /** Which rows the table shows: a frame set's light frames, or a project's
   *  plan frames. A project adds the Publisher and Reference columns. */
  subjectKind: StackSubject['kind'];
  /** The set's LIGHT frames (id + filename), independent of any run. Empty
   *  for a project. */
  lightFrames: LightFrameRef[];
  /** The project plan's frames (`StackingPlan.projectFrames`). Empty for a
   *  set. */
  projectFrames: PlanProjectFrame[];
  /** The results panel's currently-selected run, or `null` before any run
   *  has been picked / recorded — the table then falls back to
   *  `lightFrames` / `projectFrames` alone. */
  runDetail: StackingRunDetail | null;
  /** The subject's exclusion keys: `String(frameId)` for a set, the frame
   *  uuid for a project (`SubjectConfig.excluded`). */
  excludedKeys: string[];
  /** A project's manual reference frame in effect (its uuid), else `null`. */
  referenceFrameUuid: string | null;
  /** Ruling 7 — the include checkbox, a frame-level write of the exclusion
   *  list. Toggles `key`'s membership in `excludedKeys`; the caller
   *  (`StackingTab`) is responsible for routing the resulting list through
   *  the existing gated persist path. */
  onToggleExclude: (key: string) => void;
  /** The project's reference pin (D2): pins the frame `key`, or clears the
   *  pin when it is the pinned one. Every usable row has one. */
  onPin: (key: string) => void;
  collapsed: boolean;
  onToggleCollapsed: () => void;
  /** True while a run is active for this subject — disables the include
   *  checkbox and the pin the same way the rest of the tab's controls are
   *  disabled during a run (fix round 1, Important #8). */
  disabled?: boolean;
}

/** The Frames table (spec §11.2, §12) — every LIGHT frame of a set or every
 *  frame of a project, with the manual-inclusion checkbox and, once a run has
 *  produced rows, its per-frame measurement/registration outcome. A
 *  project's table adds the publisher and the reference pin. Read-only
 *  besides the checkbox (Ruling 7) and the pin. */
export function FramesTable({
  subjectKind,
  lightFrames,
  projectFrames,
  runDetail,
  excludedKeys,
  referenceFrameUuid,
  onToggleExclude,
  onPin,
  collapsed,
  onToggleCollapsed,
  disabled = false,
}: FramesTableProps) {
  const [sortKey, setSortKey] = useState<SortKey>('filename');
  const [sortDir, setSortDir] = useState<'asc' | 'desc'>('asc');
  const isProject = subjectKind === 'project';
  const columns = isProject ? PROJECT_COLUMNS : COLUMNS;

  // For the checkbox's own `checked` state below — the row builders build
  // and consume their own sets for the status chip.
  const excludedSet = useMemo(() => new Set(excludedKeys), [excludedKeys]);

  const rows = useMemo<FrameRow[]>(() => {
    const runData = runDetail ? { groups: runDetail.groups, frames: runDetail.frames } : null;
    const summary = runDetail?.summary ?? null;
    return isProject
      ? buildProjectFrameRows(projectFrames, runData, summary, excludedSet, referenceFrameUuid)
      : buildFrameRows(lightFrames, runData, summary, excludedKeys.map(Number));
  }, [isProject, lightFrames, projectFrames, runDetail, excludedKeys, excludedSet, referenceFrameUuid]);

  const sortedRows = useMemo(() => sortRows(rows, sortKey, sortDir), [rows, sortKey, sortDir]);

  const handleSort = (key: SortKey) => {
    if (key === sortKey) {
      setSortDir((d) => (d === 'asc' ? 'desc' : 'asc'));
    } else {
      setSortKey(key);
      setSortDir('asc');
    }
  };

  return (
    <div className="bg-surface-elevated rounded-lg p-3">
      <button
        type="button"
        onClick={onToggleCollapsed}
        aria-expanded={!collapsed}
        className="flex items-center gap-1.5 text-sm font-medium text-content hover:text-content-secondary transition-colors"
      >
        {collapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}
        Frames ({rows.length})
      </button>

      {!collapsed && (
        <div className="mt-2 overflow-x-auto">
          {rows.length === 0 ? (
            <p className="text-sm text-content-muted px-1">
              {isProject ? 'No frames in this project.' : 'No light frames in this set.'}
            </p>
          ) : (
            <table className="w-full text-xs">
              <thead>
                <tr className="text-content-muted text-left border-b border-border">
                  {columns.map((c) => (
                    <th key={c.key} className="py-1.5 px-2 font-medium" title={c.title}>
                      <button
                        type="button"
                        onClick={() => handleSort(c.key)}
                        className={`flex items-center gap-1 hover:text-content transition-colors ${
                          c.numeric ? 'tabular-nums' : ''
                        }`}
                      >
                        {c.label}
                        {sortKey === c.key &&
                          (sortDir === 'asc' ? <ArrowUp size={10} /> : <ArrowDown size={10} />)}
                      </button>
                    </th>
                  ))}
                  <th className="py-1.5 px-2 font-medium text-center">Include</th>
                  {isProject && <th className="py-1.5 px-2 font-medium text-center">Reference</th>}
                </tr>
              </thead>
              <tbody>
                {sortedRows.map((r) => (
                  <tr key={r.key} className="border-b border-border/50 last:border-0">
                    <td className="py-1.5 px-2 text-content font-mono truncate max-w-[220px]" title={r.filename}>
                      {r.filename}
                      {r.unavailable && (
                        <div className="text-[10px] text-warning truncate" title={r.unavailable}>
                          {r.unavailable}
                        </div>
                      )}
                    </td>
                    {isProject && (
                      <td className="py-1.5 px-2 text-content-secondary truncate max-w-[140px]" title={r.publisher ?? undefined}>
                        {r.publisher ?? '—'}
                      </td>
                    )}
                    <td
                      className="py-1.5 px-2 text-content-secondary font-mono"
                      title={r.group === '—' ? 'assigned by the next run' : undefined}
                    >
                      {r.group}
                    </td>
                    <td className="py-1.5 px-2 text-content-secondary tabular-nums">{fmtNum(r.weight, 3)}</td>
                    <td className="py-1.5 px-2 text-content-secondary tabular-nums">{fmtNum(r.fwhmPx, 2, ' px')}</td>
                    <td className="py-1.5 px-2 text-content-secondary tabular-nums">{fmtNum(r.eccentricity, 3)}</td>
                    <td className="py-1.5 px-2 text-content-secondary tabular-nums">{fmtInt(r.stars)}</td>
                    <td className="py-1.5 px-2 text-content-secondary tabular-nums">{fmtNum(r.regRmsPx, 3, ' px')}</td>
                    <td className="py-1.5 px-2 text-content-secondary tabular-nums">{fmtNum(r.lnScale, 3)}</td>
                    <td className="py-1.5 px-2 text-content-secondary tabular-nums">{fmtInt(r.regInliers)}</td>
                    <td className="py-1.5 px-2 text-content-secondary font-mono">
                      {(() => {
                        const { base, wcs } = splitRegModel(r.regModel);
                        if (base == null) return '—';
                        return (
                          <span className="inline-flex items-center gap-1">
                            <span>{base}</span>
                            {wcs && (
                              <span
                                title="seeded from the plate solves"
                                className={`inline-block px-1.5 py-0.5 rounded-full text-[10px] font-semibold ${WCS_CHIP}`}
                              >
                                WCS
                              </span>
                            )}
                          </span>
                        );
                      })()}
                    </td>
                    <td className="py-1.5 px-2">
                      <span
                        className={`inline-block px-2 py-0.5 rounded-full text-[10px] font-semibold ${STATUS_CHIP[r.statusTone]}`}
                      >
                        {r.statusLabel}
                      </span>
                    </td>
                    <td className="py-1.5 px-2 text-center">
                      {r.editable && (
                        <input
                          type="checkbox"
                          checked={!excludedSet.has(r.key)}
                          onChange={() => onToggleExclude(r.key)}
                          disabled={disabled}
                          aria-label={`Include ${r.filename} in the next run`}
                          className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent disabled:opacity-50"
                        />
                      )}
                    </td>
                    {isProject && (
                      <td className="py-1.5 px-2 text-center">
                        {r.editable && (
                          // A pinned frame can always be unpinned, even one that has become unusable since.
                          <button
                            type="button"
                            onClick={() => onPin(r.key)}
                            disabled={disabled || (!r.pinnable && !r.pinned)}
                            aria-pressed={r.pinned}
                            aria-label={
                              r.pinned
                                ? `Clear the reference frame ${r.filename}`
                                : `Use ${r.filename} as the reference frame`
                            }
                            title={
                              r.pinned
                                ? // A pinned frame that is no longer usable says why, in brackets after the clear hint.
                                  !r.pinnable && r.pinNote
                                  ? `The reference frame — click to clear (${r.pinNote})`
                                  : 'The reference frame — click to clear'
                                : (r.pinNote ?? 'Use as the reference frame')
                            }
                            className={`p-1 rounded transition-colors disabled:opacity-40 disabled:cursor-not-allowed ${
                              r.pinned ? 'text-accent' : 'text-content-muted hover:text-content'
                            }`}
                          >
                            <Pin size={14} fill={r.pinned ? 'currentColor' : 'none'} />
                          </button>
                        )}
                      </td>
                    )}
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      )}
    </div>
  );
}
