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

interface FitsImageBinary {
  image_data: Uint8Array;
  width: number;
  height: number;
  is_color: boolean;
  bit_depth: number;
  format: string;
}

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

  const canvasRef = useRef<HTMLCanvasElement>(null);
  const blinkIntervalRef = useRef<number | null>(null);
  const currentIndexRef = useRef(currentIndex);
  const loadedImagesRef = useRef(loadedImages);

  // Keep refs updated
  currentIndexRef.current = currentIndex;
  loadedImagesRef.current = loadedImages;

  // Get current frame
  const currentFrame = frames[currentIndex];

  // Filter only FITS files (for now, XISF not supported in blink)
  const fitsFrames = frames.filter((f) => f.file.format === "FITS");

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
        console.log(`Loading FITS image (PNG): ${frame.file.path}`);
        const imageData = await invoke<FitsImageBinary>("read_fits_image_png", {
          path: frame.file.path,
        });

        console.log("Received image data:", {
          hasData: !!imageData.image_data,
          dataType: typeof imageData.image_data,
          dataLength: imageData.image_data?.length || 0,
          isUint8Array: imageData.image_data instanceof Uint8Array,
          isArray: Array.isArray(imageData.image_data),
          width: imageData.width,
          height: imageData.height
        });

        // Handle both Uint8Array and regular array
        let binaryData: Uint8Array;
        if (imageData.image_data instanceof Uint8Array) {
          binaryData = imageData.image_data;
        } else if (Array.isArray(imageData.image_data)) {
          binaryData = new Uint8Array(imageData.image_data);
        } else {
          throw new Error(`Unexpected data type: ${typeof imageData.image_data}`);
        }

        // Convert binary PNG to blob URL
        const blob = new Blob([binaryData], { type: "image/png" });
        const url = URL.createObjectURL(blob);
        console.log(`Created blob URL: ${url} (size: ${blob.size} bytes)`);

        // Store blob URL instead of base64
        setLoadedImages((prev) => {
          const newMap = new Map(prev);
          // Revoke previous URL to avoid memory leaks
          const oldUrl = newMap.get(index);
          if (oldUrl && oldUrl.startsWith("blob:")) {
            URL.revokeObjectURL(oldUrl);
          }
          newMap.set(index, url);
          return newMap;
        });

        console.log(
          `Loaded PNG image ${index + 1}/${fitsFrames.length}: ${imageData.width}x${
            imageData.height
          } (${imageData.is_color ? "RGB" : "Mono"}) - Blob size: ${blob.size}`
        );
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
    },
    [fitsFrames] // Only depend on fitsFrames, not on state variables
  );

  // Render image to canvas
  const renderImage = useCallback(
    (imageUrl: string) => {
      const canvas = canvasRef.current;
      if (!canvas) return;

      const ctx = canvas.getContext("2d");
      if (!ctx) return;

      const img = new Image();
      img.onload = () => {
        // Calculate scaling to fit canvas
        const canvasAspect = canvas.width / canvas.height;
        const imageAspect = img.width / img.height;

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
      };

      img.src = imageUrl; // Can be blob: URL or data: URL
    },
    []
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
    </div>
  );
};

export default BlinkViewer;
