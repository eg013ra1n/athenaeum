import { useState, useRef, useMemo, useCallback } from 'react';
import { createPortal } from 'react-dom';
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
import { compressedToReal, METRIC_REGISTRY } from './chartDataTransform';

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

interface MetricChartProps {
  /** Registry key from METRIC_REGISTRY (e.g. 'median_fwhm', 'exptime'). */
  metricKey: string;
  dataPoints: ChartDataPoint[];
  nightBoundaries: NightBoundary[];
  seriesList: SeriesInfo[];
  segments: TimeSegment[];
  /** Optional title override. Defaults to METRIC_REGISTRY[metricKey].label. */
  title?: string;
  /** Optional subtitle override. Defaults to the metric's hint (e.g. "lower is better"). */
  subtitle?: string;
  /** Optional Y-axis label override. Defaults to the metric's unit. */
  yAxisLabel?: string;
  xDomain?: [number, number];
  onZoomSelect?: (left: number, right: number) => void;
  /** Multiplier applied to raw metric value (used for FWHM px → arcsec when arcsecConvertible). */
  valueScale?: number;

  // ---- Selection / brush ----
  selectedFrameIds?: Set<number>;
  rejectedFrameIds?: Set<number>;
  onPointClick?: (frameId: number) => void;
  /** Fires after a Shift+drag with the IDs of all enclosed points. */
  onShiftBrush?: (frameIds: number[]) => void;

  // ---- Threshold line ----
  thresholdLine?: { value: number; label: string } | null;
}

function formatXTick(ts: number): string {
  const d = new Date(ts);
  const month = d.toLocaleString('en-US', { month: 'short' });
  const day = d.getDate();
  const hours = String(d.getHours()).padStart(2, '0');
  const minutes = String(d.getMinutes()).padStart(2, '0');
  return `${month} ${day} ${hours}:${minutes}`;
}

interface ScatterPoint {
  x: number;
  y: number;
  frameId: number;
  filename: string;
  dateLabel: string;
  seriesKey: string;
}

interface HoveredPoint {
  point: ScatterPoint;
  series: SeriesInfo;
  cx: number;
  cy: number;
}

interface DotProps {
  cx?: number;
  cy?: number;
  payload?: ScatterPoint;
  fill?: string;
  dotShape: DotShape;
  series: SeriesInfo;
  isSelected: boolean;
  isRejected: boolean;
  clickable: boolean;
  onHover: (hp: HoveredPoint | null) => void;
  onPosition?: (frameId: number, cx: number, cy: number) => void;
}

