import { useState } from "react";
import { ChevronUp, ChevronDown, ChevronsUpDown, Eye, Settings, Star, Pencil } from "lucide-react";
import { CalibrationSetDetail, FileWithFrame, ImageType, isMasterType } from "../types/models";
import { format } from "date-fns";
import { invoke } from "@tauri-apps/api/core";
import BlinkViewer from "./BlinkViewer";

interface CalibrationSetTableProps {
  sets: CalibrationSetDetail[];
  showFilterColumn?: boolean;
  /** Callback to edit sub-calibration for a set (for flat/dark sets only) */
  onEditSubCalibration?: (setId: number, setType: 'flat' | 'dark') => void;
  /** Set IDs that have custom metadata edits */
  customMetadataSetIds?: number[];
}

type SortField = "id" | "imagetyp" | "filter" | "exptime" | "ccd_temp" | "gain" | "offset" | "binning" | "date_start" | "frame_count";
type SortDirection = "asc" | "desc";

export default function CalibrationSetTable({ sets, showFilterColumn = false, onEditSubCalibration, customMetadataSetIds = [] }: CalibrationSetTableProps) {
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
      return <ChevronsUpDown size={14} className="text-content-muted" />;
    }
    return sortDirection === "asc" ? (
      <ChevronUp size={14} className="text-accent" />
    ) : (
      <ChevronDown size={14} className="text-accent" />
    );
  };

  if (sets.length === 0) {
    return (
      <div className="text-center py-12 text-content-muted bg-surface-elevated rounded-lg">
        No calibration sets to display
      </div>
    );
  }

  return (
    <div className="overflow-x-auto bg-surface-elevated rounded-lg border border-border">
      <table className="w-full">
        <thead className="bg-surface sticky top-0">
          <tr>
            <th className="px-4 py-3 text-left text-xs font-medium text-content-muted uppercase tracking-wider">
              <button
                onClick={() => handleSort("id")}
                className="flex items-center gap-1 hover:text-content transition-colors"
              >
                ID
                <SortIcon field="id" />
              </button>
            </th>
            <th className="px-4 py-3 text-left text-xs font-medium text-content-muted uppercase tracking-wider">
              <button
                onClick={() => handleSort("imagetyp")}
                className="flex items-center gap-1 hover:text-content transition-colors"
              >
                Type
                <SortIcon field="imagetyp" />
              </button>
            </th>
            {showFilterColumn && (
              <th className="px-4 py-3 text-left text-xs font-medium text-content-muted uppercase tracking-wider">
                <button
                  onClick={() => handleSort("filter")}
                  className="flex items-center gap-1 hover:text-content transition-colors"
                >
                  Filter
                  <SortIcon field="filter" />
                </button>
              </th>
            )}
            <th className="px-4 py-3 text-left text-xs font-medium text-content-muted uppercase tracking-wider">
              <button
                onClick={() => handleSort("exptime")}
                className="flex items-center gap-1 hover:text-content transition-colors"
              >
                Exposure
                <SortIcon field="exptime" />
              </button>
            </th>
            <th className="px-4 py-3 text-left text-xs font-medium text-content-muted uppercase tracking-wider">
              <button
                onClick={() => handleSort("ccd_temp")}
                className="flex items-center gap-1 hover:text-content transition-colors"
              >
                Temp (°C)
                <SortIcon field="ccd_temp" />
              </button>
            </th>
            <th className="px-4 py-3 text-left text-xs font-medium text-content-muted uppercase tracking-wider">
              <button
                onClick={() => handleSort("gain")}
                className="flex items-center gap-1 hover:text-content transition-colors"
              >
                Gain
                <SortIcon field="gain" />
              </button>
            </th>
            <th className="px-4 py-3 text-left text-xs font-medium text-content-muted uppercase tracking-wider hidden md:table-cell">
              <button
                onClick={() => handleSort("offset")}
                className="flex items-center gap-1 hover:text-content transition-colors"
              >
                Offset
                <SortIcon field="offset" />
              </button>
            </th>
            <th className="px-4 py-3 text-left text-xs font-medium text-content-muted uppercase tracking-wider hidden lg:table-cell">
              <button
                onClick={() => handleSort("binning")}
                className="flex items-center gap-1 hover:text-content transition-colors"
              >
                Binning
                <SortIcon field="binning" />
              </button>
            </th>
            <th className="px-4 py-3 text-left text-xs font-medium text-content-muted uppercase tracking-wider">
              <button
                onClick={() => handleSort("date_start")}
                className="flex items-center gap-1 hover:text-content transition-colors"
              >
                Date Range
                <SortIcon field="date_start" />
              </button>
            </th>
            <th className="px-4 py-3 text-left text-xs font-medium text-content-muted uppercase tracking-wider">
              <button
                onClick={() => handleSort("frame_count")}
                className="flex items-center gap-1 hover:text-content transition-colors"
              >
                Frames
                <SortIcon field="frame_count" />
              </button>
            </th>
            <th className="px-4 py-3 text-center text-xs font-medium text-content-muted uppercase tracking-wider">
              Actions
            </th>
          </tr>
        </thead>
        <tbody className="divide-y divide-border">
          {sortedSets.map((set, index) => {
            const isExpanded = expandedRows.has(set.id || index);
            const rowId = set.id || index;

            return (
              <>
                <tr
                  key={rowId}
                  onClick={() => toggleRowExpansion(rowId)}
                  className={`${
                    index % 2 === 0 ? "bg-surface-elevated" : "bg-surface"
                  } hover:bg-surface-hover cursor-pointer transition-colors`}
                >
                  {/* ID */}
                  <td className="px-4 py-3 text-sm text-content-muted">
                    {set.id ?? "—"}
                  </td>

                  {/* Type */}
                  <td className="px-4 py-3">
                    <div className="flex items-center gap-1">
                      {isMasterType(set.imagetyp) && (
                        <Star size={12} className="text-warning fill-warning" />
                      )}
                      {customMetadataSetIds.includes(set.id!) && (
                        <span title="Custom metadata">
                          <Pencil size={12} className="text-info" />
                        </span>
                      )}
                      <span
                        className={`px-2 py-1 rounded text-xs font-medium ${
                          set.imagetyp === "Dark" || set.imagetyp === "MasterDark"
                            ? "bg-purple/10 text-purple border border-purple/50"
                            : set.imagetyp === "Flat" || set.imagetyp === "MasterFlat"
                            ? "bg-warning-muted text-warning border border-warning/50"
                            : set.imagetyp === "Bias" || set.imagetyp === "MasterBias"
                            ? "bg-info-muted text-accent border border-accent/50"
                            : set.imagetyp === "DarkFlat" || set.imagetyp === "MasterDarkFlat"
                            ? "bg-purple/5 text-purple/80 border border-purple/30"
                            : "bg-surface/30 text-content-muted border border-border"
                        }`}
                      >
                        {set.imagetyp}
                      </span>
                    </div>
                  </td>

                  {/* Filter (for flats) */}
                  {showFilterColumn && (
                    <td className="px-4 py-3 text-sm text-content">
                      {set.filter || "—"}
                    </td>
                  )}

                  {/* Exposure */}
                  <td className="px-4 py-3 text-sm text-content">
                    {set.exptime !== null ? `${set.exptime}s` : "N/A"}
                  </td>

                  {/* Temperature */}
                  <td className="px-4 py-3 text-sm">
                    <div className="flex flex-col">
                      <span className="text-content font-medium">
                        {formatTemp(set.ccd_temp)}
                      </span>
                      <span className="text-content-muted text-xs">
                        ({formatTemp(set.temp_min)} - {formatTemp(set.temp_max)})
                      </span>
                    </div>
                  </td>

                  {/* Gain */}
                  <td className="px-4 py-3 text-sm text-content">
                    {set.gain !== null ? set.gain : "N/A"}
                  </td>

                  {/* Offset - Hidden on mobile */}
                  <td className="px-4 py-3 text-sm text-content hidden md:table-cell">
                    {set.offset !== null ? set.offset : "N/A"}
                  </td>

                  {/* Binning - Hidden on tablet */}
                  <td className="px-4 py-3 text-sm text-content hidden lg:table-cell">
                    {set.binning || "N/A"}
                  </td>

                  {/* Date Range */}
                  <td className="px-4 py-3 text-sm">
                    <div className="flex flex-col">
                      <span className="text-content font-medium">
                        {set.date_display}
                      </span>
                      <span className="text-content-muted text-xs">
                        {formatDate(set.date_start)} - {formatDate(set.date_end)}
                      </span>
                    </div>
                  </td>

                  {/* Frame Count */}
                  <td className="px-4 py-3 text-sm text-content font-medium">
                    {set.frame_count}
                  </td>

                  {/* Actions */}
                  <td className="px-4 py-3 text-center">
                    <div className="flex items-center justify-center gap-2">
                      <button
                        onClick={(e) => handleViewFrames(set.id!, e)}
                        disabled={loadingFrames === set.id}
                        className="inline-flex items-center gap-1 px-3 py-1 bg-accent hover:bg-accent-hover text-white text-xs rounded transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                        title="View frames in blink viewer"
                      >
                        <Eye size={14} />
                        {loadingFrames === set.id ? 'Loading...' : 'View'}
                      </button>
                      {/* Don't show Sub-Cal button for Bias or Master types (masters don't need sub-calibration) */}
                      {onEditSubCalibration && set.id !== null && set.imagetyp !== ImageType.Bias && !isMasterType(set.imagetyp) && (
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            const setType = set.imagetyp === ImageType.Flat ? 'flat' : 'dark';
                            onEditSubCalibration(set.id!, setType);
                          }}
                          className="inline-flex items-center gap-1 px-2 py-1 bg-surface-hover hover:brightness-110 text-content text-xs rounded transition-colors"
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
                    className="bg-surface border-t border-border"
                  >
                    <td colSpan={showFilterColumn ? 11 : 10} className="px-4 py-3">
                      <div className="flex flex-wrap gap-x-6 gap-y-1 text-sm">
                        <div>
                          <span className="text-content-muted">Resolution:</span>
                          <span className="ml-2 text-content">
                            {set.naxis1 && set.naxis2 ? `${set.naxis1} × ${set.naxis2}` : "—"}
                          </span>
                        </div>
                        <div>
                          <span className="text-content-muted">Bayer:</span>
                          <span className="ml-2 text-content">
                            {set.bayerpat || "Mono"}
                          </span>
                        </div>
                        <div>
                          <span className="text-content-muted">Pixel Size:</span>
                          <span className="ml-2 text-content">
                            {set.xpixsz ? `${set.xpixsz}µm` : "—"}
                          </span>
                        </div>
                        <div>
                          <span className="text-content-muted">Format:</span>
                          <span className="ml-2 text-content">
                            {set.format || "—"}
                          </span>
                        </div>
                        <div>
                          <span className="text-content-muted">Software:</span>
                          <span className="ml-2 text-content">
                            {set.swcreate || "—"}
                          </span>
                        </div>
                        <div className="md:hidden">
                          <span className="text-content-muted">Offset:</span>
                          <span className="ml-2 text-content">
                            {set.offset !== null ? set.offset : "N/A"}
                          </span>
                        </div>
                        <div className="lg:hidden">
                          <span className="text-content-muted">Binning:</span>
                          <span className="ml-2 text-content">
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
