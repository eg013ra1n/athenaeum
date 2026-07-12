import { useState, useEffect, useMemo, useCallback, useRef } from 'react';
import { Play, Trash2, BarChart3, Download, Check, LineChart, Table as TableIcon, X, Scissors, Plus, Calendar, Camera, ArrowLeftRight, Crosshair } from 'lucide-react';
import { api } from '../api';
import type {
  CalibrationHierarchyView as CalibrationHierarchyViewData,
  FrameAnalysis,
  FrameSetReference,
  LightFrameReadiness,
  LightCalDetails,
} from '../types/models';
import { useNotifications } from '../contexts/NotificationContext';
import { CameraFilterTree } from './calibration/CameraFilterTree';
import { MergedCameraFilterTree } from './calibration/MergedCameraFilterTree';
import { LightsAnalysisTable, type EnrichedLightFrame } from './calibration/LightsAnalysisTable';
import { RejectionThresholdBar, RejectionThresholds, EMPTY_THRESHOLDS } from './calibration/RejectionThresholdBar';
import { buildCameraFilterTree, buildMergedCameraFilterTree } from './calibration/utils';
import { BlackholedFramesSection } from './calibration/BlackholedFramesSection';
import { PlateSolveBatchPanel, type PlateSolveBatchPanelHandle } from './plate-solve/PlateSolveBatchPanel';
import { LightsAnalysisChartView } from './analysis/LightsAnalysisChartView';
import { useAnalysisProgressContext } from '../contexts/AnalysisProgressContext';

interface LightsAnalysisViewProps {
  hierarchy: CalibrationHierarchyViewData;
  frameSetId: number;
  frameSetName?: string;
  blackholedFileIds: Set<number>;
  onRefresh?: () => void;
  onBlink?: (frameIds: number[]) => void;
  onSplit?: (frameIds: number[]) => void;
  onCreateCustomSet?: (frameIds: number[]) => void;
  /** When true, hide the "Locate" column in the table — used when the frame
   *  set is ZIP-archived and source files are no longer on disk. */
  hideLocateColumn?: boolean;
  /** Called when the user sets or clears a reference frame, so the parent
   *  (FrameSetDetail) can re-evaluate the Stacking Preparation tab gate. */
  onReferenceChanged?: (ref: FrameSetReference | null) => void;
  /** The currently chosen reference frame id, loaded by the parent. If the
   *  parent provides this, the component will use it instead of loading it
   *  independently (avoids a double fetch). */
  referenceFrameId?: number | null;
  /** Per-frame light-calibration readiness (keyed by frame_id), fetched once by
   *  the parent. Drives the "Calib" status column in the table. */
  readinessByFrameId?: Map<number, LightFrameReadiness>;
  /** Per-frame calibration recipe (keyed by frame_id), fetched once by the
   *  parent. Feeds the status-badge tooltip in the table (spec §12.1). */
  detailsByFrameId?: Map<number, LightCalDetails>;
}