function CustomDot({ cx, cy, payload, fill, dotShape, series, isSelected, isRejected, clickable, onHover, onPosition }: DotProps) {
  if (cx == null || cy == null || !payload) return null;

  if (onPosition) onPosition(payload.frameId, cx, cy);

  const baseR = 5;
  const r = isSelected ? baseR + 1 : baseR;
  const opacity = isSelected ? 1 : 0.55;
  const effectiveFill = isRejected ? '#bf616a' : fill;

  const handleEnter = () => onHover({ point: payload, series, cx, cy });
  const handleLeave = () => onHover(null);

  const common = {
    fill: effectiveFill,
    fillOpacity: opacity,
    stroke: isSelected ? '#eceff4' : 'none',
    strokeWidth: isSelected ? 1.5 : 0,
    onMouseEnter: handleEnter,
    onMouseLeave: handleLeave,
    style: { cursor: clickable ? ('pointer' as const) : ('default' as const) },
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

interface PlotBounds {
  left: number;
  right: number;
  top: number;
  bottom: number;
}

function getPlotBounds(container: HTMLDivElement): PlotBounds | null {
  const clipRect = container.querySelector('svg clipPath rect');
  if (!clipRect) return null;
  const x = parseFloat(clipRect.getAttribute('x') || '0');
  const y = parseFloat(clipRect.getAttribute('y') || '0');
  const width = parseFloat(clipRect.getAttribute('width') || '0');
  const height = parseFloat(clipRect.getAttribute('height') || '0');
  return { left: x, right: x + width, top: y, bottom: y + height };
}

function isInsidePlot(x: number, y: number, b: PlotBounds): boolean {
  return x >= b.left && x <= b.right && y >= b.top && y <= b.bottom;
}

function clampToPlot(x: number, y: number, b: PlotBounds): { x: number; y: number } {
  return {
    x: Math.max(b.left, Math.min(b.right, x)),
    y: Math.max(b.top, Math.min(b.bottom, y)),
  };
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

const MIN_DRAG_PX = 6;

export function MetricChart({
  metricKey,
  dataPoints,
  nightBoundaries,
  seriesList,
  segments,
  title,
  subtitle,
  yAxisLabel,
  xDomain,
  onZoomSelect,
  valueScale = 1,
  selectedFrameIds,
  rejectedFrameIds,
  onPointClick,
  onShiftBrush,
  thresholdLine,
}: MetricChartProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const dotPositionsRef = useRef<Map<number, { cx: number; cy: number }>>(new Map());
  // Reset position map at the top of each render — child dots will repopulate it.
  dotPositionsRef.current = new Map();

  const [dragStartX, setDragStartX] = useState<number | null>(null);
  const [dragStartY, setDragStartY] = useState<number | null>(null);
  const [dragCurrentX, setDragCurrentX] = useState<number | null>(null);
  const [dragCurrentY, setDragCurrentY] = useState<number | null>(null);
  const [isBrushMode, setIsBrushMode] = useState(false);
  const [hovered, setHovered] = useState<HoveredPoint | null>(null);
  const isDragging = dragStartX !== null;

  const def = METRIC_REGISTRY[metricKey];
  const resolvedTitle = title ?? (def ? `${def.label}${def.unit ? ` (${def.unit})` : ''}` : metricKey);
  const resolvedSubtitle = subtitle ?? def?.hint;

  const fullDomain = useMemo<[number, number]>(() => {
    if (dataPoints.length === 0) return [0, 1];
    let min = Infinity, max = -Infinity;
    for (const p of dataPoints) {
      if (p.compressedX < min) min = p.compressedX;
      if (p.compressedX > max) max = p.compressedX;
    }
    const pad = Math.max((max - min) * 0.02, 60_000);
    return [min - pad, max + pad];
  }, [dataPoints]);

  const activeDomain = xDomain ?? fullDomain;

  const formatTick = useCallback((compressed: number) => {
    const realTs = compressedToReal(compressed, segments);
    return formatXTick(realTs);
  }, [segments]);

  const seriesScatterData = useMemo(() => {
    const map = new Map<string, ScatterPoint[]>();
    if (!def) return map;
    for (const p of dataPoints) {
      if (xDomain && (p.compressedX < xDomain[0] || p.compressedX > xDomain[1])) continue;
      const raw = def.accessor(p.frame, p.analysis);
      if (raw == null) continue;
      const yVal = raw * valueScale;

      const arr = map.get(p.seriesKey) || [];
      arr.push({
        x: p.compressedX,
        y: yVal,
        frameId: p.frameId,
        filename: p.filename,
        dateLabel: p.dateLabel,
        seriesKey: p.seriesKey,
      });
      map.set(p.seriesKey, arr);
    }
    return map;
  }, [dataPoints, xDomain, def, valueScale]);

  const recordPosition = useCallback((frameId: number, cx: number, cy: number) => {
    dotPositionsRef.current.set(frameId, { cx, cy });
  }, []);

  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    if (!onZoomSelect && !onShiftBrush) return;
    const rect = containerRef.current?.getBoundingClientRect();
    if (!rect || !containerRef.current) return;
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;
    // Don't start a drag if the press began outside the plot area
    // (e.g. on the axis labels, grid margin, or chart title row).
    const plot = getPlotBounds(containerRef.current);
    if (plot && !isInsidePlot(x, y, plot)) return;
    setDragStartX(x);
    setDragStartY(y);
    setDragCurrentX(x);
    setDragCurrentY(y);
    setIsBrushMode(e.shiftKey && !!onShiftBrush);
  }, [onZoomSelect, onShiftBrush]);

  const handleMouseMove = useCallback((e: React.MouseEvent) => {
    const rect = containerRef.current?.getBoundingClientRect();
    if (!rect) return;
    const mouseX = e.clientX - rect.left;
    const mouseY = e.clientY - rect.top;

    if (dragStartX !== null) {
      // Clamp the drag-current position to the plot area so the selection
      // overlay never extends past the chart edges (axes / margins).
      const plot = containerRef.current ? getPlotBounds(containerRef.current) : null;
      if (plot) {
        const c = clampToPlot(mouseX, mouseY, plot);
        setDragCurrentX(c.x);
        setDragCurrentY(c.y);
      } else {
        setDragCurrentX(mouseX);
        setDragCurrentY(mouseY);
      }
    } else if (hovered) {
      const dx = mouseX - hovered.cx;
      const dy = mouseY - hovered.cy;
      if (Math.sqrt(dx * dx + dy * dy) > 10) setHovered(null);
    }
  }, [dragStartX, hovered]);

  const handleMouseUp = useCallback((e?: React.MouseEvent) => {
    if (dragStartX === null || dragCurrentX === null || dragStartY === null || dragCurrentY === null) {
      setDragStartX(null);
      setDragStartY(null);
      setDragCurrentX(null);
      setDragCurrentY(null);
      setIsBrushMode(false);
      return;
    }

    const dx = Math.abs(dragCurrentX - dragStartX);
    const dy = Math.abs(dragCurrentY - dragStartY);
    const isClick = dx < MIN_DRAG_PX && dy < MIN_DRAG_PX;

    if (isClick && !isBrushMode && onPointClick && e) {
      // It's a click, not a drag — find the nearest dot to the cursor and toggle it
      const rect = containerRef.current?.getBoundingClientRect();
      if (rect) {
        const mx = e.clientX - rect.left;
        const my = e.clientY - rect.top;
        const HIT_R = 8;
        let closest: { frameId: number; dist: number } | null = null;
        for (const [frameId, pos] of dotPositionsRef.current) {
          const d = Math.hypot(pos.cx - mx, pos.cy - my);
          if (d <= HIT_R && (!closest || d < closest.dist)) {
            closest = { frameId, dist: d };
          }
        }
        if (closest) onPointClick(closest.frameId);
      }
    } else if (isBrushMode && onShiftBrush) {
      // Shift-brush: collect frame IDs whose dots fall inside the box
      if (dx > MIN_DRAG_PX || dy > MIN_DRAG_PX) {
        const minX = Math.min(dragStartX, dragCurrentX);
        const maxX = Math.max(dragStartX, dragCurrentX);
        const minY = Math.min(dragStartY, dragCurrentY);
        const maxY = Math.max(dragStartY, dragCurrentY);
        const ids: number[] = [];
        for (const [frameId, pos] of dotPositionsRef.current) {
          if (pos.cx >= minX && pos.cx <= maxX && pos.cy >= minY && pos.cy <= maxY) {
            ids.push(frameId);
          }
        }
        if (ids.length > 0) onShiftBrush(ids);
      }
    } else if (onZoomSelect) {
      // Zoom drag: only X matters
      if (dx > MIN_DRAG_PX && containerRef.current) {
        const plotBounds = getPlotBounds(containerRef.current);
        if (plotBounds) {
          const left = Math.min(dragStartX, dragCurrentX);
          const right = Math.max(dragStartX, dragCurrentX);
          const valLeft = pixelToValue(left, plotBounds, activeDomain);
          const valRight = pixelToValue(right, plotBounds, activeDomain);
          onZoomSelect(valLeft, valRight);
        }
      }
    }

    setDragStartX(null);
    setDragStartY(null);
    setDragCurrentX(null);
    setDragCurrentY(null);
    setIsBrushMode(false);
  }, [dragStartX, dragStartY, dragCurrentX, dragCurrentY, isBrushMode, onShiftBrush, onZoomSelect, onPointClick, activeDomain]);

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
  const selTop = isDragging && dragCurrentY !== null
    ? Math.min(dragStartY!, dragCurrentY)
    : 0;
  const selHeight = isDragging && dragCurrentY !== null
    ? Math.abs(dragCurrentY - dragStartY!)
    : 0;

  const formatValue = (v: number) =>
    def ? v.toFixed(def.precision) : v.toString();

  return (
    <div className="flex flex-col h-full min-h-0">
      <div className="flex items-baseline gap-2 mb-1 px-1 flex-shrink-0">
        <h4 className="text-sm font-medium text-content">{resolvedTitle}</h4>
        {resolvedSubtitle && <span className="text-xs text-content-muted">({resolvedSubtitle})</span>}
      </div>
      <div
        ref={containerRef}
        className="relative select-none flex-1 min-h-0"
        onMouseDown={handleMouseDown}
        onMouseMove={handleMouseMove}
        onMouseUp={(e) => handleMouseUp(e)}
        onMouseLeave={() => { handleMouseUp(); setHovered(null); }}
      >
        <ResponsiveContainer width="100%" height="100%">
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

            {thresholdLine && (
              <ReferenceLine
                y={thresholdLine.value}
                stroke="#bf616a"
                strokeDasharray="4 4"
                strokeWidth={1.5}
                label={{
                  value: thresholdLine.label,
                  position: 'right',
                  fill: '#bf616a',
                  fontSize: 14,
                  fontWeight: 600,
                }}
              />
            )}

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
                      isSelected={!!props.payload && !!selectedFrameIds?.has(props.payload.frameId)}
                      isRejected={!!props.payload && !!rejectedFrameIds?.has(props.payload.frameId)}
                      clickable={!!onPointClick}
                      onHover={handleHover}
                      onPosition={recordPosition}
                    />
                  )}
                />
              );
            })}
          </ScatterChart>
        </ResponsiveContainer>

        {hovered && !isDragging && containerRef.current && (() => {
          // Portal the tooltip to document.body so it never extends past any
          // scrollable ancestor (which would otherwise trigger a scrollbar
          // and bounce the chart). Position with viewport coords + flip-on-edge
          // so it also stays inside the visible window.
          const rect = containerRef.current.getBoundingClientRect();
          const dotX = rect.left + hovered.cx;
          const dotY = rect.top + hovered.cy;
          const TT_W = 280;
          const TT_H = 100;
          const MARGIN = 8;
          const vw = document.documentElement.clientWidth;
          const vh = document.documentElement.clientHeight;
          let left = dotX + 12;
          let top = dotY - 10;
          if (left + TT_W > vw - MARGIN) left = dotX - TT_W - 12;
          if (left < MARGIN) left = MARGIN;
          if (top + TT_H > vh - MARGIN) top = dotY - TT_H - 10;
          if (top < MARGIN) top = MARGIN;
          return createPortal(
            <div
              className="fixed pointer-events-none z-50"
              style={{ left, top }}
            >
              <div className="bg-surface-elevated border border-border rounded-lg px-3 py-2 shadow-lg text-sm whitespace-nowrap">
                <div className="font-mono text-xs text-content-muted mb-1">{hovered.point.filename}</div>
                <div className="text-content-muted text-xs mb-1">{hovered.point.dateLabel}</div>
                <div className="flex items-center gap-1.5 mb-1">
                  <LegendShape shape={hovered.series.shape} color={hovered.series.color} size={10} />
                  <span className="text-xs text-content-muted">{hovered.series.label}</span>
                </div>
                <div className="text-content font-medium">
                  {def?.label ?? metricKey}: {formatValue(hovered.point.y)}
                  {def?.unit ? ` ${def.unit}` : ''}
                </div>
              </div>
            </div>,
            document.body
          );
        })()}

        {/* Zoom drag overlay (vertical band) */}
        {isDragging && !isBrushMode && selWidth > MIN_DRAG_PX && (
          <div
            className="absolute top-0 bottom-0 bg-accent/20 border-x border-accent/40 pointer-events-none"
            style={{ left: selLeft, width: selWidth }}
          />
        )}

        {/* Shift-brush overlay (2D box) */}
        {isDragging && isBrushMode && (selWidth > MIN_DRAG_PX || selHeight > MIN_DRAG_PX) && (
          <div
            className="absolute bg-success/15 border border-success/60 border-dashed pointer-events-none"
            style={{ left: selLeft, top: selTop, width: selWidth, height: selHeight }}
          />
        )}
      </div>
    </div>
  );
}
