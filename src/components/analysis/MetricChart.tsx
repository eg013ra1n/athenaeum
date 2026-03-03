import { useState, useRef, useMemo, useCallback } from 'react';
import {
  ScatterChart,
  Scatter,
  XAxis,
  YAxis,
  CartesianGrid,
  ReferenceLine,
  ResponsiveContainer,
} from 'recharts';
import type { ChartDataPoint, NightBoundary, SeriesInfo, DotShape, TimeSegment } from './chartDataTransform';
import { compressedToReal } from './chartDataTransform';

/** Renders an SVG shape matching the recharts scatter dot shapes */
export function LegendShape({ shape, color, size = 10 }: { shape: DotShape; color: string; size?: number }) {
  const half = size / 2;
  return (
    <svg width={size} height={size} viewBox={`0 0 ${size} ${size}`} className="flex-shrink-0">
      {shape === 'circle' && (
        <circle cx={half} cy={half} r={half * 0.8} fill={color} />
      )}
      {shape === 'square' && (
        <rect x={size * 0.1} y={size * 0.1} width={size * 0.8} height={size * 0.8} fill={color} />
      )}
      {shape === 'triangle' && (
        <polygon points={`${half},${size * 0.1} ${size * 0.9},${size * 0.9} ${size * 0.1},${size * 0.9}`} fill={color} />
      )}
      {shape === 'diamond' && (
        <polygon points={`${half},${size * 0.05} ${size * 0.95},${half} ${half},${size * 0.95} ${size * 0.05},${half}`} fill={color} />
      )}
    </svg>
  );
}

export type MetricKey = 'fwhm' | 'eccentricity' | 'snr' | 'stars' | 'psfSignal';

interface MetricChartProps {
  title: string;
  metricKey: MetricKey;
  dataPoints: ChartDataPoint[];
  nightBoundaries: NightBoundary[];
  seriesList: SeriesInfo[];
  segments: TimeSegment[];
  height?: number;
  yAxisLabel?: string;
  subtitle?: string;
  xDomain?: [number, number];
  onZoomSelect?: (left: number, right: number) => void;
}

function formatXTick(ts: number): string {
  const d = new Date(ts);
  const month = d.toLocaleString('en-US', { month: 'short' });
  const day = d.getDate();
  const hours = String(d.getHours()).padStart(2, '0');
  const minutes = String(d.getMinutes()).padStart(2, '0');
  return `${month} ${day} ${hours}:${minutes}`;
}

const METRIC_LABELS: Record<MetricKey, string> = {
  fwhm: 'FWHM',
  eccentricity: 'Eccentricity',
  snr: 'SNR',
  stars: 'Stars',
  psfSignal: 'PSF Signal',
};

const METRIC_PRECISION: Record<MetricKey, number> = {
  fwhm: 2,
  eccentricity: 3,
  snr: 1,
  stars: 0,
  psfSignal: 1,
};

// ---- Simple {x, y} point for unambiguous axis mapping ----

interface ScatterPoint {
  x: number;
  y: number;
  // Metadata for tooltip — stored here so custom dot can access it
  filename: string;
  dateLabel: string;
  seriesKey: string;
}

// ---- Hover tooltip state ----

interface HoveredPoint {
  point: ScatterPoint;
  series: SeriesInfo;
  cx: number;
  cy: number;
}

// ---- Custom dot renderer ----

interface DotProps {
  cx?: number;
  cy?: number;
  payload?: ScatterPoint;
  fill?: string;
  dotShape: DotShape;
  series: SeriesInfo;
  onHover: (hp: HoveredPoint | null) => void;
}

function CustomDot({ cx, cy, payload, fill, dotShape, series, onHover }: DotProps) {
  if (cx == null || cy == null || !payload) return null;
  const r = 5;

  const handleEnter = () => onHover({ point: payload, series, cx, cy });
  const handleLeave = () => onHover(null);

  const common = {
    fill,
    onMouseEnter: handleEnter,
    onMouseLeave: handleLeave,
    style: { cursor: 'default' },
  };

  switch (dotShape) {
    case 'square':
      return <rect x={cx - r} y={cy - r} width={r * 2} height={r * 2} {...common} />;
    case 'triangle': {
      const pts = `${cx},${cy - r} ${cx + r},${cy + r} ${cx - r},${cy + r}`;
      return <polygon points={pts} {...common} />;
    }
    case 'diamond': {
      const pts = `${cx},${cy - r} ${cx + r},${cy} ${cx},${cy + r} ${cx - r},${cy}`;
      return <polygon points={pts} {...common} />;
    }
    default:
      return <circle cx={cx} cy={cy} r={r} {...common} />;
  }
}

