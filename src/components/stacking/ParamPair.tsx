// Two `NumericField`s side by side — the Integrate panel's rejection-variant
// parameters (sigma low/high, percentile low/high, …) and the Register
// panel's Advanced detection pair (min SNR / max eccentricity).

import { NumericField } from './NumericField';

export interface ParamPairProps {
  leftLabel: string;
  leftValue: number;
  onLeftCommit: (n: number) => void;
  rightLabel: string;
  rightValue: number;
  onRightCommit: (n: number) => void;
  min?: number;
  max?: number;
  step?: number;
  help?: string;
  disabled?: boolean;
}

export function ParamPair({
  leftLabel,
  leftValue,
  onLeftCommit,
  rightLabel,
  rightValue,
  onRightCommit,
  min,
  max,
  step,
  help,
  disabled,
}: ParamPairProps) {
  return (
    <div>
      <div className="grid grid-cols-2 gap-2">
        <NumericField
          label={leftLabel}
          value={leftValue}
          onCommit={onLeftCommit}
          min={min}
          max={max}
          step={step}
          disabled={disabled}
        />
        <NumericField
          label={rightLabel}
          value={rightValue}
          onCommit={onRightCommit}
          min={min}
          max={max}
          step={step}
          disabled={disabled}
        />
      </div>
      {help && <p className="mt-1 text-[11px] text-content-muted">{help}</p>}
    </div>
  );
}
