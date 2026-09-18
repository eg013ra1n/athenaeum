// Stage 1 (Calibrate) inspector panel: a read-only readiness readout off
// `plan.readiness` (masters resolved / unlinked lights / missing master
// files) plus the three light-calibration options the Export tab already
// exposes for the same underlying `CalibratedLightOptions` — same labels,
// reused verbatim so the two surfaces never disagree.

import { Checkbox } from '../../settings/Checkbox';
import type { StackingConfig, StackingPlan } from '../../../types/stacking';

export interface CalibratePanelProps {
  config: StackingConfig;
  onChange: (next: StackingConfig) => void;
  plan: StackingPlan | null;
  disabled?: boolean;
}

export function CalibratePanel({ config, onChange, plan, disabled }: CalibratePanelProps) {
  const cal = config.calibration;
  const readiness = plan?.readiness;

  const patchCalibration = (patch: Partial<StackingConfig['calibration']>) => {
    onChange({ ...config, calibration: { ...cal, ...patch } });
  };

  return (
    <div className="space-y-4">
      <div>
        <h4 className="text-xs font-medium text-content-secondary mb-1.5">Masters resolved</h4>
        {readiness ? (
          <ul className="space-y-1 text-xs text-content-muted">
            <li>
              <span className="text-content-secondary tabular-nums">
                {readiness.total - readiness.unlinkedLights}/{readiness.total}
              </span>{' '}
              lights have calibration links
            </li>
            <li>
              <span className="text-content-secondary tabular-nums">{readiness.rawSetsWithoutMaster}</span>{' '}
              calibration set(s) without a built master
            </li>
            <li>
              <span className="text-content-secondary tabular-nums">{readiness.missingMasterFiles}</span>{' '}
              master file(s) missing on disk
            </li>
          </ul>
        ) : (
          <p className="text-xs text-content-muted">No plan loaded yet.</p>
        )}
      </div>

      <div className="pt-3 border-t border-border/60 space-y-3">
        <div>
          <Checkbox
            checked={cal.flatNorm}
            onChange={(checked) => patchCalibration({ flatNorm: checked })}
            disabled={disabled}
            label="Normalize master flat (recommended)"
          />

          {cal.flatNorm && (
            <div className="mt-2 ml-6 space-y-1.5">
              <label className="flex items-center gap-2 text-xs text-content-secondary cursor-pointer">
                <input
                  type="radio"
                  name="stacking-flatnorm-mode"
                  checked={cal.flatNormMode === 'centralThird'}
                  disabled={disabled}
                  onChange={() => patchCalibration({ flatNormMode: 'centralThird' })}
                  className="w-3.5 h-3.5 text-accent border-border focus:ring-accent disabled:opacity-50"
                />
                Central third mean (Athenaeum)
              </label>
              <label className="flex items-center gap-2 text-xs text-content-secondary cursor-pointer">
                <input
                  type="radio"
                  name="stacking-flatnorm-mode"
                  checked={cal.flatNormMode === 'pixinsightTrimmed'}
                  disabled={disabled}
                  onChange={() => patchCalibration({ flatNormMode: 'pixinsightTrimmed' })}
                  className="w-3.5 h-3.5 text-accent border-border focus:ring-accent disabled:opacity-50"
                />
                Full-frame trimmed mean
              </label>
            </div>
          )}
        </div>

        <Checkbox
          checked={cal.hotPixelCorrection}
          onChange={(checked) => patchCalibration({ hotPixelCorrection: checked })}
          disabled={disabled}
          label="Hot-pixel correction"
        />

        <Checkbox
          checked={cal.debayerOsc}
          onChange={(checked) => patchCalibration({ debayerOsc: checked })}
          disabled={disabled}
          label="Debayer OSC lights (VNG)"
        />

        <p className="text-[11px] text-content-muted">
          Intermediate calibrated frames in the working folder are always FITS. The export and send
          format is chosen on the Export tab.
        </p>
      </div>
    </div>
  );
}
