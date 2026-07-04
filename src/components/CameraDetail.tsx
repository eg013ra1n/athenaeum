import { useState, useEffect, useMemo } from "react";
import { useSearchParams } from "react-router-dom";
import { ArrowLeft, RefreshCw, Package, Calendar, Filter } from "lucide-react";
import DarkLibrary, { LibraryStats } from "./DarkLibrary";
import MasterDarkLibrary from "./MasterDarkLibrary";
import MasterFlatLibrary from "./MasterFlatLibrary";
import DualPaneFileBrowser from "./dualpane/DualPaneFileBrowser";
import { api } from '../api';
import { ImageTypeValues } from "../types/helpers";
import { useScanRootsWithAvailability } from "../hooks/useTauri";
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

type TabType = "files" | "darks" | "flats" | "master-darks" | "master-flats";

interface CameraDetailProps {
  instrume: string;
  onClose: () => void;
  /** Initial tab to open. Defaults to "files". */
  initialTab?: TabType;
  /** Calibration set ID to scroll to + highlight + auto-expand on first render. */
  highlightSetId?: number | null;
}

export default function CameraDetail({ instrume, onClose, initialTab, highlightSetId }: CameraDetailProps) {
  const [activeTab, setActiveTab] = useState<TabType>(initialTab ?? "files");
  // Consume the highlight + initialTab once. After ~3.5s (slightly longer
  // than the child's flash fade) we clear pendingHighlightSetId so re-mounts
  // (e.g. switching tabs back to this one) don't re-flash the same row.
  const [pendingHighlightSetId, setPendingHighlightSetId] = useState<number | null>(highlightSetId ?? null);
  const [searchParams, setSearchParams] = useSearchParams();
  useEffect(() => {
    if (searchParams.has("camera") || searchParams.has("tab") || searchParams.has("highlightSet")) {
      const next = new URLSearchParams(searchParams);
      next.delete("camera");
      next.delete("tab");
      next.delete("highlightSet");
      setSearchParams(next, { replace: true });
    }
    if (pendingHighlightSetId == null) return;
    const t = setTimeout(() => setPendingHighlightSetId(null), 3500);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [successMessage, setSuccessMessage] = useState<string | null>(null);
  const [currentStats, setCurrentStats] = useState<LibraryStats | null>(null);
  const [cameraDirectories, setCameraDirectories] = useState<string[]>([]);
  /** Distinguishes "still loading the camera-directory list" from
   *  "loaded and the camera has no files" — different placeholders. */
  const [cameraDirsLoaded, setCameraDirsLoaded] = useState(false);

  const { scanRoots } = useScanRootsWithAvailability();

  // Load camera directories on mount
  useEffect(() => {
    setCameraDirsLoaded(false);
    const loadDirs = async () => {
      try {
        const dirs = await api.invoke<string[]>('get_camera_directories', { instrume });
        setCameraDirectories(dirs);
      } catch (err) {
        console.error('Failed to load camera directories:', err);
      } finally {
        setCameraDirsLoaded(true);
      }
    };
    loadDirs();
  }, [instrume]);

  // Memoize imageTypeFilter arrays to prevent infinite re-render loops
  const darksFilter = useMemo(() => [ImageTypeValues.Dark, ImageTypeValues.Bias, ImageTypeValues.DarkFlat], []);
  const flatsFilter = useMemo(() => [ImageTypeValues.Flat], []);

  const handleRefreshLibrary = async () => {
    try {
      setRefreshing(true);
      setError(null);
      setSuccessMessage(null);

      const result = await api.invoke<CalibrationScanResult>("refresh_calibration_library_for_camera", { instrume });

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
    // h-full flex column so the Files tab can flex-fill the remaining
    // viewport — same pattern FileManager uses to host the dual-pane. The
    // header / tab bar above stay at content height; the tab content area
    // below takes whatever's left.
    <div className="p-4 pt-3 h-full flex flex-col min-h-0">
      {/* Header — matches the FrameSetDetail (Object details) card style:
          surface-elevated, bordered, single-row layout with icon-only back
          button + vertical separator on the left, title and inline stat
          chips next to it, and action buttons on the right. */}
      <div className="bg-surface-elevated rounded-lg p-3 mb-2 border border-border flex-shrink-0">
        {/* min-h-9 pins the inner row at 36px so the card height stays
            constant whether the right-side actions (stat chips + Refresh
            button) are rendered or not. Otherwise the bare-title state on
            the Files / Master tabs is ~6px shorter than the Darks/Flats
            state, and the layout below shifts when switching tabs. */}
        <div className="flex items-center justify-between min-h-9">
          <div className="flex items-center gap-3 flex-wrap">
            <button
              onClick={onClose}
              title="Back to Equipment"
              className="flex items-center text-content-muted hover:text-content transition pr-3 mr-1 border-r border-border"
            >
              <ArrowLeft size={18} />
            </button>
            <h1 className="text-xl font-bold">{instrume}</h1>

            {/* Filter notice — explains that the dual-pane on the Files tab
                is camera-scoped on the left side. The tooltip gives the
                full detail (right pane is unrestricted). */}
            {activeTab === "files" && (
              <div
                className="flex items-center gap-2 text-content-muted"
                title={`The left pane lists only folders that contain ${instrume} frames, and within them only the files captured with this camera. The right pane is unrestricted — use it as the destination for Move, Mkdir, and so on.`}
              >
                <Filter size={16} />
                <span className="text-sm">
                  Left pane filtered to this camera's folders &amp; files
                </span>
              </div>
            )}

            {/* Inline stat chips — only meaningful on the calibration tabs
                (Darks / Flats) where currentStats is populated. */}
            {(activeTab === "darks" || activeTab === "flats")
              && currentStats
              && currentStats.totalSets > 0 && (
              <>
                <div className="flex items-center gap-2 text-content-muted">
                  <Package size={16} />
                  <span className="font-mono text-sm">
                    {currentStats.totalSets} {currentStats.totalSets === 1 ? "set" : "sets"}
                  </span>
                </div>
                {currentStats.dateFrom && currentStats.dateTo && (
                  <div className="flex items-center gap-2 text-content-muted">
                    <Calendar size={16} />
                    <span className="font-mono text-sm">
                      {format(new Date(currentStats.dateFrom), "MMM yyyy")} – {format(new Date(currentStats.dateTo), "MMM yyyy")}
                    </span>
                  </div>
                )}
              </>
            )}
          </div>

          <div className="flex items-center gap-3">
            {(activeTab === "darks" || activeTab === "flats")
              && currentStats
              && currentStats.totalSets > 0 && (
              <button
                onClick={handleRefreshLibrary}
                disabled={refreshing}
                title="Re-scan monitored directories for calibration frames and rebuild calibration sets for this camera"
                className="flex items-center gap-2 rounded-lg border border-border bg-surface-hover px-3 py-1.5 text-sm hover:brightness-110 disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <RefreshCw size={14} className={refreshing ? "animate-spin" : ""} />
                {refreshing ? "Refreshing…" : "Refresh Calibration Sets"}
              </button>
            )}
          </div>
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

      {/* Tab Bar — same shape as FrameSetDetail's: `flex items-center gap-1
          border-b ... flex-shrink-0` outer, buttons use `px-4 py-2.5 text-sm
          font-medium ... -mb-px` so the active border lines up with the
          container's bottom border. */}
      <div className="flex items-center gap-1 border-b border-border mb-3 flex-shrink-0">
        <button
          onClick={() => setActiveTab("files")}
          className={`flex items-center gap-2 px-4 py-2.5 text-sm font-medium border-b-2 transition-colors -mb-px ${
            activeTab === "files"
              ? "border-accent text-accent"
              : "border-transparent text-content-muted hover:text-content hover:border-border"
          }`}
        >
          Files
        </button>
        <button
          onClick={() => setActiveTab("darks")}
          className={`flex items-center gap-2 px-4 py-2.5 text-sm font-medium border-b-2 transition-colors -mb-px ${
            activeTab === "darks"
              ? "border-accent text-accent"
              : "border-transparent text-content-muted hover:text-content hover:border-border"
          }`}
        >
          Darks
          <span className="text-xs text-content-muted">(Dark/Bias/DarkFlat)</span>
        </button>
        <button
          onClick={() => setActiveTab("flats")}
          className={`flex items-center gap-2 px-4 py-2.5 text-sm font-medium border-b-2 transition-colors -mb-px ${
            activeTab === "flats"
              ? "border-accent text-accent"
              : "border-transparent text-content-muted hover:text-content hover:border-border"
          }`}
        >
          Flats
          <span className="text-xs text-content-muted">(Flat Calibration)</span>
        </button>
        <button
          onClick={() => setActiveTab("master-darks")}
          className={`flex items-center gap-2 px-4 py-2.5 text-sm font-medium border-b-2 transition-colors -mb-px ${
            activeTab === "master-darks"
              ? "border-purple text-purple"
              : "border-transparent text-content-muted hover:text-content hover:border-border"
          }`}
        >
          Master Darks
          <span className="text-xs text-content-muted">(MasterDark/Bias/DarkFlat)</span>
        </button>
        <button
          onClick={() => setActiveTab("master-flats")}
          className={`flex items-center gap-2 px-4 py-2.5 text-sm font-medium border-b-2 transition-colors -mb-px ${
            activeTab === "master-flats"
              ? "border-purple text-purple"
              : "border-transparent text-content-muted hover:text-content hover:border-border"
          }`}
        >
          Master Flats
          <span className="text-xs text-content-muted">(MasterFlat)</span>
        </button>
      </div>

      {/* Tab Content. Files needs an unrestricted flex-1 host so the
          dual-pane's `h-full` resolves to the visible remainder of the
          page; the other tabs need overflow-auto so their content can
          scroll inside the same bounded container. */}
      <div className={`flex-1 min-h-0 ${activeTab === "files" ? "flex flex-col" : "overflow-auto"}`}>
        {activeTab === "files" && (
          // Dual-pane file browser scoped to this camera. The LEFT pane is
          // clamped to camera-touching scan roots and routes listings through
          // get_camera_directory_contents (same backend filter the legacy
          // DirectoryTree used). The RIGHT pane stays unfiltered so Move/F6
          // can still target arbitrary destinations (e.g. an archive folder).
          // The catalog search bar is also scoped to this camera.
          (() => {
            if (!cameraDirsLoaded) {
              return (
                <div className="bg-surface-elevated rounded-lg p-8 text-center text-content-muted">
                  Loading {instrume} files…
                </div>
              );
            }
            if (scanRoots.length === 0 || cameraDirectories.length === 0) {
              return (
                <div className="bg-surface-elevated rounded-lg p-8 text-center text-content-muted">
                  No files for {instrume}.
                </div>
              );
            }
            return (
              <div className="flex-1 min-h-0">
                <DualPaneFileBrowser
                  key={instrume}
                  scanRoots={scanRoots}
                  leftCameraFilter={{ instrume, cameraDirectories }}
                />
              </div>
            );
          })()
        )}
        {activeTab === "darks" && (
          <DarkLibrary
            instrume={instrume}
            isTabView={true}
            imageTypeFilter={darksFilter}
            onStatsChange={setCurrentStats}
            highlightSetId={pendingHighlightSetId}
          />
        )}
        {activeTab === "flats" && (
          <DarkLibrary
            instrume={instrume}
            isTabView={true}
            imageTypeFilter={flatsFilter}
            onStatsChange={setCurrentStats}
            highlightSetId={pendingHighlightSetId}
          />
        )}
        {activeTab === "master-darks" && (
          <MasterDarkLibrary instrume={instrume} isTabView={true} highlightSetId={pendingHighlightSetId} />
        )}
        {activeTab === "master-flats" && (
          <MasterFlatLibrary instrume={instrume} isTabView={true} highlightSetId={pendingHighlightSetId} />
        )}
      </div>

    </div>
  );
}
