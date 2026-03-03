import { useEffect, useMemo, useState, useCallback } from 'react';
import { X, ZoomOut } from 'lucide-react';
import type { FrameAnalysis } from '../../types/models';
import type { EnrichedLightFrame } from '../calibration/LightsAnalysisTable';
import { transformToChartData } from './chartDataTransform';
import { MetricChart, LegendShape } from './MetricChart';

interface AnalysisChartsModalProps {
  isOpen: boolean;
  onClose: () => void;
  displayedFrames: EnrichedLightFrame[];
  analysisData: Map<number, FrameAnalysis>;
  frameSetName?: string;
}

export function AnalysisChartsModal({
  isOpen,
  onClose,
  displayedFrames,
  analysisData,
  frameSetName,
}: AnalysisChartsModalProps) {
  const [xDomain, setXDomain] = useState<[number, number] | null>(null);

  // Close on Escape
  useEffect(() => {
    if (!isOpen) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [isOpen, onClose]);

  // Reset zoom when data changes or modal reopens
  useEffect(() => {
    setXDomain(null);
  }, [displayedFrames, analysisData, isOpen]);

  const chartData = useMemo(
    () => transformToChartData(displayedFrames, analysisData),
    [displayedFrames, analysisData]
  );

  const handleZoomSelect = useCallback((left: number, right: number) => {
    setXDomain([left, right]);
  }, []);

  const handleResetZoom = useCallback(() => {
    setXDomain(null);
  }, []);

  if (!isOpen) return null;

  const hasData = chartData.dataPoints.length > 0;
  const isZoomed = xDomain !== null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/70 backdrop-blur-sm" onClick={onClose} />

      {/* Modal */}
      <div className="relative bg-surface-elevated rounded-xl shadow-2xl w-[95vw] max-w-7xl max-h-[90vh] mx-4 border border-border flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-border flex-shrink-0">
          <h2 className="text-lg font-semibold text-content">
            Analysis Charts
            {frameSetName && (
              <span className="text-content-muted font-normal"> — {frameSetName}</span>
            )}
          </h2>
          <button
            onClick={onClose}
            className="p-1.5 rounded-lg hover:bg-surface-hover text-content-muted hover:text-content transition-colors"
          >
            <X size={18} />
          </button>
        </div>

        {hasData ? (
          <>
            {/* Shared legend + zoom controls */}
            <div className="flex items-center gap-x-4 gap-y-1 px-6 py-3 border-b border-border flex-shrink-0">
              <div className="flex flex-wrap items-center gap-x-4 gap-y-1 flex-1">
                {chartData.seriesList.map((series) => (
                  <div key={series.key} className="flex items-center gap-1.5">
                    <LegendShape shape={series.shape} color={series.color} size={10} />
                    <span className="text-xs text-content-muted">{series.label}</span>
                  </div>
                ))}
              </div>
              {isZoomed ? (
                <button
                  onClick={handleResetZoom}
                  className="inline-flex items-center gap-1.5 px-3 py-1.5 bg-surface-hover hover:bg-border text-content-secondary text-xs rounded-lg transition-colors flex-shrink-0"
                >
                  <ZoomOut size={14} />
                  Reset Zoom
                </button>
              ) : (
                <span className="text-xs text-content-muted flex-shrink-0">
                  Drag to zoom
                </span>
              )}
            </div>

            {/* Scrollable chart body */}
            <div className="flex-1 overflow-y-auto px-6 py-4 space-y-6">
              <MetricChart
                title="FWHM"
                metricKey="fwhm"
                dataPoints={chartData.dataPoints}
                nightBoundaries={chartData.nightBoundaries}
                seriesList={chartData.seriesList}
                segments={chartData.segments}
                yAxisLabel="px"
                subtitle="lower is better"
                xDomain={xDomain ?? undefined}
                onZoomSelect={handleZoomSelect}
              />
              <MetricChart
                title="Eccentricity"
                metricKey="eccentricity"
                dataPoints={chartData.dataPoints}
                nightBoundaries={chartData.nightBoundaries}
                seriesList={chartData.seriesList}
                segments={chartData.segments}
                subtitle="lower is better"
                xDomain={xDomain ?? undefined}
                onZoomSelect={handleZoomSelect}
              />
              <MetricChart
                title="SNR"
                metricKey="snr"
                dataPoints={chartData.dataPoints}
                nightBoundaries={chartData.nightBoundaries}
                seriesList={chartData.seriesList}
                segments={chartData.segments}
                subtitle="higher is better"
                xDomain={xDomain ?? undefined}
                onZoomSelect={handleZoomSelect}
              />
              <MetricChart
                title="PSF Signal"
                metricKey="psfSignal"
                dataPoints={chartData.dataPoints}
                nightBoundaries={chartData.nightBoundaries}
                seriesList={chartData.seriesList}
                segments={chartData.segments}
                subtitle="higher is better"
                xDomain={xDomain ?? undefined}
                onZoomSelect={handleZoomSelect}
              />
            </div>
          </>
        ) : (
          <div className="flex-1 flex items-center justify-center py-16">
            <p className="text-content-muted text-sm">
              No analysis data available. Run "Analyze All" first.
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
