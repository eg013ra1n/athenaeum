import React, { useState, useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  X,
  Play,
  Pause,
  ChevronLeft,
  ChevronRight,
  Loader2,
  Database,
} from "lucide-react";
import type { FileWithFrame } from "../types/models";

interface BlinkViewerProps {
  frames: FileWithFrame[];
  initialIndex?: number;
  onClose: () => void;
}

const BlinkViewer: React.FC<BlinkViewerProps> = ({
  frames,
  initialIndex = 0,
  onClose,
}) => {
  const [currentIndex, setCurrentIndex] = useState(initialIndex);
  const [isPlaying, setIsPlaying] = useState(false);
  const [blinkSpeed, setBlinkSpeed] = useState(2); // FPS
  const [loadedImages, setLoadedImages] = useState<Map<number, string>>(
    new Map()
  );
  const [loadingIndices, setLoadingIndices] = useState<Set<number>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [isCaching, setIsCaching] = useState(false);
  const [cacheProgress, setCacheProgress] = useState({ current: 0, total: 0 });
  const [debugLogs, setDebugLogs] = useState<string[]>([]);
  const [showDebug, setShowDebug] = useState(true);

  const canvasRef = useRef<HTMLCanvasElement>(null);
  const blinkIntervalRef = useRef<number | null>(null);
  const currentIndexRef = useRef(currentIndex);
  const loadedImagesRef = useRef(loadedImages);
  const renderLockRef = useRef(false); // Prevent concurrent renders

  // Keep refs updated
  currentIndexRef.current = currentIndex;
  loadedImagesRef.current = loadedImages;

  // Debug logging helper
  const addDebugLog = useCallback((message: string) => {
    const timestamp = new Date().toLocaleTimeString();
    const logMessage = `[${timestamp}] ${message}`;
    console.log(logMessage);
    setDebugLogs((prev) => [...prev.slice(-50), logMessage]); // Keep last 50 logs
  }, []);

  // Get current frame
  const currentFrame = frames[currentIndex];

  // Filter FITS and XISF files (both supported by rustafits 0.2+)
  const fitsFrames = frames.filter(
    (f) => f.file.format === "FITS" || f.file.format === "XISF"
  );

  // Load image from backend
  const loadImage = useCallback(
    async (index: number) => {
      if (index < 0 || index >= fitsFrames.length) return;

      // Use functional updates to check and set loading state atomically
      const shouldLoad = await new Promise<boolean>((resolve) => {
        setLoadingIndices((prev) => {
          // Check if already loading
          if (prev.has(index)) {
            resolve(false);
            return prev;
          }

          // Check if already loaded using ref
          if (loadedImagesRef.current.has(index)) {
            resolve(false);
            return prev;
          }

          // Mark as loading
          resolve(true);
          return new Set(prev).add(index);
        });
      });

      if (!shouldLoad) {
        return;
      }

      const frame = fitsFrames[index];
      if (!frame) return;

      setError(null);

      try {
        addDebugLog(`📥 Loading image ${index}: ${frame.file.filename}`);

        // Use rustafits for fast FITS to JPEG conversion
        // Resolution is read from settings database (no parameter = use setting)
        const imageData = await invoke<Uint8Array>("read_fits_image_rustafits", {
          path: frame.file.path,
        });

        addDebugLog(`📦 Received data for image ${index}: ${imageData?.length || 0} bytes`);

        // Handle both Uint8Array and regular array
        let binaryData: Uint8Array;
        if (imageData instanceof Uint8Array) {
          binaryData = imageData;
        } else if (Array.isArray(imageData)) {
          binaryData = new Uint8Array(imageData);
        } else {
          throw new Error(`Unexpected data type: ${typeof imageData}`);
        }

        // Convert binary JPEG to blob URL (JPEG is much smaller than PNG!)
        const blob = new Blob([binaryData], { type: "image/jpeg" });
        const url = URL.createObjectURL(blob);

        addDebugLog(`🔗 Created blob URL for image ${index}: ${(blob.size / 1024).toFixed(1)}KB`);

        // Store blob URL instead of base64
        setLoadedImages((prev) => {
          const newMap = new Map(prev);
          newMap.set(index, url);
          return newMap;
        });

        addDebugLog(`✅ Loaded image ${index + 1}/${fitsFrames.length}`);
      } catch (err) {
        addDebugLog(`❌ Failed to load image ${index}: ${err}`);
        setError(`Failed to load image: ${err}`);
      } finally {
        setLoadingIndices((prev) => {
          const newSet = new Set(prev);
          newSet.delete(index);
          return newSet;
        });
      }
    },
    [fitsFrames, addDebugLog]
  );

  // Render image to canvas
  const renderImage = useCallback(
    (imageUrl: string) => {
      addDebugLog(`🖼️  Starting render: ${imageUrl.substring(0, 50)}...`);

      const canvas = canvasRef.current;
      if (!canvas) {
        addDebugLog("❌ Canvas ref is null");
        return;
      }

      // Validate canvas dimensions
      if (!canvas.width || !canvas.height || canvas.width <= 0 || canvas.height <= 0) {
        addDebugLog(`❌ Invalid canvas dimensions: ${canvas.width}x${canvas.height}`);
        return;
      }

      const ctx = canvas.getContext("2d");
      if (!ctx) {
        addDebugLog("❌ Failed to get canvas 2d context");
        return;
      }

      // Check for lost context
      if ((ctx as any).isContextLost?.()) {
        addDebugLog("❌ Canvas context is lost");
        return;
      }

      // Prevent concurrent renders
      if (renderLockRef.current) {
        addDebugLog("⏸️  Render already in progress, skipping");
        return;
      }

      renderLockRef.current = true;
      addDebugLog("🔒 Render lock acquired");

      const img = new Image();

      img.onload = () => {
        try {
          addDebugLog(`📸 Image loaded: ${img.width}x${img.height}`);

          // Validate image dimensions
          if (!img.width || !img.height || img.width <= 0 || img.height <= 0) {
            addDebugLog(`❌ Invalid image dimensions: ${img.width}x${img.height}`);
            return;
          }

          // Calculate scaling to fit canvas
          const canvasAspect = canvas.width / canvas.height;
          const imageAspect = img.width / img.height;

          addDebugLog(`📐 Aspect ratios - Canvas: ${canvasAspect.toFixed(2)}, Image: ${imageAspect.toFixed(2)}`);

          // Check for NaN or Infinity
          if (!isFinite(canvasAspect) || !isFinite(imageAspect)) {
            addDebugLog(`❌ Invalid aspect ratios: canvas=${canvasAspect}, image=${imageAspect}`);
            return;
          }

          let renderWidth, renderHeight, offsetX, offsetY;

          if (imageAspect > canvasAspect) {
            // Image is wider - fit to width
            renderWidth = canvas.width;
            renderHeight = canvas.width / imageAspect;
            offsetX = 0;
            offsetY = (canvas.height - renderHeight) / 2;
          } else {
            // Image is taller - fit to height
            renderHeight = canvas.height;
            renderWidth = canvas.height * imageAspect;
            offsetX = (canvas.width - renderWidth) / 2;
            offsetY = 0;
          }

          // Validate calculated dimensions
          if (!isFinite(renderWidth) || !isFinite(renderHeight) ||
              !isFinite(offsetX) || !isFinite(offsetY)) {
            addDebugLog(`❌ Invalid render dimensions: w=${renderWidth}, h=${renderHeight}, x=${offsetX}, y=${offsetY}`);
            return;
          }

          addDebugLog(`🎯 Drawing at: ${offsetX.toFixed(0)},${offsetY.toFixed(0)} size: ${renderWidth.toFixed(0)}x${renderHeight.toFixed(0)}`);

          // Clear only letterbox areas (black bars) - keeps previous image visible during transitions
          ctx.fillStyle = "#000000";

          // Clear top/bottom bars (if image is wider than canvas aspect)
          if (offsetY > 0) {
            ctx.fillRect(0, 0, canvas.width, offsetY); // Top bar
            ctx.fillRect(0, offsetY + renderHeight, canvas.width, canvas.height - offsetY - renderHeight); // Bottom bar
          }

          // Clear left/right bars (if image is taller than canvas aspect)
          if (offsetX > 0) {
            ctx.fillRect(0, 0, offsetX, canvas.height); // Left bar
            ctx.fillRect(offsetX + renderWidth, 0, canvas.width - offsetX - renderWidth, canvas.height); // Right bar
          }

          // Draw image (overlays previous image for smooth transition)
          ctx.drawImage(img, offsetX, offsetY, renderWidth, renderHeight);
          addDebugLog("✅ Image rendered successfully");
        } catch (err) {
          addDebugLog(`❌ Error in img.onload handler: ${err}`);
          setError(`Failed to render image: ${err}`);
        } finally {
          renderLockRef.current = false;
          addDebugLog("🔓 Render lock released");
        }
      };

      img.onerror = (err) => {
        addDebugLog(`❌ Image load/decode error: ${err}`);
        setError(`Failed to load/decode image: ${err}`);
        renderLockRef.current = false;
        addDebugLog("🔓 Render lock released (error)");
      };

      img.src = imageUrl; // Can be blob: URL or data: URL
      addDebugLog("🔗 Image src set, waiting for load...");
    },
    [addDebugLog]
  );

  // Load current image and preload next few
  useEffect(() => {
    // Load current image
    loadImage(currentIndex);

    // Preload next 2 images
    loadImage(currentIndex + 1);
    loadImage(currentIndex + 2);
  }, [currentIndex, loadImage]);

  // Cleanup blob URLs when component unmounts
  useEffect(() => {
    return () => {
      // Clean up all blob URLs when component unmounts
      loadedImagesRef.current.forEach((url) => {
        if (url && url.startsWith("blob:")) {
          URL.revokeObjectURL(url);
        }
      });
    };
  }, []); // Empty dependency array - only run on mount/unmount

  // Render current image when loaded
  useEffect(() => {
    const imageUrl = loadedImages.get(currentIndex);
    if (imageUrl) {
      renderImage(imageUrl);
    }
  }, [currentIndex, loadedImages, renderImage]);

  // Handle window resize - update canvas size
  // NOTE: Canvas size change clears the canvas, so we only do this on mount and actual window resize
  useEffect(() => {
    const updateCanvasSize = () => {
      const canvas = canvasRef.current;
      if (!canvas) return;

      const newWidth = window.innerWidth * 0.75;
      const newHeight = window.innerHeight - 80;

      // Only update if size actually changed (avoid clearing canvas unnecessarily)
      if (canvas.width !== newWidth || canvas.height !== newHeight) {
        canvas.width = newWidth;
        canvas.height = newHeight;

        // Canvas was cleared by size change, re-render current image using refs for current values
        const imageUrl = loadedImagesRef.current.get(currentIndexRef.current);
        if (imageUrl) {
          renderImage(imageUrl);
        }
      }
    };

    updateCanvasSize();
    window.addEventListener("resize", updateCanvasSize);
    return () => window.removeEventListener("resize", updateCanvasSize);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [renderImage]); // Only depends on renderImage - doesn't re-run when currentIndex changes!

  // Blink playback
  useEffect(() => {
    if (isPlaying) {
      const interval = 1000 / blinkSpeed;
      blinkIntervalRef.current = setInterval(() => {
        setCurrentIndex((prev) => {
          const next = prev + 1;
          return next >= fitsFrames.length ? 0 : next;
        });
      }, interval);
    } else {
      if (blinkIntervalRef.current) {
        clearInterval(blinkIntervalRef.current);
        blinkIntervalRef.current = null;
      }
    }

    return () => {
      if (blinkIntervalRef.current) {
        clearInterval(blinkIntervalRef.current);
      }
    };
  }, [isPlaying, blinkSpeed, fitsFrames.length]);

  // Keyboard shortcuts
  useEffect(() => {
    const handleKeyPress = (e: KeyboardEvent) => {
      switch (e.key) {
        case " ": // Space - play/pause
          e.preventDefault();
          setIsPlaying((prev) => !prev);
          break;
        case "ArrowLeft": // Previous frame
          e.preventDefault();
          setIsPlaying(false);
          setCurrentIndex((prev) => Math.max(0, prev - 1));
          break;
        case "ArrowRight": // Next frame
          e.preventDefault();
          setIsPlaying(false);
          setCurrentIndex((prev) => Math.min(fitsFrames.length - 1, prev + 1));
          break;
        case "Escape": // Close
          e.preventDefault();
          onClose();
          break;
      }
    };

    window.addEventListener("keydown", handleKeyPress);
    return () => window.removeEventListener("keydown", handleKeyPress);
  }, [fitsFrames.length, onClose]);

  const handlePrevious = () => {
    setIsPlaying(false);
    setCurrentIndex((prev) => Math.max(0, prev - 1));
  };

  const handleNext = () => {
    setIsPlaying(false);
    setCurrentIndex((prev) => Math.min(fitsFrames.length - 1, prev + 1));
  };

  const handleTogglePlay = () => {
    setIsPlaying((prev) => !prev);
  };

  const handleFrameClick = (index: number) => {
    setIsPlaying(false);
    setCurrentIndex(index);
  };

  const handleCacheAll = async () => {
    setIsCaching(true);
    setIsPlaying(false); // Stop playback during caching

    // Get list of uncached indices
    const uncachedIndices = fitsFrames
      .map((_, i) => i)
      .filter((i) => !loadedImages.has(i));

    setCacheProgress({ current: 0, total: uncachedIndices.length });

    if (uncachedIndices.length === 0) {
      console.log("All images already cached!");
      setIsCaching(false);
      return;
    }

    console.log(`Caching ${uncachedIndices.length} images in parallel batches of 4...`);

    // Process images in batches of 4 for optimal parallel performance
    const BATCH_SIZE = 4;
    let completed = 0;

    for (let i = 0; i < uncachedIndices.length; i += BATCH_SIZE) {
      const batch = uncachedIndices.slice(i, i + BATCH_SIZE);

      // Load batch in parallel
      const results = await Promise.allSettled(
        batch.map((idx) => loadImage(idx))
      );

      // Count successful loads
      results.forEach((result, batchIdx) => {
        if (result.status === "rejected") {
          console.error(`Failed to cache frame ${batch[batchIdx]}:`, result.reason);
        }
      });

      completed += batch.length;
      setCacheProgress({ current: completed, total: uncachedIndices.length });
    }

    console.log(`Caching complete! Loaded ${completed} images.`);
    setIsCaching(false);
  };

  return (
    <div className="fixed inset-0 z-50 bg-black flex flex-col">
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-2 bg-gray-900 border-b border-gray-700">
        <div className="text-white">
          <span className="font-semibold">Blink Viewer</span>
          {currentFrame && (
            <span className="ml-4 text-sm text-gray-400">
              {currentFrame.file.filename}
              {currentFrame.frame?.filter && (
                <span className="ml-2">• {currentFrame.frame.filter}</span>
              )}
              {currentFrame.frame?.exptime && (
                <span className="ml-2">• {currentFrame.frame.exptime}s</span>
              )}
            </span>
          )}
        </div>
        <button
          onClick={onClose}
          className="p-2 hover:bg-gray-800 rounded transition-colors"
        >
          <X className="text-white" size={20} />
        </button>
      </div>

      {/* Main content area */}
      <div className="flex-1 flex overflow-hidden">
        {/* Canvas area (left 75%) */}
        <div className="flex-1 relative bg-black flex items-center justify-center">
          <canvas
            ref={canvasRef}
            className="max-w-full max-h-full"
            style={{ imageRendering: "pixelated" }}
          />

          {/* Loading indicator - non-intrusive corner spinner */}
          {loadingIndices.has(currentIndex) && (
            <div className="absolute top-4 right-4 bg-gray-900 bg-opacity-75 rounded-full p-2">
              <Loader2 className="animate-spin text-white" size={24} />
            </div>
          )}

          {/* Error message */}
          {error && (
            <div className="absolute top-4 left-1/2 transform -translate-x-1/2 bg-red-600 text-white px-4 py-2 rounded shadow-lg">
              {error}
            </div>
          )}
        </div>

        {/* File list (right 25%) */}
        <div className="w-1/4 bg-gray-900 border-l border-gray-700 overflow-y-auto">
          <div className="p-2">
            <h3 className="text-sm font-semibold text-gray-400 mb-2">
              Frames ({fitsFrames.length})
            </h3>
            <div className="space-y-1">
              {fitsFrames.map((frame, index) => (
                <button
                  key={frame.file.id}
                  onClick={() => handleFrameClick(index)}
                  className={`w-full text-left px-3 py-2 rounded text-sm transition-colors ${
                    index === currentIndex
                      ? "bg-blue-600 text-white"
                      : "bg-gray-800 text-gray-300 hover:bg-gray-700"
                  }`}
                >
                  <div className="font-medium truncate">
                    {frame.file.filename}
                  </div>
                  <div className="text-xs opacity-75 mt-1">
                    {frame.frame?.filter && (
                      <span>{frame.frame.filter}</span>
                    )}
                    {frame.frame?.exptime && (
                      <span className="ml-2">{frame.frame.exptime}s</span>
                    )}
                  </div>
                  {loadingIndices.has(index) && (
                    <Loader2
                      className="animate-spin inline ml-2"
                      size={12}
                    />
                  )}
                </button>
              ))}
            </div>
          </div>
        </div>
      </div>

      {/* Caching progress bar */}
      {isCaching && (
        <div className="bg-gray-800 border-t border-gray-700 px-4 py-2">
          <div className="max-w-4xl mx-auto">
            <div className="flex items-center justify-between mb-1">
              <span className="text-sm text-gray-300">Caching images...</span>
              <span className="text-sm text-gray-400">
                {cacheProgress.current} / {cacheProgress.total}
              </span>
            </div>
            <div className="w-full bg-gray-700 rounded-full h-2">
              <div
                className="bg-green-600 h-2 rounded-full transition-all duration-200"
                style={{
                  width: `${
                    cacheProgress.total > 0
                      ? (cacheProgress.current / cacheProgress.total) * 100
                      : 0
                  }%`,
                }}
              />
            </div>
          </div>
        </div>
      )}

      {/* Controls bar */}
      <div className="bg-gray-900 border-t border-gray-700 px-4 py-3">
        <div className="flex items-center justify-between max-w-4xl mx-auto">
          {/* Playback controls */}
          <div className="flex items-center gap-2">
            <button
              onClick={handlePrevious}
              disabled={currentIndex === 0}
              className="p-2 rounded bg-gray-800 hover:bg-gray-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            >
              <ChevronLeft className="text-white" size={20} />
            </button>

            <button
              onClick={handleTogglePlay}
              className="p-2 rounded bg-blue-600 hover:bg-blue-700 transition-colors"
            >
              {isPlaying ? (
                <Pause className="text-white" size={20} />
              ) : (
                <Play className="text-white" size={20} />
              )}
            </button>

            <button
              onClick={handleNext}
              disabled={currentIndex === fitsFrames.length - 1}
              className="p-2 rounded bg-gray-800 hover:bg-gray-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            >
              <ChevronRight className="text-white" size={20} />
            </button>

            <button
              onClick={handleCacheAll}
              disabled={isCaching}
              className="p-2 rounded bg-green-600 hover:bg-green-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors ml-4"
              title="Cache all images"
            >
              <Database className="text-white" size={20} />
            </button>
          </div>

          {/* Frame counter */}
          <div className="text-white text-sm">
            <span className="font-semibold">{currentIndex + 1}</span>
            <span className="text-gray-400"> / {fitsFrames.length}</span>
          </div>

          {/* Speed control */}
          <div className="flex items-center gap-3">
            <label className="text-sm text-gray-400">Speed:</label>
            <input
              type="range"
              min="0.5"
              max="10"
              step="0.5"
              value={blinkSpeed}
              onChange={(e) => setBlinkSpeed(parseFloat(e.target.value))}
              className="w-32"
            />
            <span className="text-sm text-white w-12">{blinkSpeed} FPS</span>
          </div>
        </div>
      </div>

      {/* Keyboard shortcuts help */}
      <div className="absolute bottom-20 right-6 bg-gray-900 bg-opacity-90 text-gray-300 text-xs p-3 rounded shadow-lg">
        <div className="font-semibold mb-1">Keyboard Shortcuts:</div>
        <div>Space: Play/Pause</div>
        <div>← →: Previous/Next frame</div>
        <div>Esc: Close</div>
      </div>

      {/* Debug Console */}
      {showDebug && (
        <div className="absolute bottom-20 left-6 w-96 bg-gray-900 bg-opacity-95 text-gray-300 text-xs rounded shadow-lg max-h-96 flex flex-col">
          <div className="flex items-center justify-between px-3 py-2 border-b border-gray-700">
            <div className="font-semibold">Debug Console</div>
            <button
              onClick={() => setShowDebug(false)}
              className="text-gray-400 hover:text-white"
            >
              <X size={14} />
            </button>
          </div>
          <div className="overflow-y-auto p-2 space-y-1 max-h-80">
            {debugLogs.length === 0 ? (
              <div className="text-gray-500 italic">No logs yet...</div>
            ) : (
              debugLogs.map((log, i) => (
                <div key={i} className="font-mono text-xs">
                  {log}
                </div>
              ))
            )}
          </div>
        </div>
      )}

      {/* Debug Console Toggle (when hidden) */}
      {!showDebug && (
        <button
          onClick={() => setShowDebug(true)}
          className="absolute bottom-20 left-6 bg-gray-900 bg-opacity-90 text-gray-300 text-xs px-3 py-2 rounded shadow-lg hover:bg-gray-800"
        >
          Show Debug Console
        </button>
      )}
    </div>
  );
};

export default BlinkViewer;
