import { Eye, Settings, Loader2 } from 'lucide-react';
import type {
  CalibrationFilterGroup,
  CalibrationSetWithFrameCount,
  SubCalibrationDetail,
} from '../../types/models';
import { MatchBadges, extractLightParams } from './MatchBadges';

type CalibrationType = 'flat' | 'dark' | 'bias';

interface CalibrationSetsTableProps {
  filterGroup: CalibrationFilterGroup;
  onViewFrames: (setId: number) => void;
  onEditSubCalibration: (setId: number, setType: 'flat' | 'dark') => void;
  loadingSetId: number | null;
}

// ── Type styling ────────────────────────────────────────────────────────

export const typeColors: Record<CalibrationType, { border: string; badge: string; dot: string }> = {
  flat: {
    border: 'border-l-info',
    badge: 'bg-info/10 text-info border border-info/50',
    dot: 'bg-info',
  },
  dark: {
    border: 'border-l-purple',
    badge: 'bg-purple/10 text-purple border border-purple/50',
    dot: 'bg-purple',
  },
  bias: {
    border: 'border-l-success',
    badge: 'bg-success/10 text-success border border-success/50',
    dot: 'bg-success',
  },
};

export const subCalTypeColors: Record<string, { dot: string; text: string }> = {
  Dark: { dot: 'bg-purple', text: 'text-purple' },
  DarkFlat: { dot: 'bg-info', text: 'text-info' },
  Bias: { dot: 'bg-success', text: 'text-success' },
};

// ── Helpers ─────────────────────────────────────────────────────────────

function formatDateTime(dateStr: string | null): { date: string; time: string } {
  if (!dateStr) return { date: '-', time: '' };
  try {
    const d = new Date(dateStr);
    const date = d.toLocaleDateString('en-GB', { day: 'numeric', month: 'short', year: 'numeric' });
    const time = d.toLocaleTimeString('en-US', { hour: '2-digit', minute: '2-digit', hour12: false });
    return { date, time };
  } catch {
    return { date: '-', time: '' };
  }
}

// ── Row Components ──────────────────────────────────────────────────────

function CalSetRow({
  type,
  data,
  lightParams,
  groupIdx,
  isFirstGroup,
  onViewFrames,
  onEditSubCalibration,
  loadingSetId,
}: {
  type: CalibrationType;
  data: CalibrationSetWithFrameCount;
  lightParams: ReturnType<typeof extractLightParams>;
  groupIdx: number;
  isFirstGroup: boolean;
  onViewFrames: (setId: number) => void;
  onEditSubCalibration: (setId: number, setType: 'flat' | 'dark') => void;
  loadingSetId: number | null;
}) {
  const { set, warnings, frame_count } = data;
  const colors = typeColors[type];
  const hasDateWarning = warnings.some(w => w.warning_type === 'date');
  const bgClass = groupIdx % 2 === 0 ? 'bg-surface-elevated' : 'bg-surface';
  const dt = formatDateTime(set.date_start);
  // Border between groups, but not between parent and its sub-cal rows
  const topBorder = isFirstGroup ? '' : 'border-t border-border';

  return (
    <tr className={`${bgClass} ${topBorder} hover:bg-surface-hover transition-colors border-l-[3px] ${colors.border}`}>
      {/* Type */}
      <td className="w-28 px-3 py-3">
        <div className="flex items-center gap-1.5 min-w-0">
          <span className={`px-1.5 py-0.5 rounded text-xs font-semibold whitespace-nowrap ${colors.badge}`}>
            {type.charAt(0).toUpperCase() + type.slice(1)}
          </span>
          {set.id !== null && (
            <span className="text-xs text-content-muted">#{set.id}</span>
          )}
          {set.is_master && (
            <span className="px-1 py-0.5 text-[10px] font-medium bg-amber-500/20 text-amber-400 border border-amber-500/40 rounded whitespace-nowrap">
              Master
            </span>
          )}
        </div>
      </td>

      {/* Date + Time */}
      <td className={`w-36 px-3 py-3 ${hasDateWarning ? 'text-warning' : 'text-content-secondary'}`}>
        <div className="text-sm leading-tight">{dt.date}</div>
        {dt.time && <div className="text-xs text-content-muted leading-tight">{dt.time}</div>}
      </td>

      {/* Qty */}
      <td className="w-14 px-3 py-3 text-sm text-content-secondary text-center">
        {set.frame_count}
      </td>

      {/* Exp */}
      <td className="w-20 px-3 py-3 text-sm text-content-secondary text-center">
        {set.exptime !== null ? `${set.exptime}s` : '-'}
      </td>

      {/* Lights */}
      <td className="w-14 px-3 py-3 text-sm text-accent text-center font-medium">
        {frame_count}
      </td>

      {/* Match */}
      <td className="w-24 px-3 py-3">
        <MatchBadges set={set} warnings={warnings} lightParams={lightParams} />
      </td>

      {/* Actions */}
      <td className="w-16 px-3 py-3">
        <div className="flex items-center gap-1">
          {set.id !== null && (
            <button
              onClick={() => onViewFrames(set.id!)}
              disabled={loadingSetId === set.id}
              className="p-1 text-content-muted hover:text-content hover:bg-surface-hover/50 rounded transition-colors disabled:opacity-50"
              title="View calibration frames"
            >
              {loadingSetId === set.id ? (
                <Loader2 size={14} className="animate-spin" />
              ) : (
                <Eye size={14} />
              )}
            </button>
          )}
          {type !== 'bias' && set.id !== null && (
            <button
              onClick={() => onEditSubCalibration(set.id!, type)}
              className="p-1 text-content-muted hover:text-content hover:bg-surface-hover/50 rounded transition-colors"
              title="Edit sub-calibration"
            >
              <Settings size={14} />
            </button>
          )}
        </div>
      </td>
    </tr>
  );
}

