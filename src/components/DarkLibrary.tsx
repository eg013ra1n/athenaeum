import { useState, useEffect, useMemo } from "react";
import { api } from '../api';
import { ArrowLeft } from "lucide-react";
import { CalibrationSetDetail, ImageType } from "../types/models";
import CalibrationSetTable from "./CalibrationSetTable";
import DarkLibraryFilters, { FilterState, emptyFilters, FilterMode } from "./DarkLibraryFilters";
import { CalibrationPicker } from "./calibration/CalibrationPicker";

export interface LibraryStats {
  totalSets: number;
  dateFrom: string | null;
  dateTo: string | null;
}

interface DarkLibraryProps {
  instrume: string;
  onClose?: () => void;
  isTabView?: boolean;
  imageTypeFilter?: ImageType[];
  onStatsChange?: (stats: LibraryStats) => void;
  /** Calibration set ID to highlight + auto-expand + scroll to in the table. */
  highlightSetId?: number | null;
  /** Show a "Create Master" action on raw (non-master, non-superseded) sets. */
  onCreateMaster?: (setId: number) => void;
  /** Set IDs with an in-flight master build (renders a spinner label). */
  buildingSetIds?: number[];
}

export default function DarkLibrary({ instrume, onClose, isTabView = false, imageTypeFilter, onStatsChange, highlightSetId, onCreateMaster, buildingSetIds }: DarkLibraryProps) {
  const [sets, setSets] = useState<CalibrationSetDetail[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [filters, setFilters] = useState<FilterState>(emptyFilters);

  // Sub-calibration modal state
  const [subCalModalSetId, setSubCalModalSetId] = useState<number | null>(null);
  const [subCalModalType, setSubCalModalType] = useState<'flat' | 'dark'>('flat');

  const handleEditSubCalibration = (setId: number, setType: 'flat' | 'dark') => {
    setSubCalModalSetId(setId);
    setSubCalModalType(setType);
  };

  const handleSubCalApply = () => {
    // Refresh the library to show updated sub-calibration
    checkAndLoadLibrary();
  };

  useEffect(() => {
    checkAndLoadLibrary();
  }, [instrume]);

  // Listen for library updates (when created from parent)
  useEffect(() => {
    const handleLibraryUpdate = () => {
      checkAndLoadLibrary();
    };

    window.addEventListener("library-updated", handleLibraryUpdate);
    return () => {
      window.removeEventListener("library-updated", handleLibraryUpdate);
    };
  }, [instrume]);

  const checkAndLoadLibrary = async () => {
    try {
      setLoading(true);
      setError(null);
      const hasLibrary = await api.invoke<boolean>("has_dark_library", { instrume });

      if (hasLibrary) {
        const result = await api.invoke<CalibrationSetDetail[]>("get_dark_library", { instrume });
        setSets(result);
      } else {
        setSets([]);
      }
    } catch (err) {
      setError(err as string);
    } finally {
      setLoading(false);
    }
  };

  const handleFilterChange = (newFilters: FilterState) => {
    setFilters(newFilters);
  };

  // Determine filter mode based on imageTypeFilter
  const filterMode: FilterMode = imageTypeFilter?.length === 1 && imageTypeFilter[0] === "Flat" ? "flats" : "darks";

  // Pre-filter sets by imageTypeFilter (for filter dropdown options)
  const tabFilteredSets = useMemo(() => {
    if (!imageTypeFilter || imageTypeFilter.length === 0) {
      return sets;
    }
    return sets.filter(set => imageTypeFilter.includes(set.imagetyp));
  }, [sets, imageTypeFilter]);

  // Apply user filters to sets
  const filteredSets = useMemo(() => {
    return tabFilteredSets.filter(set => {
      // Type filter (from user selection) - darks mode only
      if (filters.type !== null && set.imagetyp !== filters.type) {
        return false;
      }

      // Optical filter (for flats)
      if (filters.filter !== null && set.filter !== filters.filter) {
        return false;
      }

      // Exposure filter (exact match - darks mode)
      if (filters.exposure !== null && set.exptime !== filters.exposure) {
        return false;
      }

      // Exposure range filter (flats mode)
      if (filters.expFrom !== null && set.exptime !== null && set.exptime < filters.expFrom) {
        return false;
      }
      if (filters.expTo !== null && set.exptime !== null && set.exptime > filters.expTo) {
        return false;
      }

      // Temperature band filter
      if (filters.tempBand !== null) {
        const temp = set.ccd_temp;
        const match = filters.tempBand.match(/(-?\d+) to (-?\d+)/);
        if (match) {
          const min = parseInt(match[1]);
          const max = parseInt(match[2]);
          if (temp < min || temp >= max) {
            return false;
          }
        }
      }

      // Date range filter
      if (filters.dateFrom !== null) {
        const setDate = set.date_start.split('T')[0];
        if (setDate < filters.dateFrom) {
          return false;
        }
      }

      if (filters.dateTo !== null) {
        const setDate = set.date_end.split('T')[0];
        if (setDate > filters.dateTo) {
          return false;
        }
      }

      return true;
    });
  }, [tabFilteredSets, filters]);

  // Report stats to parent when filtered sets change
  useEffect(() => {
    if (onStatsChange && filteredSets.length > 0) {
      const allDates = filteredSets.flatMap(set => [set.date_start, set.date_end]).filter(d => d);
      const sortedDates = allDates.sort();
      onStatsChange({
        totalSets: filteredSets.length,
        dateFrom: sortedDates.length > 0 ? sortedDates[0] : null,
        dateTo: sortedDates.length > 0 ? sortedDates[sortedDates.length - 1] : null,
      });
    } else if (onStatsChange) {
      onStatsChange({ totalSets: 0, dateFrom: null, dateTo: null });
    }
  }, [filteredSets, onStatsChange]);

  return (
    <div className={isTabView ? "" : "p-6"}>
      {/* Header - only show if not in tab view */}
      {!isTabView && (
        <div className="mb-6">
          <button
            onClick={onClose}
            className="flex items-center gap-2 text-content-muted hover:text-content mb-4 transition-colors"
          >
            <ArrowLeft size={20} />
            Back to Equipment
          </button>

          <div>
            <h2 className="text-3xl font-bold mb-2">Dark Library</h2>
            <p className="text-content-muted">{instrume}</p>
          </div>
        </div>
      )}

      {/* Error Message */}
      {error && (
        <div className="bg-error-muted border border-error/50 rounded-lg p-4 mb-4">
          <p className="text-error">{error}</p>
        </div>
      )}

      {/* Loading */}
      {loading && (
        <div className="text-center py-12 text-content-muted">
          Loading dark library...
        </div>
      )}

      {/* Empty state */}
      {!loading && sets.length === 0 && (
        <div className="bg-surface-elevated rounded-lg p-12 text-center">
          <p className="text-content-muted">
            No calibration sets found. Use the Refresh button above to scan for calibration frames.
          </p>
        </div>
      )}

      {/* Table view with filters */}
      {!loading && sets.length > 0 && (
        <div>
          {/* Filters */}
          <DarkLibraryFilters sets={tabFilteredSets} filters={filters} onFilterChange={handleFilterChange} mode={filterMode} />

          {/* Result count */}
          <div className="mb-4 text-sm text-content-muted">
            Showing {filteredSets.length} of {tabFilteredSets.length} calibration sets
          </div>

          {/* Table */}
          {filteredSets.length > 0 ? (
            <CalibrationSetTable
              sets={filteredSets}
              showFilterColumn={imageTypeFilter?.length === 1 && imageTypeFilter[0] === "Flat"}
              onEditSubCalibration={handleEditSubCalibration}
              highlightSetId={highlightSetId}
              onCreateMaster={onCreateMaster}
              buildingSetIds={buildingSetIds}
            />
          ) : (
            <div className="text-center py-12 bg-surface-elevated rounded-lg">
              <p className="text-content-muted mb-4">
                No calibration sets match your filters
              </p>
              <button
                onClick={() => setFilters(emptyFilters)}
                className="px-4 py-2 bg-surface-hover hover:bg-surface-hover text-white rounded-md transition-colors"
              >
                Clear Filters
              </button>
            </div>
          )}
        </div>
      )}

      {/* Sub-Calibration Modal */}
      {subCalModalSetId !== null && (
        <CalibrationPicker
          subject={{ kind: "set", setId: subCalModalSetId, sourceType: subCalModalType }}
          onApplied={handleSubCalApply}
          onClose={() => setSubCalModalSetId(null)}
        />
      )}
    </div>
  );
}
