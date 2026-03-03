import type { FrameAnalysis } from '../../types/models';
import type { EnrichedLightFrame } from '../calibration/LightsAnalysisTable';
import { getFilterColor, buildSeriesLabel } from '../../utils/filterColors';

export interface ChartDataPoint {
  timestamp: number;
  compressedX: number;
  dateLabel: string;
  filename: string;
  seriesKey: string;
  fwhm: number | null;
  eccentricity: number | null;
  snr: number | null;
  stars: number | null;
  psfSignal: number | null;
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
  // In a gap between segments — return nearest segment boundary
  for (let i = 0; i < segments.length - 1; i++) {
    if (compressed > segments[i].compressedEnd && compressed < segments[i + 1].compressedStart) {
      return segments[i].realEnd;
    }
  }
  // Before first or after last
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
 * Transform displayed frames + analysis data into chart-ready data.
 * Joins frames with analysis by frame_id, sorts by time, detects session
 * boundaries, and builds a compressed timeline that collapses multi-day gaps.
 */
export function transformToChartData(
  frames: EnrichedLightFrame[],
  analysisData: Map<number, FrameAnalysis>
): ChartDataResult {
  // 1. Join frames with analysis, filter out missing date_obs or analysis
  const joined: { frame: EnrichedLightFrame; analysis: FrameAnalysis; ts: number }[] = [];

  for (const frame of frames) {
    if (!frame.date_obs) continue;
    const analysis = analysisData.get(frame.frame_id);
    if (!analysis) continue;

    const ts = new Date(frame.date_obs).getTime();
    if (isNaN(ts)) continue;

    joined.push({ frame, analysis, ts });
  }

  // 2. Sort by timestamp ascending
  joined.sort((a, b) => a.ts - b.ts);

  if (joined.length === 0) {
    return { dataPoints: [], nightBoundaries: [], seriesList: [], segments: [] };
  }

  // 3. Detect multiple cameras and assign shapes by camera
  const cameraList = [...new Set(joined.map((j) => j.frame.camera))];
  const multipleCamera = cameraList.length > 1;
  const cameraShapeMap = new Map<string, DotShape>();
  for (let i = 0; i < cameraList.length; i++) {
    cameraShapeMap.set(cameraList[i], DOT_SHAPES[i % DOT_SHAPES.length]);
  }

  // 4. Detect sessions (groups separated by >4h gaps)
  const segmentRanges: { startIdx: number; endIdx: number }[] = [];
  let segStart = 0;

  for (let i = 1; i < joined.length; i++) {
    if (joined[i].ts - joined[i - 1].ts > FOUR_HOURS_MS) {
      segmentRanges.push({ startIdx: segStart, endIdx: i - 1 });
      segStart = i;
    }
  }
  segmentRanges.push({ startIdx: segStart, endIdx: joined.length - 1 });

  // 5. Build compressed timeline — proportional within sessions, fixed small gaps between
  let totalDuration = 0;
  for (const seg of segmentRanges) {
    totalDuration += joined[seg.endIdx].ts - joined[seg.startIdx].ts;
  }

  // Gap = 3% of total data duration (min 1 minute), only if multiple segments
  const gapSize = segmentRanges.length > 1
    ? Math.max(totalDuration * 0.03, 60_000)
    : 0;

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

  // Compress a real timestamp to compressed-axis position
  const compress = (ts: number): number => {
    for (const seg of segments) {
      if (ts >= seg.realStart && ts <= seg.realEnd) {
        return seg.compressedStart + (ts - seg.realStart);
      }
    }
    // Fallback: nearest segment boundary
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

  // 6. Build series info map and data points
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
      timestamp: ts,
      compressedX: compress(ts),
      dateLabel: formatChartDate(ts),
      filename: frame.filename,
      seriesKey,
      fwhm: analysis.median_fwhm,
      eccentricity: analysis.median_eccentricity,
      snr: analysis.median_snr,
      stars: analysis.stars_detected,
      psfSignal: analysis.psf_signal,
    });
  }

  // 7. Night boundaries — placed in compressed gaps between segments
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