function SubCalRow({
  sub,
  groupIdx,
}: {
  sub: SubCalibrationDetail;
  groupIdx: number;
}) {
  const colors = subCalTypeColors[sub.calibration_type] ?? { dot: 'bg-content-muted', text: 'text-content' };
  // Sub-rows share parent's band but are slightly muted to show nesting
  const bgClass = groupIdx % 2 === 0 ? 'bg-surface-elevated/60' : 'bg-surface/60';
  const dt = formatDateTime(sub.set.date_start);

  return (
    <tr className={bgClass}>
      {/* Type — indented with dot */}
      <td className="w-28 px-3 py-2 pl-8">
        <div className="flex items-center gap-1.5 min-w-0">
          <span className={`w-1.5 h-1.5 rounded-full flex-shrink-0 ${colors.dot}`} />
          <span className={`text-xs font-semibold ${colors.text}`}>{sub.calibration_type}</span>
          {sub.set.id !== null && (
            <span className="text-xs text-content-muted">#{sub.set.id}</span>
          )}
          {sub.set.is_master && (
            <span className="px-1 py-0.5 text-[10px] font-medium bg-amber-500/20 text-amber-400 border border-amber-500/40 rounded whitespace-nowrap">
              Master
            </span>
          )}
        </div>
      </td>

      {/* Date + Time */}
      <td className={`w-36 px-3 py-2 ${sub.date_warning ? 'text-warning' : 'text-content-muted'}`}>
        <div className="text-xs leading-tight">{dt.date}</div>
        {dt.time && <div className="text-[11px] leading-tight opacity-70">{dt.time}</div>}
      </td>

      {/* Qty */}
      <td className="w-14 px-3 py-2 text-xs text-content-muted text-center">
        {sub.set.frame_count}
      </td>

      {/* Exp */}
      <td className="w-20 px-3 py-2 text-xs text-content-muted text-center">
        {sub.set.exptime !== null ? `${sub.set.exptime}s` : '-'}
      </td>

      {/* Lights — empty */}
      <td className="w-14" />

      {/* Match — empty */}
      <td className="w-24" />

      {/* Actions — empty */}
      <td className="w-16" />
    </tr>
  );
}

