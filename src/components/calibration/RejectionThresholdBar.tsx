import { Minus, Plus, X, RotateCw } from 'lucide-react';

export interface RejectionThresholds {
  fwhm: string;
  eccentricity: string;
  median_snr: string;
  frame_snr: string;
  psf_signal: string;
  snr_weight: string;
  trail: string;
  stars: string;
  score: string;
}

export interface ThresholdFieldDef {
  key: keyof RejectionThresholds;
  label: string;
  placeholder: string;
  step: number;
  min: number;
  max?: number;
}

export const THRESHOLD_FIELDS: ThresholdFieldDef[] = [
  { key: 'stars', label: 'Stars <', placeholder: '#', step: 1, min: 0 },
  { key: 'fwhm', label: 'FWHM (px) >', placeholder: 'px', step: 0.1, min: 0 },
  { key: 'eccentricity', label: 'Ecc >', placeholder: '0-1', step: 0.01, min: 0, max: 1 },
  { key: 'median_snr', label: 'SNR <', placeholder: 'ratio', step: 1, min: 0 },
  { key: 'frame_snr', label: 'Frame SNR (dB) <', placeholder: 'dB', step: 0.5, min: 0 },
  { key: 'psf_signal', label: 'PSF (ADU) <', placeholder: 'ADU', step: 1, min: 0 },
  { key: 'snr_weight', label: 'SNR Wt <', placeholder: 'wt', step: 0.1, min: 0 },
  { key: 'score', label: 'Score <', placeholder: '%', step: 1, min: 0, max: 100 },
];

export const EMPTY_THRESHOLDS: RejectionThresholds = {
  fwhm: '',
  eccentricity: '',
  median_snr: '',
  frame_snr: '',
  psf_signal: '',
  snr_weight: '',
  trail: '',
  stars: '',
  score: '',
};

interface RejectionThresholdBarProps {
  thresholds: RejectionThresholds;
  onChange: (thresholds: RejectionThresholds) => void;
  onClear?: () => void;
  onLoadDefaults?: () => void;
  hasDefaults?: boolean;
  /** When true, FWHM label/placeholder shows arcsec instead of px */
  useArcsec?: boolean;
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
  onLoadDefaults,
  hasDefaults,
  useArcsec,
}: RejectionThresholdBarProps) {
  const handleChange = (field: keyof RejectionThresholds, value: string) => {
    onChange({ ...thresholds, [field]: value });
  };

  return (
    <div className="flex items-center gap-3 px-3 py-2 bg-surface-elevated border border-border rounded-lg">
      <span className="text-xs font-medium text-content-muted uppercase tracking-wide whitespace-nowrap">
        Reject
      </span>
      <div className="flex items-center gap-2 flex-1 flex-wrap">
        {THRESHOLD_FIELDS.map((rawField) => {
          // Override FWHM label/placeholder when in arcsec mode
          const field = (useArcsec && rawField.key === 'fwhm')
            ? { ...rawField, label: 'FWHM (") >', placeholder: '"' }
            : rawField;
          const value = thresholds[field.key];
          const numVal = parseFloat(value);
          const atMin = !isNaN(numVal) && numVal <= field.min;
          const atMax = field.max != null && !isNaN(numVal) && numVal >= field.max;
          const isEmpty = value === '';

          return (
            <div key={field.key} className="flex items-center gap-1">
              <label className="text-xs text-content-secondary whitespace-nowrap">{field.label}</label>
              <div className="flex items-center">
                <button
                  type="button"
                  disabled={isEmpty || atMin}
                  onClick={() => handleChange(field.key, stepValue(value, field, -1))}
                  className="flex items-center justify-center w-5 h-6 rounded-l border border-r-0 border-border bg-surface-hover hover:bg-surface-elevated text-content-muted disabled:opacity-30 disabled:cursor-default transition-colors"
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
                  onChange={e => handleChange(field.key, e.target.value)}
                  className="w-14 h-6 px-1 text-xs text-center bg-surface-hover text-content border-y border-border focus:outline-none focus:border-accent [appearance:textfield] [&::-webkit-inner-spin-button]:hidden [&::-webkit-outer-spin-button]:hidden"
                  placeholder={field.placeholder}
                />
                <button
                  type="button"
                  disabled={atMax}
                  onClick={() => handleChange(field.key, stepValue(value, field, 1))}
                  className="flex items-center justify-center w-5 h-6 rounded-r border border-l-0 border-border bg-surface-hover hover:bg-surface-elevated text-content-muted disabled:opacity-30 disabled:cursor-default transition-colors"
                  tabIndex={-1}
                >
                  <Plus size={10} />
                </button>
              </div>
            </div>
          );
        })}
        <label className="flex items-center gap-1 cursor-pointer select-none">
          <input
            type="checkbox"
            checked={thresholds.trail === 'true'}
            onChange={e => handleChange('trail', e.target.checked ? 'true' : '')}
            className="rounded border-border text-accent focus:ring-accent"
          />
          <span className="text-xs text-content-secondary whitespace-nowrap">Trailed</span>
        </label>
      </div>
      <div className="flex items-center gap-1.5 flex-shrink-0">
        {hasDefaults && onLoadDefaults && (
          <button
            type="button"
            onClick={onLoadDefaults}
            className="flex items-center justify-center w-6 h-6 text-content-muted hover:text-content transition-colors"
            title="Load saved defaults"
          >
            <RotateCw size={14} />
          </button>
        )}
        {onClear && (
          <button
            type="button"
            onClick={onClear}
            className="flex items-center justify-center w-6 h-6 text-content-muted hover:text-content transition-colors"
            title="Clear all thresholds"
          >
            <X size={14} />
          </button>
        )}
      </div>
    </div>
  );
}
