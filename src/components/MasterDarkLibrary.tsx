import { useState, useEffect, useMemo } from "react";
import { api } from '../api';
import { ArrowLeft, Pencil } from "lucide-react";
import { CalibrationSetDetail } from "../types/models";
import CalibrationSetTable from "./CalibrationSetTable";
import DarkLibraryFilters, { FilterState, emptyFilters, FilterMode } from "./DarkLibraryFilters";
import BulkEditMetadataModal from "./BulkEditMetadataModal";

interface MasterDarkLibraryProps {
  instrume: string;
  onClose?: () => void;
  isTabView?: boolean;
  /** Calibration set ID to highlight + auto-expand + scroll to in the table. */
  highlightSetId?: number | null;
  /** Show a "Create Master" action on raw (non-master, non-superseded) sets. */
  onCreateMaster?: (setId: number) => void;
  /** Set IDs with an in-flight master build (renders a spinner label). */
  buildingSetIds?: number[];
}

export default function MasterDarkLibrary({ instrume, onClose, isTabView = false, highlightSetId, onCreateMaster, buildingSetIds }: MasterDarkLibraryProps) {
  const [sets, setSets] = useState<CalibrationSetDetail[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [filters, setFilters] = useState<FilterState>(emptyFilters);
  const [showBulkEdit, setShowBulkEdit] = useState(false);
  const [customMetadataSetIds, setCustomMetadataSetIds] = useState<number[]>([]);

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
      const hasLibrary = await api.invoke<boolean>("has_master_dark_library", { instrume });

      if (hasLibrary) {
        const result = await api.invoke<CalibrationSetDetail[]>("get_master_dark_library", { instrume });
        setSets(result);

        // Load custom metadata set IDs
        const customIds = await api.invoke<number[]>("get_custom_metadata_set_ids", { instrume });
        setCustomMetadataSetIds(customIds);
      } else {
        setSets([]);
        setCustomMetadataSetIds([]);
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

  // Master library is always darks mode
  const filterMode: FilterMode = "darks";

  // Apply filters to sets
  const filteredSets = useMemo(() => {
    return sets.filter(set => {
      // Type filter
      if (filters.type !== null && set.imagetyp !== filters.type) {
        return false;
      }

      // Exposure filter
      if (filters.exposure !== null && set.exptime !== filters.exposure) {
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
  }, [sets, filters]);

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

          <div className="flex items-center justify-between">
            <div>
              <h2 className="text-3xl font-bold mb-2">Master Dark Library</h2>
              <p className="text-content-muted">{instrume}</p>
            </div>

            {sets.length > 0 && (
              <div className="flex gap-2">
                <button
                  onClick={() => setShowBulkEdit(true)}
                  className="flex items-center gap-2 px-4 py-2 bg-surface-hover hover:brightness-110 text-white rounded-md transition-colors"
                >
                  <Pencil size={16} />
                  Edit Metadata
                </button>
              </div>
            )}
          </div>
        </div>
      )}

      {/* Messages */}
      {error && (
        <div className="bg-error-muted border border-error/50 rounded-lg p-4 mb-4">
          <p className="text-error">{error}</p>
        </div>
      )}

      {/* Loading */}
      {loading && (
        <div className="text-center py-12 text-content-muted">
          Loading master dark library...
        </div>
      )}

      {/* Empty state */}
      {!loading && sets.length === 0 && (
        <div className="bg-surface-elevated rounded-lg p-12 text-center">
          <p className="text-content-muted">
            No master calibration sets for this camera yet. Master files (MasterDark, MasterBias, MasterDarkFlat)
            found in your monitored folders are registered automatically when scanned, and masters built in-app
            appear here as well.
          </p>
        </div>
      )}

      {/* Table view with filters */}
      {!loading && sets.length > 0 && (
        <div>
          {/* Filters */}
          <DarkLibraryFilters
            sets={sets}
            filters={filters}
            onFilterChange={handleFilterChange}
            mode={filterMode}
            actions={isTabView ? (
              <button
                onClick={() => setShowBulkEdit(true)}
                className="flex items-center gap-2 px-3 py-1.5 bg-surface-hover hover:brightness-110 text-white rounded text-sm transition-colors"
              >
                <Pencil size={14} />
                Edit Metadata
              </button>
            ) : undefined}
          />

          {/* Result count */}
          <div className="mb-4 text-sm text-content-muted">
            Showing {filteredSets.length} of {sets.length} master calibration sets
          </div>

          {/* Table */}
          {filteredSets.length > 0 ? (
            <CalibrationSetTable
              sets={filteredSets}
              customMetadataSetIds={customMetadataSetIds}
              highlightSetId={highlightSetId}
              onCreateMaster={onCreateMaster}
              buildingSetIds={buildingSetIds}
            />
          ) : (
            <div className="text-center py-12 bg-surface-elevated rounded-lg">
              <p className="text-content-muted mb-4">
                No master calibration sets match your filters
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

      {/* Bulk Edit Modal */}
      <BulkEditMetadataModal
        isOpen={showBulkEdit}
        sets={sets}
        customMetadataSetIds={customMetadataSetIds}
        onClose={() => setShowBulkEdit(false)}
        onSave={async () => { await checkAndLoadLibrary(); }}
      />
    </div>
  );
}