function EmptyTypeRow({ type, isFirstGroup }: { type: CalibrationType; isFirstGroup: boolean }) {
  const colors = typeColors[type];
  const topBorder = isFirstGroup ? '' : 'border-t border-border';
  return (
    <tr className={`${topBorder} border-l-[3px] border-dashed ${colors.border} opacity-60`}>
      <td colSpan={7} className="px-3 py-3">
        <span className={`text-sm ${colors.badge.split(' ').find(c => c.startsWith('text-'))}`}>
          No {type} calibration linked
        </span>
      </td>
    </tr>
  );
}

// ── Main Table ──────────────────────────────────────────────────────────

export function CalibrationSetsTable({
  filterGroup,
  onViewFrames,
  onEditSubCalibration,
  loadingSetId,
}: CalibrationSetsTableProps) {
  const lightParams = extractLightParams(filterGroup.light_frames);

  // Build rows with group indices — parent + its sub-cals share a groupIdx.
  // Each sub-cal also carries its parent's set ID for unique React keys.
  type RowEntry =
    | { kind: 'set'; type: CalibrationType; data: CalibrationSetWithFrameCount; groupIdx: number; isFirstInGroup: boolean }
    | { kind: 'sub'; sub: SubCalibrationDetail; groupIdx: number; parentSetId: number | null }
    | { kind: 'empty'; type: CalibrationType; groupIdx: number };

  const rows: RowEntry[] = [];
  let groupIdx = 0;

  const addSets = (sets: CalibrationSetWithFrameCount[], type: CalibrationType) => {
    if (sets.length === 0) {
      rows.push({ kind: 'empty', type, groupIdx });
      groupIdx++;
      return;
    }
    for (const data of sets) {
      const currentGroup = groupIdx;
      rows.push({ kind: 'set', type, data, groupIdx: currentGroup, isFirstInGroup: true });
      if (data.sub_calibration?.length) {
        for (const sub of data.sub_calibration) {
          rows.push({ kind: 'sub', sub, groupIdx: currentGroup, parentSetId: data.set.id });
        }
      }
      groupIdx++;
    }
  };

  addSets(filterGroup.flat_sets, 'flat');
  addSets(filterGroup.dark_sets, 'dark');
  if (filterGroup.bias_sets.length > 0) {
    addSets(filterGroup.bias_sets, 'bias');
  }

  return (
    <div className="border border-border rounded-xl overflow-hidden">
      <table className="w-full" role="table">
        <thead className="bg-surface">
          <tr>
            <th scope="col" className="w-28 px-3 py-3 text-left text-sm font-semibold text-content-secondary">Type</th>
            <th scope="col" className="w-36 px-3 py-3 text-left text-sm font-semibold text-content-secondary">Date</th>
            <th scope="col" className="w-14 px-3 py-3 text-center text-sm font-semibold text-content-secondary">Qty</th>
            <th scope="col" className="w-20 px-3 py-3 text-center text-sm font-semibold text-content-secondary">Exp</th>
            <th scope="col" className="w-14 px-3 py-3 text-center text-sm font-semibold text-content-secondary">Lights</th>
            <th scope="col" className="w-24 px-3 py-3 text-left text-sm font-semibold text-content-secondary">Match</th>
            <th scope="col" className="w-16 px-3 py-3 text-center text-sm font-semibold text-content-secondary">Actions</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row, idx) => {
            switch (row.kind) {
              case 'set':
                return (
                  <CalSetRow
                    key={`${row.type}-${row.data.set.id ?? idx}`}
                    type={row.type}
                    data={row.data}
                    lightParams={lightParams}
                    groupIdx={row.groupIdx}
                    isFirstGroup={row.groupIdx === 0}
                    onViewFrames={onViewFrames}
                    onEditSubCalibration={onEditSubCalibration}
                    loadingSetId={loadingSetId}
                  />
                );
              case 'sub':
                return (
                  <SubCalRow
                    key={`sub-${row.parentSetId}-${row.sub.calibration_type}-${row.sub.set.id ?? idx}`}
                    sub={row.sub}
                    groupIdx={row.groupIdx}
                  />
                );
              case 'empty':
                return <EmptyTypeRow key={`empty-${row.type}`} type={row.type} isFirstGroup={row.groupIdx === 0} />;
            }
          })}
        </tbody>
      </table>
    </div>
  );
}
