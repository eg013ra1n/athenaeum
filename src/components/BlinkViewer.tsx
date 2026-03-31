import React, { useState, useEffect, useRef, useCallback, useMemo } from "react";
import { api } from '../api';
import { isTauri } from '../utils/platform';
import {
  X,
  Loader2,
  Trash2,
  AlertTriangle,
} from "lucide-react";
import type { FileWithFrame } from "../types/models";
import type { AnnotationSettings } from "../types/analysis-config";
import { DEFAULT_ANNOTATION_SETTINGS } from "../types/analysis-config";
import { ToolBar, FrameList, FrameInfoPanel } from "./blink";
import { useBlinkCache } from "../hooks/useBlinkCache";
import { useStarMetricsCache } from "../hooks/useStarMetricsCache";
import { drawStarOverlay } from "./blink/StarOverlay";

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
  // Gate: wait for backend settings to be ready before loading images
  const [cacheModeReady, setCacheModeReady] = useState(false);
  // Max concurrent image requests — queried from backend (= available cores)
  const [maxConcurrent, setMaxConcurrent] = useState(8);

  // Selection state
  const [selectedFrames, setSelectedFrames] = useState<Set<number>>(new Set());
  const [lastSelectedIndex, setLastSelectedIndex] = useState<number | null>(null);

  // Blackhole state
  const [showBlackholeConfirm, setShowBlackholeConfirm] = useState(false);
  const [isBlackholing, setIsBlackholing] = useState(false);
  const [blackholeError, setBlackholeError] = useState<string | null>(null);
  const [blackholedFileIds, setBlackholedFileIds] = useState<Set<number>>(new Set());

  // Annotation state
  const [showAnnotations, setShowAnnotations] = useState(false);
  const [annotationSettings, setAnnotationSettings] = useState<AnnotationSettings>(DEFAULT_ANNOTATION_SETTINGS);
  const [fullResMode, setFullResMode] = useState(false);
  const [loadingFullRes, setLoadingFullRes] = useState(false);
  const fullResBlobRef = useRef<string | null>(null);
  const showAnnotationsRef = useRef(showAnnotations);
  const annotationSettingsRef = useRef(annotationSettings);
  showAnnotationsRef.current = showAnnotations;
  annotationSettingsRef.current = annotationSettings;

  const canvasRef = useRef<HTMLCanvasElement>(null);
  const blinkIntervalRef = useRef<number | null>(null);
  const currentIndexRef = useRef(currentIndex);
  const loadedImagesRef = useRef(loadedImages);
  const renderLockRef = useRef(false);

  // Zoom and pan refs (using refs for performance during rapid updates)
  const zoomRef = useRef<number>(1.0);
  const panRef = useRef<{ x: number; y: number }>({ x: 0, y: 0 });
  const isPanningRef = useRef<boolean>(false);
  const lastMousePosRef = useRef<{ x: number; y: number }>({ x: 0, y: 0 });
  const imageSizeRef = useRef<{ width: number; height: number } | null>(null);
  const currentImageRef = useRef<HTMLImageElement | HTMLCanvasElement | null>(null);

  // For UI indicator (debounced to avoid excessive re-renders)
  const [displayZoom, setDisplayZoom] = useState<number>(1.0);
  const zoomDisplayTimeoutRef = useRef<number | null>(null);
  // Cursor state for panning (separate from ref to trigger re-render for cursor change)
  const [isPanning, setIsPanning] = useState(false);

  // Keep refs updated
  currentIndexRef.current = currentIndex;
  loadedImagesRef.current = loadedImages;

  // Query backend capacity and signal ready on mount
  useEffect(() => {
    api.invoke<number>('get_blink_threads_max')
      .then((max) => setMaxConcurrent(max || 8))
      .catch(() => {}) // fallback already set to 8
      .finally(() => setCacheModeReady(true));
  }, []);

  // Load annotation display settings
  useEffect(() => {
    api.invoke<string>("get_setting", { key: "blink.annotation_config" })
      .then((json) => {
        if (json) {
          try {
            setAnnotationSettings(JSON.parse(json));
          } catch { /* use defaults */ }
        }
      })
      .catch(() => { /* use defaults */ });
  }, []);

  // Filter FITS and XISF files
  const fitsFrames = useMemo(
    () => frames.filter((f) => f.file.format === "FITS" || f.file.format === "XISF"),
    [frames]
  );

  const currentFrame = fitsFrames[currentIndex];

  const frameIds = useMemo(() => fitsFrames.map(f => f.frame?.id ?? 0), [fitsFrames]);

  const { getMetrics: getStarMetrics } = useStarMetricsCache({
    frameIds,
    currentIndex,
    enabled: showAnnotations,
  });
  const getStarMetricsRef = useRef(getStarMetrics);
  getStarMetricsRef.current = getStarMetrics;

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
      const imageData = isTauri
        ? await api.invoke<Uint8Array>("read_fits_image_rustafits", {
            path: frame.file.path,
          })
        : await api.invoke<Uint8Array>("get_frame_preview", {
            frameId: frame.file.id,
          });

      const binaryData = imageData instanceof Uint8Array
        ? imageData
        : new Uint8Array(imageData as number[]);

      // Both modes now return JPEG — create blob URL
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

  const { isCaching, cacheProgress, cacheStats } = useBlinkCache({
    frames: fitsFrames,
    currentIndex,
    cacheModeReady,
    maxConcurrent,
    loadedImages,
    loadImage,
  });

  // Helper to update zoom display with debouncing
  const updateZoomDisplay = useCallback((zoom: number) => {
    if (zoomDisplayTimeoutRef.current) {
      clearTimeout(zoomDisplayTimeoutRef.current);
    }
    zoomDisplayTimeoutRef.current = setTimeout(() => {
      setDisplayZoom(zoom);
    }, 50) as unknown as number;
  }, []);

  // Render the canvas: draw base image + optional star overlay.
  // Single function eliminates duplicated transform math and ensures overlay always follows image.
  const renderCanvas = useCallback(() => {
    const canvas = canvasRef.current;
    const img = currentImageRef.current;
    if (!canvas || !img || !canvas.width || !canvas.height) return;

    const ctx = canvas.getContext("2d");
    if (!ctx || (ctx as any).isContextLost?.()) return;

    const imgWidth = img.width;
    const imgHeight = img.height;
    if (!imgWidth || !imgHeight) return;

    const canvasAspect = canvas.width / canvas.height;
    const imageAspect = imgWidth / imgHeight;
    if (!isFinite(canvasAspect) || !isFinite(imageAspect)) return;

    // Fit-to-view base dimensions (zoom 1.0)
    let baseWidth: number, baseHeight: number;
    if (imageAspect > canvasAspect) {
      baseWidth = canvas.width;
      baseHeight = canvas.width / imageAspect;
    } else {
      baseHeight = canvas.height;
      baseWidth = canvas.height * imageAspect;
    }
    imageSizeRef.current = { width: baseWidth, height: baseHeight };

    const zoom = zoomRef.current;
    const renderWidth = baseWidth * zoom;
    const renderHeight = baseHeight * zoom;
    const centerX = canvas.width / 2 + panRef.current.x;
    const centerY = canvas.height / 2 + panRef.current.y;
    const offsetX = centerX - renderWidth / 2;
    const offsetY = centerY - renderHeight / 2;

    // Draw base image
    ctx.fillStyle = "#000000";
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    ctx.drawImage(img, offsetX, offsetY, renderWidth, renderHeight);

    // Draw star overlay if annotations are enabled (read from refs for stability)
    if (!showAnnotationsRef.current) return;
    const metricsResponse = getStarMetricsRef.current(currentIndexRef.current);
    if (!metricsResponse) return;

    drawStarOverlay(ctx, metricsResponse.stars, annotationSettingsRef.current, {
      offsetX, offsetY, renderWidth, renderHeight,
      imageWidth: metricsResponse.image_width,
      imageHeight: metricsResponse.image_height,
      flipVertical: metricsResponse.flip_vertical,
    });
  }, []);

  // Load a blob URL into an Image element and render to canvas.
  // If the same URL is already loaded, just re-renders (fast path for zoom/pan/overlay changes).
  const renderImage = useCallback((imageValue: string) => {
    const canvas = canvasRef.current;
    if (!canvas || !canvas.width || !canvas.height) return;

    const ctx = canvas.getContext("2d");
    if (!ctx || (ctx as any).isContextLost?.() || renderLockRef.current) return;

    if (currentImageRef.current instanceof HTMLImageElement && currentImageRef.current.src === imageValue) {
      renderCanvas();
      return;
    }

    renderLockRef.current = true;
    const img = new Image();

    img.onload = () => {
      try {
        currentImageRef.current = img;
        renderCanvas();
      } finally {
        renderLockRef.current = false;
      }
    };

    img.onerror = () => {
      setError("Failed to load/decode image");
      renderLockRef.current = false;
    };

    img.src = imageValue;
  }, [renderCanvas]);

  // Ensure current frame loads immediately on navigation
  useEffect(() => {
    if (!cacheModeReady) return;
    loadImage(currentIndex);
  }, [currentIndex, loadImage, cacheModeReady]);

  // Cleanup blob URLs on unmount
  useEffect(() => {
    return () => {
      loadedImagesRef.current.forEach((value) => {
        if (value.startsWith("blob:")) {
          URL.revokeObjectURL(value);
        }
      });
      if (fullResBlobRef.current) URL.revokeObjectURL(fullResBlobRef.current);
    };
  }, []);

  // Render when image loads or frame changes
  useEffect(() => {
    const imageValue = loadedImages.get(currentIndex);
    if (imageValue) renderImage(imageValue);
  }, [currentIndex, loadedImages, renderImage]);

  // Re-render when annotation state changes (renderCanvas reads from refs, just needs a kick)
  useEffect(() => {
    renderCanvas();
  }, [showAnnotations, getStarMetrics, annotationSettings, renderCanvas]);

  // Reset full-res mode when navigating to a different frame
  useEffect(() => {
    if (fullResMode) {
      if (fullResBlobRef.current) {
        URL.revokeObjectURL(fullResBlobRef.current);
        fullResBlobRef.current = null;
      }
      setFullResMode(false);
    }
  }, [currentIndex]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    const updateCanvasSize = () => {
      const canvas = canvasRef.current;
      if (!canvas) return;

      const newWidth = window.innerWidth * 0.75;
      const newHeight = window.innerHeight - 48;

      if (canvas.width !== newWidth || canvas.height !== newHeight) {
        canvas.width = newWidth;
        canvas.height = newHeight;
        const imageValue = loadedImagesRef.current.get(currentIndexRef.current);
        if (imageValue) renderImage(imageValue);
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

  // Toggle annotations
  const handleToggleAnnotations = useCallback(() => {
    setShowAnnotations((prev) => !prev);
  }, []);

  // Toggle full-resolution rendering for the current frame
  const handleToggleFullRes = useCallback(async () => {
    if (fullResMode) {
      // Turn off: revert to preview image
      if (fullResBlobRef.current) {
        URL.revokeObjectURL(fullResBlobRef.current);
        fullResBlobRef.current = null;
      }
      setFullResMode(false);
      // Re-render with preview image
      const previewUrl = loadedImagesRef.current.get(currentIndex);
      if (previewUrl) renderImage(previewUrl);
      return;
    }

    const frame = fitsFrames[currentIndex];
    if (!frame || loadingFullRes) return;

    setLoadingFullRes(true);
    try {
      const imageData = isTauri
        ? await api.invoke<Uint8Array>("read_fits_image_rustafits", {
            path: frame.file.path,
            resolution: "full",
          })
        : await api.invoke<Uint8Array>("get_frame_preview", {
            frameId: frame.file.id,
            resolution: "full",
          });

      const binaryData = imageData instanceof Uint8Array
        ? imageData
        : new Uint8Array(imageData as number[]);

      // Revoke previous full-res blob if any
      if (fullResBlobRef.current) URL.revokeObjectURL(fullResBlobRef.current);

      const blob = new Blob([binaryData], { type: "image/jpeg" });
      const url = URL.createObjectURL(blob);
      fullResBlobRef.current = url;

      setFullResMode(true);
      renderImage(url);
    } catch (err) {
      console.error("Failed to load full-res image:", err);
    } finally {
      setLoadingFullRes(false);
    }
  }, [fullResMode, loadingFullRes, currentIndex, fitsFrames, renderImage]);

  // Constrain pan to keep at least 10% of image visible
  const constrainPan = useCallback((newPan: { x: number; y: number }) => {
    const canvas = canvasRef.current;
    const imageSize = imageSizeRef.current;
    if (!canvas || !imageSize) return newPan;

    const zoom = zoomRef.current;
    const renderWidth = imageSize.width * zoom;
    const renderHeight = imageSize.height * zoom;

    // Allow panning only when zoomed in
    if (zoom <= 1.0) {
      return { x: 0, y: 0 };
    }

    // Calculate max pan (keep 10% of image visible)
    const marginX = Math.max(0, (renderWidth - canvas.width) / 2 + canvas.width * 0.1);
    const marginY = Math.max(0, (renderHeight - canvas.height) / 2 + canvas.height * 0.1);

    return {
      x: Math.max(-marginX, Math.min(marginX, newPan.x)),
      y: Math.max(-marginY, Math.min(marginY, newPan.y)),
    };
  }, []);

  // Reset zoom and pan to fit-to-view
  const resetZoom = useCallback(() => {
    zoomRef.current = 1.0;
    panRef.current = { x: 0, y: 0 };
    updateZoomDisplay(1.0);
    renderCanvas();
  }, [updateZoomDisplay, renderCanvas]);

  // Zoom in by keyboard (from center)
  const zoomIn = useCallback(() => {
    const oldZoom = zoomRef.current;
    const newZoom = Math.min(20, oldZoom * 1.25);
    if (newZoom === oldZoom) return;

    zoomRef.current = newZoom;
    panRef.current = constrainPan(panRef.current);
    updateZoomDisplay(newZoom);
    renderCanvas();
  }, [constrainPan, updateZoomDisplay, renderCanvas]);

  // Zoom out by keyboard (from center)
  const zoomOut = useCallback(() => {
    const oldZoom = zoomRef.current;
    const newZoom = Math.max(1.0, oldZoom / 1.25);
    if (newZoom === oldZoom) return;

    zoomRef.current = newZoom;
    panRef.current = constrainPan(panRef.current);
    updateZoomDisplay(newZoom);
    renderCanvas();
  }, [constrainPan, updateZoomDisplay, renderCanvas]);

  // Keyboard shortcuts
  useEffect(() => {
    const handleKeyPress = (e: KeyboardEvent) => {
      switch (e.key) {
        case " ":
          e.preventDefault();
          toggleCurrentFrameSelection();
          break;
        case "Enter":
          e.preventDefault();
          setIsPlaying((p) => !p);
          break;
        case "ArrowUp":
          e.preventDefault();
          setIsPlaying(false);
          setCurrentIndex((p) => Math.max(0, p - 1));
          break;
        case "ArrowDown":
          e.preventDefault();
          setIsPlaying(false);
          setCurrentIndex((p) => Math.min(fitsFrames.length - 1, p + 1));
          break;
        case "ArrowLeft":
          e.preventDefault();
          setBlinkSpeed((p) => Math.max(1, p - 1));
          break;
        case "ArrowRight":
          e.preventDefault();
          setBlinkSpeed((p) => Math.min(20, p + 1));
          break;
        case "Escape":
          e.preventDefault();
          onClose();
          break;
        case "a":
          if (e.ctrlKey || e.metaKey) {
            e.preventDefault();
            setSelectedFrames(new Set(fitsFrames.map((_, i) => i)));
          } else {
            // Bare 'a' toggles annotations
            e.preventDefault();
            handleToggleAnnotations();
          }
          break;
        // Zoom controls
        case "+":
        case "=":
          e.preventDefault();
          zoomIn();
          break;
        case "-":
          e.preventDefault();
          zoomOut();
          break;
        case "0":
        case "Home":
          e.preventDefault();
          resetZoom();
          break;
      }
    };

    window.addEventListener("keydown", handleKeyPress);
    return () => window.removeEventListener("keydown", handleKeyPress);
  }, [fitsFrames.length, onClose, toggleCurrentFrameSelection, fitsFrames, zoomIn, zoomOut, resetZoom, handleToggleAnnotations]);

  // Check blackhole status on mount
  useEffect(() => {
    const check = async () => {
      const ids = frames.map((f) => f.file.id).filter((id): id is number => id != null);
      if (ids.length === 0) return;
      try {
        const blackholed = await api.invoke<number[]>('get_blackholed_file_ids', { fileIds: ids });
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
    if (e.shiftKey && lastSelectedIndex !== null) {
      // Range selection with shift+click
      const [start, end] = [Math.min(lastSelectedIndex, index), Math.max(lastSelectedIndex, index)];
      setSelectedFrames((prev) => {
        const newSet = new Set(prev);
        for (let i = start; i <= end; i++) newSet.add(i);
        return newSet;
      });
    } else {
      // Toggle single item
      setSelectedFrames((prev) => {
        const newSet = new Set(prev);
        newSet.has(index) ? newSet.delete(index) : newSet.add(index);
        return newSet;
      });
    }
    setLastSelectedIndex(index);
  }, [lastSelectedIndex]);

  const handleSelectAll = useCallback(() => {
    setSelectedFrames(new Set(fitsFrames.map((_, i) => i)));
  }, [fitsFrames]);

  const handleClearSelection = useCallback(() => {
    setSelectedFrames(new Set());
    setLastSelectedIndex(null);
  }, []);

  // Handle mouse wheel for zoom
  const handleWheel = useCallback((e: React.WheelEvent<HTMLDivElement>) => {
    e.preventDefault();

    const canvas = canvasRef.current;
    if (!canvas) return;

    const rect = canvas.getBoundingClientRect();
    const mouseX = e.clientX - rect.left;
    const mouseY = e.clientY - rect.top;

    // Calculate cursor position relative to canvas center
    const cursorOffsetX = mouseX - canvas.width / 2;
    const cursorOffsetY = mouseY - canvas.height / 2;

    // Zoom factor
    const zoomFactor = e.deltaY < 0 ? 1.1 : 1 / 1.1;
    const oldZoom = zoomRef.current;
    const newZoom = Math.max(1.0, Math.min(20, oldZoom * zoomFactor));

    if (newZoom === oldZoom) return;

    // Zoom toward cursor: adjust pan so cursor stays over same image point
    const zoomRatio = newZoom / oldZoom;
    const newPan = {
      x: cursorOffsetX - (cursorOffsetX - panRef.current.x) * zoomRatio,
      y: cursorOffsetY - (cursorOffsetY - panRef.current.y) * zoomRatio,
    };

    zoomRef.current = newZoom;
    panRef.current = constrainPan(newPan);
    updateZoomDisplay(newZoom);
    renderCanvas();
  }, [constrainPan, updateZoomDisplay, renderCanvas]);

  // Handle mouse down for pan start
  const handleMouseDown = useCallback((e: React.MouseEvent<HTMLDivElement>) => {
    if (zoomRef.current <= 1.0) return; // Only pan when zoomed in
    if (e.button !== 0) return; // Only left click

    isPanningRef.current = true;
    setIsPanning(true);
    lastMousePosRef.current = { x: e.clientX, y: e.clientY };
    e.preventDefault();
  }, []);

  // Handle mouse move for panning
  const handleMouseMove = useCallback((e: React.MouseEvent<HTMLDivElement>) => {
    if (!isPanningRef.current) return;

    const deltaX = e.clientX - lastMousePosRef.current.x;
    const deltaY = e.clientY - lastMousePosRef.current.y;

    lastMousePosRef.current = { x: e.clientX, y: e.clientY };

    const newPan = {
      x: panRef.current.x + deltaX,
      y: panRef.current.y + deltaY,
    };

    panRef.current = constrainPan(newPan);
    renderCanvas();
  }, [constrainPan, renderCanvas]);

  // Handle mouse up for pan end
  const handleMouseUp = useCallback(() => {
    isPanningRef.current = false;
    setIsPanning(false);
  }, []);

  // Handle mouse leave (end panning if cursor leaves canvas)
  const handleMouseLeave = useCallback(() => {
    isPanningRef.current = false;
    setIsPanning(false);
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
        await api.invoke('move_to_black_hole', { fileId: frame.file.id, fromWhere: sourceType });
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
        await api.invoke('restore_from_black_hole', { fileId: id });
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
      onFramesRemoved?.(restored);
    }

    if (errors.length > 0) setBlackholeError(`Restore errors: ${errors[0]}`);
    setIsBlackholing(false);
  }, [selectedFrames, fitsFrames, blackholedFileIds, onFramesRemoved]);

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
        showAnnotations={showAnnotations}
        onToggleAnnotations={handleToggleAnnotations}
        fullResMode={fullResMode}
        loadingFullRes={loadingFullRes}
        onToggleFullRes={handleToggleFullRes}
        isCaching={isCaching}
        cacheProgress={cacheProgress}
        cacheStats={cacheStats}
        onClose={onClose}
      />

      {/* MAIN CONTENT AREA */}
      <div className="flex-1 flex overflow-hidden">
        {/* Canvas area */}
        <div className="flex-1 relative bg-black flex items-center justify-center">
          <canvas
            ref={canvasRef}
            style={{ imageRendering: "pixelated" }}
          />
          <div
            className="absolute inset-0"
            style={{
              cursor: displayZoom > 1 ? (isPanning ? "grabbing" : "grab") : "default",
            }}
            onWheel={handleWheel}
            onMouseDown={handleMouseDown}
            onMouseMove={handleMouseMove}
            onMouseUp={handleMouseUp}
            onMouseLeave={handleMouseLeave}
          />
          {/* Zoom indicator */}
          {displayZoom !== 1.0 && (
            <div className="absolute bottom-4 left-4 bg-surface/80 text-white px-3 py-1.5 rounded text-sm font-mono">
              {displayZoom < 1
                ? `${Math.round(displayZoom * 100)}%`
                : `${displayZoom.toFixed(1)}x`}
            </div>
          )}
          {loadingIndices.has(currentIndex) && (
            <div className="absolute top-4 right-4 bg-surface bg-opacity-75 rounded-full p-2">
              <Loader2 className="animate-spin text-white" size={24} />
            </div>
          )}
          {error && (
            <div className="absolute top-4 left-1/2 transform -translate-x-1/2 bg-error text-white px-4 py-2 rounded shadow-lg">
              {error}
            </div>
          )}
        </div>

        {/* Right sidebar: Frame list + Info panel */}
        <div className="w-1/4 bg-surface border-l border-border flex flex-col">
          {/* Frame list — takes available space */}
          <div className="flex-1 min-h-0">
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
          {/* Info panel — fixed at bottom */}
          <FrameInfoPanel
            currentFrame={currentFrame}
            metrics={showAnnotations ? (getStarMetrics(currentIndex)?.metrics ?? null) : null}
          />
        </div>
      </div>

      {/* Blackhole error notification */}
      {blackholeError && (
        <div className="fixed bottom-12 left-1/2 transform -translate-x-1/2 bg-error text-white px-4 py-2 rounded shadow-lg z-60 flex items-center gap-2">
          <AlertTriangle size={18} />
          {blackholeError}
          <button onClick={() => setBlackholeError(null)} className="ml-2 p-1 hover:brightness-90 rounded">
            <X size={16} />
          </button>
        </div>
      )}

      {/* Blackhole confirmation dialog */}
      {showBlackholeConfirm && (
        <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-60">
          <div className="bg-surface-elevated rounded-lg shadow-xl border border-border p-6 max-w-md w-full mx-4">
            <div className="flex items-center gap-3 mb-4">
              <div className="p-2 bg-error/20 rounded-full">
                <Trash2 className="text-error" size={24} />
              </div>
              <h3 className="text-lg font-semibold text-white">Send to Blackhole?</h3>
            </div>
            <p className="text-content-secondary mb-6">
              Are you sure you want to send{" "}
              <span className="font-semibold text-white">{nonBlackholedInSelectionCount} frame{nonBlackholedInSelectionCount !== 1 ? "s" : ""}</span>{" "}
              to the blackhole? This is a soft delete - files can be restored later.
            </p>
            <div className="flex justify-end gap-3">
              <button
                onClick={() => setShowBlackholeConfirm(false)}
                disabled={isBlackholing}
                className="px-4 py-2 text-content-secondary hover:text-white hover:bg-surface-hover rounded transition-colors disabled:opacity-50"
              >
                Cancel
              </button>
              <button
                onClick={handleBlackholeSelected}
                disabled={isBlackholing}
                className="flex items-center gap-2 px-4 py-2 bg-error hover:brightness-90 text-white rounded transition-colors disabled:opacity-50"
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
