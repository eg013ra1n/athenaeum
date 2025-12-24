import { useState } from "react";
import { ChevronUp, ChevronDown, ChevronsUpDown, Eye, Settings } from "lucide-react";
import { CalibrationSetDetail, FileWithFrame, ImageType } from "../types/models";
import { format } from "date-fns";
import { invoke } from "@tauri-apps/api/core";
import BlinkViewer from "./BlinkViewer";

interface CalibrationSetTableProps {
  sets: CalibrationSetDetail[];
  showFilterColumn?: boolean;
  /** Callback to edit sub-calibration for a set (for flat/dark sets only) */
  onEditSubCalibration?: (setId: number, setType: 'flat' | 'dark') => void;
}

type SortField = "imagetyp" | "filter" | "exptime" | "ccd_temp" | "gain" | "offset" | "binning" | "date_start" | "frame_count";
type SortDirection = "asc" | "desc";

export default function CalibrationSetTable({ sets, showFilterColumn = false, onEditSubCalibration }: CalibrationSetTableProps) {
  const [sortField, setSortField] = useState<SortField>("exptime");
  const [sortDirection, setSortDirection] = useState<SortDirection>("asc");
  const [expandedRows, setExpandedRows] = useState<Set<number>>(new Set());
  const [blinkFrames, setBlinkFrames] = useState<FileWithFrame[] | null>(null);
  const [loadingFrames, setLoadingFrames] = useState<number | null>(null);

  const handleSort = (field: SortField) => {
    if (sortField === field) {
      // Toggle direction
      setSortDirection(sortDirection === "asc" ? "desc" : "asc");
    } else {
      // New field, default to ascending
      setSortField(field);
      setSortDirection("asc");
    }
  };

  const toggleRowExpansion = (id: number) => {
    const newExpanded = new Set(expandedRows);
    if (newExpanded.has(id)) {
      newExpanded.delete(id);
    } else {
      newExpanded.add(id);
    }
    setExpandedRows(newExpanded);
  };

  const handleViewFrames = async (setId: number, event: React.MouseEvent) => {
    event.stopPropagation(); // Prevent row expansion
    try {
      setLoadingFrames(setId);
      const frames = await invoke<FileWithFrame[]>('get_calibration_set_frames', { setId });
      setBlinkFrames(frames);
    } catch (err) {
      console.error('Failed to load calibration frames:', err);
      alert(`Failed to load frames: ${err}`);
    } finally {
      setLoadingFrames(null);
    }
  };

  // Sort sets
  const sortedSets = [...sets].sort((a, b) => {
    let aVal: any = a[sortField];
    let bVal: any = b[sortField];

    // Handle null values
    if (aVal === null || aVal === undefined) return 1;
    if (bVal === null || bVal === undefined) return -1;

    // String comparison
    if (typeof aVal === "string") {
      return sortDirection === "asc"
        ? aVal.localeCompare(bVal)
        : bVal.localeCompare(aVal);
    }

    // Numeric comparison
    return sortDirection === "asc" ? aVal - bVal : bVal - aVal;
  });

  const formatDate = (dateStr: string) => {
    try {
      return format(new Date(dateStr), "MMM d, yyyy");
    } catch {
      return dateStr;
    }
  };

  const formatTemp = (temp: number) => {
    return `${temp.toFixed(1)}°C`;
  };

  const SortIcon = ({ field }: { field: SortField }) => {
    if (sortField !== field) {
      return <ChevronsUpDown size={14} className="text-gray-600" />;
    }
    return sortDirection === "asc" ? (
      <ChevronUp size={14} className="text-blue-400" />
    ) : (
      <ChevronDown size={14} className="text-blue-400" />
    );
  };

  if (sets.length === 0) {
    return (
      <div className="text-center py-12 text-gray-400 bg-gray-800 rounded-lg">
        No calibration sets to display
      </div>
    );
  }

  return (
    <div className="overflow-x-auto bg-gray-800 rounded-lg border border-gray-700">
      <table className="w-full">
        <thead className="bg-gray-900 sticky top-0">
          <tr>
            <th className="px-4 py-3 text-left text-xs font-medium text-gray-400 uppercase tracking-wider">
              <button
                onClick={() => handleSort("imagetyp")}
                className="flex items-center gap-1 hover:text-gray-200 transition-colors"
              >
                Type
                <SortIcon field="imagetyp" />
              </button>
            </th>
            {showFilterColumn && (
              <th className="px-4 py-3 text-left text-xs font-medium text-gray-400 uppercase tracking-wider">
                <button
                  onClick={() => handleSort("filter")}
                  className="flex items-center gap-1 hover:text-gray-200 transition-colors"
                >
                  Filter
                  <SortIcon field="filter" />
                </button>
              </th>
            )}
            <th className="px-4 py-3 text-left text-xs font-medium text-gray-400 uppercase tracking-wider">
              <button
                onClick={() => handleSort("exptime")}
                className="flex items-center gap-1 hover:text-gray-200 transition-colors"
              >
                Exposure
                <SortIcon field="exptime" />
              </button>
            </th>
            <th className="px-4 py-3 text-left text-xs font-medium text-gray-400 uppercase tracking-wider">
              <button
                onClick={() => handleSort("ccd_temp")}
                className="flex items-center gap-1 hover:text-gray-200 transition-colors"
              >
                Temp (°C)
                <SortIcon field="ccd_temp" />
              </button>
            </th>
            <th className="px-4 py-3 text-left text-xs font-medium text-gray-400 uppercase tracking-wider">
              <button
                onClick={() => handleSort("gain")}
                className="flex items-center gap-1 hover:text-gray-200 transition-colors"
              >
                Gain
                <SortIcon field="gain" />
              </button>
            </th>
            <th className="px-4 py-3 text-left text-xs font-medium text-gray-400 uppercase tracking-wider hidden md:table-cell">
              <button
                onClick={() => handleSort("offset")}
                className="flex items-center gap-1 hover:text-gray-200 transition-colors"
              >
                Offset
                <SortIcon field="offset" />
              </button>
            </th>
            <th className="px-4 py-3 text-left text-xs font-medium text-gray-400 uppercase tracking-wider hidden lg:table-cell">
              <button
                onClick={() => handleSort("binning")}
                className="flex items-center gap-1 hover:text-gray-200 transition-colors"
              >
                Binning
                <SortIcon field="binning" />
              </button>
            </th>
            <th className="px-4 py-3 text-left text-xs font-medium text-gray-400 uppercase tracking-wider">
              <button
                onClick={() => handleSort("date_start")}
                className="flex items-center gap-1 hover:text-gray-200 transition-colors"
              >
                Date Range
                <SortIcon field="date_start" />
              </button>
            </th>
            <th className="px-4 py-3 text-left text-xs font-medium text-gray-400 uppercase tracking-wider">
              <button
                onClick={() => handleSort("frame_count")}
                className="flex items-center gap-1 hover:text-gray-200 transition-colors"
              >
                Frames
                <SortIcon field="frame_count" />
              </button>
            </th>
            <th className="px-4 py-3 text-center text-xs font-medium text-gray-400 uppercase tracking-wider">
              Actions
            </th>
          </tr>
        </thead>
        <tbody className="divide-y divide-gray-700">
          {sortedSets.map((set, index) => {
            const isExpanded = expandedRows.has(set.id || index);
            const rowId = set.id || index;

            return (
              <>
                <tr
                  key={rowId}
                  onClick={() => toggleRowExpansion(rowId)}
                  className={`${
                    index % 2 === 0 ? "bg-gray-800" : "bg-gray-850"
                  } hover:bg-gray-750 cursor-pointer transition-colors`}
                >
                  {/* Type */}
                  <td className="px-4 py-3">
                    <span
                      className={`px-2 py-1 rounded text-xs font-medium ${
                        set.imagetyp === "Dark"
                          ? "bg-purple-900/30 text-purple-400 border border-purple-700"
                          : set.imagetyp === "Flat"
                          ? "bg-yellow-900/30 text-yellow-400 border border-yellow-700"
                          : "bg-blue-900/30 text-blue-400 border border-blue-700"
                      }`}
                    >
                      {set.imagetyp}
                    </span>
                  </td>

                  {/* Filter (for flats) */}
                  {showFilterColumn && (
                    <td className="px-4 py-3 text-sm text-gray-100">
                      {set.filter || "—"}
                    </td>
                  )}

                  {/* Exposure */}
                  <td className="px-4 py-3 text-sm text-gray-100">
                    {set.exptime !== null ? `${set.exptime}s` : "N/A"}
                  </td>

                  {/* Temperature */}
                  <td className="px-4 py-3 text-sm">
                    <div className="flex flex-col">
                      <span className="text-gray-100 font-medium">
                        {formatTemp(set.ccd_temp)}
                      </span>
                      <span className="text-gray-500 text-xs">
                        ({formatTemp(set.temp_min)} - {formatTemp(set.temp_max)})
                      </span>
                    </div>
                  </td>

                  {/* Gain */}
                  <td className="px-4 py-3 text-sm text-gray-100">
                    {set.gain !== null ? set.gain : "N/A"}
                  </td>

                  {/* Offset - Hidden on mobile */}
                  <td className="px-4 py-3 text-sm text-gray-100 hidden md:table-cell">
                    {set.offset !== null ? set.offset : "N/A"}
                  </td>

                  {/* Binning - Hidden on tablet */}
                  <td className="px-4 py-3 text-sm text-gray-100 hidden lg:table-cell">
                    {set.binning || "N/A"}
                  </td>

                  {/* Date Range */}
                  <td className="px-4 py-3 text-sm">
                    <div className="flex flex-col">
                      <span className="text-gray-100 font-medium">
                        {set.date_display}
                      </span>
                      <span className="text-gray-500 text-xs">
                        {formatDate(set.date_start)} - {formatDate(set.date_end)}
                      </span>
                    </div>
                  </td>

                  {/* Frame Count */}
                  <td className="px-4 py-3 text-sm text-gray-100 font-medium">
                    {set.frame_count}
                  </td>

                  {/* Actions */}
                  <td className="px-4 py-3 text-center">
                    <div className="flex items-center justify-center gap-2">
                      <button
                        onClick={(e) => handleViewFrames(set.id!, e)}
                        disabled={loadingFrames === set.id}
                        className="inline-flex items-center gap-1 px-3 py-1 bg-blue-600 hover:bg-blue-700 text-white text-xs rounded transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                        title="View frames in blink viewer"
                      >
                        <Eye size={14} />
                        {loadingFrames === set.id ? 'Loading...' : 'View'}
                      </button>
                      {onEditSubCalibration && set.id !== null && set.imagetyp !== ImageType.Bias && (
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            const setType = set.imagetyp === ImageType.Flat ? 'flat' : 'dark';
                            onEditSubCalibration(set.id!, setType);
                          }}
                          className="inline-flex items-center gap-1 px-2 py-1 bg-gray-700 hover:bg-gray-600 text-gray-200 text-xs rounded transition-colors"
                          title="Edit sub-calibration"
                        >
                          <Settings size={14} />
                          Sub-Cal
                        </button>
                      )}
                    </div>
                  </td>
                </tr>

                {/* Expanded Row */}
                {isExpanded && (
                  <tr
                    key={`${rowId}-expanded`}
                    className="bg-gray-900 border-t border-gray-700"
                  >
                    <td colSpan={showFilterColumn ? 10 : 9} className="px-4 py-4">
                      <div className="grid grid-cols-2 md:grid-cols-4 gap-4 text-sm">
                        <div>
                          <span className="text-gray-400">Set ID:</span>
                          <span className="ml-2 text-gray-100">{set.id}</span>
                        </div>
                        {showFilterColumn && set.filter && (
                          <div>
                            <span className="text-gray-400">Filter:</span>
                            <span className="ml-2 text-gray-100">{set.filter}</span>
                          </div>
                        )}
                        <div>
                          <span className="text-gray-400">Avg Temp:</span>
                          <span className="ml-2 text-gray-100">
                            {formatTemp(set.ccd_temp)}
                          </span>
                        </div>
                        <div>
                          <span className="text-gray-400">Min Temp:</span>
                          <span className="ml-2 text-gray-100">
                            {formatTemp(set.temp_min)}
                          </span>
                        </div>
                        <div>
                          <span className="text-gray-400">Max Temp:</span>
                          <span className="ml-2 text-gray-100">
                            {formatTemp(set.temp_max)}
                          </span>
                        </div>
                        <div>
                          <span className="text-gray-400">Start Date:</span>
                          <span className="ml-2 text-gray-100">
                            {formatDate(set.date_start)}
                          </span>
                        </div>
                        <div>
                          <span className="text-gray-400">End Date:</span>
                          <span className="ml-2 text-gray-100">
                            {formatDate(set.date_end)}
                          </span>
                        </div>
                        <div className="md:hidden">
                          <span className="text-gray-400">Offset:</span>
                          <span className="ml-2 text-gray-100">
                            {set.offset !== null ? set.offset : "N/A"}
                          </span>
                        </div>
                        <div className="lg:hidden">
                          <span className="text-gray-400">Binning:</span>
                          <span className="ml-2 text-gray-100">
                            {set.binning || "N/A"}
                          </span>
                        </div>
                      </div>
                    </td>
                  </tr>
                )}
              </>
            );
          })}
        </tbody>
      </table>

      {/* BlinkViewer Modal */}
      {blinkFrames && (
        <BlinkViewer
          frames={blinkFrames}
          onClose={() => setBlinkFrames(null)}
          sourceType="calibration"
        />
      )}
    </div>
  );
}
