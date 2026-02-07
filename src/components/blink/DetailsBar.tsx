import React, { memo } from "react";
import type { DetailsBarProps } from "./types";

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

/** Bottom details bar showing current frame metadata */
export const DetailsBar: React.FC<DetailsBarProps> = memo(function DetailsBar({
  currentFrame,
}) {
  return (
    <div className="bg-surface border-t border-border px-4 py-3">
      <div className="flex items-center justify-between text-sm">
        {/* Left side: File info */}
        <div className="flex items-center gap-6">
          {/* Filename */}
          <div>
            <span className="text-content-muted">File: </span>
            <span className="text-content font-medium">
              {currentFrame?.file.filename || "-"}
            </span>
          </div>

          {/* Date */}
          <div>
            <span className="text-content-muted">Date: </span>
            <span className="text-content-secondary">
              {formatDate(currentFrame?.frame?.date_obs)}
            </span>
          </div>

          {/* Telescope */}
          {currentFrame?.frame?.telescop && (
            <div>
              <span className="text-content-muted">Telescope: </span>
              <span className="text-content-secondary">{currentFrame.frame.telescop}</span>
            </div>
          )}

          {/* Camera */}
          {currentFrame?.frame?.instrume && (
            <div>
              <span className="text-content-muted">Camera: </span>
              <span className="text-content-secondary">{currentFrame.frame.instrume}</span>
            </div>
          )}
        </div>

        {/* Right side: Camera settings */}
        <div className="flex items-center gap-6">
          {/* Gain/Offset */}
          {(currentFrame?.frame?.gain !== null || currentFrame?.frame?.offset !== null) && (
            <div>
              <span className="text-content-muted">Gain: </span>
              <span className="text-content-secondary">{currentFrame?.frame?.gain ?? "-"}</span>
              <span className="text-content-muted ml-2">Offset: </span>
              <span className="text-content-secondary">{currentFrame?.frame?.offset ?? "-"}</span>
            </div>
          )}

          {/* Temperature */}
          <div>
            <span className="text-content-muted">Temp: </span>
            <span className="text-content-secondary">
              {formatTemp(currentFrame?.frame?.ccd_temp)}
            </span>
          </div>

          {/* Filter & Exposure */}
          <div>
            {currentFrame?.frame?.filter && (
              <>
                <span className="text-content-muted">Filter: </span>
                <span className="text-content-secondary">{currentFrame.frame.filter}</span>
              </>
            )}
            {currentFrame?.frame?.exptime && (
              <>
                <span className="text-content-muted ml-3">Exp: </span>
                <span className="text-content-secondary">{currentFrame.frame.exptime}s</span>
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
});
