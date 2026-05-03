import type { FrameAnalysis } from '../../types/models';
import type { EnrichedLightFrame } from '../calibration/LightsAnalysisTable';
import { getFilterColor, buildSeriesLabel } from '../../utils/filterColors';

// ============================================================
// Metric registry — all plottable metrics live here.
// Adding a new metric: add an entry; the chart picker, accessor,
// label, and unit handling all read from this registry.
// ============================================================

export interface MetricDefinition {
  key: string;
  label: string;
  unit?: string;
  precision: number;
  hint?: string;
  /** Returns the metric value in its native unit, or null when unavailable. */
  accessor: (frame: EnrichedLightFrame, analysis: FrameAnalysis | null) => number | null;
  /**
   * If FWHM-style display unit conversion (px ↔ arcsec) applies, set true.
   * Only `median_fwhm` uses this today.
   */
  arcsecConvertible?: boolean;
}

export const METRIC_REGISTRY: Record<string, MetricDefinition> = {
  median_fwhm: {
    key: 'median_fwhm',
    label: 'FWHM',
    unit: 'px',
    precision: 2,
    hint: 'lower is better',
    accessor: (_f, a) => a?.median_fwhm ?? null,
    arcsecConvertible: true,
  },
  median_hfr: {
    key: 'median_hfr',
    label: 'HFR',
    unit: 'px',
    precision: 2,
    hint: 'lower is better',
    accessor: (_f, a) => a?.median_hfr ?? null,
  },
  median_eccentricity: {
    key: 'median_eccentricity',
    label: 'Eccentricity',
    precision: 3,
    hint: 'lower is better',
    accessor: (_f, a) => a?.median_eccentricity ?? null,
  },
  median_snr: {
    key: 'median_snr',
    label: 'SNR (median)',
    precision: 1,
    hint: 'higher is better',
    accessor: (_f, a) => a?.median_snr ?? null,
  },
  frame_snr: {
    key: 'frame_snr',
    label: 'Frame SNR',
    unit: 'dB',
    precision: 1,
    hint: 'higher is better',
    accessor: (_f, a) => a?.frame_snr ?? null,
  },
  snr_weight: {
    key: 'snr_weight',
    label: 'SNR Weight',
    precision: 2,
    hint: 'higher is better',
    accessor: (_f, a) => a?.snr_weight ?? null,
  },
  psf_signal: {
    key: 'psf_signal',
    label: 'PSF Signal',
    unit: 'ADU',
    precision: 1,
    hint: 'higher is better',
    accessor: (_f, a) => a?.psf_signal ?? null,
  },
  stars_detected: {
    key: 'stars_detected',
    label: 'Stars',
    precision: 0,
    hint: 'higher is better',
    accessor: (_f, a) => a?.stars_detected ?? null,
  },
  background: {
    key: 'background',
    label: 'Background',
    unit: 'ADU',
    precision: 1,
    accessor: (_f, a) => a?.background ?? null,
  },
  noise: {
    key: 'noise',
    label: 'Noise',
    unit: 'ADU',
    precision: 2,
    accessor: (_f, a) => a?.noise ?? null,
  },
  trail_r_squared: {
    key: 'trail_r_squared',
    label: 'Trail R²',
    precision: 3,
    hint: 'lower is better',
    accessor: (_f, a) => a?.trail_r_squared ?? null,
  },
  median_beta: {
    key: 'median_beta',
    label: 'Moffat β',
    precision: 2,
    accessor: (_f, a) => a?.median_beta ?? null,
  },
};

export const METRIC_KEYS = Object.keys(METRIC_REGISTRY);

// ============================================================
// Chart data shaping
// ============================================================

export interface ChartDataPoint {
  frameId: number;
  fileId: number;
  timestamp: number;
  compressedX: number;
  dateLabel: string;
  filename: string;
  seriesKey: string;
  /** Reference to the underlying frame — read by metric accessors at render time. */
  frame: EnrichedLightFrame;
  /** Reference to the underlying analysis row (null when frame hasn't been analyzed yet). */
  analysis: FrameAnalysis | null;
}

export interface NightBoundary {
  timestamp: number;
  compressedX: number;
  label: string;
}

export type DotShape = 'circle' | 'square' | 'triangle' | 'diamond';

const DOT_SHAPES: DotShape[] = ['circle', 'square', 'triangle', 'diamond'];

export interface SeriesInfo {
  key: string;
  label: string;
  color: string;
  shape: DotShape;
}

export interface TimeSegment {
  realStart: number;
  realEnd: number;
  compressedStart: number;
  compressedEnd: number;
}

export interface ChartDataResult {
  dataPoints: ChartDataPoint[];
  nightBoundaries: NightBoundary[];
  seriesList: SeriesInfo[];
  segments: TimeSegment[];
}

const FOUR_HOURS_MS = 4 * 60 * 60 * 1000;

/**
 * Map a compressed-space value back to a real timestamp using segment info.
 * Used by MetricChart for tick label formatting.
 */
export function compressedToReal(compressed: number, segments: TimeSegment[]): number {
  for (const seg of segments) {
    if (compressed >= seg.compressedStart && compressed <= seg.compressedEnd) {
      return seg.realStart + (compressed - seg.compressedStart);
    }
  }
  for (let i = 0; i < segments.length - 1; i++) {
    if (compressed > segments[i].compressedEnd && compressed < segments[i + 1].compressedStart) {
      return segments[i].realEnd;
    }
  }
  if (segments.length > 0) {
    if (compressed <= segments[0].compressedStart) {
      return segments[0].realStart - (segments[0].compressedStart - compressed);
    }
    const last = segments[segments.length - 1];
    return last.realEnd + (compressed - last.compressedEnd);
  }
  return compressed;
}

