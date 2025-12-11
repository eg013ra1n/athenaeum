import { useState } from 'react';
import { ChevronDown, ChevronRight } from 'lucide-react';
import type { LightFrameWithCalibration } from '../../types/models';
import { StatusIndicator } from './StatusIndicator';

interface LightFrameListProps {
  frames: LightFrameWithCalibration[];
  /** Initially collapsed to reduce visual clutter */
  defaultExpanded?: boolean;
}

/**
 * Accessible light frame list with proper table semantics.
 * Features:
 * - Collapsible to reduce information overload
 * - Proper table structure with headers
 * - Status indicators with full labels
 * - Minimum 14px text for all content
 */
export function LightFrameList({ frames, defaultExpanded = false }: LightFrameListProps) {
  const [isExpanded, setIsExpanded] = useState(defaultExpanded);

  return (
    <div className="mt-6">
      {/* Collapsible header */}
      <button
        onClick={() => setIsExpanded(!isExpanded)}
        className="
          w-full flex items-center gap-3
          text-left py-2 px-1
          hover:bg-gray-800/50 rounded-lg
          focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500
          transition-colors
        "
        aria-expanded={isExpanded}
        aria-controls="light-frames-table"
      >
        {isExpanded ? (
          <ChevronDown size={20} className="text-gray-400" aria-hidden="true" />
        ) : (
          <ChevronRight size={20} className="text-gray-400" aria-hidden="true" />
        )}
        <h3 className="text-lg font-semibold text-gray-100">
          Light Frames
        </h3>
        <span className="text-sm text-gray-400">
          ({frames.length} frame{frames.length !== 1 ? 's' : ''})
        </span>
      </button>

      {/* Expandable table */}
      {isExpanded && (
        <div
          id="light-frames-table"
          className="mt-3 border border-gray-600 rounded-xl overflow-hidden"
        >
          <table className="w-full" role="table">
            <thead className="bg-gray-900">
              <tr>
                <th
                  scope="col"
                  className="px-4 py-3 text-left text-sm font-semibold text-gray-200"
                >
                  Filename
                </th>
                <th
                  scope="col"
                  className="px-4 py-3 text-right text-sm font-semibold text-gray-200 w-24"
                >
                  Exposure
                </th>
                <th
                  scope="col"
                  className="px-4 py-3 text-left text-sm font-semibold text-gray-200"
                >
                  Calibration Status
                </th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-700">
              {frames.map((frame, idx) => (
                <tr
                  key={frame.frame_id}
                  className={`
                    ${idx % 2 === 0 ? 'bg-gray-800' : 'bg-gray-850'}
                    hover:bg-gray-700 transition-colors
                  `}
                >
                  <td className="px-4 py-3">
                    <span
                      className="text-sm font-mono text-gray-100 truncate block max-w-[300px]"
                      title={frame.filename}
                    >
                      {frame.filename}
                    </span>
                  </td>
                  <td className="px-4 py-3 text-right">
                    <span className="text-sm text-gray-300">
                      {frame.exptime !== null ? `${frame.exptime}s` : '-'}
                    </span>
                  </td>
                  <td className="px-4 py-3">
                    <StatusIndicator status={frame.calibration_status} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>

          {/* Empty state */}
          {frames.length === 0 && (
            <div className="px-4 py-8 text-center text-gray-400">
              <p className="text-sm">No light frames in this group</p>
            </div>
          )}
        </div>
      )}

      {/* Summary when collapsed */}
      {!isExpanded && frames.length > 0 && (
        <div className="mt-2 px-4 py-3 bg-gray-800/50 rounded-lg">
          <FrameSummary frames={frames} />
        </div>
      )}
    </div>
  );
}

/**
 * Summary view of frame calibration status.
 */
function FrameSummary({ frames }: { frames: LightFrameWithCalibration[] }) {
  const stats = frames.reduce(
    (acc, frame) => {
      const status = frame.calibration_status;
      if (status.has_flats && status.has_darks) {
        acc.complete++;
      } else if (status.has_flats || status.has_darks || status.has_bias) {
        acc.partial++;
      } else {
        acc.none++;
      }
      if (status.flats_warning || status.darks_warning || status.bias_warning) {
        acc.warnings++;
      }
      return acc;
    },
    { complete: 0, partial: 0, none: 0, warnings: 0 }
  );

  return (
    <div className="flex items-center gap-6 text-sm">
      <div className="flex items-center gap-2">
        <span className="w-3 h-3 rounded-full bg-emerald-500" aria-hidden="true" />
        <span className="text-gray-300">
          <span className="font-medium text-gray-100">{stats.complete}</span> complete
        </span>
      </div>
      <div className="flex items-center gap-2">
        <span className="w-3 h-3 rounded-full bg-amber-500" aria-hidden="true" />
        <span className="text-gray-300">
          <span className="font-medium text-gray-100">{stats.partial}</span> partial
        </span>
      </div>
      <div className="flex items-center gap-2">
        <span className="w-3 h-3 rounded-full bg-red-500" aria-hidden="true" />
        <span className="text-gray-300">
          <span className="font-medium text-gray-100">{stats.none}</span> none
        </span>
      </div>
      {stats.warnings > 0 && (
        <div className="flex items-center gap-2">
          <span className="w-3 h-3 rounded-full bg-amber-400" aria-hidden="true" />
          <span className="text-amber-300">
            <span className="font-medium">{stats.warnings}</span> with warnings
          </span>
        </div>
      )}
    </div>
  );
}
