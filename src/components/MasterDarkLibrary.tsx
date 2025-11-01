import { useState, useEffect, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ArrowLeft, Plus, RefreshCw } from "lucide-react";
import { CalibrationSetDetail, DarkLibraryResult } from "../types/models";
import CalibrationSetTable from "./CalibrationSetTable";
import DarkLibraryFilters, { FilterState } from "./DarkLibraryFilters";
import QuickStats from "./QuickStats";

interface MasterDarkLibraryProps {
  instrume: string;
  onClose?: () => void;
  isTabView?: boolean;
}

export default function MasterDarkLibrary({ instrume, onClose, isTabView = false }: MasterDarkLibraryProps) {
  const [sets, setSets] = useState<CalibrationSetDetail[]>([]);
  const [loading, setLoading] = useState(true);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [successMessage, setSuccessMessage] = useState<string | null>(null);
  const [filters, setFilters] = useState<FilterState>({
    types: [],
    gains: [],
    offsets: [],
    binnings: [],
    tempBands: [],
    exposures: [],
  });

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
      const hasLibrary = await invoke<boolean>("has_master_dark_library", { instrume });

      if (hasLibrary) {
        const result = await invoke<CalibrationSetDetail[]>("get_master_dark_library", { instrume });
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

  const handleCreateLibrary = async () => {
    try {
      setCreating(true);
      setError(null);
      setSuccessMessage(null);

      const result = await invoke<DarkLibraryResult>("create_master_dark_library", { instrume });

      setSuccessMessage(
        `Created ${result.sets_created} master calibration sets with ${result.frames_grouped} frames`
      );

      // Reload the library
      await checkAndLoadLibrary();
    } catch (err) {
      setError(err as string);
    } finally {
      setCreating(false);
    }
  };

  const handleRegenerateLibrary = async () => {
    if (!confirm("This will delete the existing master dark library and recreate it. Continue?")) {
      return;
    }
    await handleCreateLibrary();
  };

  const handleFilterChange = (newFilters: FilterState) => {
    setFilters(newFilters);
  };

  // Apply filters to sets
  const filteredSets = useMemo(() => {
    return sets.filter(set => {
      // Type filter
      if (filters.types.length > 0 && !filters.types.includes(set.imagetyp)) {
        return false;
      }

      // Gain filter
      if (filters.gains.length > 0 && !filters.gains.includes(set.gain!)) {
        return false;
      }

      // Offset filter
      if (filters.offsets.length > 0 && !filters.offsets.includes(set.offset!)) {
        return false;
      }

      // Binning filter
      if (filters.binnings.length > 0 && !filters.binnings.includes(set.binning!)) {
        return false;
      }

      // Exposure filter
      if (filters.exposures.length > 0 && !filters.exposures.includes(set.exptime!)) {
        return false;
      }

      // Temperature band filter
      if (filters.tempBands.length > 0) {
        const temp = set.ccd_temp;
        let matchesBand = false;

        for (const band of filters.tempBands) {
          const match = band.match(/(-?\d+) to (-?\d+)/);
          if (match) {
            const min = parseInt(match[1]);
            const max = parseInt(match[2]);
            if (temp >= min && temp < max) {
              matchesBand = true;
              break;
            }
          }
        }

        if (!matchesBand) {
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
            className="flex items-center gap-2 text-gray-400 hover:text-gray-200 mb-4 transition-colors"
          >
            <ArrowLeft size={20} />
            Back to Equipment
          </button>

          <div className="flex items-center justify-between">
            <div>
              <h2 className="text-3xl font-bold mb-2">Master Dark Library</h2>
              <p className="text-gray-400">{instrume}</p>
            </div>

            {sets.length > 0 && (
              <button
                onClick={handleRegenerateLibrary}
                disabled={creating}
                className="flex items-center gap-2 px-4 py-2 bg-gray-700 hover:bg-gray-600 text-white rounded-md transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <RefreshCw size={16} className={creating ? "animate-spin" : ""} />
                Regenerate
              </button>
            )}
          </div>
        </div>
      )}

      {/* In tab view, show regenerate button at the top */}
      {isTabView && sets.length > 0 && (
        <div className="mb-4 flex justify-end">
          <button
            onClick={handleRegenerateLibrary}
            disabled={creating}
            className="flex items-center gap-2 px-4 py-2 bg-gray-700 hover:bg-gray-600 text-white rounded-md transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <RefreshCw size={16} className={creating ? "animate-spin" : ""} />
            Regenerate Master Library
          </button>
        </div>
      )}

      {/* Messages */}
      {error && (
        <div className="bg-red-900/20 border border-red-700 rounded-lg p-4 mb-4">
          <p className="text-red-400">{error}</p>
        </div>
      )}

      {successMessage && (
        <div className="bg-green-900/20 border border-green-700 rounded-lg p-4 mb-4">
          <p className="text-green-400">{successMessage}</p>
        </div>
      )}

      {/* Loading */}
      {loading && (
        <div className="text-center py-12 text-gray-400">
          Loading master dark library...
        </div>
      )}

      {/* Empty state */}
      {!loading && sets.length === 0 && (
        <div className="bg-gray-800 rounded-lg p-12 text-center">
          <p className="text-gray-400 mb-6">
            No master dark library created yet. This library organizes your master calibration frames
            (MasterDark, MasterBias, MasterDarkFlat) by date, temperature, gain, offset, and binning.
          </p>
          <button
            onClick={handleCreateLibrary}
            disabled={creating}
            className="inline-flex items-center gap-2 px-6 py-3 bg-blue-600 hover:bg-blue-700 text-white rounded-md transition-colors disabled:opacity-50 disabled:cursor-not-allowed text-lg font-medium"
          >
            <Plus size={20} />
            {creating ? "Creating..." : "Create Master Dark Library"}
          </button>
        </div>
      )}

      {/* Table view with filters */}
      {!loading && sets.length > 0 && (
        <div>
          {/* Filters */}
          <DarkLibraryFilters sets={sets} onFilterChange={handleFilterChange} />

          {/* Quick Stats */}
          <QuickStats sets={filteredSets} />

          {/* Result count */}
          <div className="mb-4 text-sm text-gray-400">
            Showing {filteredSets.length} of {sets.length} master calibration sets
          </div>

          {/* Table */}
          {filteredSets.length > 0 ? (
            <CalibrationSetTable sets={filteredSets} />
          ) : (
            <div className="text-center py-12 bg-gray-800 rounded-lg">
              <p className="text-gray-400 mb-4">
                No master calibration sets match your filters
              </p>
              <button
                onClick={() => setFilters({
                  types: [],
                  gains: [],
                  offsets: [],
                  binnings: [],
                  tempBands: [],
                  exposures: [],
                })}
                className="px-4 py-2 bg-gray-700 hover:bg-gray-600 text-white rounded-md transition-colors"
              >
                Clear Filters
              </button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
