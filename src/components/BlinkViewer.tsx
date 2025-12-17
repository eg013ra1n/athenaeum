import React, { useState, useEffect, useRef, useCallback, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  X,
  Loader2,
  Trash2,
  AlertTriangle,
} from "lucide-react";
import type { FileWithFrame } from "../types/models";
import { ToolBar, FrameList, DetailsBar } from "./blink";

interface BlinkViewerProps {
  frames: FileWithFrame[];
  initialIndex?: number;
  onClose: () => void;
  /** Context for actions - 'light' or 'calibration' */
  sourceType?: 'light' | 'calibration';
  /** Callback when frames are removed (sent to blackhole) */
  onFramesRemoved?: (frameIds: number[]) => void;
}

const BlinkViewer: React.FC<BlinkViewerProps> = ({
  frames,
  initialIndex = 0,
  onClose,
  sourceType = 'light',
  onFramesRemoved,
}) => {
  const [currentIndex, setCurrentIndex] = useState(initialIndex);
  const [isPlaying, setIsPlaying] = useState(false);
  const [blinkSpeed, setBlinkSpeed] = useState(2);
  const [loadedImages, setLoadedImages] = useState<Map<number, string>>(new Map());
  const [loadingIndices, setLoadingIndices] = useState<Set<number>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [isCaching, setIsCaching] = useState(false);
  const [cacheProgress, setCacheProgress] = useState({ current: 0, total: 0 });

  // Selection state
  const [selectedFrames, setSelectedFrames] = useState<Set<number>>(new Set());
  const [lastSelectedIndex, setLastSelectedIndex] = useState<number | null>(null);

  // Blackhole state
  const [showBlackholeConfirm, setShowBlackholeConfirm] = useState(false);
  const [isBlackholing, setIsBlackholing] = useState(false);
  const [blackholeError, setBlackholeError] = useState<string | null>(null);
  const [blackholedFileIds, setBlackholedFileIds] = useState<Set<number>>(new Set());

  const canvasRef = useRef<HTMLCanvasElement>(null);
  const blinkIntervalRef = useRef<number | null>(null);
  const currentIndexRef = useRef(currentIndex);
  const loadedImagesRef = useRef(loadedImages);
  const renderLockRef = useRef(false);

  // Keep refs updated
  currentIndexRef.current = currentIndex;
  loadedImagesRef.current = loadedImages;

  // Filter FITS and XISF files
  const fitsFrames = useMemo(
    () => frames.filter((f) => f.file.format === "FITS" || f.file.format === "XISF"),
    [frames]
  );

  const currentFrame = fitsFrames[currentIndex];

  // Selection counts
  const selectionCount = selectedFrames.size;

  const blackholedInSelectionCount = useMemo(() => {
    return Array.from(selectedFrames).filter((index) => {
      const frame = fitsFrames[index];
      return frame?.file?.id && blackholedFileIds.has(frame.file.id);
    }).length;
  }, [selectedFrames, fitsFrames, blackholedFileIds]);

  const nonBlackholedInSelectionCount = selectionCount - blackholedInSelectionCount;

  // Load image from backend
  const loadImage = useCallback(async (index: number) => {
    if (index < 0 || index >= fitsFrames.length) return;

    const shouldLoad = await new Promise<boolean>((resolve) => {
      setLoadingIndices((prev) => {
        if (prev.has(index) || loadedImagesRef.current.has(index)) {
          resolve(false);
          return prev;
        }
        resolve(true);
        return new Set(prev).add(index);
      });
    });

    if (!shouldLoad) return;

    const frame = fitsFrames[index];
    if (!frame) return;

    setError(null);

    try {
      const imageData = await invoke<Uint8Array>("read_fits_image_rustafits", {
        path: frame.file.path,
      });

      const binaryData = imageData instanceof Uint8Array
        ? imageData
        : new Uint8Array(imageData as number[]);

      const blob = new Blob([binaryData], { type: "image/jpeg" });
      const url = URL.createObjectURL(blob);

      setLoadedImages((prev) => new Map(prev).set(index, url));
    } catch (err) {
      console.error(`Failed to load image ${index}:`, err);
      setError(`Failed to load image: ${err}`);
    } finally {
      setLoadingIndices((prev) => {
        const newSet = new Set(prev);
        newSet.delete(index);
        return newSet;
      });
    }
  }, [fitsFrames]);

  // Render image to canvas
  const renderImage = useCallback((imageUrl: string) => {
    const canvas = canvasRef.current;
    if (!canvas || !canvas.width || !canvas.height) return;

    const ctx = canvas.getContext("2d");
    if (!ctx || (ctx as any).isContextLost?.() || renderLockRef.current) return;

    renderLockRef.current = true;
    const img = new Image();

    img.onload = () => {
      try {
        if (!img.width || !img.height) return;

        const canvasAspect = canvas.width / canvas.height;
        const imageAspect = img.width / img.height;

        if (!isFinite(canvasAspect) || !isFinite(imageAspect)) return;

        let renderWidth, renderHeight, offsetX, offsetY;

        if (imageAspect > canvasAspect) {
          renderWidth = canvas.width;
          renderHeight = canvas.width / imageAspect;
          offsetX = 0;
          offsetY = (canvas.height - renderHeight) / 2;
        } else {
          renderHeight = canvas.height;
          renderWidth = canvas.height * imageAspect;
          offsetX = (canvas.width - renderWidth) / 2;
          offsetY = 0;
        }

        ctx.fillStyle = "#000000";
        ctx.fillRect(0, 0, canvas.width, canvas.height);
        ctx.drawImage(img, offsetX, offsetY, renderWidth, renderHeight);
      } finally {
        renderLockRef.current = false;
      }
    };

    img.onerror = () => {
      setError("Failed to load/decode image");
      renderLockRef.current = false;
    };

    img.src = imageUrl;
  }, []);

  // Load current and preload next images
  useEffect(() => {
    loadImage(currentIndex);
    loadImage(currentIndex + 1);
    loadImage(currentIndex + 2);
  }, [currentIndex, loadImage]);

  // Cleanup blob URLs on unmount
  useEffect(() => {
    return () => {
      loadedImagesRef.current.forEach((url) => {
        if (url?.startsWith("blob:")) URL.revokeObjectURL(url);
      });
    };
  }, []);

  // Render current image
  useEffect(() => {
    const imageUrl = loadedImages.get(currentIndex);
    if (imageUrl) renderImage(imageUrl);
  }, [currentIndex, loadedImages, renderImage]);

  // Handle window resize
  useEffect(() => {
    const updateCanvasSize = () => {
      const canvas = canvasRef.current;
      if (!canvas) return;

      const newWidth = window.innerWidth * 0.75;
      const newHeight = window.innerHeight - 140;

      if (canvas.width !== newWidth || canvas.height !== newHeight) {
        canvas.width = newWidth;
        canvas.height = newHeight;
        const imageUrl = loadedImagesRef.current.get(currentIndexRef.current);
        if (imageUrl) renderImage(imageUrl);
      }
    };

    updateCanvasSize();
    window.addEventListener("resize", updateCanvasSize);
    return () => window.removeEventListener("resize", updateCanvasSize);
  }, [renderImage]);

  // Blink playback
  useEffect(() => {
    if (isPlaying) {
      blinkIntervalRef.current = setInterval(() => {
        setCurrentIndex((prev) => (prev + 1 >= fitsFrames.length ? 0 : prev + 1));
      }, 1000 / blinkSpeed);
    } else if (blinkIntervalRef.current) {
      clearInterval(blinkIntervalRef.current);
      blinkIntervalRef.current = null;
    }

    return () => {
      if (blinkIntervalRef.current) clearInterval(blinkIntervalRef.current);
    };
  }, [isPlaying, blinkSpeed, fitsFrames.length]);

  // Toggle selection for current frame
  const toggleCurrentFrameSelection = useCallback(() => {
    setSelectedFrames((prev) => {
      const newSet = new Set(prev);
      newSet.has(currentIndex) ? newSet.delete(currentIndex) : newSet.add(currentIndex);
      return newSet;
    });
    setLastSelectedIndex(currentIndex);
  }, [currentIndex]);

  // Keyboard shortcuts
  useEffect(() => {
    const handleKeyPress = (e: KeyboardEvent) => {
      switch (e.key) {
        case " ":
          e.preventDefault();
          (e.ctrlKey || e.metaKey) ? setIsPlaying((p) => !p) : toggleCurrentFrameSelection();
          break;
        case "ArrowLeft":
          e.preventDefault();
          setIsPlaying(false);
          setCurrentIndex((p) => Math.max(0, p - 1));
          break;
        case "ArrowRight":
          e.preventDefault();
          setIsPlaying(false);
          setCurrentIndex((p) => Math.min(fitsFrames.length - 1, p + 1));
          break;
        case "Escape":
          e.preventDefault();
          onClose();
          break;
        case "a":
          if (e.ctrlKey || e.metaKey) {
            e.preventDefault();
            setSelectedFrames(new Set(fitsFrames.map((_, i) => i)));
          }
          break;
      }
    };

    window.addEventListener("keydown", handleKeyPress);
    return () => window.removeEventListener("keydown", handleKeyPress);
  }, [fitsFrames.length, onClose, toggleCurrentFrameSelection, fitsFrames]);

  // Auto-cache on mount
  useEffect(() => {
    const startCaching = async () => {
      await new Promise((r) => setTimeout(r, 100));
      const uncached = fitsFrames.map((_, i) => i).filter((i) => !loadedImages.has(i) && i !== currentIndex);
      if (uncached.length === 0) return;

      setIsCaching(true);
      setCacheProgress({ current: 0, total: uncached.length });

      const BATCH = 4;
      for (let i = 0; i < uncached.length; i += BATCH) {
        await Promise.allSettled(uncached.slice(i, i + BATCH).map(loadImage));
        setCacheProgress({ current: Math.min(i + BATCH, uncached.length), total: uncached.length });
      }
      setIsCaching(false);
    };
    startCaching();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Check blackhole status on mount
  useEffect(() => {
    const check = async () => {
      const ids = frames.map((f) => f.file.id).filter((id): id is number => id != null);
      if (ids.length === 0) return;
      try {
        const blackholed = await invoke<number[]>('get_blackholed_file_ids', { fileIds: ids });
        setBlackholedFileIds(new Set(blackholed));
      } catch (err) {
        console.error('Failed to check blackhole status:', err);
      }
    };
    check();
  }, [frames]);

  // Handlers
  const handlePrevious = useCallback(() => {
    setIsPlaying(false);
    setCurrentIndex((p) => Math.max(0, p - 1));
  }, []);

  const handleNext = useCallback(() => {
    setIsPlaying(false);
    setCurrentIndex((p) => Math.min(fitsFrames.length - 1, p + 1));
  }, [fitsFrames.length]);

  const handleTogglePlay = useCallback(() => setIsPlaying((p) => !p), []);

  const handleSpeedChange = useCallback((speed: number) => setBlinkSpeed(speed), []);

  const handleFrameClick = useCallback((index: number, e: React.MouseEvent) => {
    setIsPlaying(false);
    if (e.shiftKey && lastSelectedIndex !== null) {
      const [start, end] = [Math.min(lastSelectedIndex, index), Math.max(lastSelectedIndex, index)];
      setSelectedFrames((prev) => {
        const newSet = new Set(prev);
        for (let i = start; i <= end; i++) newSet.add(i);
        return newSet;
      });
      setLastSelectedIndex(index);
    } else {
      setCurrentIndex(index);
      setLastSelectedIndex(index);
    }
  }, [lastSelectedIndex]);

  const handleCheckboxClick = useCallback((index: number, e: React.MouseEvent) => {
    e.stopPropagation();
    setSelectedFrames((prev) => {
      const newSet = new Set(prev);
      newSet.has(index) ? newSet.delete(index) : newSet.add(index);
      return newSet;
    });
    setLastSelectedIndex(index);
  }, []);

  const handleSelectAll = useCallback(() => {
    setSelectedFrames(new Set(fitsFrames.map((_, i) => i)));
  }, [fitsFrames]);

  const handleClearSelection = useCallback(() => {
    setSelectedFrames(new Set());
    setLastSelectedIndex(null);
  }, []);

  const handleBlackholeSelected = useCallback(async () => {
    if (selectedFrames.size === 0) return;
    setIsBlackholing(true);
    setBlackholeError(null);

    const blackholedIds: number[] = [];
    const errors: string[] = [];

    for (const index of selectedFrames) {
      const frame = fitsFrames[index];
      if (!frame?.file?.id) continue;
      try {
        await invoke('move_to_black_hole', { fileId: frame.file.id, fromWhere: sourceType });
        blackholedIds.push(frame.file.id);
      } catch (err) {
        errors.push(`${frame.file.filename}: ${err}`);
      }
    }

    if (blackholedIds.length > 0) {
      setBlackholedFileIds((prev) => new Set([...prev, ...blackholedIds]));
      setSelectedFrames(new Set());
      setLastSelectedIndex(null);
      onFramesRemoved?.(blackholedIds);
    }

    if (errors.length > 0) setBlackholeError(`${errors.length} error(s): ${errors[0]}`);
    setIsBlackholing(false);
    setShowBlackholeConfirm(false);
  }, [selectedFrames, fitsFrames, sourceType, onFramesRemoved]);

  const handleRestoreSelected = useCallback(async () => {
    const toRestore = Array.from(selectedFrames)
      .map((i) => fitsFrames[i])
      .filter((f) => f?.file?.id && blackholedFileIds.has(f.file.id))
      .map((f) => f.file.id!);

    if (toRestore.length === 0) return;
    setIsBlackholing(true);
    setBlackholeError(null);

    const restored: number[] = [];
    const errors: string[] = [];

    for (const id of toRestore) {
      try {
        await invoke('restore_from_black_hole', { fileId: id });
        restored.push(id);
      } catch (err) {
        errors.push(`${id}: ${err}`);
      }
    }

    if (restored.length > 0) {
      setBlackholedFileIds((prev) => {
        const newSet = new Set(prev);
        restored.forEach((id) => newSet.delete(id));
        return newSet;
      });
      setSelectedFrames(new Set());
      setLastSelectedIndex(null);
    }

    if (errors.length > 0) setBlackholeError(`Restore errors: ${errors[0]}`);
    setIsBlackholing(false);
  }, [selectedFrames, fitsFrames, blackholedFileIds]);

  return (
    <div className="fixed inset-0 z-50 bg-black flex flex-col">
      {/* TOP TOOLBAR */}
      <ToolBar
        currentIndex={currentIndex}
        totalFrames={fitsFrames.length}
        isPlaying={isPlaying}
        blinkSpeed={blinkSpeed}
        onPrevious={handlePrevious}
        onNext={handleNext}
        onTogglePlay={handleTogglePlay}
        onSpeedChange={handleSpeedChange}
        selectionCount={selectionCount}
        blackholedInSelectionCount={blackholedInSelectionCount}
        nonBlackholedInSelectionCount={nonBlackholedInSelectionCount}
        onClearSelection={handleClearSelection}
        onBlackhole={() => setShowBlackholeConfirm(true)}
        onRestore={handleRestoreSelected}
        isBlackholing={isBlackholing}
        isCaching={isCaching}
        cacheProgress={cacheProgress}
        onClose={onClose}
      />

      {/* MAIN CONTENT AREA */}
      <div className="flex-1 flex overflow-hidden">
        {/* Canvas area */}
        <div className="flex-1 relative bg-black flex items-center justify-center">
          <canvas ref={canvasRef} className="max-w-full max-h-full" style={{ imageRendering: "pixelated" }} />
          {loadingIndices.has(currentIndex) && (
            <div className="absolute top-4 right-4 bg-gray-900 bg-opacity-75 rounded-full p-2">
              <Loader2 className="animate-spin text-white" size={24} />
            </div>
          )}
          {error && (
            <div className="absolute top-4 left-1/2 transform -translate-x-1/2 bg-red-600 text-white px-4 py-2 rounded shadow-lg">
              {error}
            </div>
          )}
        </div>

        {/* Frame list */}
        <FrameList
          frames={fitsFrames}
          currentIndex={currentIndex}
          selectedFrames={selectedFrames}
          blackholedFileIds={blackholedFileIds}
          loadingIndices={loadingIndices}
          onFrameClick={handleFrameClick}
          onCheckboxClick={handleCheckboxClick}
          onSelectAll={handleSelectAll}
          onClearSelection={handleClearSelection}
        />
      </div>

      {/* BOTTOM DETAILS BAR */}
      <DetailsBar currentFrame={currentFrame} />

      {/* Blackhole error notification */}
      {blackholeError && (
        <div className="fixed bottom-24 left-1/2 transform -translate-x-1/2 bg-red-600 text-white px-4 py-2 rounded shadow-lg z-60 flex items-center gap-2">
          <AlertTriangle size={18} />
          {blackholeError}
          <button onClick={() => setBlackholeError(null)} className="ml-2 p-1 hover:bg-red-700 rounded">
            <X size={16} />
          </button>
        </div>
      )}

      {/* Blackhole confirmation dialog */}
      {showBlackholeConfirm && (
        <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-60">
          <div className="bg-gray-800 rounded-lg shadow-xl border border-gray-600 p-6 max-w-md w-full mx-4">
            <div className="flex items-center gap-3 mb-4">
              <div className="p-2 bg-red-600/20 rounded-full">
                <Trash2 className="text-red-400" size={24} />
              </div>
              <h3 className="text-lg font-semibold text-white">Send to Blackhole?</h3>
            </div>
            <p className="text-gray-300 mb-6">
              Are you sure you want to send{" "}
              <span className="font-semibold text-white">{nonBlackholedInSelectionCount} frame{nonBlackholedInSelectionCount !== 1 ? "s" : ""}</span>{" "}
              to the blackhole? This is a soft delete - files can be restored later.
            </p>
            <div className="flex justify-end gap-3">
              <button
                onClick={() => setShowBlackholeConfirm(false)}
                disabled={isBlackholing}
                className="px-4 py-2 text-gray-300 hover:text-white hover:bg-gray-700 rounded transition-colors disabled:opacity-50"
              >
                Cancel
              </button>
              <button
                onClick={handleBlackholeSelected}
                disabled={isBlackholing}
                className="flex items-center gap-2 px-4 py-2 bg-red-600 hover:bg-red-700 text-white rounded transition-colors disabled:opacity-50"
              >
                {isBlackholing ? (
                  <><Loader2 className="animate-spin" size={16} />Processing...</>
                ) : (
                  <><Trash2 size={16} />Send to Blackhole</>
                )}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
};

export default BlinkViewer;
