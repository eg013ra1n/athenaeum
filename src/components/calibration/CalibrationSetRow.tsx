import { AlertTriangle, Eye, Settings, Loader2 } from 'lucide-react';
import type {
  CalibrationSetDetail,
  CalibrationSetWithFrameCount,
  SubCalibrationDetail,
} from '../../types/models';

type CalibrationType = 'flat' | 'dark' | 'bias';

interface CalibrationSetRowProps {
  type: CalibrationType;
  data: CalibrationSetWithFrameCount;
  onViewFrames?: (setId: number) => void;
  onEditSubCalibration?: (setId: number, setType: CalibrationType) => void;
  isLoadingFrames?: boolean;
}

const typeStyles: Record<CalibrationType, { bar: string; text: string; border: string }> = {
  flat: { bar: 'bg-info', text: 'text-info', border: 'border-info/50' },
  dark: { bar: 'bg-purple', text: 'text-purple', border: 'border-purple/50' },
  bias: { bar: 'bg-success', text: 'text-success', border: 'border-success/50' },
};

/** Map sub-calibration type string to color style */
const subCalStyles: Record<string, { bar: string; text: string }> = {
  Dark:     { bar: 'bg-purple', text: 'text-purple' },
  DarkFlat: { bar: 'bg-info', text: 'text-info' },
  Bias:     { bar: 'bg-success', text: 'text-success' },
};

function formatTemp(temp: number | null): string {
  if (temp === null) return 'N/A';
  return `${temp.toFixed(1)}\u00B0C`;
}

function formatDate(dateStr: string | null): string {
  if (!dateStr) return 'N/A';
  return new Date(dateStr).toLocaleDateString('en-GB', {
    day: 'numeric',
    month: 'short',
    year: 'numeric',
  });
}

function getRowTitle(type: CalibrationType, set: CalibrationSetDetail): string {
  switch (type) {
    case 'flat':
      return set.filter ? `Flat (${set.filter})` : 'Flat';
    case 'dark':
      return set.exptime !== null ? `Dark (${set.exptime}s)` : 'Dark';
    case 'bias':
      return 'Bias';
  }
}

function PropChip({ label, value, warn }: { label: string; value: string; warn?: boolean }) {
  return (
    <span className="inline-flex items-center gap-1 text-xs">
      <span className="text-content-muted font-normal">{label}</span>
      <span className={`font-medium ${warn ? 'text-warning' : 'text-content'}`}>{value}</span>
    </span>
  );
}

function ChipSeparator() {
  return <span className="text-border mx-0.5">|</span>;
}

/**
 * Sub-calibration row with its own type-colored left bar.
 */
function SubCalRow({ sub }: { sub: SubCalibrationDetail }) {
  const hasTempWarn = sub.temp_warning;
  const hasDateWarn = sub.date_warning;
  const colors = subCalStyles[sub.calibration_type] ?? { bar: 'bg-content-muted', text: 'text-content' };

  return (
    <div className="flex items-center min-h-[36px] bg-surface-elevated/50">
      {/* Type color bar */}
      <div className={`w-1 self-stretch ${colors.bar} flex-shrink-0`} />

      {/* Indent + title */}
      <div className="flex items-center gap-1.5 pl-4 mr-3">
        <span className={`text-xs font-semibold ${colors.text}`}>{sub.calibration_type}</span>
        {sub.set.id !== null && (
          <span className="text-xs text-content-muted">#{sub.set.id}</span>
        )}
        {sub.set.is_master && (
          <span className="px-1 py-0.5 text-[10px] font-medium bg-amber-500/20 text-amber-400 border border-amber-500/40 rounded">
            Master
          </span>
        )}
      </div>

      {/* Properties */}
      <div className="flex items-center flex-1 min-w-0 overflow-hidden mr-2">
        <PropChip label="Frames" value={String(sub.set.frame_count)} />
        <ChipSeparator />
        <PropChip label="Date" value={formatDate(sub.set.date_start)} warn={hasDateWarn} />
        {sub.set.exptime !== null && (
          <><ChipSeparator /><PropChip label="Exp" value={`${sub.set.exptime}s`} /></>
        )}
        <ChipSeparator />
        <PropChip label="Temp" value={formatTemp(sub.set.ccd_temp)} warn={hasTempWarn} />
        {sub.set.gain !== null && (
          <><ChipSeparator /><PropChip label="Gain" value={String(sub.set.gain)} /></>
        )}
        {sub.set.offset !== null && (
          <><ChipSeparator /><PropChip label="Offset" value={String(sub.set.offset)} /></>
        )}
        {sub.set.binning && (
          <><ChipSeparator /><PropChip label="Bin" value={sub.set.binning} /></>
        )}
        {(hasTempWarn || hasDateWarn) && (
          <>
            <ChipSeparator />
            <span className="flex items-center gap-0.5 text-warning">
              <AlertTriangle size={11} />
              <span className="text-xs font-medium">
                {[hasTempWarn && 'temp', hasDateWarn && 'date'].filter(Boolean).join(', ')}
              </span>
            </span>
          </>
        )}
      </div>
    </div>
  );
}

