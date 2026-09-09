// Stage 2 (Debayer) inspector panel: info only — debayering is a step of
// calibration (`calibration.debayerOsc`, edited on the Calibrate panel), not
// a separate stage with its own settings.

import type { StackingConfig } from '../../../types/stacking';

export interface DebayerPanelProps {
  config: StackingConfig;
}

export function DebayerPanel({ config }: DebayerPanelProps) {
  return (
    <div className="space-y-2">
      <p className="text-sm text-content-secondary">
        OSC groups are debayered (VNG) inside calibration; mono groups pass through.
      </p>
      <p className="text-xs text-content-muted">
        {config.calibration.debayerOsc
          ? 'Debayering is on — set on the Calibrate panel.'
          : 'Debayering is off — OSC lights stay Bayer-mosaiced. Turn it on from the Calibrate panel.'}
      </p>
    </div>
  );
}