/**
 * Transform displayed frames into chart-ready data.
 * Frames without `date_obs` are skipped (no X-axis position).
 * Frames without analysis are still included (analysis ref is null) so capture-side
 * metrics like exptime/gain/temp can be plotted before analysis runs.
 */
export function transformToChartData(
  frames: EnrichedLightFrame[],
  analysisData: Map<number, FrameAnalysis>
): ChartDataResult {
  const joined: { frame: EnrichedLightFrame; analysis: FrameAnalysis | null; ts: number }[] = [];

  for (const frame of frames) {
    if (!frame.date_obs) continue;
    const ts = new Date(frame.date_obs).getTime();
    if (isNaN(ts)) continue;
    joined.push({ frame, analysis: analysisData.get(frame.frame_id) ?? null, ts });
  }

  joined.sort((a, b) => a.ts - b.ts);

  if (joined.length === 0) {
    return { dataPoints: [], nightBoundaries: [], seriesList: [], segments: [] };
  }

  const cameraList = [...new Set(joined.map((j) => j.frame.camera))];
  const multipleCamera = cameraList.length > 1;
  const cameraShapeMap = new Map<string, DotShape>();
  for (let i = 0; i < cameraList.length; i++) {
    cameraShapeMap.set(cameraList[i], DOT_SHAPES[i % DOT_SHAPES.length]);
  }

  // Detect sessions (groups separated by >4h gaps)
  const segmentRanges: { startIdx: number; endIdx: number }[] = [];
  let segStart = 0;
  for (let i = 1; i < joined.length; i++) {
    if (joined[i].ts - joined[i - 1].ts > FOUR_HOURS_MS) {
      segmentRanges.push({ startIdx: segStart, endIdx: i - 1 });
      segStart = i;
    }
  }
  segmentRanges.push({ startIdx: segStart, endIdx: joined.length - 1 });

  let totalDuration = 0;
  for (const seg of segmentRanges) {
    totalDuration += joined[seg.endIdx].ts - joined[seg.startIdx].ts;
  }

  const gapSize = segmentRanges.length > 1 ? Math.max(totalDuration * 0.03, 60_000) : 0;

  const segments: TimeSegment[] = [];
  let compressedPos = 0;

  for (const seg of segmentRanges) {
    const realStart = joined[seg.startIdx].ts;
    const realEnd = joined[seg.endIdx].ts;
    const duration = realEnd - realStart;

    segments.push({
      realStart,
      realEnd,
      compressedStart: compressedPos,
      compressedEnd: compressedPos + duration,
    });

    compressedPos += duration + gapSize;
  }

  const compress = (ts: number): number => {
    for (const seg of segments) {
      if (ts >= seg.realStart && ts <= seg.realEnd) {
        return seg.compressedStart + (ts - seg.realStart);
      }
    }
    let best = segments[0].compressedStart;
    let minDist = Math.abs(ts - segments[0].realStart);
    for (const seg of segments) {
      const dStart = Math.abs(ts - seg.realStart);
      const dEnd = Math.abs(ts - seg.realEnd);
      if (dStart < minDist) { minDist = dStart; best = seg.compressedStart; }
      if (dEnd < minDist) { minDist = dEnd; best = seg.compressedEnd; }
    }
    return best;
  };

  const seriesMap = new Map<string, SeriesInfo>();
  const dataPoints: ChartDataPoint[] = [];

  for (const { frame, analysis, ts } of joined) {
    const seriesKey = `${frame.camera}::${frame.filter ?? ''}`;

    if (!seriesMap.has(seriesKey)) {
      seriesMap.set(seriesKey, {
        key: seriesKey,
        label: buildSeriesLabel(frame.camera, frame.filter, multipleCamera),
        color: getFilterColor(frame.filter),
        shape: cameraShapeMap.get(frame.camera) ?? 'circle',
      });
    }

    dataPoints.push({
      frameId: frame.frame_id,
      fileId: frame.file_id,
      timestamp: ts,
      compressedX: compress(ts),
      dateLabel: formatChartDate(ts),
      filename: frame.filename,
      seriesKey,
      frame,
      analysis,
    });
  }

  const nightBoundaries: NightBoundary[] = [];
  for (let i = 1; i < segments.length; i++) {
    const compressedMidpoint = (segments[i - 1].compressedEnd + segments[i].compressedStart) / 2;
    const realMidpoint = (segments[i - 1].realEnd + segments[i].realStart) / 2;

    nightBoundaries.push({
      timestamp: realMidpoint,
      compressedX: compressedMidpoint,
      label: `Night ${i + 1}`,
    });
  }

  const seriesList = Array.from(seriesMap.values());

  return { dataPoints, nightBoundaries, seriesList, segments };
}

function formatChartDate(ts: number): string {
  const d = new Date(ts);
  const month = d.toLocaleString('en-US', { month: 'short' });
  const day = d.getDate();
  const hours = String(d.getHours()).padStart(2, '0');
  const minutes = String(d.getMinutes()).padStart(2, '0');
  return `${month} ${day} ${hours}:${minutes}`;
}
