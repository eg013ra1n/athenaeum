// The Frames table's rows (spec §6.3, §6.6): the columns, one frame row, and the clickable header row of a group.
// Presentational: `FramesTable` decides what is shown, selected and collapsed.
import type { MouseEvent } from 'react';
import { ChevronDown, ChevronRight, Star } from 'lucide-react';
import { FilterDot } from '../ui/Dots';
import { formatDuration, pluralize } from '../collab/format';
import { sortRows, type FrameRow, type Outcome, type SortKey } from './frameRows';
import type { FrameGroupView, GroupHeaderInfo, GroupMode, OutlierFlags } from './frameView';

/** What a column header sorts by: a row-model key, or the reference star (pinned rows first). */
export type TableSortKey = SortKey | 'reference';

type ColumnId =
  | 'file' | 'publisher' | 'weight' | 'fwhm' | 'ecc' | 'seeds'
  | 'regRms' | 'inliers' | 'model' | 'scale' | 'status' | 'reference';

export interface FrameColumn {
  id: ColumnId;
  label: string;
  sortKey: TableSortKey;
  numeric?: boolean;
  title?: string;
}

const FILE: FrameColumn = { id: 'file', label: 'File', sortKey: 'filename' };
const PUBLISHER: FrameColumn = { id: 'publisher', label: 'Publisher', sortKey: 'publisher' };
const METRICS: FrameColumn[] = [
  { id: 'weight', label: 'Weight', sortKey: 'weight', numeric: true },
  { id: 'fwhm', label: 'FWHM', sortKey: 'fwhmPx', numeric: true },
  { id: 'ecc', label: 'Ecc', sortKey: 'eccentricity', numeric: true },
  { id: 'seeds', label: 'Seeds', sortKey: 'stars', numeric: true },
];
/** The Columns toggle's registration columns. */
const REGISTRATION: FrameColumn[] = [
  { id: 'regRms', label: 'Reg RMS', sortKey: 'regRmsPx', numeric: true },
  { id: 'inliers', label: 'Inliers', sortKey: 'regInliers', numeric: true },
  { id: 'model', label: 'Model', sortKey: 'regModel', title: 'Registration alignment model' },
  { id: 'scale', label: 'Scale', sortKey: 'lnScale', numeric: true, title: 'Local-normalization relative scale' },
];
const TAIL: FrameColumn[] = [
  { id: 'status', label: 'Status', sortKey: 'status' },
  { id: 'reference', label: 'Reference', sortKey: 'reference', title: 'The next run’s manual reference' },
];

/** The table's columns after the selection checkbox: Publisher for a project, the registration columns on demand. */
export function frameColumns(isProject: boolean, registration: boolean): FrameColumn[] {
  return [FILE, ...(isProject ? [PUBLISHER] : []), ...METRICS, ...(registration ? REGISTRATION : []), ...TAIL];
}

/** `sortRows`, plus the Reference column: the pinned row first (last when descending), the rest in their order. */
export function sortTableRows(rows: FrameRow[], key: TableSortKey, dir: 'asc' | 'desc'): FrameRow[] {
  if (key !== 'reference') return sortRows(rows, key, dir);
  const pinned = rows.filter((r) => r.pinned);
  const rest = rows.filter((r) => !r.pinned);
  return dir === 'asc' ? [...pinned, ...rest] : [...rest, ...pinned];
}

const OUTCOME_CHIP: Record<Outcome['tone'], string> = {
  accent: 'bg-accent/20 text-accent',
  success: 'bg-success-muted text-success',
  warning: 'bg-warning-muted text-warning',
  error: 'bg-error-muted text-error',
};
const CHIP = 'inline-block px-2 py-0.5 rounded-full text-[10px] font-semibold';
/** Same muted-chip tokens `ProvenanceModal.tsx` uses for its own "off" state. */
const WCS_CHIP = 'bg-surface-hover text-content-muted';
const OUTLIER_CELL = 'bg-warning-muted text-warning';
const TD = 'py-1.5 px-2';

function fmtNum(n: number | null, digits: number, suffix = ''): string {
  return n == null ? '—' : `${n.toFixed(digits)}${suffix}`;
}

function fmtInt(n: number | null): string {
  return n == null ? '—' : String(n);
}

/** FWHM in arcsec when the row's group has a pixel scale and arcsec is on (`scale` set), else in px. */
export function fmtFwhm(px: number | null, scale: number | null): string {
  if (px == null) return '—';
  return scale == null ? `${px.toFixed(2)} px` : `${(px * scale).toFixed(2)}"`;
}

/** Splits a `regModel` string on its optional trailing `+wcs` (M4b) so the Model column can render the alignment
 *  model text and the WCS-seed chip separately — e.g. `homography+polynomial3+wcs` → base `homography+polynomial3`,
 *  `wcs: true`. */
