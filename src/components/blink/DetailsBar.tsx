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
    <div className="bg-gray-900 border-t border-gray-700 px-4 py-3">
      <div className="flex items-center justify-between text-sm">
        {/* Left side: File info */}
        <div className="flex items-center gap-6">
          {/* Filename */}
          <div>
            <span className="text-gray-500">File: </span>
            <span className="text-white font-medium">
              {currentFrame?.file.filename || "-"}
            </span>
          </div>

          {/* Date */}
          <div>
            <span className="text-gray-500">Date: </span>
            <span className="text-gray-300">
              {formatDate(currentFrame?.frame?.date_obs)}
            </span>
          </div>

          {/* Telescope */}
          {currentFrame?.frame?.telescop && (
            <div>
              <span className="text-gray-500">Telescope: </span>
              <span className="text-gray-300">{currentFrame.frame.telescop}</span>
            </div>
          )}

          {/* Camera */}
          {currentFrame?.frame?.instrume && (
            <div>
              <span className="text-gray-500">Camera: </span>
              <span className="text-gray-300">{currentFrame.frame.instrume}</span>
            </div>
          )}
        </div>

        {/* Right side: Camera settings */}
        <div className="flex items-center gap-6">
          {/* Gain/Offset */}
          {(currentFrame?.frame?.gain !== null || currentFrame?.frame?.offset !== null) && (
            <div>
              <span className="text-gray-500">Gain: </span>
              <span className="text-gray-300">{currentFrame?.frame?.gain ?? "-"}</span>
              <span className="text-gray-500 ml-2">Offset: </span>
              <span className="text-gray-300">{currentFrame?.frame?.offset ?? "-"}</span>
            </div>
          )}

          {/* Temperature */}
          <div>
            <span className="text-gray-500">Temp: </span>
            <span className="text-gray-300">
              {formatTemp(currentFrame?.frame?.ccd_temp)}
            </span>
          </div>

          {/* Filter & Exposure */}
          <div>
            {currentFrame?.frame?.filter && (
              <>
                <span className="text-gray-500">Filter: </span>
                <span className="text-gray-300">{currentFrame.frame.filter}</span>
              </>
            )}
            {currentFrame?.frame?.exptime && (
              <>
                <span className="text-gray-500 ml-3">Exp: </span>
                <span className="text-gray-300">{currentFrame.frame.exptime}s</span>
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
});
