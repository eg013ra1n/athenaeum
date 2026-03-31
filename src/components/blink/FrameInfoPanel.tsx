import React, { memo, useState } from "react";
import { AlertTriangle } from "lucide-react";
import type { FrameInfoPanelProps } from "./types";

function formatDate(dateStr: string | null | undefined): string {
  if (!dateStr) return "-";
  try {
    const date = new Date(dateStr);
    return date.toLocaleDateString() + " " + date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
  } catch { return dateStr; }
}

function formatTemp(temp: number | null | undefined): string {
  if (temp === null || temp === undefined) return "-";
  return `${temp.toFixed(1)}°C`;
}

const InfoRow: React.FC<{ label: string; value: string | React.ReactNode }> = ({ label, value }) => (
  <div className="flex justify-between text-xs py-1 border-b border-border/30 last:border-b-0">
    <span className="text-content-muted">{label}</span>
    <span className="text-content-secondary font-medium">{value}</span>
  </div>
);

type Tab = 'metrics' | 'header';

export const FrameInfoPanel: React.FC<FrameInfoPanelProps> = memo(function FrameInfoPanel({
  currentFrame,
  metrics,
}) {
  const [activeTab, setActiveTab] = useState<Tab>('metrics');

  const plateScale = (currentFrame?.frame?.focallen && currentFrame?.frame?.xpixsz)
    ? (currentFrame.frame.xpixsz / currentFrame.frame.focallen) * 206.265
    : null;

  return (
    <div className="border-t border-border flex flex-col flex-shrink-0" style={{ maxHeight: '45%' }}>
      {/* Tab bar */}
      <div className="flex border-b border-border/50 bg-surface">
        <button
          onClick={() => setActiveTab('metrics')}
          className={`flex-1 px-3 py-1.5 text-xs font-medium tracking-wide transition-colors ${
            activeTab === 'metrics'
              ? 'text-accent border-b-2 border-accent -mb-px'
              : 'text-content-muted hover:text-content-secondary'
          }`}
        >
          Metrics
        </button>
        <button
          onClick={() => setActiveTab('header')}
          className={`flex-1 px-3 py-1.5 text-xs font-medium tracking-wide transition-colors ${
            activeTab === 'header'
              ? 'text-accent border-b-2 border-accent -mb-px'
              : 'text-content-muted hover:text-content-secondary'
          }`}
        >
          Header
        </button>
      </div>

      {/* Tab content */}
      <div className="px-3 py-1.5 overflow-y-auto">
        {activeTab === 'metrics' ? (
          metrics ? (
            <div className="space-y-0">
              <InfoRow label="Stars" value={metrics.stars_detected.toString()} />
              <InfoRow label="FWHM (px)" value={metrics.median_fwhm.toFixed(2)} />
              {plateScale && (
                <InfoRow label={'FWHM (")'} value={(metrics.median_fwhm * plateScale).toFixed(2)} />
              )}
              <InfoRow label="Eccentricity" value={metrics.median_eccentricity.toFixed(3)} />
              <InfoRow label="HFR (px)" value={metrics.median_hfr.toFixed(2)} />
              {plateScale && (
                <InfoRow label={'HFR (")'} value={(metrics.median_hfr * plateScale).toFixed(2)} />
              )}
              <InfoRow label="Frame SNR (dB)" value={metrics.frame_snr.toFixed(1)} />
              <InfoRow label="SNR Weight" value={metrics.snr_weight.toFixed(1)} />
              <InfoRow label="PSF Signal (ADU)" value={metrics.psf_signal.toFixed(1)} />
              <InfoRow
                label="Trail R²"
                value={
                  <span className="flex items-center gap-1">
                    {metrics.trail_r_squared.toFixed(3)}
                    {metrics.possibly_trailed && (
                      <span title={metrics.trail_r_squared >= 0.3
                        ? "Directional trail detected (RA drift)"
                        : "Guiding issue (wind/vibration)"
                      }>
                        <AlertTriangle size={12} className="text-warning" />
                      </span>
                    )}
                  </span>
                }
              />
              {metrics.median_beta != null && (
                <InfoRow label="Moffat β" value={metrics.median_beta.toFixed(2)} />
              )}
            </div>
          ) : (
            <p className="text-xs text-content-muted py-2">No analysis data</p>
          )
        ) : (
          <div className="space-y-0">
            <InfoRow label="File" value={currentFrame?.file.filename || "-"} />
            <InfoRow label="Date" value={formatDate(currentFrame?.frame?.date_obs)} />
            {currentFrame?.frame?.telescop && (
              <InfoRow label="Telescope" value={currentFrame.frame.telescop} />
            )}
            {currentFrame?.frame?.instrume && (
              <InfoRow label="Camera" value={currentFrame.frame.instrume} />
            )}
            <InfoRow label="Gain" value={currentFrame?.frame?.gain?.toString() ?? "-"} />
            <InfoRow label="Offset" value={currentFrame?.frame?.offset?.toString() ?? "-"} />
            <InfoRow label="Temp" value={formatTemp(currentFrame?.frame?.ccd_temp)} />
            {currentFrame?.frame?.filter && (
              <InfoRow label="Filter" value={currentFrame.frame.filter} />
            )}
            {currentFrame?.frame?.exptime != null && (
              <InfoRow label="Exposure" value={`${currentFrame.frame.exptime}s`} />
            )}
          </div>
        )}
      </div>
    </div>
  );
});