function splitRegModel(model: string | null): { base: string | null; wcs: boolean } {
  if (model == null) return { base: null, wcs: false };
  if (model.endsWith('+wcs')) return { base: model.slice(0, -'+wcs'.length), wcs: true };
  return { base: model, wcs: false };
}

function ModelCell({ model }: { model: string | null }) {
  const { base, wcs } = splitRegModel(model);
  if (base == null) return <>—</>;
  return (
    <span className="inline-flex items-center gap-1">
      <span>{base}</span>
      {wcs && (
        <span title="seeded from the plate solves" className={`${CHIP} px-1.5 ${WCS_CHIP}`}>
          WCS
        </span>
      )}
    </span>
  );
}

function StatusCell({ row }: { row: FrameRow }) {
  return (
    <div className="flex items-center gap-1">
      {row.outcome && <span className={`${CHIP} ${OUTCOME_CHIP[row.outcome.tone]}`}>{row.outcome.label}</span>}
      {row.outOfNextRun && <span className={`${CHIP} ${OUTCOME_CHIP.warning}`}>Out of next run</span>}
      {/* "Not stackable here" repeats the reason already printed under the file name. */}
      {row.outcome?.reason && row.outcome.kind !== 'notStackable' && (
        <span className="text-[10px] text-content-muted truncate max-w-[200px]" title={row.outcome.reason}>
          {row.outcome.reason}
        </span>
      )}
    </div>
  );
}

function ReferenceStar({ row, disabled, onPin }: { row: FrameRow; disabled: boolean; onPin: (key: string) => void }) {
  // A pinned frame can always be unpinned, even one that has become unusable since.
  const title = row.pinned
    ? !row.pinnable && row.pinNote
      ? `The reference frame — click to clear (${row.pinNote})`
      : 'The reference frame — click to clear'
    : (row.pinNote ?? 'Use as the reference frame');
  return (
    <button
      type="button"
      onClick={() => onPin(row.key)}
      disabled={disabled || (!row.pinnable && !row.pinned)}
      aria-pressed={row.pinned}
      aria-label={row.pinned ? `Clear the reference frame ${row.filename}` : `Use ${row.filename} as the reference frame`}
      title={title}
      className={`inline-flex items-center justify-center p-0.5 rounded transition-colors disabled:opacity-40 disabled:cursor-not-allowed ${
        row.pinned ? 'text-accent' : 'text-content-muted hover:text-accent hover:bg-accent/10'
      }`}
    >
      <Star size={13} className={row.pinned ? 'fill-current text-accent' : undefined} />
    </button>
  );
}

export interface FrameTableRowProps {
  row: FrameRow;
  columns: FrameColumn[];
  selected: boolean;
  outlier: OutlierFlags | undefined;
  /** Arcsec per px for the FWHM cell, `null` to keep px (arcsec off, or the row's group has no scale). */
  fwhmScale: number | null;
  /** The row's group, named in the file tooltip when the table is not grouped by stacking group. */
  groupContext: string | null;
  /** A run is active: the star is disabled. */
  disabled: boolean;
  onSelect: (key: string, e: MouseEvent) => void;
  onPin: (key: string) => void;
}