// ---- Zoom helpers ----

function getPlotBounds(container: HTMLDivElement): { left: number; right: number } | null {
  const clipRect = container.querySelector('svg clipPath rect');
  if (!clipRect) return null;
  const x = parseFloat(clipRect.getAttribute('x') || '0');
  const width = parseFloat(clipRect.getAttribute('width') || '0');
  return { left: x, right: x + width };
}

function pixelToValue(
  px: number,
  plotBounds: { left: number; right: number },
  domain: [number, number]
): number {
  const ratio = (px - plotBounds.left) / (plotBounds.right - plotBounds.left);
  const clamped = Math.max(0, Math.min(1, ratio));
  return domain[0] + clamped * (domain[1] - domain[0]);
}

const MIN_DRAG_PX = 8;

export function MetricChart({
  title,
  metricKey,
  dataPoints,
  nightBoundaries,
  seriesList,
  segments,
  height = 250,
  yAxisLabel,
  subtitle,
  xDomain,
  onZoomSelect,
}: MetricChartProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [dragStartX, setDragStartX] = useState<number | null>(null);
  const [dragCurrentX, setDragCurrentX] = useState<number | null>(null);
  const [hovered, setHovered] = useState<HoveredPoint | null>(null);
  const isDragging = dragStartX !== null;

  // Full X domain from compressed data, with 2% padding on each side
  const fullDomain = useMemo<[number, number]>(() => {
    if (dataPoints.length === 0) return [0, 1];
    let min = Infinity, max = -Infinity;
    for (const p of dataPoints) {
      if (p.compressedX < min) min = p.compressedX;
      if (p.compressedX > max) max = p.compressedX;
    }
    const pad = Math.max((max - min) * 0.02, 60_000); // at least 1 minute
    return [min - pad, max + pad];
  }, [dataPoints]);

  const activeDomain = xDomain ?? fullDomain;

  // Tick formatter: convert compressed position → real timestamp → formatted string
  const formatTick = useCallback((compressed: number) => {
    const realTs = compressedToReal(compressed, segments);
    return formatXTick(realTs);
  }, [segments]);

  // Filter to visible X range when zoomed, then map to {x, y} per series
  const seriesScatterData = useMemo(() => {
    const map = new Map<string, ScatterPoint[]>();
    for (const p of dataPoints) {
      if (xDomain && (p.compressedX < xDomain[0] || p.compressedX > xDomain[1])) continue;
      const yVal = p[metricKey];
      if (yVal == null) continue;

      const arr = map.get(p.seriesKey) || [];
      arr.push({
        x: p.compressedX,
        y: yVal,
        filename: p.filename,
        dateLabel: p.dateLabel,
        seriesKey: p.seriesKey,
      });
      map.set(p.seriesKey, arr);
    }
    return map;
  }, [dataPoints, xDomain, metricKey]);

  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    if (!onZoomSelect) return;
    const rect = containerRef.current?.getBoundingClientRect();
    if (!rect) return;
    setDragStartX(e.clientX - rect.left);
    setDragCurrentX(e.clientX - rect.left);
  }, [onZoomSelect]);

  const handleMouseMove = useCallback((e: React.MouseEvent) => {
    const rect = containerRef.current?.getBoundingClientRect();
    if (!rect) return;
    const mouseX = e.clientX - rect.left;
    const mouseY = e.clientY - rect.top;

    if (dragStartX !== null) {
      setDragCurrentX(mouseX);
    } else if (hovered) {
      const dx = mouseX - hovered.cx;
      const dy = mouseY - hovered.cy;
      if (Math.sqrt(dx * dx + dy * dy) > 10) {
        setHovered(null);
      }
    }
  }, [dragStartX, hovered]);

  const handleMouseUp = useCallback(() => {
    if (dragStartX === null || dragCurrentX === null) {
      setDragStartX(null);
      setDragCurrentX(null);
      return;
    }

    const dx = Math.abs(dragCurrentX - dragStartX);
    if (dx > MIN_DRAG_PX && onZoomSelect && containerRef.current) {
      const plotBounds = getPlotBounds(containerRef.current);
      if (plotBounds) {
        const left = Math.min(dragStartX, dragCurrentX);
        const right = Math.max(dragStartX, dragCurrentX);
        const valLeft = pixelToValue(left, plotBounds, activeDomain);
        const valRight = pixelToValue(right, plotBounds, activeDomain);
        onZoomSelect(valLeft, valRight);
      }
    }

    setDragStartX(null);
    setDragCurrentX(null);
  }, [dragStartX, dragCurrentX, onZoomSelect, activeDomain]);

  const handleHover = useCallback((hp: HoveredPoint | null) => {
    if (!isDragging) setHovered(hp);
  }, [isDragging]);

  // Selection overlay bounds
  const selLeft = isDragging && dragCurrentX !== null
    ? Math.min(dragStartX!, dragCurrentX)
    : 0;
  const selWidth = isDragging && dragCurrentX !== null
    ? Math.abs(dragCurrentX - dragStartX!)
    : 0;

  return (
    <div>
      <div className="flex items-baseline gap-2 mb-2 px-1">
        <h4 className="text-sm font-medium text-content">{title}</h4>
        {subtitle && <span className="text-xs text-content-muted">({subtitle})</span>}
      </div>
      <div
        ref={containerRef}
        className="relative select-none"
        onMouseDown={handleMouseDown}
        onMouseMove={handleMouseMove}
        onMouseUp={handleMouseUp}
        onMouseLeave={() => { handleMouseUp(); setHovered(null); }}
      >
        <ResponsiveContainer width="100%" height={height}>
          <ScatterChart margin={{ top: 5, right: 20, bottom: 5, left: 10 }}>
            <CartesianGrid strokeDasharray="3 3" stroke="#4c566a" />
            <XAxis
              dataKey="x"
              type="number"
              domain={activeDomain}
              tickFormatter={formatTick}
              stroke="#d8dee9"
              tick={{ fontSize: 11, fill: '#d8dee9' }}
              allowDuplicatedCategory={false}
              name="time"
            />
            <YAxis
              dataKey="y"
              type="number"
              stroke="#d8dee9"
              tick={{ fontSize: 11, fill: '#d8dee9' }}
              name={metricKey}
              label={
                yAxisLabel
                  ? {
                      value: yAxisLabel,
                      angle: -90,
                      position: 'insideLeft',
                      style: { fill: '#d8dee9', fontSize: 11 },
                    }
                  : undefined
              }
            />

            {/* Night boundary lines */}
            {nightBoundaries.map((nb, i) => (
              <ReferenceLine
                key={`night-${i}`}
                x={nb.compressedX}
                stroke="#4c566a"
                strokeDasharray="6 4"
                label={{
                  value: nb.label,
                  position: 'top',
                  fill: '#9CA3AF',
                  fontSize: 10,
                }}
              />
            ))}

            {/* One Scatter per series */}
            {seriesList.map((series) => {
              const data = seriesScatterData.get(series.key);
              if (!data || data.length === 0) return null;
              return (
                <Scatter
                  key={series.key}
                  name={series.label}
                  data={data}
                  fill={series.color}
                  isAnimationActive={false}
                  shape={(props: { cx?: number; cy?: number; payload?: ScatterPoint }) => (
                    <CustomDot
                      cx={props.cx}
                      cy={props.cy}
                      payload={props.payload}
                      fill={series.color}
                      dotShape={series.shape}
                      series={series}
                      onHover={handleHover}
                    />
                  )}
                />
              );
            })}
          </ScatterChart>
        </ResponsiveContainer>

        {/* Custom tooltip — only when directly hovering a dot */}
        {hovered && !isDragging && (
          <div
            className="absolute pointer-events-none z-10"
            style={{
              left: hovered.cx + 12,
              top: hovered.cy - 10,
            }}
          >
            <div className="bg-surface-elevated border border-border rounded-lg px-3 py-2 shadow-lg text-sm whitespace-nowrap">
              <div className="font-mono text-xs text-content-muted mb-1">{hovered.point.filename}</div>
              <div className="text-content-muted text-xs mb-1">{hovered.point.dateLabel}</div>
              <div className="flex items-center gap-1.5 mb-1">
                <LegendShape shape={hovered.series.shape} color={hovered.series.color} size={10} />
                <span className="text-xs text-content-muted">{hovered.series.label}</span>
              </div>
              <div className="text-content font-medium">
                {METRIC_LABELS[metricKey]}: {hovered.point.y.toFixed(METRIC_PRECISION[metricKey])}
              </div>
            </div>
          </div>
        )}

        {/* Drag selection overlay */}
        {isDragging && selWidth > MIN_DRAG_PX && (
          <div
            className="absolute top-0 bottom-0 bg-accent/20 border-x border-accent/40 pointer-events-none"
            style={{ left: selLeft, width: selWidth }}
          />
        )}
      </div>
    </div>
  );
}
