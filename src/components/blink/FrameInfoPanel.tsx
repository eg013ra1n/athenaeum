import React, { memo } from "react";
import { AlertTriangle } from "lucide-react";
import type { FrameInfoPanelProps } from "./types";

/** Format date for display */
function formatDate(dateStr: string | null | undefined): string {
  if (!dateStr) return "-";
  try {
    const date = new Date(dateStr);
    return date.toLocaleDateString() + " " + date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  } catch {
    return dateStr;
  }
}

/** Format temperature for display */
function formatTemp(temp: number | null | undefined): string {
  if (temp === null || temp === undefined) return "-";
  return `${temp.toFixed(1)}°C`;
}

/** Info row with label and value */
const InfoRow: React.FC<{ label: string; value: string | React.ReactNode }> = ({ label, value }) => (
  <div className="flex justify-between text-xs py-0.5">
    <span className="text-content-muted">{label}</span>
    <span className="text-content-secondary font-medium">{value}</span>
  </div>
);

/** Right sidebar panel showing FITS header info and analysis metrics */
export const FrameInfoPanel: React.FC<FrameInfoPanelProps> = memo(function FrameInfoPanel({
  currentFrame,
  metrics,
}) {
  return (
    <div className="border-t border-border px-3 py-2 overflow-y-auto flex-shrink-0" style={{ maxHeight: '45%' }}>
      {/* FITS Header section */}
      <h4 className="text-xs font-semibold text-content-muted uppercase tracking-wider mb-1">Frame Info</h4>
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
        {currentFrame?.frame?.exptime && (
          <InfoRow label="Exposure" value={`${currentFrame.frame.exptime}s`} />
        )}
      </div>

      {/* Analysis Metrics section */}
      {metrics && (
        <>
          <h4 className="text-xs font-semibold text-content-muted uppercase tracking-wider mt-3 mb-1">Analysis</h4>
          <div className="space-y-0">
            <InfoRow label="Stars" value={metrics.stars_detected.toString()} />
            <InfoRow label="FWHM" value={`${metrics.median_fwhm.toFixed(2)} px`} />
            <InfoRow label="Eccentricity" value={metrics.median_eccentricity.toFixed(3)} />
            <InfoRow label="SNR" value={metrics.median_snr.toFixed(1)} />
            <InfoRow label="HFR" value={`${metrics.median_hfr.toFixed(2)} px`} />
            <InfoRow label="Frame SNR" value={metrics.frame_snr.toFixed(1)} />
            <InfoRow label="SNR Weight" value={metrics.snr_weight.toFixed(1)} />
            <InfoRow label="PSF Signal" value={metrics.psf_signal.toFixed(1)} />
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
              <InfoRow label="Moffat Beta" value={metrics.median_beta.toFixed(2)} />
            )}
          </div>
        </>
      )}
    </div>
  );
});
