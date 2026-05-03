import { Minus, Plus, RotateCw } from 'lucide-react';

export interface RejectionThresholds {
  fwhm: string;
  eccentricity: string;
  frame_snr: string;
  snr_weight: string;
  trail: string;
  /** Unit the saved `fwhm` value was entered in. Metadata only — never rendered. */
  fwhm_unit?: 'px' | 'arcsec';
}

/** The user-editable numeric/string form fields (excludes the `fwhm_unit` metadata field). */
export type ThresholdFieldKey = 'fwhm' | 'eccentricity' | 'frame_snr' | 'snr_weight' | 'trail';

export interface ThresholdFieldDef {
  key: ThresholdFieldKey;
  label: string;
  placeholder: string;
  step: number;
  min: number;
  max?: number;
}

export const THRESHOLD_FIELDS: ThresholdFieldDef[] = [
  { key: 'fwhm', label: 'FWHM (px) >', placeholder: 'px', step: 0.1, min: 0 },
  { key: 'eccentricity', label: 'Ecc >', placeholder: '0.8', step: 0.01, min: 0, max: 1 },
  { key: 'frame_snr', label: 'Frame SNR (dB) <', placeholder: 'dB', step: 0.5, min: 0 },
  { key: 'snr_weight', label: 'SNR Wt <', placeholder: 'wt', step: 0.1, min: 0 },
];

export const EMPTY_THRESHOLDS: RejectionThresholds = {
  fwhm: '',
  eccentricity: '0.7',
  frame_snr: '',
  snr_weight: '',
  trail: '',
};

interface RejectionThresholdBarProps {
  thresholds: RejectionThresholds;
  onChange: (thresholds: RejectionThresholds) => void;
  onClear?: () => void;
  onLoadDefaults?: () => void;
  hasDefaults?: boolean;
  /** When true, FWHM label/placeholder shows arcsec instead of px */
  useArcsec?: boolean;
  /** When true, rejection auto-selects frames; when false, only highlights */
  rejectActive?: boolean;
  onToggleReject?: () => void;
}

function stepValue(
  current: string,
  field: ThresholdFieldDef,
  direction: 1 | -1,
): string {
  const val = parseFloat(current);
  if (isNaN(val)) {
    if (direction === -1) return current;
    return String(field.min ?? field.step);
  }
  let next = val + direction * field.step;
  const decimals = (field.step.toString().split('.')[1] || '').length;
  next = parseFloat(next.toFixed(decimals));
  if (next < field.min) next = field.min;
  if (field.max != null && next > field.max) next = field.max;
  return String(next);
}

export function RejectionThresholdBar({
  thresholds,
  onChange,
  onClear,
  useArcsec,
  rejectActive = true,
  onToggleReject,
}: RejectionThresholdBarProps) {
  const handleChange = (field: ThresholdFieldKey, value: string) => {
    onChange({ ...thresholds, [field]: value });
  };

  return (
    <div className="flex items-start gap-2 flex-wrap">
      <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
        <button
          onClick={onToggleReject}
          className={`h-7 px-2 text-[10px] font-medium uppercase tracking-wide rounded-lg border transition-colors ${
            rejectActive
              ? 'bg-error/20 border-error/30 text-error'
              : 'bg-surface-hover border-border text-content-muted hover:text-content-secondary'
          }`}
          title={rejectActive ? 'Rejection active: auto-selects rejected frames' : 'Rejection inactive: only highlights rejected frames'}
        >
          Reject
        </button>
        <span className="text-[10px] text-content-muted leading-tight">{rejectActive ? 'Select' : 'Highlight'}</span>
      </div>
      {THRESHOLD_FIELDS.map((rawField) => {
        const field = (useArcsec && rawField.key === 'fwhm')
          ? { ...rawField, label: 'FWHM (")', placeholder: '"' }
          : rawField;
        const value = thresholds[field.key];
        const numVal = parseFloat(value);
        const atMin = !isNaN(numVal) && numVal <= field.min;
        const atMax = field.max != null && !isNaN(numVal) && numVal >= field.max;
        const isEmpty = value === '';

        return (
          <div key={field.key} className="flex flex-col items-center gap-0.5">
            <div className="flex items-center">
              <button
                type="button"
                disabled={isEmpty || atMin}
                onClick={() => handleChange(field.key, stepValue(value, field, -1))}
                className="flex items-center justify-center w-5 h-7 rounded-l border border-r-0 border-border bg-surface-hover hover:bg-surface-elevated text-content-muted disabled:opacity-30 disabled:cursor-default transition-colors"
                tabIndex={-1}
              >
                <Minus size={10} />
              </button>
              <input
                type="number"
                step={field.step}
                min={field.min}
                max={field.max}
                value={value}
                onChange={e => {
                  const raw = e.target.value;
                  if (raw === '') { handleChange(field.key, ''); return; }
                  const n = parseFloat(raw);
                  if (isNaN(n)) return;
                  const clamped = field.max != null ? Math.min(n, field.max) : n;
                  handleChange(field.key, String(Math.max(field.min, clamped)));
                }}
                className="w-12 h-7 px-1 text-xs text-center bg-surface-hover text-content border-y border-border focus:outline-none focus:border-accent [appearance:textfield] [&::-webkit-inner-spin-button]:hidden [&::-webkit-outer-spin-button]:hidden"
                placeholder={field.placeholder}
              />
              <button
                type="button"
                disabled={atMax}
                onClick={() => handleChange(field.key, stepValue(value, field, 1))}
                className="flex items-center justify-center w-5 h-7 rounded-r border border-l-0 border-border bg-surface-hover hover:bg-surface-elevated text-content-muted disabled:opacity-30 disabled:cursor-default transition-colors"
                tabIndex={-1}
              >
                <Plus size={10} />
              </button>
            </div>
            <span className="text-[10px] text-content-muted leading-tight whitespace-nowrap">{field.label}</span>
          </div>
        );
      })}
      <div className="flex flex-col items-center gap-0.5">
        <label className="flex items-center justify-center h-7 cursor-pointer select-none">
          <input
            type="checkbox"
            checked={thresholds.trail === 'true'}
            onChange={e => handleChange('trail', e.target.checked ? 'true' : '')}
            className="rounded border-border text-accent focus:ring-accent"
          />
        </label>
        <span className="text-[10px] text-content-muted leading-tight">Trailed</span>
      </div>
      {onClear && (
        <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
          <button
            type="button"
            onClick={onClear}
            className="w-10 h-7 inline-flex items-center justify-center text-xs font-medium bg-surface-hover hover:bg-surface-elevated text-content-secondary rounded-lg border border-border transition-colors"
            title="Reset rejection thresholds"
          >
            <RotateCw size={12} />
          </button>
          <span className="text-[10px] text-content-muted leading-tight">Reset</span>
        </div>
      )}
    </div>
  );
}