/** One frame: a click selects it (Shift+click a range), the star pins it. */
export function FrameTableRow({
  row, columns, selected, outlier, fwhmScale, groupContext, disabled, onSelect, onPin,
}: FrameTableRowProps) {
  // The run's registration failure is the stronger signal; the exclusion chip still shows beside it.
  const tint = row.outcome?.kind === 'regFailed' ? 'bg-error/10' : row.outOfNextRun ? 'bg-warning/10' : '';
  const cell = (c: FrameColumn) => {
    switch (c.id) {
      case 'file':
        return (
          <td
            key={c.id}
            data-col="file"
            className={`${TD} text-content font-mono truncate max-w-[220px]`}
            title={groupContext === null ? row.filename : `${row.filename} · ${groupContext}`}
          >
            {row.filename}
            {row.unavailable && (
              <div className="text-[10px] text-warning truncate" title={row.unavailable}>
                {row.unavailable}
              </div>
            )}
          </td>
        );
      case 'publisher':
        return (
          <td key={c.id} className={`${TD} text-content-secondary truncate max-w-[140px]`} title={row.publisher ?? undefined}>
            {row.publisher ?? '—'}
          </td>
        );
      case 'weight':
        return <td key={c.id} className={`${TD} text-content-secondary tabular-nums`}>{fmtNum(row.weight, 3)}</td>;
      case 'fwhm':
        return (
          <td key={c.id} className={`${TD} tabular-nums ${outlier?.fwhm ? OUTLIER_CELL : 'text-content-secondary'}`}>
            {fmtFwhm(row.fwhmPx, fwhmScale)}
          </td>
        );
      case 'ecc':
        return (
          <td key={c.id} className={`${TD} tabular-nums ${outlier?.ecc ? OUTLIER_CELL : 'text-content-secondary'}`}>
            {fmtNum(row.eccentricity, 3)}
          </td>
        );
      case 'seeds':
        return <td key={c.id} className={`${TD} text-content-secondary tabular-nums`}>{fmtInt(row.stars)}</td>;
      case 'regRms':
        return <td key={c.id} className={`${TD} text-content-secondary tabular-nums`}>{fmtNum(row.regRmsPx, 3, ' px')}</td>;
      case 'inliers':
        return <td key={c.id} className={`${TD} text-content-secondary tabular-nums`}>{fmtInt(row.regInliers)}</td>;
      case 'model':
        return (
          <td key={c.id} className={`${TD} text-content-secondary font-mono`}>
            <ModelCell model={row.regModel} />
          </td>
        );
      case 'scale':
        return <td key={c.id} className={`${TD} text-content-secondary tabular-nums`}>{fmtNum(row.lnScale, 3)}</td>;
      case 'status':
        return (
          <td key={c.id} className={TD}>
            <StatusCell row={row} />
          </td>
        );
      case 'reference':
        return (
          // The cell keeps its clicks: choosing the reference never changes the selection.
          <td key={c.id} className={`${TD} text-center`} onClick={(e) => e.stopPropagation()}>
            {row.editable && <ReferenceStar row={row} disabled={disabled} onPin={onPin} />}
          </td>
        );
    }
  };
  return (
    <tr
      data-testid="frame-row"
      onClick={(e) => onSelect(row.key, e)}
      className={`border-b border-border/50 last:border-0 cursor-pointer select-none hover:bg-surface-hover transition-colors ${tint}`.trimEnd()}
    >
      <td className={`w-8 ${TD} text-center`} onClick={(e) => e.stopPropagation()}>
        <input
          type="checkbox"
          checked={selected}
          readOnly
          onClick={(e) => onSelect(row.key, e)}
          aria-label={`Select ${row.filename}`}
          className="rounded border-border text-accent focus:ring-accent cursor-pointer"
        />
      </td>
      {columns.map(cell)}
    </tr>
  );
}

/** A group header's counts (spec §6.6). By stacking group: `N / M included · <integration> · median FWHM <x>`, or,
 *  for a group the run lost entirely, its outcome counts and most frequent reason; by night, publisher or status:
 *  `M frames · N included`. */
export function groupSummary(
  info: GroupHeaderInfo,
  mode: GroupMode,
  fmtMedianFwhm: (px: number) => string,
): { text: string; lost: boolean } {
  if (mode !== 'group') {
    return { text: `${info.total} ${pluralize(info.total, 'frame')} · ${info.included} included`, lost: false };
  }
  if (info.included === 0 && info.failed + info.excluded > 0) {
    const parts: string[] = [];
    if (info.failed > 0) {
      parts.push(`${info.failed} registration failed${info.topReason === null ? '' : ` (${info.topReason})`}`);
    }
    if (info.excluded > 0) parts.push(`${info.excluded} excluded`);
    return { text: `0 / ${info.total} included — ${parts.join(' · ')}`, lost: true };
  }
  const parts = [`${info.included} / ${info.total} included`];
  if (info.integrationSec > 0) parts.push(formatDuration(info.integrationSec));
  if (info.medianFwhmPx !== null) parts.push(`median FWHM ${fmtMedianFwhm(info.medianFwhmPx)}`);
  return { text: parts.join(' · '), lost: false };
}

export interface FrameGroupHeaderProps {
  group: FrameGroupView;
  colSpan: number;
  collapsed: boolean;
  onToggle: () => void;
  /** The filter of a stacking group, for its dot; `null` for none. */
  filter: string | null;
  summary: { text: string; lost: boolean };
  /** The references the selected run used for this group (`Reference: <file>`, `LN ref: <file>`). */
  references: string[];
}

/** A group's header row: a click collapses or expands the group. */
export function FrameGroupHeader({ group, colSpan, collapsed, onToggle, filter, summary, references }: FrameGroupHeaderProps) {
  return (
    <tr data-testid="frame-group-header" className="bg-table-group hover:bg-table-group-hover">
      <td colSpan={colSpan} className="py-1 px-2">
        <button
          type="button"
          onClick={onToggle}
          aria-expanded={!collapsed}
          className="w-full flex items-center gap-1.5 text-left text-xs"
        >
          {collapsed ? (
            <ChevronRight size={12} className="shrink-0 text-content-muted" />
          ) : (
            <ChevronDown size={12} className="shrink-0 text-content-muted" />
          )}
          {filter !== null && <FilterDot filter={filter} />}
          <span className="font-medium text-content">{group.title}</span>{' '}
          <span className={summary.lost ? 'text-error' : 'text-content-muted'}>{summary.text}</span>
          {references.map((ref) => (
            <span key={ref} className="text-content-muted">
              {' '}· {ref}
            </span>
          ))}
        </button>
      </td>
    </tr>
  );
}
