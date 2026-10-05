import type { LocalNormalizationConfig, LnScaleMode, StackingPlan } from '../../../types/stacking';

// Spec 2026-10-04 §2.1 (L-7): the supported LN scales.
export const LN_SCALES = [256, 384, 512, 768, 1024, 1536, 2048, 3072, 4096, 6144, 8192] as const;

// L-9, mirrored for a config the backend has not resolved yet.
export function effectiveScaleMode(local: Pick<LocalNormalizationConfig, 'scaleMode' | 'scale'>): LnScaleMode {
  return local.scaleMode ?? (local.scale === 1024 ? 'auto' : 'fixed');
}

export function resolvedScaleLines(plan: StackingPlan | null | undefined): string[] {
  if (!plan) return ["A frame set's Stacking tab shows the resolved value."];
  const groups = plan.groups.filter((g) => g.lnScalePx != null);
  if (groups.length === 0) return [];
  const text = (g: (typeof groups)[number]) =>
    `${g.lnScalePx} px${g.lnWidth != null && g.lnHeight != null ? ` (${g.lnWidth} × ${g.lnHeight})` : ''}` +
    (g.lnScaleEstimated ? ' — estimated, the run confirms it' : '');
  const distinct = new Set(groups.map(text));
  return distinct.size === 1 ? [text(groups[0])] : groups.map((g) => `${g.key}: ${text(g)}`);
}
