import { AlertTriangle } from 'lucide-react';
import type {
  CalibrationSetDetail,
  CalibrationSetWithFrameCount,
  SubCalibrationDetail,
} from '../../types/models';

type CalibrationType = 'flat' | 'dark' | 'bias';

interface CalibrationSetCardProps {
  type: CalibrationType;
  data: CalibrationSetWithFrameCount;
}

const typeStyles: Record<CalibrationType, { bg: string; border: string; header: string }> = {
  flat: {
    bg: 'bg-blue-900/25',
    border: 'border-blue-600/50',
    header: 'text-blue-300',
  },
  dark: {
    bg: 'bg-violet-900/25',
    border: 'border-violet-600/50',
    header: 'text-violet-300',
  },
  bias: {
    bg: 'bg-emerald-900/25',
    border: 'border-emerald-600/50',
    header: 'text-emerald-300',
  },
};

/**
 * Format temperature value for display.
 */
function formatTemp(temp: number | null): string {
  if (temp === null) return 'N/A';
  return `${temp.toFixed(1)}°C`;
}

/**
 * Format date for display.
 */
function formatDate(dateStr: string | null): string {
  if (!dateStr) return 'N/A';
  return new Date(dateStr).toLocaleDateString('en-GB', {
    day: 'numeric',
    month: 'short',
    year: 'numeric',
  });
}

/**
 * Generate card title based on calibration type and set details.
 */
function getCardTitle(type: CalibrationType, set: CalibrationSetDetail): string {
  switch (type) {
    case 'flat':
      return set.filter ? `Flat (${set.filter})` : 'Flat';
    case 'dark':
      return set.exptime !== null ? `Dark (${set.exptime}s)` : 'Dark';
    case 'bias':
      return 'Bias';
  }
}

interface PropertyRowProps {
  label: string;
  value: string;
  highlight?: boolean;
}

/**
 * Property row with accessible label and value display.
 * Uses minimum 14px text for labels, 16px for values.
 */
function PropertyRow({ label, value, highlight = false }: PropertyRowProps) {
  return (
    <div className="flex flex-col gap-0.5">
      <dt className="text-sm text-gray-300">{label}</dt>
      <dd className={`text-base font-medium ${highlight ? 'text-amber-300' : 'text-gray-100'}`}>
        {value}
      </dd>
    </div>
  );
}

/**
 * Sub-calibration card for nested calibration (e.g., Dark for Flat).
 */
function SubCalibrationCard({ sub }: { sub: SubCalibrationDetail }) {
  const subTypeStyles: Record<string, string> = {
    Dark: 'bg-violet-900/30 border-violet-700/40',
    DarkFlat: 'bg-indigo-900/30 border-indigo-700/40',
    Bias: 'bg-emerald-900/30 border-emerald-700/40',
  };

  const hasWarning = sub.temp_warning || sub.date_warning;
  const warningParts: string[] = [];
  if (sub.temp_warning) warningParts.push('Temperature differs');
  if (sub.date_warning) warningParts.push('Date differs');

  return (
    <div
      className={`border rounded-lg p-3 ${subTypeStyles[sub.calibration_type] || 'bg-gray-800/30 border-gray-700/40'}`}
    >
      <div className="flex items-center justify-between mb-2">
        <span className="text-sm font-semibold text-gray-200">{sub.calibration_type}</span>
        <span className="text-sm text-gray-400">{sub.set.frame_count} frames</span>
      </div>

      {hasWarning && (
        <div className="flex items-start gap-2 mb-2 text-sm">
          <AlertTriangle size={16} className="text-amber-400 flex-shrink-0 mt-0.5" aria-hidden="true" />
          <span className="text-amber-300">{warningParts.join(', ')}</span>
        </div>
      )}

      <div className="grid grid-cols-2 gap-2 text-sm">
        {sub.set.exptime !== null && (
          <div>
            <span className="text-gray-400">Exp:</span>{' '}
            <span className="text-gray-200">{sub.set.exptime}s</span>
          </div>
        )}
        <div>
          <span className="text-gray-400">Temp:</span>{' '}
          <span className={sub.temp_warning ? 'text-amber-300 font-medium' : 'text-gray-200'}>
            {formatTemp(sub.set.ccd_temp)}
          </span>
        </div>
        <div className="col-span-2">
          <span className="text-gray-400">Date:</span>{' '}
          <span className={sub.date_warning ? 'text-amber-300 font-medium' : 'text-gray-200'}>
            {formatDate(sub.set.date_start)}
          </span>
        </div>
      </div>
    </div>
  );
}

