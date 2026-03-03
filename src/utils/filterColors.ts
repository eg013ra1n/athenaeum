/**
 * Shared filter color mapping for astrophotography filters.
 * Extracted from FilterGroupCard — used by chart series and filter cards.
 */

const KNOWN_FILTER_COLORS: Record<string, string> = {};

// Build lookup once
function initKnownColors() {
  // Narrowband
  const narrowband: [string[], string][] = [
    [['ha', 'h-alpha'], '#DC2626'],  // red
    [['oiii', 'o3'], '#06B6D4'],     // cyan
    [['sii', 's2'], '#7C3AED'],      // purple
  ];
  // Broadband
  const broadband: [string[], string][] = [
    [['r', 'red'], '#EF4444'],
    [['g', 'green'], '#22C55E'],
    [['b', 'blue'], '#3B82F6'],
    [['l', 'lum', 'luminance'], '#9CA3AF'],
  ];
  for (const [keys, color] of [...narrowband, ...broadband]) {
    for (const k of keys) {
      KNOWN_FILTER_COLORS[k] = color;
    }
  }
}
initKnownColors();

// Palette for unknown filters — visually distinct, dark-theme friendly
const UNKNOWN_PALETTE = [
  '#F59E0B', // amber
  '#EC4899', // pink
  '#14B8A6', // teal
  '#F97316', // orange
  '#8B5CF6', // violet
  '#06B6D4', // cyan
  '#84CC16', // lime
  '#FB7185', // rose
];

const unknownColorCache = new Map<string, string>();
let nextUnknownIdx = 0;

/**
 * Get a display color for an astrophotography filter name.
 * Known filters (Ha, OIII, SII, R, G, B, L) get fixed colors.
 * Unknown filters get a persistent random color from a palette.
 */
export function getFilterColor(filter: string | null): string {
  if (!filter) return '#9CA3AF'; // gray for luminance / no filter

  const f = filter.toLowerCase();

  // Check narrowband with includes (Ha can appear as "Ha 7nm", etc.)
  if (f.includes('ha') || f === 'h-alpha') return '#DC2626';
  if (f.includes('oiii') || f === 'o3') return '#06B6D4';
  if (f.includes('sii') || f === 's2') return '#7C3AED';

  // Check broadband exact match
  const known = KNOWN_FILTER_COLORS[f];
  if (known) return known;

  // Unknown filter — assign from palette with caching
  const cached = unknownColorCache.get(f);
  if (cached) return cached;

  const color = UNKNOWN_PALETTE[nextUnknownIdx % UNKNOWN_PALETTE.length];
  nextUnknownIdx++;
  unknownColorCache.set(f, color);
  return color;
}

/**
 * Build a display label for a chart series.
 * When multiple cameras are present, shows "Camera — Filter".
 * Otherwise just shows the filter name.
 */
export function buildSeriesLabel(
  camera: string | null,
  filter: string | null,
  multipleCamera: boolean
): string {
  const filterLabel = filter || 'No Filter';
  if (multipleCamera && camera) {
    return `${camera} — ${filterLabel}`;
  }
  return filterLabel;
}
