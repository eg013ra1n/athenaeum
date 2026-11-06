// Stage 1 (Prepare) inspector panel: what the run does with the frames of a project or an attested set, which are
// already calibrated (by their publishers, or externally) — so there is no readiness readout and no flat, hot-pixel
// or master control here. Those belong to `CalibratePanel`, which this replaces for such a plan. The counts are the
// plan's own `prepare` (one per action, from the header probe the run reads); the one option is the debayer switch,
// the same `calibration.debayerOsc` the Calibrate panel edits, under the same label.

import { Checkbox } from '../../settings/Checkbox';
import { KV } from '../../ui';
import type { StackingConfig, StackingPlan } from '../../../types/stacking';

export interface PreparePanelProps {
  config: StackingConfig;
  plan: StackingPlan | null;
  disabled?: boolean;
  onPatchCalibration: (patch: Partial<StackingConfig['calibration']>) => void;
}

/** One count: a zero recedes, and a count of frames the run leaves out is the one that asks for a look. */
function Count({ n, warn }: { n: number; warn?: boolean }) {
  const tone = n === 0 ? 'text-content-faint' : warn ? 'text-warning' : 'text-content-secondary';
  return <span className={`tabular-nums ${tone}`}>{n}</span>;
}

export function PreparePanel({ config, plan, disabled, onPatchCalibration }: PreparePanelProps) {
  const prepare = plan?.prepare;

  return (
    <div className="space-y-4">
      {prepare ? (
        <KV
          items={[
            [<span title="Float files, used where they lie">In place</span>, <Count n={prepare.inPlace} />],
            [<span title="Colour-sensor mosaics, debayered into a new three-plane copy">Debayered</span>, <Count n={prepare.debayered} />],
            [
              <span title="Integer FITS and XISF files, converted into a float32 copy (debayered too when the file is a mosaic)">
                Converted to float FITS
              </span>,
              <Count n={prepare.converted} />,
            ],
            [
              <span title="Files no reader here decodes">Unsupported {'—'} left out of the run</span>,
              <Count n={prepare.unsupported} warn />,
            ],
            [
              <span title="Files inside a zip: the run reads them after extracting them">Checked when the run starts</span>,
              <Count n={prepare.atRun} />,
            ],
          ]}
        />
      ) : (
        <p className="text-xs text-content-muted">{plan == null ? 'No plan loaded yet.' : 'Counts unavailable'}</p>
      )}

      <div className="pt-3 border-t border-border/60 space-y-3">
        <Checkbox
          checked={config.calibration.debayerOsc}
          onChange={(checked) => onPatchCalibration({ debayerOsc: checked })}
          disabled={disabled}
          label="Debayer OSC lights (VNG)"
        />

        <p className="text-[11px] text-content-muted">
          Other calibration settings do not apply: these frames are already calibrated.
        </p>
      </div>
    </div>
  );
}
