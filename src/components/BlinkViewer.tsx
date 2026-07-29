import React, { useState, useEffect, useRef, useCallback, useMemo } from "react";
import { api } from '../api';
import { isTauri } from '../utils/platform';
import {
  X,
  Loader2,
  Trash2,
  AlertTriangle,
} from "lucide-react";
import type { FrameAnalysis } from "../types/models";
import type { AnnotationSettings } from "../types/analysis-config";
import { DEFAULT_ANNOTATION_SETTINGS } from "../types/helpers";
import { ToolBar, FrameList, FrameInfoPanel } from "./blink";
import { fetchAndComposeFlatContour, loadFlatContourOpts } from "./blink/flatContour";
import { useBlinkCache } from "../hooks/useBlinkCache";
import { useStarMetricsCache } from "../hooks/useStarMetricsCache";
import { drawStarOverlay } from "./blink/StarOverlay";

import type { BlinkViewerProps, SortField, SortDirection } from "./blink/types";

const BlinkViewer: React.FC<BlinkViewerProps> = ({
  frames,
  initialIndex = 0,
  onClose,
  sourceType = 'light',
  frameSetId,
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
  const [showHelp, setShowHelp] = useState(false);
  const [loadingFullRes, setLoadingFullRes] = useState(false);
  // Flat Contour Plot — when ON, the main canvas shows the contour plot
  // (image + legend strip composite from the backend) instead of the FITS
  // preview. Toggleable from the toolbar; only available on FLAT/MASTERFLAT.
  const [contourMode, setContourMode] = useState(false);
  /** Cache of contour-plot composite dataURLs, keyed by file_id, so
   *  navigating between flats with the toggle on doesn't re-fetch the
   *  same plot every time. */
  const contourCacheRef = useRef<Map<number, string>>(new Map());
  /** Refs that mirror reactive contour state for use inside listeners that
   *  capture once (resize, keyboard handlers). Without these, the resize
   *  handler renders the FITS preview after a window resize while contour
   *  mode is active, tearing the contour off the screen. */
  const contourModeRef = useRef(false);
  const canShowContourRef = useRef(false);
  const currentFileIdRef = useRef<number | null>(null);
  const fullResBlobRef = useRef<string | null>(null);
  const showAnnotationsRef = useRef(showAnnotations);
  const annotationSettingsRef = useRef(annotationSettings);
  showAnnotationsRef.current = showAnnotations;
  annotationSettingsRef.current = annotationSettings;

  // Resizable sidebar (stored in pixels, clamped on render)
  const [sidebarWidth, setSidebarWidth] = useState(320);
  const isDraggingDivider = useRef(false);

  // Load persisted sidebar width
  useEffect(() => {
    (async () => {
      try {
        const saved = await api.invoke<string>('get_setting', { key: 'ui.blink_sidebar_px', defaultValue: '320' });
        const val = parseInt(saved);
        if (!isNaN(val) && val >= 200 && val <= 800) setSidebarWidth(val);
      } catch { /* use default */ }
    })();
  }, []);

  // Analysis data + sorting
  const [analysisMap, setAnalysisMap] = useState<Map<number, FrameAnalysis>>(new Map());
  const [sortField, setSortField] = useState<SortField>('time');
  const [sortDirection, setSortDirection] = useState<SortDirection>('asc');

  const sortedIndicesRef = useRef<number[]>([]);
  const sortedPositionOfRef = useRef<Map<number, number>>(new Map());

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
  contourModeRef.current = contourMode;

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
        ? await api.invoke<Uint8Array<ArrayBuffer>>("read_fits_image_rustafits", {
            path: frame.file.path,
          })
        : await api.invoke<Uint8Array<ArrayBuffer>>("get_frame_preview", {
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

  // ── Flat Contour Plot integration ──────────────────────────────────────
  // Derive whether the current frame is eligible for contour analysis
  // (FLAT / MASTERFLAT). When the user navigates to a non-flat frame
  // while contourMode is on, auto-disable it so the toolbar button reflects
  // the now-disabled state and the canvas falls back to the FITS preview.
  const canShowContour = (() => {
    const t = currentFrame?.frame?.imagetyp?.toUpperCase().trim();
    return t === 'FLAT' || t === 'MASTERFLAT';
  })();
  // Mirror reactive contour state into refs the resize handler reads.
  canShowContourRef.current = canShowContour;
  currentFileIdRef.current = currentFrame?.file?.id ?? null;

  useEffect(() => {
    if (!canShowContour && contourMode) setContourMode(false);
  }, [canShowContour, contourMode]);

  // Reset zoom & pan whenever the displayed image SOURCE flips between
  // FITS preview and contour composite. The two have very different
  // aspect ratios (the composite is wider because of the legend strip),
  // so a carried-over zoom/pan from the previous source produces a
  // mis-fit (legend cut off, image overshooting) until the user resets
  // manually. Resetting on toggle replicates "fit to view" each time.
  useEffect(() => {
    zoomRef.current = 1.0;
    panRef.current = { x: 0, y: 0 };
    setDisplayZoom(1.0);
  }, [contourMode]);

  // Render when image loads or frame changes. When contourMode is on for
  // an eligible frame, fetch the contour composite (image + legend strip)
  // from the backend and render IT instead of the FITS preview. Cached by
  // file_id so toggling on/off or navigating between flats is instant.
  useEffect(() => {
    if (contourMode && canShowContour && currentFrame?.file?.id != null) {
      const fileId = currentFrame.file.id;
      const cached = contourCacheRef.current.get(fileId);
      if (cached) {
        renderImage(cached);
        return;
      }
      let cancelled = false;
      void (async () => {
        try {
          const opts = await loadFlatContourOpts();
          const dataUrl = await fetchAndComposeFlatContour(fileId, opts);
          if (cancelled) return;
          contourCacheRef.current.set(fileId, dataUrl);
          renderImage(dataUrl);
        } catch (err) {
          if (cancelled) return;
          console.error('Flat contour fetch failed:', err);
          setError(typeof err === 'string' ? err : (err as Error).message ?? String(err));
        }
      })();
      return () => { cancelled = true; };
    }
    const imageValue = loadedImages.get(currentIndex);
    if (imageValue) renderImage(imageValue);
  }, [currentIndex, loadedImages, renderImage, contourMode, canShowContour, currentFrame]);

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

      const clampedSidebar = Math.max(200, Math.min(window.innerWidth * 0.4, sidebarWidth));
      const newWidth = window.innerWidth - clampedSidebar - 4; // 4px for divider
      const newHeight = window.innerHeight - 48;

      if (canvas.width !== newWidth || canvas.height !== newHeight) {
        canvas.width = newWidth;
        canvas.height = newHeight;
        // Resize must respect contour mode: re-render the contour
        // composite if it's active, otherwise the FITS preview.
        // Without this branch the resize handler tears the contour off
        // the screen and replaces it with the regular preview.
        if (
          contourModeRef.current
          && canShowContourRef.current
          && currentFileIdRef.current != null
        ) {
          const cached = contourCacheRef.current.get(currentFileIdRef.current);
          if (cached) {
            renderImage(cached);
            return;
          }
        }
        const imageValue = loadedImagesRef.current.get(currentIndexRef.current);
        if (imageValue) renderImage(imageValue);
      }
    };

    updateCanvasSize();
    window.addEventListener("resize", updateCanvasSize);
    return () => window.removeEventListener("resize", updateCanvasSize);
  }, [renderImage, sidebarWidth]);

  // Blink playback
  useEffect(() => {
    if (isPlaying) {
      blinkIntervalRef.current = setInterval(() => {
        setCurrentIndex((prev) => {
          const si = sortedIndicesRef.current;
          const sp = sortedPositionOfRef.current;
          const pos = sp.get(prev) ?? 0;
          return pos + 1 >= si.length ? si[0] : si[pos + 1];
        });
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
        ? await api.invoke<Uint8Array<ArrayBuffer>>("read_fits_image_rustafits", {
            path: frame.file.path,
            resolution: "full",
          })
        : await api.invoke<Uint8Array<ArrayBuffer>>("get_frame_preview", {
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
          setCurrentIndex((p) => {
            const pos = sortedPositionOfRef.current.get(p) ?? 0;
            return pos > 0 ? sortedIndicesRef.current[pos - 1] : p;
          });
          break;
        case "ArrowDown":
          e.preventDefault();
          setIsPlaying(false);
          setCurrentIndex((p) => {
            const si = sortedIndicesRef.current;
            const pos = sortedPositionOfRef.current.get(p) ?? 0;
            return pos < si.length - 1 ? si[pos + 1] : p;
          });
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

  // Load analysis data on mount
  useEffect(() => {
    if (!frameSetId) return;
    (async () => {
      try {
        const results = await api.invoke<FrameAnalysis[]>('get_analysis_for_frame_set', { frameSetId });
        const map = new Map<number, FrameAnalysis>();
        for (const a of results) map.set(a.frame_id, a);
        setAnalysisMap(map);
      } catch (err) {
        console.error('Failed to load analysis data:', err);
      }
    })();
  }, [frameSetId]);

  // Sort handler
  const handleSortChange = useCallback((field: SortField) => {
    if (field === sortField) {
      setSortDirection(d => d === 'asc' ? 'desc' : 'asc');
    } else {
      setSortField(field);
      setSortDirection('asc');
    }
  }, [sortField]);

  // Sorted indices — shared between FrameList display and blink navigation
  const sortedIndices = useMemo(() => {
    const indices = fitsFrames.map((_, i) => i);
    indices.sort((a, b) => {
      const fa = fitsFrames[a];
      const fb = fitsFrames[b];
      const aa = fa.frame?.id ? analysisMap.get(fa.frame.id) : undefined;
      const ab = fb.frame?.id ? analysisMap.get(fb.frame.id) : undefined;
      let cmp = 0;
      switch (sortField) {
        case 'time':
          cmp = (fa.frame?.date_obs ?? '').localeCompare(fb.frame?.date_obs ?? '');
          break;
        case 'filter':
          cmp = (fa.frame?.filter ?? '').localeCompare(fb.frame?.filter ?? '');
          break;
        case 'exptime':
          cmp = (fa.frame?.exptime ?? 0) - (fb.frame?.exptime ?? 0);
          break;
        case 'fwhm':
          cmp = (aa?.median_fwhm ?? Infinity) - (ab?.median_fwhm ?? Infinity);
          break;
        case 'eccentricity':
          cmp = (aa?.median_eccentricity ?? Infinity) - (ab?.median_eccentricity ?? Infinity);
          break;
        case 'frame_snr':
          cmp = (aa?.frame_snr ?? -Infinity) - (ab?.frame_snr ?? -Infinity);
          break;
      }
      return sortDirection === 'asc' ? cmp : -cmp;
    });
    return indices;
  }, [fitsFrames, analysisMap, sortField, sortDirection]);

  // Map from original index to position in sorted order
  const sortedPositionOf = useMemo(() => {
    const map = new Map<number, number>();
    sortedIndices.forEach((origIdx, sortPos) => map.set(origIdx, sortPos));
    return map;
  }, [sortedIndices]);
  sortedIndicesRef.current = sortedIndices;
  sortedPositionOfRef.current = sortedPositionOf;

  // Handlers
  const handlePrevious = useCallback(() => {
    setIsPlaying(false);
    setCurrentIndex((p) => {
      const pos = sortedPositionOf.get(p) ?? 0;
      return pos > 0 ? sortedIndices[pos - 1] : p;
    });
  }, [sortedIndices, sortedPositionOf]);

  const handleNext = useCallback(() => {
    setIsPlaying(false);
    setCurrentIndex((p) => {
      const pos = sortedPositionOf.get(p) ?? 0;
      return pos < sortedIndices.length - 1 ? sortedIndices[pos + 1] : p;
    });
  }, [sortedIndices, sortedPositionOf]);

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

  const handleInvertSelection = useCallback(() => {
    setSelectedFrames(prev => {
      const inverted = new Set<number>();
      for (let i = 0; i < fitsFrames.length; i++) {
        if (!prev.has(i)) inverted.add(i);
      }
      return inverted;
    });
  }, [fitsFrames.length]);

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
      // Skip files already in the black hole — re-blackholing them would create a
      // duplicate record. Matches the `nonBlackholedInSelectionCount` shown on the
      // toolbar button and confirm dialog. Mirrors handleRestoreSelected's filter.
      if (blackholedFileIds.has(frame.file.id)) continue;
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
  }, [selectedFrames, fitsFrames, blackholedFileIds, sourceType, onFramesRemoved]);

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
        showHelp={showHelp}
        onToggleHelp={() => setShowHelp(prev => !prev)}
        canShowContour={canShowContour}
        contourActive={contourMode && canShowContour}
        onShowContourPlot={() => {
          if (!canShowContour) return;
          setContourMode(prev => !prev);
        }}
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
          {showHelp && (
            <div className="absolute inset-0 flex items-center justify-center bg-black/60 z-20 pointer-events-none">
              <div className="bg-surface/90 backdrop-blur-sm rounded-xl px-8 py-6 text-sm text-content space-y-2 shadow-2xl">
                <h3 className="text-base font-semibold text-content mb-3">Keyboard Shortcuts</h3>
                <div className="grid grid-cols-2 gap-x-8 gap-y-1.5 text-xs">
                  <span className="text-content-muted">Space</span><span>Select / Deselect</span>
                  <span className="text-content-muted">Enter</span><span>Play / Pause</span>
                  <span className="text-content-muted">↑ ↓</span><span>Navigate frames</span>
                  <span className="text-content-muted">← →</span><span>Adjust speed</span>
                  <span className="text-content-muted">A</span><span>Toggle annotations</span>
                  <span className="text-content-muted">+ / -</span><span>Zoom in / out</span>
                  <span className="text-content-muted">0 / Home</span><span>Reset zoom</span>
                  <span className="text-content-muted">Ctrl+A</span><span>Select all</span>
                  <span className="text-content-muted">Shift+Click</span><span>Range select</span>
                  <span className="text-content-muted">Esc</span><span>Close viewer</span>
                </div>
              </div>
            </div>
          )}
        </div>

        {/* Draggable divider */}
        <div
          className="w-1 cursor-col-resize bg-border/50 hover:bg-accent/50 active:bg-accent transition-colors flex-shrink-0"
          onMouseDown={(e) => {
            e.preventDefault();
            isDraggingDivider.current = true;
            const onMove = (ev: MouseEvent) => {
              if (!isDraggingDivider.current) return;
              const px = window.innerWidth - ev.clientX;
              const clamped = Math.max(200, Math.min(window.innerWidth * 0.4, px));
              setSidebarWidth(clamped);
            };
            const onUp = () => {
              isDraggingDivider.current = false;
              document.removeEventListener('mousemove', onMove);
              document.removeEventListener('mouseup', onUp);
              setSidebarWidth(prev => {
                api.invoke('set_setting', { key: 'ui.blink_sidebar_px', value: String(Math.round(prev)) }).catch(() => {});
                return prev;
              });
            };
            document.addEventListener('mousemove', onMove);
            document.addEventListener('mouseup', onUp);
          }}
        />

        {/* Right sidebar: Frame list + Info panel */}
        <div className="bg-surface flex flex-col flex-shrink-0" style={{ width: `${Math.max(200, Math.min(window.innerWidth * 0.4, sidebarWidth))}px` }}>
          {/* Frame list — takes available space */}
          <div className="flex-1 min-h-0">
            <FrameList
              frames={fitsFrames}
              currentIndex={currentIndex}
              selectedFrames={selectedFrames}
              blackholedFileIds={blackholedFileIds}
              loadingIndices={loadingIndices}
              analysisMap={analysisMap}
              sortField={sortField}
              sortDirection={sortDirection}
              onSortChange={handleSortChange}
              onFrameClick={handleFrameClick}
              onCheckboxClick={handleCheckboxClick}
              onSelectAll={handleSelectAll}
              onClearSelection={handleClearSelection}
              onInvertSelection={handleInvertSelection}
            />
          </div>
          {/* Info panel — fixed at bottom */}
          <FrameInfoPanel
            currentFrame={currentFrame}
            metrics={
              (showAnnotations ? getStarMetrics(currentIndex)?.metrics : null)
              ?? (currentFrame?.frame?.id ? analysisMap.get(currentFrame.frame.id) ?? null : null)
            }
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
