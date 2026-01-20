import { useState, useMemo } from "react";
import { ArrowLeft, RefreshCw, Package, Calendar } from "lucide-react";
import DarkLibrary, { LibraryStats } from "./DarkLibrary";
import MasterDarkLibrary from "./MasterDarkLibrary";
import MasterFlatLibrary from "./MasterFlatLibrary";
import { invoke } from "@tauri-apps/api/core";
import { ImageType } from "../types/models";
import { format } from "date-fns";

interface CalibrationScanResult {
  sets_created: number;
  flat_sets_created: number;
  dark_sets_created: number;
  bias_sets_created: number;
  darkflat_sets_created: number;
  // Master calibration sets (1 file = 1 set)
  master_dark_sets_created: number;
  master_flat_sets_created: number;
  master_bias_sets_created: number;
  master_darkflat_sets_created: number;
}

interface CameraDetailProps {
  instrume: string;
  onClose: () => void;
}

type TabType = "darks" | "flats" | "master-darks" | "master-flats";

export default function CameraDetail({ instrume, onClose }: CameraDetailProps) {
  const [activeTab, setActiveTab] = useState<TabType>("darks");
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [successMessage, setSuccessMessage] = useState<string | null>(null);
  const [currentStats, setCurrentStats] = useState<LibraryStats | null>(null);

  // Memoize imageTypeFilter arrays to prevent infinite re-render loops
  const darksFilter = useMemo(() => [ImageType.Dark, ImageType.Bias, ImageType.DarkFlat], []);
  const flatsFilter = useMemo(() => [ImageType.Flat], []);

  const handleRefreshLibrary = async () => {
    try {
      setRefreshing(true);
      setError(null);
      setSuccessMessage(null);

      const result = await invoke<CalibrationScanResult>("refresh_calibration_library_for_camera", { instrume });

      // Build success message
      const masterTotal = result.master_dark_sets_created + result.master_flat_sets_created +
                          result.master_bias_sets_created + result.master_darkflat_sets_created;
      const regularTotal = result.flat_sets_created + result.dark_sets_created +
                           result.bias_sets_created + result.darkflat_sets_created;

      let message = `Refreshed library: ${result.sets_created} sets`;
      if (regularTotal > 0) {
        message += ` (${result.flat_sets_created} flat, ${result.dark_sets_created} dark, ${result.bias_sets_created} bias, ${result.darkflat_sets_created} darkflat)`;
      }
      if (masterTotal > 0) {
        message += ` + ${masterTotal} master sets`;
      }

      setSuccessMessage(message);

      // Trigger refresh in child components
      window.dispatchEvent(new CustomEvent("library-updated"));
    } catch (err) {
      setError(err as string);
    } finally {
      setRefreshing(false);
    }
  };

  return (
    <div className="p-6">
      {/* Header */}
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
            <h2 className="text-3xl font-bold mb-2">Calibration Library</h2>
            <div className="flex items-center gap-4">
              <span className="text-content-muted">{instrume}</span>
              {currentStats && currentStats.totalSets > 0 && (
                <>
                  <div className="flex items-center gap-1.5 text-sm">
                    <Package className="text-accent" size={14} />
                    <span className="text-content-muted">Sets:</span>
                    <span className="text-content font-medium">{currentStats.totalSets}</span>
                  </div>
                  {currentStats.dateFrom && currentStats.dateTo && (
                    <div className="flex items-center gap-1.5 text-sm">
                      <Calendar className="text-orange" size={14} />
                      <span className="text-content-muted">Coverage:</span>
                      <span className="text-content font-medium">
                        {format(new Date(currentStats.dateFrom), "MMM yyyy")} - {format(new Date(currentStats.dateTo), "MMM yyyy")}
                      </span>
                    </div>
                  )}
                </>
              )}
            </div>
          </div>

          <button
            onClick={handleRefreshLibrary}
            disabled={refreshing}
            className="flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover text-white rounded-md transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <RefreshCw size={16} className={refreshing ? "animate-spin" : ""} />
            {refreshing ? "Refreshing..." : "Refresh"}
          </button>
        </div>
      </div>

      {/* Messages */}
      {error && (
        <div className="bg-error-muted border border-error/50 rounded-lg p-4 mb-4">
          <p className="text-error">{error}</p>
        </div>
      )}

      {successMessage && (
        <div className="bg-success-muted border border-success/50 rounded-lg p-4 mb-4">
          <p className="text-success">{successMessage}</p>
        </div>
      )}

      {/* Tabs */}
      <div className="border-b border-border mb-6">
        <div className="flex gap-4">
          <button
            onClick={() => setActiveTab("darks")}
            className={`px-4 py-2 border-b-2 transition-colors ${
              activeTab === "darks"
                ? "border-accent text-accent"
                : "border-transparent text-content-muted hover:text-content"
            }`}
          >
            Darks
            <span className="text-xs ml-2 text-content-muted">(Dark/Bias/DarkFlat)</span>
          </button>
          <button
            onClick={() => setActiveTab("flats")}
            className={`px-4 py-2 border-b-2 transition-colors ${
              activeTab === "flats"
                ? "border-accent text-accent"
                : "border-transparent text-content-muted hover:text-content"
            }`}
          >
            Flats
            <span className="text-xs ml-2 text-content-muted">(Flat Calibration)</span>
          </button>
          <button
            onClick={() => setActiveTab("master-darks")}
            className={`px-4 py-2 border-b-2 transition-colors ${
              activeTab === "master-darks"
                ? "border-purple text-purple"
                : "border-transparent text-content-muted hover:text-content"
            }`}
          >
            Master Darks
            <span className="text-xs ml-2 text-content-muted">(MasterDark/Bias/DarkFlat)</span>
          </button>
          <button
            onClick={() => setActiveTab("master-flats")}
            className={`px-4 py-2 border-b-2 transition-colors ${
              activeTab === "master-flats"
                ? "border-purple text-purple"
                : "border-transparent text-content-muted hover:text-content"
            }`}
          >
            Master Flats
            <span className="text-xs ml-2 text-content-muted">(MasterFlat)</span>
          </button>
        </div>
      </div>

      {/* Tab Content */}
      <div>
        {activeTab === "darks" && (
          <DarkLibrary
            instrume={instrume}
            isTabView={true}
            imageTypeFilter={darksFilter}
            onStatsChange={setCurrentStats}
          />
        )}
        {activeTab === "flats" && (
          <DarkLibrary
            instrume={instrume}
            isTabView={true}
            imageTypeFilter={flatsFilter}
            onStatsChange={setCurrentStats}
          />
        )}
        {activeTab === "master-darks" && (
          <MasterDarkLibrary instrume={instrume} />
        )}
        {activeTab === "master-flats" && (
          <MasterFlatLibrary instrume={instrume} />
        )}
      </div>

    </div>
  );
}
