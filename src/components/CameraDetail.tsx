import { useState } from "react";
import { ArrowLeft, Zap } from "lucide-react";
import DarkLibrary from "./DarkLibrary";
import MasterDarkLibrary from "./MasterDarkLibrary";
import { invoke } from "@tauri-apps/api/core";
import { DarkLibraryResult } from "../types/models";

interface CameraDetailProps {
  instrume: string;
  onClose: () => void;
}

type TabType = "dark" | "master";

export default function CameraDetail({ instrume, onClose }: CameraDetailProps) {
  const [activeTab, setActiveTab] = useState<TabType>("dark");
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [successMessage, setSuccessMessage] = useState<string | null>(null);

  const handleCreateBothLibraries = async () => {
    if (!confirm("This will create both Dark Library (single frames) and Master Library (master frames) in one go. Continue?")) {
      return;
    }

    try {
      setCreating(true);
      setError(null);
      setSuccessMessage(null);

      // Create dark library (single frames)
      const darkResult = await invoke<DarkLibraryResult>("create_dark_library", { instrume });

      // Create master library (master frames)
      const masterResult = await invoke<DarkLibraryResult>("create_master_dark_library", { instrume });

      setSuccessMessage(
        `Created ${darkResult.sets_created} dark sets (${darkResult.frames_grouped} frames) and ${masterResult.sets_created} master sets (${masterResult.frames_grouped} frames)`
      );

      // Trigger refresh in child components by forcing re-render
      // The child components will reload when they detect the change
      window.dispatchEvent(new CustomEvent("library-updated"));
    } catch (err) {
      setError(err as string);
    } finally {
      setCreating(false);
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
            <p className="text-gray-400">{instrume}</p>
          </div>

          <button
            onClick={handleCreateBothLibraries}
            disabled={creating}
            className="flex items-center gap-2 px-4 py-2 bg-blue-600 hover:bg-blue-700 text-white rounded-md transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Zap size={16} className={creating ? "animate-pulse" : ""} />
            {creating ? "Creating..." : "Create Both Libraries"}
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
            onClick={() => setActiveTab("dark")}
            className={`px-4 py-2 border-b-2 transition-colors ${
              activeTab === "dark"
                ? "border-blue-500 text-blue-400"
                : "border-transparent text-gray-400 hover:text-gray-200"
            }`}
          >
            Dark Library
            <span className="text-xs ml-2 text-gray-500">(Single Frames)</span>
          </button>
          <button
            onClick={() => setActiveTab("master")}
            className={`px-4 py-2 border-b-2 transition-colors ${
              activeTab === "master"
                ? "border-blue-500 text-blue-400"
                : "border-transparent text-gray-400 hover:text-gray-200"
            }`}
          >
            Master Library
            <span className="text-xs ml-2 text-gray-500">(Master Frames)</span>
          </button>
        </div>
      </div>

      {/* Tab Content */}
      <div>
        {activeTab === "dark" && (
          <DarkLibrary instrume={instrume} isTabView={true} />
        )}
        {activeTab === "master" && (
          <MasterDarkLibrary instrume={instrume} isTabView={true} />
        )}
      </div>
    </div>
  );
}