/**
 * Calibration set card with accessible design.
 * Features:
 * - Spacious layout with larger text (14px labels, 16px values)
 * - Clear warning display
 * - Sub-calibration section
 */
export function CalibrationSetCard({ type, data }: CalibrationSetCardProps) {
  const { set, warnings, sub_calibration, frame_count } = data;
  const styles = typeStyles[type];

  const hasTempWarning = warnings.some((w) => w.warning_type === 'temperature');
  const hasDateWarning = warnings.some((w) => w.warning_type === 'date');

  return (
    <article
      className={`${styles.bg} ${styles.border} border-2 rounded-xl p-5`}
      aria-label={`${type} calibration set`}
    >
      {/* Header */}
      <header className="flex items-center justify-between mb-4">
        <h4 className={`text-lg font-semibold ${styles.header}`}>
          {getCardTitle(type, set)}
        </h4>
        <span className="text-sm text-gray-300">
          {frame_count} light{frame_count !== 1 ? 's' : ''}
        </span>
      </header>

      {/* Warnings */}
      {warnings.length > 0 && (
        <div className="mb-4 p-3 bg-amber-900/20 rounded-lg border border-amber-700/50">
          {warnings.map((w, i) => (
            <div key={i} className="flex items-start gap-2 text-sm">
              <AlertTriangle size={18} className="text-amber-400 flex-shrink-0 mt-0.5" aria-hidden="true" />
              <span className="text-amber-200">{w.message}</span>
            </div>
          ))}
        </div>
      )}

      {/* Properties Grid */}
      <dl className="grid grid-cols-2 gap-x-4 gap-y-3">
        {set.exptime !== null && (
          <PropertyRow label="Exposure" value={`${set.exptime}s`} />
        )}
        <PropertyRow
          label="Temperature"
          value={
            set.temp_min !== set.temp_max
              ? `${formatTemp(set.ccd_temp)} (${formatTemp(set.temp_min)} - ${formatTemp(set.temp_max)})`
              : formatTemp(set.ccd_temp)
          }
          highlight={hasTempWarning}
        />
        {set.gain !== null && (
          <PropertyRow label="Gain" value={String(set.gain)} />
        )}
        {set.binning && (
          <PropertyRow label="Binning" value={set.binning} />
        )}
        {type === 'flat' && set.filter && (
          <PropertyRow label="Filter" value={set.filter} />
        )}
        <PropertyRow label="Date" value={formatDate(set.date_start)} highlight={hasDateWarning} />
        <PropertyRow label="Set Frames" value={String(set.frame_count)} />
      </dl>

      {/* Sub-calibration Section */}
      {sub_calibration && sub_calibration.length > 0 && (
        <div className="mt-4 pt-4 border-t border-gray-600/50">
          <h5 className="text-sm font-semibold text-gray-300 mb-3">Sub-calibration</h5>
          <div className="space-y-2">
            {sub_calibration.map((sub, i) => (
              <SubCalibrationCard key={i} sub={sub} />
            ))}
          </div>
        </div>
      )}
    </article>
  );
}

/**
 * Empty state card when no calibration of a type is linked.
 */
export function EmptyCalibrationCard({ type }: { type: CalibrationType }) {
  const styles = typeStyles[type];
  const labels: Record<CalibrationType, string> = {
    flat: 'Flat',
    dark: 'Dark',
    bias: 'Bias',
  };

  return (
    <div
      className={`${styles.bg} ${styles.border} border-2 border-dashed rounded-xl p-5 flex flex-col items-center justify-center min-h-[120px]`}
    >
      <span className={`text-lg font-semibold ${styles.header} opacity-60`}>
        No {labels[type]}
      </span>
      <span className="text-sm text-gray-400 mt-1">Not linked</span>
    </div>
  );
}