export function LightsAnalysisView({ hierarchy, frameSetId, frameSetName, blackholedFileIds, onRefresh, onBlink, onSplit, onCreateCustomSet, hideLocateColumn, onReferenceChanged, referenceFrameId: referenceFrameIdProp, readinessByFrameId, detailsByFrameId }: LightsAnalysisViewProps) {
  // View mode: by-night (date→camera→filter) or by-camera (camera→filter)
  const [viewMode, setViewMode] = useState<'by-night' | 'by-camera'>('by-camera');

  // Load persisted view mode on mount
  useEffect(() => {
    (async () => {
      try {
        const saved = await api.invoke<string>('get_setting', { key: 'ui.tree_view_mode', defaultValue: 'by-camera' });
        if (saved === 'by-night') setViewMode('by-night');
        else setViewMode('by-camera');
      } catch { /* use default */ }
    })();
  }, []);

  const handleViewModeChange = useCallback((mode: 'by-night' | 'by-camera') => {
    setViewMode(mode);
    setCheckedKeys(new Set());
    api.invoke('set_setting', { key: 'ui.tree_view_mode', value: mode }).catch(() => {});
  }, []);

  // Tree checkbox state — which filter groups are checked (for filtering the table)
  const [checkedKeys, setCheckedKeys] = useState<Set<string>>(new Set());
  // Table row selection — which individual frames are selected (for mass actions)
  const [selectedFrameIds, setSelectedFrameIds] = useState<Set<number>>(new Set());
  const [blackholing, setBlackholing] = useState(false);
  const [thresholds, setThresholds] = useState<RejectionThresholds>(EMPTY_THRESHOLDS);
  const [defaultThresholds, setDefaultThresholds] = useState<RejectionThresholds | null>(null);
  const [rejectActive, setRejectActive] = useState(true);

  const { notify } = useNotifications();

  // Reference frame — loaded locally unless the parent already provides it.
  // When the parent provides referenceFrameIdProp we skip the fetch but still
  // track a local override so the table updates immediately on set/clear.
  const [localReferenceFrameId, setLocalReferenceFrameId] = useState<number | null | undefined>(undefined);
  const [settingReference, setSettingReference] = useState(false);

  // Derive the effective reference frame id: prefer local override (once loaded),
  // fall back to what the parent passes.
  const effectiveReferenceFrameId: number | null =
    localReferenceFrameId !== undefined
      ? localReferenceFrameId
      : (referenceFrameIdProp ?? null);

  // Load the current reference on mount / frameSetId change (only if parent
  // doesn't supply the value — when parent supplies it we still seed local state
  // from it so we can apply immediate optimistic updates).
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const ref = await api.invoke<FrameSetReference | null>('get_frame_set_reference', {
          framesSetId: frameSetId,
        });
        if (!cancelled) setLocalReferenceFrameId(ref?.referenceFrameId ?? null);
      } catch (err) {
        console.error('[LightsAnalysisView] get_frame_set_reference failed:', err);
        if (!cancelled) setLocalReferenceFrameId(null);
      }
    })();
    return () => { cancelled = true; };
  }, [frameSetId]);

  const handleSetReference = useCallback(async (frameId: number) => {
    if (settingReference) return;
    setSettingReference(true);
    try {
      await api.invoke<void>('set_frame_set_reference', {
        framesSetId: frameSetId,
        frameId,
      });
      setLocalReferenceFrameId(frameId);
      const newRef: FrameSetReference = {
        framesSetId: frameSetId,
        referenceFrameId: frameId,
        setAt: new Date().toISOString(),
      };
      onReferenceChanged?.(newRef);
      notify({
        title: 'Reference frame set',
        detail: 'Open Stacking Preparation to run alignment.',
        kind: 'registration',
        tone: 'success',
        dedupeKey: `ref-set-${frameSetId}`,
      });
    } catch (err) {
      console.error('[LightsAnalysisView] set_frame_set_reference failed:', err);
    } finally {
      setSettingReference(false);
    }
  }, [frameSetId, settingReference, onReferenceChanged, notify]);

  // Analysis state — uses global context for queue/progress
  const { enqueueAnalysis, isAnalyzing, cancelAnalysis, activeAnalyses } = useAnalysisProgressContext();
  const analyzing = isAnalyzing(frameSetId);
  const currentAnalysis = activeAnalyses.get(frameSetId);
  const analysisProgress = currentAnalysis?.progress ?? null;

  const [analysisData, setAnalysisData] = useState<Map<number, FrameAnalysis>>(new Map());
  const [csvExportedMsg, setCsvExportedMsg] = useState<string | null>(null);
  const csvTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const plateSolveRef = useRef<PlateSolveBatchPanelHandle>(null);
  const [frameViewMode, setFrameViewMode] = useState<'table' | 'chart'>('table');
  const [useArcsec, setUseArcsec] = useState(false);
  const [defaultFwhmUnit, setDefaultFwhmUnit] = useState<'px' | 'arcsec'>('px');
  const defaultUnitAppliedRef = useRef(false);
  const [defaultsLoaded, setDefaultsLoaded] = useState(false);

  // Build both tree structures
  const dateTree = useMemo(() => buildCameraFilterTree(hierarchy), [hierarchy]);
  const mergedTree = useMemo(() => buildMergedCameraFilterTree(hierarchy), [hierarchy]);

  // Select based on viewMode
  const framesByKey = viewMode === 'by-night' ? dateTree.framesByKey : mergedTree.framesByKey;
  const allFramesRaw = viewMode === 'by-night' ? dateTree.allFrames : mergedTree.allFrames;

  // Split into active and blackholed frames
  const allFrames = useMemo(
    () => allFramesRaw.filter(f => !blackholedFileIds.has(f.file_id)),
    [allFramesRaw, blackholedFileIds]
  );
  const blackholedFrames = useMemo(
    () => allFramesRaw.filter(f => blackholedFileIds.has(f.file_id)),
    [allFramesRaw, blackholedFileIds]
  );

  // Compute stacked SNR per filter group: convert dB→linear, sqrt(sum(linear²)), back to dB
  const filterSnrMap = useMemo(() => {
    if (analysisData.size === 0) return undefined;
    const map = new Map<string, number>();
    const allKeys = viewMode === 'by-night' ? dateTree.framesByKey : mergedTree.framesByKey;
    for (const [key, frames] of allKeys) {
      let sumSq = 0;
      let count = 0;
      for (const f of frames) {
        if (blackholedFileIds.has(f.file_id)) continue;
        const a = analysisData.get(f.frame_id);
        if (a) {
          const linear = Math.pow(10, a.frame_snr / 20);
          sumSq += linear * linear;
          count++;
        }
      }
      if (count > 0) {
        const stackedLinear = Math.sqrt(sumSq);
        map.set(key, 20 * Math.log10(stackedLinear));
      }
    }
    return map.size > 0 ? map : undefined;
  }, [analysisData, viewMode, dateTree.framesByKey, mergedTree.framesByKey, blackholedFileIds]);

  // Load existing analysis data on mount
  useEffect(() => {
    loadAnalysisData();
  }, [frameSetId]);

  // Load saved rejection defaults + FWHM default unit on mount.
  // FWHM is excluded from the live pre-fill — it's auto-computed per frame set
  // (see fwhmAutoFilledForRef effect below). The saved FWHM is still kept in
  // `defaultThresholds` so the "Load defaults" button can restore it on demand.
  //
  // The saved `fwhm_unit` lives inside the JSON next to the value, so loading
  // the pair gives us an unambiguous interpretation of the saved number.
  // Legacy migration: if the JSON has no `fwhm_unit` field, fall back to the
  // standalone `analysis.fwhm_default_unit` setting (older saves).
  useEffect(() => {
    (async () => {
      try {
        const unitPref = await api.invoke<string>('get_setting', {
          key: 'analysis.fwhm_default_unit',
          defaultValue: 'px',
        });
        const prefersArcsec = unitPref === 'arcsec';
        if (prefersArcsec) setDefaultFwhmUnit('arcsec');

        const json = await api.invoke<string>('get_setting', {
          key: 'analysis.rejection_defaults',
          defaultValue: '',
        });
        if (json) {
          const saved = JSON.parse(json) as Partial<RejectionThresholds>;
          const merged: RejectionThresholds = { ...EMPTY_THRESHOLDS, ...saved };
          // Pair the saved value with its unit. Prefer the inline field; fall
          // back to the legacy standalone setting for older saves.
          if (merged.fwhm && merged.fwhm !== '') {
            merged.fwhm_unit = saved.fwhm_unit ?? (prefersArcsec ? 'arcsec' : 'px');
          } else {
            merged.fwhm_unit = undefined;
          }
          setDefaultThresholds(merged);
          setThresholds(prev => ({
            ...prev,
            eccentricity: merged.eccentricity,
            frame_snr: merged.frame_snr,
            snr_weight: merged.snr_weight,
            trail: merged.trail,
          }));
        }
      } catch (err) {
        console.error('Failed to load rejection defaults:', err);
      } finally {
        setDefaultsLoaded(true);
      }
    })();
  }, []);

  const loadAnalysisData = useCallback(async () => {
    try {
      const results = await api.invoke<FrameAnalysis[]>('get_analysis_for_frame_set', {
        frameSetId,
      });
      const map = new Map<number, FrameAnalysis>();
      for (const a of results) {
        map.set(a.frame_id, a);
      }
      setAnalysisData(map);
    } catch (err) {
      console.error('Failed to load analysis data:', err);
    }
  }, [frameSetId]);

  // Enqueue analysis via global queue
  const handleAnalyzeAll = useCallback((force?: boolean) => {
    enqueueAnalysis(frameSetId, frameSetName, force);
  }, [frameSetId, frameSetName, enqueueAnalysis]);

  // Reload analysis data when analysis completes
  useEffect(() => {
    if (currentAnalysis?.isComplete) {
      loadAnalysisData();
    }
  }, [currentAnalysis?.isComplete, loadAnalysisData]);

  // Plate scale from first frame with optics data (arcsec/pixel)
  const plateScale = useMemo(() => {
    for (const f of allFrames) {
      if (f.focallen && f.xpixsz) {
        return (f.xpixsz / f.focallen) * 206.265;
      }
    }
    return null;
  }, [allFrames]);

  // Apply the saved default FWHM unit once plateScale is available (one-shot).
  // If the user later toggles units manually, we don't override them.
  // Auto-fill is gated on this having run (see the fwhmAutoFilledForRef effect),
  // so no value-conversion is needed here — auto-fill won't fire until useArcsec
  // is settled.
  useEffect(() => {
    if (defaultUnitAppliedRef.current) return;
    if (defaultFwhmUnit === 'arcsec' && plateScale) {
      setUseArcsec(true);
      defaultUnitAppliedRef.current = true;
    }
  }, [defaultFwhmUnit, plateScale]);

  // Frames shown in the table: filtered by checked tree items, or all if nothing checked
  const displayedFrames = useMemo(() => {
    if (checkedKeys.size === 0) return allFrames;
    const frames: EnrichedLightFrame[] = [];
    for (const key of checkedKeys) {
      const keyFrames = framesByKey.get(key);
      if (keyFrames) frames.push(...keyFrames);
    }
    return frames.filter(f => !blackholedFileIds.has(f.file_id));
  }, [checkedKeys, allFrames, framesByKey, blackholedFileIds]);

  // Auto FWHM threshold in PIXELS. Pure data transform — no UI-state dependency.
  // Rule: median + 3 × max(MAD, 0.1 × median) over the *displayed* frames, so
  // the threshold tracks whatever cohort the user has filtered to. The MAD floor
  // keeps very tight clusters from collapsing the threshold onto the median
  // (which would reject ~half the frames).
  const autoFwhmPx = useMemo((): number | null => {
    if (analysisData.size === 0) return null;
    const values: number[] = [];
    for (const f of displayedFrames) {
      const a = analysisData.get(f.frame_id);
      if (a) values.push(a.median_fwhm);
    }
    if (values.length === 0) return null;
    values.sort((a, b) => a - b);
    const median = values[Math.floor(values.length / 2)];
    const deviations = values.map(v => Math.abs(v - median));
    deviations.sort((a, b) => a - b);
    const mad = deviations[Math.floor(deviations.length / 2)];
    const effectiveMad = Math.max(mad, median * 0.1);
    return median + 3 * effectiveMad;
  }, [analysisData, displayedFrames]);

  // Format a pixel value into the threshold-bar string in whatever unit is
  // currently displayed. Keep all internal math in pixels; convert only here.
  const pxToDisplayString = useCallback((px: number): string => {
    const v = (useArcsec && plateScale) ? px * plateScale : px;
    return v.toFixed(2);
  }, [useArcsec, plateScale]);

  const handleClearThresholds = useCallback(() => {
    const fwhm = autoFwhmPx != null ? pxToDisplayString(autoFwhmPx) : '';
    setThresholds({ ...EMPTY_THRESHOLDS, fwhm });
  }, [autoFwhmPx, pxToDisplayString]);

  // Load the saved defaults into the threshold bar, converting the FWHM value
  // from its saved unit to the unit the threshold bar is currently displaying.
  // The unit toggle itself is NOT flipped — the value adapts to the view, not
  // the other way around. If a unit conversion is required but no plate scale
  // is available, the FWHM field is left blank (we have no way to convert).
  const handleLoadDefaults = useCallback(() => {
    if (!defaultThresholds) return;
    const next: RejectionThresholds = { ...defaultThresholds };
    const savedUnit = defaultThresholds.fwhm_unit ?? 'px';
    const savedFwhm = parseFloat(defaultThresholds.fwhm);
    if (!isNaN(savedFwhm)) {
      const wantArcsec = useArcsec;
      if (savedUnit === 'arcsec' && !wantArcsec) {
        next.fwhm = plateScale ? (savedFwhm / plateScale).toFixed(2) : '';
      } else if (savedUnit === 'px' && wantArcsec) {
        next.fwhm = plateScale ? (savedFwhm * plateScale).toFixed(2) : '';
      }
      // else: units already match — keep the saved value as-is
    }
    setThresholds(next);
  }, [defaultThresholds, plateScale, useArcsec]);

  // Resolve checked tree keys to concrete frame IDs using the active-view framesByKey,
  // so callers don't need to know the by-night vs by-camera key shape.
  const collectCheckedFrameIds = useCallback((): number[] => {
    const ids: number[] = [];
    for (const key of checkedKeys) {
      const frames = framesByKey.get(key);
      if (frames) ids.push(...frames.map(f => f.frame_id));
    }
    return ids;
  }, [checkedKeys, framesByKey]);

  const handleSplit = useCallback(() => {
    if (!onSplit || checkedKeys.size === 0) return;
    onSplit(collectCheckedFrameIds());
  }, [onSplit, checkedKeys, collectCheckedFrameIds]);

  const handleCreateCustomSet = useCallback(() => {
    if (!onCreateCustomSet || checkedKeys.size === 0) return;
    onCreateCustomSet(collectCheckedFrameIds());
  }, [onCreateCustomSet, checkedKeys, collectCheckedFrameIds]);

  // Toggle the px ↔ arcsec display unit, converting the current FWHM threshold
  // by the plate scale so the threshold's physical meaning stays the same.
  //
  // IMPORTANT: do NOT call `setThresholds` from inside `setUseArcsec`'s updater.
  // React StrictMode invokes state updaters twice in development to detect
  // impure code; if `setThresholds` is queued from inside that updater, it ends
  // up queued twice and the second run sees the *already-converted* value,
  // dividing by plate scale a second time. (E.g. 5.85 → 3.38 → 1.95 for a
  // plate scale of 1.7312 arcsec/px.) Calling `setThresholds` with a single
  // pure updater outside avoids that — its updater can run twice with the
  // same `t.fwhm` and still produce the same converted value.
  const toggleUnits = useCallback(() => {
    const next = !useArcsec;
    setUseArcsec(next);
    if (plateScale) {
      setThresholds(t => {
        const val = parseFloat(t.fwhm);
        if (isNaN(val)) return t;
        const converted = next ? val * plateScale : val / plateScale;
        return { ...t, fwhm: converted.toFixed(2) };
      });
    }
  }, [useArcsec, plateScale]);

  // Auto-fill FWHM rejection from the analysis data when it first loads for this
  // frame set. Overrides any pre-filled saved default for FWHM (FWHM depends on
  // plate scale + atmospheric conditions, so a single saved value rarely applies
  // cleanly). Other fields keep saved-default precedence (handled in the
  // load-defaults effect). Per-frameSetId ref prevents re-firing after manual
  // edits.
  //
  // Gating: wait until (a) the saved-defaults load has finished, and (b) if the
  // user prefers arcsec AND the frame set has a plate scale, `useArcsec` has
  // actually flipped to true. Together these guarantee the conversion in
  // `pxToDisplayString` writes the value in the user's intended unit on the
  // very first auto-fill — so the initial-load value matches the value Reset
  // would produce.
  const fwhmAutoFilledForRef = useRef<number | null>(null);
  useEffect(() => {
    if (!defaultsLoaded) return;
    if (autoFwhmPx == null) return;
    if (defaultFwhmUnit === 'arcsec' && plateScale && !useArcsec) return;
    if (fwhmAutoFilledForRef.current === frameSetId) return;
    fwhmAutoFilledForRef.current = frameSetId;
    setThresholds(prev => ({ ...prev, fwhm: pxToDisplayString(autoFwhmPx) }));
  }, [defaultsLoaded, autoFwhmPx, pxToDisplayString, frameSetId, defaultFwhmUnit, plateScale, useArcsec]);

  // Count of frames in checked filter groups (for the action bar)
  const checkedFrameCount = useMemo(() => {
    if (checkedKeys.size === 0) return 0;
    let count = 0;
    for (const key of checkedKeys) {
      const keyFrames = framesByKey.get(key);
      if (keyFrames) count += keyFrames.length;
    }
    return count;
  }, [checkedKeys, framesByKey]);

  // Compute rejected frame IDs from thresholds
  const rejectedFrameIds = useMemo(() => {
    const rejected = new Set<number>();
    // When in arcsec mode, the user enters thresholds in arcsec — convert back to pixels for comparison
    const ps = (useArcsec && plateScale) ? plateScale : null;
    const rawFwhm = parseFloat(thresholds.fwhm);
    const fwhmThreshold = ps ? rawFwhm / ps : rawFwhm;
    const eccThreshold = parseFloat(thresholds.eccentricity);
    const snrDbThreshold = parseFloat(thresholds.frame_snr);
    const snrWtThreshold = parseFloat(thresholds.snr_weight);
    const rejectTrailed = thresholds.trail === 'true';

    for (const frame of displayedFrames) {
      const a = analysisData.get(frame.frame_id);
      if (!a) continue;

      // FWHM > threshold = rejected (worse)
      if (!isNaN(fwhmThreshold) && a.median_fwhm > fwhmThreshold) {
        rejected.add(frame.frame_id);
        continue;
      }
      // Eccentricity > threshold = rejected (worse)
      if (!isNaN(eccThreshold) && a.median_eccentricity > eccThreshold) {
        rejected.add(frame.frame_id);
        continue;
      }
      // Frame SNR < threshold = rejected (worse)
      if (!isNaN(snrDbThreshold) && a.frame_snr < snrDbThreshold) {
        rejected.add(frame.frame_id);
        continue;
      }
      // SNR Weight < threshold = rejected (worse)
      if (!isNaN(snrWtThreshold) && a.snr_weight < snrWtThreshold) {
        rejected.add(frame.frame_id);
        continue;
      }
      // Reject frames flagged as trailed
      if (rejectTrailed && a.possibly_trailed) {
        rejected.add(frame.frame_id);
        continue;
      }
    }

    return rejected;
  }, [thresholds, displayedFrames, analysisData, useArcsec, plateScale]);

  // Auto-select rejected frames when thresholds change (only if reject mode is active)
  useEffect(() => {
    if (rejectActive) {
      setSelectedFrameIds(rejectedFrameIds);
    }
  }, [rejectedFrameIds, rejectActive]);

  // Clear table selection when tree filter changes
  const handleCheckedChange = useCallback((keys: Set<string>) => {
    setCheckedKeys(keys);
    setSelectedFrameIds(new Set());
  }, []);

  const handleBlinkSelected = useCallback(() => {
    if (selectedFrameIds.size === 0) return;
    onBlink?.([...selectedFrameIds]);
  }, [selectedFrameIds, onBlink]);

  const handleBlackholeSelected = useCallback(async () => {
    if (selectedFrameIds.size === 0) return;

    const fileIds = allFrames
      .filter(f => selectedFrameIds.has(f.frame_id))
      .map(f => f.file_id);

    if (fileIds.length === 0) return;

    setBlackholing(true);
    try {
      for (const fileId of fileIds) {
        await api.invoke('move_to_black_hole', { fileId, fromWhere: 'frame_set_detail' });
      }
      setSelectedFrameIds(new Set());
    } catch (err) {
      console.error('Failed to blackhole selected frames:', err);
    } finally {
      setBlackholing(false);
    }
  }, [selectedFrameIds, allFrames]);

  const handleExportCsv = useCallback(() => {
    const headers = ['Filename', 'Date/Time', 'Camera', 'Filter', 'Exposure', 'Stars', 'FWHM (px)', 'Eccentricity', 'SNR', 'Frame SNR (dB)', 'PSF Signal (ADU)', 'SNR Weight', 'Trail R\u00B2'];
    const rows = displayedFrames.map(frame => {
      const a = analysisData.get(frame.frame_id);
      return [
        frame.filename,
        frame.date_obs ?? '',
        frame.camera ?? '',
        frame.filter ?? '',
        frame.exptime != null ? String(frame.exptime) : '',
        a ? String(a.stars_detected) : '',
        a ? a.median_fwhm.toFixed(2) : '',
        a ? a.median_eccentricity.toFixed(3) : '',
        a ? a.median_snr.toFixed(1) : '',
        a ? a.frame_snr.toFixed(1) : '',
        a ? a.psf_signal.toFixed(1) : '',
        a ? a.snr_weight.toFixed(1) : '',
        a ? a.trail_r_squared.toFixed(4) : '',
      ].map(v => `"${String(v).replace(/"/g, '""')}"`).join(',');
    });

    const csv = [headers.join(','), ...rows].join('\n');
    const blob = new Blob([csv], { type: 'text/csv;charset=utf-8;' });
    const url = URL.createObjectURL(blob);
    const now = new Date();
    const timestamp = `${now.getFullYear()}${String(now.getMonth() + 1).padStart(2, '0')}${String(now.getDate()).padStart(2, '0')}_${String(now.getHours()).padStart(2, '0')}${String(now.getMinutes()).padStart(2, '0')}${String(now.getSeconds()).padStart(2, '0')}`;
    const safeName = (frameSetName || 'analysis').replace(/[^a-zA-Z0-9_-]/g, '_');
    const a = document.createElement('a');
    a.href = url;
    a.download = `${timestamp}_${safeName}.csv`;
    a.click();
    URL.revokeObjectURL(url);

    if (csvTimerRef.current) clearTimeout(csvTimerRef.current);
    setCsvExportedMsg(`Exported ${rows.length} frames to CSV`);
    csvTimerRef.current = setTimeout(() => setCsvExportedMsg(null), 3000);
  }, [displayedFrames, analysisData]);

  const analyzedCount = analysisData.size;
  const totalLightFrames = allFrames.length;
  const allAnalyzed = analyzedCount >= totalLightFrames;
  const hasSelection = selectedFrameIds.size > 0;

  return (
    <div className="flex flex-col h-full">
      {/* Progress Bar */}
      {analyzing && analysisProgress && (
        <div className="mb-2 flex-shrink-0">
          <div className="flex items-center justify-between text-xs text-content-muted mb-1">
            <span>{analysisProgress.current_file}</span>
            <div className="flex items-center gap-2">
              <span>{analysisProgress.current} / {analysisProgress.total} ({analysisProgress.percent.toFixed(0)}%)</span>
              <button
                onClick={() => cancelAnalysis(frameSetId)}
                className="text-content-muted hover:text-error transition-colors"
                title="Cancel analysis"
              >
                <X size={14} />
              </button>
            </div>
          </div>
          <div className="w-full bg-surface-hover rounded-full h-2">
            <div
              className="bg-accent h-2 rounded-full transition-all duration-300"
              style={{ width: `${analysisProgress.percent}%` }}
            />
          </div>
        </div>
      )}

      {/* Main Content — two-panel layout */}
      <div className="flex flex-1 min-h-0 gap-4">
        {/* Left panel — Tree with view toggle */}
        <div className="w-80 flex-shrink-0 flex flex-col">
          {/* View Mode Toggle */}
          <div className="flex mb-1 rounded-lg border border-border/50 overflow-hidden">
            <button
              onClick={() => handleViewModeChange('by-night')}
              className={`flex-1 flex items-center justify-center gap-1.5 px-2 py-1 text-xs font-medium transition-colors ${
                viewMode === 'by-night'
                  ? 'bg-accent/20 text-accent'
                  : 'bg-surface-elevated/50 text-content-muted hover:text-content-secondary'
              }`}
            >
              <Calendar size={12} />
              By Night
            </button>
            <button
              onClick={() => handleViewModeChange('by-camera')}
              className={`flex-1 flex items-center justify-center gap-1.5 px-2 py-1 text-xs font-medium transition-colors ${
                viewMode === 'by-camera'
                  ? 'bg-accent/20 text-accent'
                  : 'bg-surface-elevated/50 text-content-muted hover:text-content-secondary'
              }`}
            >
              <Camera size={12} />
              By Camera
            </button>
          </div>

          {/* Tree */}
          {(() => {
            const treeFooter = checkedKeys.size > 0 && (onSplit || onCreateCustomSet) ? (
              <>
                <div className="flex items-center gap-2 flex-wrap">
                  {onSplit && (
                    <button
                      onClick={handleSplit}
                      className="inline-flex items-center gap-1.5 px-3 py-1.5 bg-accent hover:bg-accent-hover text-white text-sm rounded transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-accent"
                    >
                      <Scissors size={14} aria-hidden="true" />
                      Split
                    </button>
                  )}
                  {onCreateCustomSet && (
                    <button
                      onClick={handleCreateCustomSet}
                      className="inline-flex items-center gap-1.5 px-3 py-1.5 bg-success hover:brightness-90 text-white text-sm rounded transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-success"
                    >
                      <Plus size={14} aria-hidden="true" />
                      Create Set
                    </button>
                  )}
                </div>
              </>
            ) : undefined;

            const label = checkedKeys.size > 0
              ? `${checkedKeys.size} group${checkedKeys.size !== 1 ? 's' : ''} · ${checkedFrameCount} frame${checkedFrameCount !== 1 ? 's' : ''}`
              : undefined;

            return viewMode === 'by-night' ? (
              <CameraFilterTree
                nodes={dateTree.nodes}
                checkedKeys={checkedKeys}
                onCheckedChange={handleCheckedChange}
                className="flex-1 min-h-0"
                checkedLabel={label}
                filterSnrMap={filterSnrMap}
                footer={treeFooter}
              />
            ) : (
              <MergedCameraFilterTree
                nodes={mergedTree.nodes}
                checkedKeys={checkedKeys}
                onCheckedChange={handleCheckedChange}
                className="flex-1 min-h-0"
                checkedLabel={label}
                filterSnrMap={filterSnrMap}
                footer={treeFooter}
              />
            );
          })()}
        </div>

        {/* Right panel */}
        <div className="flex-1 min-w-0 flex flex-col gap-2">
          {/* Unified toolbar */}
          <div className="flex items-start gap-2 flex-wrap flex-shrink-0">
            {/* Analyze button */}
            {!allAnalyzed ? (
              <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
                <button
                  onClick={() => handleAnalyzeAll(false)}
                  disabled={analyzing}
                  className="w-20 h-7 inline-flex items-center justify-center gap-1.5 text-xs font-medium bg-accent hover:bg-accent-hover disabled:bg-surface-hover disabled:cursor-not-allowed text-white rounded-lg transition-colors"
                >
                  <BarChart3 size={12} />
                  {analyzing ? 'Analyzing...' : 'Analyze'}
                </button>
                <span className="text-[10px] text-content-muted leading-tight">
                  {analyzedCount > 0 ? `${analyzedCount}/${totalLightFrames}` : 'Analyze'}
                </span>
              </div>
            ) : (
              <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
                <button
                  onClick={() => handleAnalyzeAll(true)}
                  disabled={analyzing}
                  className="w-20 h-7 inline-flex items-center justify-center text-xs font-medium bg-success/20 hover:bg-success/30 text-success rounded-lg border border-success/30 transition-colors disabled:opacity-30 disabled:cursor-default"
                  title="Re-analyze all frames"
                >
                  <BarChart3 size={12} />
                </button>
                <span className="text-[10px] text-content-muted leading-tight">Re-Analyze</span>
              </div>
            )}

            <span className="text-border h-7 flex items-center">|</span>

            {/* Units toggle */}
            {plateScale && (
              <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
                <button
                  onClick={toggleUnits}
                  className={`w-10 h-7 text-xs font-medium rounded-lg border transition-colors ${
                    useArcsec
                      ? 'bg-accent/20 border-accent text-accent'
                      : 'bg-surface-hover border-border text-content-secondary hover:text-content'
                  }`}
                  title="Toggle units between pixels and arcseconds"
                >
                  {useArcsec ? '"' : 'px'}
                </button>
                <span className="text-[10px] text-content-muted leading-tight">Units</span>
              </div>
            )}

            {/* Selection: clear + invert split button */}
            <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
              <div className="flex h-7">
                <button
                  onClick={() => setSelectedFrameIds(new Set())}
                  disabled={!hasSelection}
                  className="w-10 h-7 inline-flex items-center justify-center text-xs font-medium bg-surface-hover hover:bg-surface-elevated text-content-secondary rounded-l-lg border border-r-0 border-border transition-colors disabled:opacity-30 disabled:cursor-default"
                  title="Clear selection"
                >
                  {hasSelection ? selectedFrameIds.size : 0}
                </button>
                <button
                  onClick={() => {
                    const all = new Set(displayedFrames.map(f => f.frame_id));
                    const inverted = new Set<number>();
                    for (const id of all) {
                      if (!selectedFrameIds.has(id)) inverted.add(id);
                    }
                    setSelectedFrameIds(inverted);
                  }}
                  className="w-7 h-7 inline-flex items-center justify-center text-xs font-medium bg-surface-hover hover:bg-surface-elevated text-content-secondary rounded-r-lg border border-border transition-colors"
                  title="Invert selection"
                >
                  <ArrowLeftRight size={10} />
                </button>
              </div>
              <span className="text-[10px] text-content-muted leading-tight">Selection</span>
            </div>
            <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
              <button
                onClick={handleBlinkSelected}
                disabled={!hasSelection}
                className="w-10 h-7 inline-flex items-center justify-center text-xs font-medium bg-cyan-600 hover:bg-cyan-700 text-white rounded-lg transition-colors disabled:opacity-30 disabled:cursor-default"
                title="Blink selected frames"
              >
                <Play size={12} />
              </button>
              <span className="text-[10px] text-content-muted leading-tight">Blink</span>
            </div>
            <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
              <button
                onClick={handleBlackholeSelected}
                disabled={!hasSelection || blackholing}
                className="w-10 h-7 inline-flex items-center justify-center text-xs font-medium bg-error hover:brightness-90 text-white rounded-lg transition-colors disabled:opacity-30 disabled:cursor-default"
                title="Blackhole selected frames"
              >
                <Trash2 size={12} />
              </button>
              <span className="text-[10px] text-content-muted leading-tight">Blackhole</span>
            </div>
            <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
              <button
                onClick={() => {
                  if (selectedFrameIds.size === 0) return;
                  plateSolveRef.current?.start([...selectedFrameIds]);
                }}
                disabled={!hasSelection}
                className="w-10 h-7 inline-flex items-center justify-center text-xs font-medium bg-accent hover:bg-accent-hover text-white rounded-lg transition-colors disabled:opacity-30 disabled:cursor-default"
                title={
                  hasSelection
                    ? `Plate solve ${selectedFrameIds.size} selected frame${selectedFrameIds.size === 1 ? '' : 's'} (updates WCS)`
                    : 'Select frames to plate solve'
                }
              >
                <Crosshair size={12} />
              </button>
              <span className="text-[10px] text-content-muted leading-tight">Plate Solve</span>
            </div>

            <span className="text-border h-7 flex items-center">|</span>

            {/* Rejection thresholds — inline */}
            <RejectionThresholdBar
              thresholds={thresholds}
              onChange={setThresholds}
              onClear={handleClearThresholds}
              onLoadDefaults={handleLoadDefaults}
              hasDefaults={defaultThresholds !== null}
              useArcsec={useArcsec && !!plateScale}
              rejectActive={rejectActive}
              onToggleReject={() => setRejectActive(prev => !prev)}
            />

            <span className="text-border h-7 flex items-center">|</span>

            {/* Export / Charts */}
            <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
              <button
                onClick={handleExportCsv}
                disabled={analysisData.size === 0}
                className="w-10 h-7 inline-flex items-center justify-center text-xs font-medium bg-surface-hover hover:bg-surface-elevated text-content-secondary rounded-lg border border-border transition-colors disabled:opacity-30 disabled:cursor-default"
                title="Export CSV"
              >
                <Download size={12} />
              </button>
              <span className="text-[10px] text-content-muted leading-tight">CSV</span>
            </div>
            <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
              <div className="flex h-7">
                <button
                  onClick={() => setFrameViewMode('table')}
                  className={`w-10 h-7 inline-flex items-center justify-center text-xs font-medium rounded-l-lg border border-r-0 transition-colors ${
                    frameViewMode === 'table'
                      ? 'bg-accent/20 text-accent border-accent/40'
                      : 'bg-surface-hover hover:bg-surface-elevated text-content-secondary border-border'
                  }`}
                  title="Table view"
                >
                  <TableIcon size={12} />
                </button>
                <button
                  onClick={() => setFrameViewMode('chart')}
                  className={`w-10 h-7 inline-flex items-center justify-center text-xs font-medium rounded-r-lg border transition-colors ${
                    frameViewMode === 'chart'
                      ? 'bg-accent/20 text-accent border-accent/40'
                      : 'bg-surface-hover hover:bg-surface-elevated text-content-secondary border-border'
                  }`}
                  title="Chart view"
                >
                  <LineChart size={12} />
                </button>
              </div>
              <span className="text-[10px] text-content-muted leading-tight">View</span>
            </div>
          </div>

          <PlateSolveBatchPanel
            ref={plateSolveRef}
            hideTriggerButtons
            onSolveComplete={() => onRefresh?.()}
          />

          {csvExportedMsg && (
            <div className="inline-flex items-center gap-1 text-xs text-green-400 flex-shrink-0">
              <Check size={14} />
              {csvExportedMsg}
            </div>
          )}

          <div className="flex-1 min-h-0 overflow-hidden">
            {frameViewMode === 'table' ? (
              <div className="h-full overflow-y-auto border border-border rounded-xl">
                <LightsAnalysisTable
                  frames={displayedFrames}
                  selectedFrameIds={selectedFrameIds}
                  onSelectionChange={setSelectedFrameIds}
                  analysisData={analysisData}
                  rejectedFrameIds={rejectedFrameIds}
                  plateScale={useArcsec ? plateScale : null}
                  hideLocateColumn={hideLocateColumn}
                  referenceFrameId={effectiveReferenceFrameId}
                  onSetReference={onReferenceChanged !== undefined ? handleSetReference : undefined}
                  settingReference={settingReference}
                  readinessByFrameId={readinessByFrameId}
                  detailsByFrameId={detailsByFrameId}
                />
              </div>
            ) : (
              <LightsAnalysisChartView
                displayedFrames={displayedFrames}
                analysisData={analysisData}
                selectedFrameIds={selectedFrameIds}
                onSelectionChange={setSelectedFrameIds}
                rejectedFrameIds={rejectedFrameIds}
                plateScale={plateScale}
                useArcsec={useArcsec}
                thresholds={thresholds}
                onAnalyze={() => handleAnalyzeAll(false)}
                analyzing={analyzing}
              />
            )}
          </div>

          <BlackholedFramesSection frames={blackholedFrames} analysisData={analysisData} onBlink={onBlink} />
        </div>
      </div>
    </div>
  );
}