/**
 * Compact row for a single calibration set.
 *
 * Main row (~44px) with type-colored left bar, inline property chips, action buttons.
 * Sub-calibration rows always visible below, each with their own type-colored bar.
 */
export function CalibrationSetRow({
  type,
  data,
  onViewFrames,
  onEditSubCalibration,
  isLoadingFrames = false,
}: CalibrationSetRowProps) {
  const { set, warnings, sub_calibration, frame_count } = data;
  const styles = typeStyles[type];

  const hasTempWarning = warnings.some(w => w.warning_type === 'temperature');
  const hasDateWarning = warnings.some(w => w.warning_type === 'date');
  const hasWarnings = warnings.length > 0;
  const hasSubCal = sub_calibration && sub_calibration.length > 0;

  return (
    <div className={`rounded-lg border ${styles.border} overflow-hidden`}>
      {/* Main row */}
      <div className="flex items-center min-h-[44px]">
        {/* Type color bar */}
        <div className={`w-1 self-stretch ${styles.bar} flex-shrink-0`} />

        {/* Title + ID + Master badge */}
        <div className="flex items-center gap-1.5 min-w-0 pl-3 mr-2">
          <span className={`text-sm font-semibold ${styles.text} whitespace-nowrap`}>
            {getRowTitle(type, set)}
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

        {/* Inline property chips */}
        <div className="flex items-center flex-1 min-w-0 overflow-hidden mr-2">
          <PropChip label="Frames" value={String(set.frame_count)} />
          <ChipSeparator />
          <PropChip label="Date" value={formatDate(set.date_start)} warn={hasDateWarning} />
          {set.exptime !== null && (
            <><ChipSeparator /><PropChip label="Exp" value={`${set.exptime}s`} /></>
          )}
          <ChipSeparator />
          <PropChip label="Temp" value={formatTemp(set.ccd_temp)} warn={hasTempWarning} />
          {set.gain !== null && (
            <><ChipSeparator /><PropChip label="Gain" value={String(set.gain)} /></>
          )}
          {set.offset !== null && (
            <><ChipSeparator /><PropChip label="Offset" value={String(set.offset)} /></>
          )}
          {set.binning && (
            <><ChipSeparator /><PropChip label="Bin" value={set.binning} /></>
          )}
          <ChipSeparator />
          <PropChip label="Lights" value={String(frame_count)} />
          {hasWarnings && (
            <>
              <ChipSeparator />
              <span
                className="flex items-center gap-0.5 text-warning cursor-help"
                title={warnings.map(w => w.message).join('\n')}
              >
                <AlertTriangle size={12} />
                <span className="text-xs font-medium">{warnings.length}</span>
              </span>
            </>
          )}
        </div>

        {/* Action buttons */}
        <div className="flex items-center gap-1 mr-2 flex-shrink-0">
          {onViewFrames && set.id !== null && (
            <button
              onClick={(e) => {
                e.stopPropagation();
                onViewFrames(set.id!);
              }}
              disabled={isLoadingFrames}
              className="flex items-center gap-1 px-2 py-1 text-xs text-content-muted hover:text-content hover:bg-surface-hover/50 rounded transition-colors disabled:opacity-50"
              title="View calibration frames"
            >
              {isLoadingFrames ? (
                <Loader2 size={13} className="animate-spin" />
              ) : (
                <Eye size={13} />
              )}
            </button>
          )}
          {onEditSubCalibration && type !== 'bias' && set.id !== null && (
            <button
              onClick={(e) => {
                e.stopPropagation();
                onEditSubCalibration(set.id!, type);
              }}
              className="flex items-center gap-1 px-2 py-1 text-xs text-content-muted hover:text-content hover:bg-surface-hover/50 rounded transition-colors"
              title="Edit sub-calibration"
            >
              <Settings size={13} />
            </button>
          )}
        </div>
      </div>

      {/* Sub-calibration rows — always visible */}
      {hasSubCal && (
        <div className={`border-t ${styles.border} divide-y divide-border/30`}>
          {sub_calibration.map((sub, i) => (
            <SubCalRow key={i} sub={sub} />
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * Empty state row for a missing calibration type.
 */
export function EmptyCalibrationRow({ type }: { type: CalibrationType }) {
  const labels: Record<CalibrationType, string> = {
    flat: 'flat',
    dark: 'dark',
    bias: 'bias',
  };
  const styles = typeStyles[type];

  return (
    <div className={`flex items-center min-h-[36px] rounded-lg border-2 border-dashed ${styles.border} opacity-60`}>
      <div className={`w-1 self-stretch ${styles.bar} rounded-l opacity-40 flex-shrink-0`} />
      <span className={`text-xs ${styles.text} px-3`}>No {labels[type]} linked</span>
    </div>
  );
}
