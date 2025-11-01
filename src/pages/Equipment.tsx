import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { CameraStats } from "../types/models";
import CameraCard from "../components/CameraCard";
import DarkLibrary from "../components/DarkLibrary";
import MasterDarkLibrary from "../components/MasterDarkLibrary";

type LibraryView = "equipment" | "dark" | "master";

export default function Equipment() {
  const [cameras, setCameras] = useState<CameraStats[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selectedCamera, setSelectedCamera] = useState<string | null>(null);
  const [currentView, setCurrentView] = useState<LibraryView>("equipment");

  useEffect(() => {
    loadCameras();
  }, []);

  const loadCameras = async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await invoke<CameraStats[]>("get_equipment_cameras");
      setCameras(result);
    } catch (err) {
      setError(err as string);
    } finally {
      setLoading(false);
    }
  };

  const handleOpenDarkLibrary = (instrume: string) => {
    setSelectedCamera(instrume);
    setCurrentView("dark");
  };

  const handleOpenMasterDarkLibrary = (instrume: string) => {
    setSelectedCamera(instrume);
    setCurrentView("master");
  };

  const handleCloseLibrary = () => {
    setSelectedCamera(null);
    setCurrentView("equipment");
  };

  // Render dark library view
  if (currentView === "dark" && selectedCamera) {
    return (
      <DarkLibrary
        instrume={selectedCamera}
        onClose={handleCloseLibrary}
      />
    );
  }

  // Render master dark library view
  if (currentView === "master" && selectedCamera) {
    return (
      <MasterDarkLibrary
        instrume={selectedCamera}
        onClose={handleCloseLibrary}
      />
    );
  }

  return (
    <div className="p-6">
      <div className="mb-6">
        <h2 className="text-3xl font-bold mb-2">Equipment Library</h2>
        <p className="text-gray-400">
          Browse instruments with captured frames and dark library management
        </p>
      </div>

      {loading && (
        <div className="text-center py-12 text-gray-400">
          Loading equipment...
        </div>
      )}

      {error && (
        <div className="bg-red-900/20 border border-red-700 rounded-lg p-4 mb-4">
          <p className="text-red-400">{error}</p>
        </div>
      )}

      {!loading && !error && cameras.length === 0 && (
        <div className="bg-gray-800 rounded-lg p-8 text-center">
          <p className="text-gray-400">
            No cameras found. Scan some directories to populate your equipment library.
          </p>
        </div>
      )}

      {!loading && cameras.length > 0 && (
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4 gap-4">
          {cameras.map((camera) => (
            <CameraCard
              key={camera.instrume}
              camera={camera}
              onOpenDarkLibrary={handleOpenDarkLibrary}
              onOpenMasterDarkLibrary={handleOpenMasterDarkLibrary}
            />
          ))}
        </div>
      )}
    </div>
  );
}
