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
  /** Shared bounds/step for both fields. A field's own `left*`/`right*`
   *  override (below) takes precedence when given — e.g. the Register
   *  panel's detection pair shares `min={0}` but only `maxEccentricity`
   *  (the right field) has an upper bound (fix round 1, Minor #7). */
  min?: number;
  max?: number;
  step?: number;
  leftMin?: number;
  leftMax?: number;
  leftStep?: number;
  rightMin?: number;
  rightMax?: number;
  rightStep?: number;
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
  leftMin,
  leftMax,
  leftStep,
  rightMin,
  rightMax,
  rightStep,
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
          min={leftMin ?? min}
          max={leftMax ?? max}
          step={leftStep ?? step}
          disabled={disabled}
        />
        <NumericField
          label={rightLabel}
          value={rightValue}
          onCommit={onRightCommit}
          min={rightMin ?? min}
          max={rightMax ?? max}
          step={rightStep ?? step}
          disabled={disabled}
        />
      </div>
      {help && <p className="mt-1 text-[11px] text-content-muted">{help}</p>}
    </div>
  );
}
