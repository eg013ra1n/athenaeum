// Stage 2 (Debayer) inspector panel: info only — debayering is a step of
// calibration (`calibration.debayerOsc`, edited on the Calibrate panel), not
// a separate stage with its own settings. For a project or an attested set
// the same switch sits on the Prepare panel, and this panel says so.

import { boardMode } from '../stageSummary';
import type { StackingConfig, StackingPlan } from '../../../types/stacking';

export interface DebayerPanelProps {
  config: StackingConfig;
  plan?: StackingPlan | null;
}

export function DebayerPanel({ config, plan = null }: DebayerPanelProps) {
  const prepare = boardMode(plan) === 'prepare';
  const where = prepare ? 'Prepare' : 'Calibrate';

  return (
    <div className="space-y-2">
      <p className="text-sm text-content-secondary">
        {prepare
          ? 'OSC frames are debayered (VNG) while the run prepares them; mono frames pass through.'
          : 'OSC groups are debayered (VNG) inside calibration; mono groups pass through.'}
      </p>
      <p className="text-xs text-content-muted">
        {config.calibration.debayerOsc
          ? `Debayering is on — set on the ${where} panel.`
          : `Debayering is off — OSC lights stay Bayer-mosaiced. Turn it on from the ${where} panel.`}
      </p>
    </div>
  );
}
