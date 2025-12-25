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
          className="flex items-center gap-2 text-gray-400 hover:text-gray-200 mb-4 transition-colors"
        >
          <ArrowLeft size={20} />
          Back to Equipment
        </button>

        <div className="flex items-center justify-between">
          <div>
            <h2 className="text-3xl font-bold mb-2">Calibration Library</h2>
            <div className="flex items-center gap-4">
              <span className="text-gray-400">{instrume}</span>
              {currentStats && currentStats.totalSets > 0 && (
                <>
                  <div className="flex items-center gap-1.5 text-sm">
                    <Package className="text-blue-400" size={14} />
                    <span className="text-gray-400">Sets:</span>
                    <span className="text-gray-100 font-medium">{currentStats.totalSets}</span>
                  </div>
                  {currentStats.dateFrom && currentStats.dateTo && (
                    <div className="flex items-center gap-1.5 text-sm">
                      <Calendar className="text-orange-400" size={14} />
                      <span className="text-gray-400">Coverage:</span>
                      <span className="text-gray-100 font-medium">
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
            className="flex items-center gap-2 px-4 py-2 bg-blue-600 hover:bg-blue-700 text-white rounded-md transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <RefreshCw size={16} className={refreshing ? "animate-spin" : ""} />
            {refreshing ? "Refreshing..." : "Refresh"}
          </button>
        </div>
      </div>

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

      {/* Tabs */}
      <div className="border-b border-gray-700 mb-6">
        <div className="flex gap-4">
          <button
            onClick={() => setActiveTab("darks")}
            className={`px-4 py-2 border-b-2 transition-colors ${
              activeTab === "darks"
                ? "border-blue-500 text-blue-400"
                : "border-transparent text-gray-400 hover:text-gray-200"
            }`}
          >
            Darks
            <span className="text-xs ml-2 text-gray-500">(Dark/Bias/DarkFlat)</span>
          </button>
          <button
            onClick={() => setActiveTab("flats")}
            className={`px-4 py-2 border-b-2 transition-colors ${
              activeTab === "flats"
                ? "border-blue-500 text-blue-400"
                : "border-transparent text-gray-400 hover:text-gray-200"
            }`}
          >
            Flats
            <span className="text-xs ml-2 text-gray-500">(Flat Calibration)</span>
          </button>
          <button
            onClick={() => setActiveTab("master-darks")}
            className={`px-4 py-2 border-b-2 transition-colors ${
              activeTab === "master-darks"
                ? "border-purple-500 text-purple-400"
                : "border-transparent text-gray-400 hover:text-gray-200"
            }`}
          >
            Master Darks
            <span className="text-xs ml-2 text-gray-500">(MasterDark/Bias/DarkFlat)</span>
          </button>
          <button
            onClick={() => setActiveTab("master-flats")}
            className={`px-4 py-2 border-b-2 transition-colors ${
              activeTab === "master-flats"
                ? "border-purple-500 text-purple-400"
                : "border-transparent text-gray-400 hover:text-gray-200"
            }`}
          >
            Master Flats
            <span className="text-xs ml-2 text-gray-500">(MasterFlat)</span>
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
