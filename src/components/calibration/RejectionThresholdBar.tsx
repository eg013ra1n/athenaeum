export interface RejectionThresholds {
  fwhm: string;
  eccentricity: string;
  snr: string;
  alt: string;
}

interface RejectionThresholdBarProps {
  thresholds: RejectionThresholds;
  onChange: (thresholds: RejectionThresholds) => void;
}

export function RejectionThresholdBar({ thresholds, onChange }: RejectionThresholdBarProps) {
  const handleChange = (field: keyof RejectionThresholds, value: string) => {
    onChange({ ...thresholds, [field]: value });
  };

  return (
    <div className="flex items-center gap-4 px-3 py-2 bg-surface-elevated border border-border rounded-lg">
      <span className="text-xs font-medium text-content-muted uppercase tracking-wide whitespace-nowrap">
        Rejection Thresholds
      </span>
      <div className="flex items-center gap-3 flex-1">
        {([
          { key: 'fwhm' as const, label: 'FWHM' },
          { key: 'eccentricity' as const, label: 'Eccentricity' },
          { key: 'snr' as const, label: 'SNR' },
          { key: 'alt' as const, label: 'ALT' },
        ]).map(({ key, label }) => (
          <div key={key} className="flex items-center gap-1.5">
            <label className="text-xs text-content-secondary whitespace-nowrap">{label}</label>
            <input
              type="number"
              step="any"
              value={thresholds[key]}
              onChange={e => handleChange(key, e.target.value)}
              className="w-16 px-2 py-1 text-xs bg-surface-hover text-content rounded border border-border focus:outline-none focus:border-accent"
              placeholder="-"
            />
          </div>
        ))}
      </div>
    </div>
  );
}
