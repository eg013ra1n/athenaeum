import { useState, useEffect } from "react";
import { useSearchParams } from "react-router-dom";
import { api } from '../api';
import { CameraStats } from "../types/models";
import CameraCard from "../components/CameraCard";
import CameraDetail from "../components/CameraDetail";

const CAMERA_DETAIL_TABS = ["files", "darks", "flats", "master-darks", "master-flats"] as const;
type CameraDetailTab = typeof CAMERA_DETAIL_TABS[number];

export default function Equipment() {
  const [cameras, setCameras] = useState<CameraStats[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selectedCamera, setSelectedCamera] = useState<string | null>(null);
  const [searchParams] = useSearchParams();

  // Cross-page jump from Calibration Coverage: ?camera=…&tab=…&highlightSet=…
  // Auto-select the matching camera once cameras have loaded; CameraDetail
  // consumes initialTab + highlightSetId once and clears the URL params.
  const requestedCamera = searchParams.get("camera");
  const requestedTabParam = searchParams.get("tab");
  const requestedTab: CameraDetailTab | undefined =
    requestedTabParam && (CAMERA_DETAIL_TABS as readonly string[]).includes(requestedTabParam)
      ? (requestedTabParam as CameraDetailTab)
      : undefined;
  const requestedHighlightSetParam = searchParams.get("highlightSet");
  const requestedHighlightSet =
    requestedHighlightSetParam != null && /^\d+$/.test(requestedHighlightSetParam)
      ? parseInt(requestedHighlightSetParam, 10)
      : null;

  useEffect(() => {
    loadCameras();
  }, []);

  useEffect(() => {
    if (!requestedCamera || cameras.length === 0) return;
    if (cameras.some(c => c.instrume === requestedCamera)) {
      setSelectedCamera(requestedCamera);
    }
  }, [cameras, requestedCamera]);

  const loadCameras = async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await api.invoke<CameraStats[]>("get_equipment_cameras");
      setCameras(result);
    } catch (err) {
      setError(err as string);
    } finally {
      setLoading(false);
    }
  };

  const handleOpenCamera = (instrume: string) => {
    setSelectedCamera(instrume);
  };

  const handleCloseCamera = () => {
    setSelectedCamera(null);
  };

  // Render camera detail view with tabs. Keying by selectedCamera means a
  // camera switch remounts (so init state re-applies), but a same-camera
  // re-render (e.g. CameraDetail clearing URL params after consuming them)
  // does NOT remount — otherwise initialTab would reset to "files".
  if (selectedCamera) {
    return (
      <CameraDetail
        key={selectedCamera}
        instrume={selectedCamera}
        onClose={handleCloseCamera}
        initialTab={selectedCamera === requestedCamera ? requestedTab : undefined}
        highlightSetId={selectedCamera === requestedCamera ? requestedHighlightSet : null}
      />
    );
  }

  return (
    <div className="p-4 pt-3">
      <div className="mb-4">
        <h2 className="text-2xl font-bold">
          Equipment Library
          <span className="text-sm font-normal text-content-muted ml-3">Browse instruments with captured frames and calibration library management</span>
        </h2>
      </div>

      {loading && (
        <div className="text-center py-12 text-content-muted">
          Loading equipment...
        </div>
      )}

      {error && (
        <div className="bg-error-muted border border-error/50 rounded-lg p-4 mb-4">
          <p className="text-error">{error}</p>
        </div>
      )}

      {!loading && !error && cameras.length === 0 && (
        <div className="bg-surface-elevated rounded-lg p-8 text-center">
          <p className="text-content-muted">
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
              onOpenCamera={handleOpenCamera}
            />
          ))}
        </div>
      )}
    </div>
  );
}
