export type SkyFieldMetric = 'grouping' | 'frames' | 'integration';

export interface FieldStyle {
  label: string;
  className: string;
}

const groupingStyles: FieldStyle[] = [
  { label: 'Automatic object', className: 'text-info' },
  { label: 'Custom object', className: 'text-purple' },
  { label: 'Unorganized', className: 'text-success' },
];

// Fixed ranges keep the meaning of a color stable across date filters.
const metricClasses = [
  'text-content-muted',
  'text-info',
  'text-accent',
  'text-warning',
  'text-success',
];
const metricRanges = {
  frames: {
    bounds: [25, 100, 250],
    labels: [
      '0 / unknown',
      '1–24 exposures',
      '25–99 exposures',
      '100–249 exposures',
      '250+ exposures',
    ],
  },
  integration: {
    bounds: [3600, 18000, 72000],
    labels: ['0 / unknown', '< 1 h', '1–< 5 h', '5–< 20 h', '20+ h'],
  },
};

export function isSkyFieldMetric(value: string): value is SkyFieldMetric {
  return value === 'grouping' || value === 'frames' || value === 'integration';
}

export function getFieldLegend(metric: SkyFieldMetric): FieldStyle[] {
  if (metric === 'grouping') return groupingStyles;
  return metricRanges[metric].labels.map((label, index) => ({
    label,
    className: metricClasses[index],
  }));
}

/**
 * Map a field to fixed legend bins. `totalExposure` is seconds; `frameCount` is
 * the caller's exposure count. Nonpositive/nonfinite values use the unknown bin.
 * These colours express totals, not completion against an observing goal.
 */
export function getFieldStyle(
  metric: SkyFieldMetric,
  field: { locationType: string; isCustom: boolean; frameCount: number; totalExposure: number },
): FieldStyle {
  if (metric === 'grouping') {
    return groupingStyles[field.locationType === 'cluster' ? 2 : field.isCustom ? 1 : 0];
  }
  const value = metric === 'frames' ? field.frameCount : field.totalExposure;
  const index =
    !Number.isFinite(value) || value <= 0
      ? 0
      : 1 + metricRanges[metric].bounds.filter(bound => value >= bound).length;
  return getFieldLegend(metric)[index];
}
