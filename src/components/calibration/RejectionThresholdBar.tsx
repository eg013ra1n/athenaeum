export interface RejectionThresholds {
  fwhm: string;
  eccentricity: string;
  median_snr: string;
  snr_db: string;
  psf_signal: string;
  snr_weight: string;
  trail: string;
  stars: string;
  score: string;
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
          { key: 'stars' as const, label: 'Stars <', placeholder: '#' },
          { key: 'fwhm' as const, label: 'FWHM >', placeholder: 'px' },
          { key: 'eccentricity' as const, label: 'Ecc >', placeholder: '0-1' },
          { key: 'median_snr' as const, label: 'SNR <', placeholder: 'ratio' },
          { key: 'snr_db' as const, label: 'SNR dB <', placeholder: 'dB' },
          { key: 'psf_signal' as const, label: 'PSF <', placeholder: 'sig' },
          { key: 'snr_weight' as const, label: 'SNR Wt <', placeholder: 'wt' },
          { key: 'trail' as const, label: 'Trail >', placeholder: 'R²' },
          { key: 'score' as const, label: 'Score <', placeholder: '%' },
        ]).map(({ key, label, placeholder }) => (
          <div key={key} className="flex items-center gap-1.5">
            <label className="text-xs text-content-secondary whitespace-nowrap">{label}</label>
            <input
              type="number"
              step="any"
              value={thresholds[key]}
              onChange={e => handleChange(key, e.target.value)}
              className="w-16 px-2 py-1 text-xs bg-surface-hover text-content rounded border border-border focus:outline-none focus:border-accent"
              placeholder={placeholder}
            />
          </div>
        ))}
      </div>
    </div>
  );
}
