import { describe, expect, it } from 'vitest';
import { effectiveScaleMode, resolvedScaleLines } from './lnScale';
import type { PlanGroup, StackingPlan } from '../../../types/stacking';

const group = (over: Partial<PlanGroup>): PlanGroup => ({
  key: 'mono__L__bin1__180s', lnScalePx: 1536, lnScaleEstimated: false, lnWidth: 9576, lnHeight: 6388,
  ...over,
} as PlanGroup);
const planOf = (groups: PlanGroup[]) => ({ groups } as unknown as StackingPlan);

describe('effectiveScaleMode', () => {
  it('reads a stored mode as is', () => {
    expect(effectiveScaleMode({ scaleMode: 'fixed', scale: 1024 } as never)).toBe('fixed');
  });
  it('reads a legacy document by the L-9 rule', () => {
    expect(effectiveScaleMode({ scaleMode: null, scale: 1024 } as never)).toBe('auto');
    expect(effectiveScaleMode({ scaleMode: null, scale: 2048 } as never)).toBe('fixed');
  });
});

describe('resolvedScaleLines', () => {
  it('points Settings at the Stacking tab', () => {
    expect(resolvedScaleLines(null)).toEqual(["A frame set's Stacking tab shows the resolved value."]);
  });
  it('shows one line when every group agrees', () => {
    expect(resolvedScaleLines(planOf([group({}), group({ key: 'b' })]))).toEqual(['1536 px (9576 × 6388)']);
  });
  it('marks an estimate', () => {
    expect(resolvedScaleLines(planOf([group({ lnScaleEstimated: true })]))).toEqual([
      '1536 px (9576 × 6388) — estimated, the run confirms it',
    ]);
  });
  it('lists groups when they differ', () => {
    expect(
      resolvedScaleLines(planOf([group({}), group({ key: 'b', lnScalePx: 1024, lnWidth: 6248, lnHeight: 4176 })])),
    ).toEqual(['mono__L__bin1__180s: 1536 px (9576 × 6388)', 'b: 1024 px (6248 × 4176)']);
  });
});
