import React, { useState, useEffect } from 'react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';
import {
  X,
  Camera,
  Thermometer,
  Clock,
  Aperture,
  Check,
  Eye,
  EyeOff,
  Calendar,
  Hash,
  AlertTriangle,
  CheckCircle,
  RotateCcw,
  Loader2,
} from 'lucide-react';
import type {
  LightFrameParameters,
  CalibrationSetWithScore,
} from '../types/models';

/**
 * What the user did to one calibration slot before pressing Apply.
 *
 * - `number` — a set was picked (or swapped) → assign it.
 * - `null`   — the slot was not touched → the parent must do nothing.
 * - `'clear'` — the slot was explicitly deselected → the parent must clear
 *   the link for that type. Without this third state a deselect is
 *   indistinguishable from "untouched" and silently does nothing.
 */
export type ManualPick = number | null | 'clear';

/** Map a slot's selection against the backend truth onto a {@link ManualPick}. */
const pick = (selected: number | null, current: number | null): ManualPick => {
  if (selected === current) return null; // untouched
  if (selected === null) return 'clear'; // user explicitly deselected
  return selected;
};

interface ManualCalibrationModalProps {
  isOpen: boolean;
  frameIds: number[];
  filterDisplay: string;
  currentFlatSetId?: number | null;
  currentDarkSetId?: number | null;
  currentBiasSetId?: number | null;
  useBiasForDarkOptimization: boolean;
  onApply: (flatSetId: ManualPick, darkSetId: ManualPick, biasSetId: ManualPick) => void;
  onClose: () => void;
  /** Reload the hierarchy — same refresh path the save (onApply) flow uses. */
  onRefresh?: () => void;
}

type TabType = 'flat' | 'dark' | 'bias';

export const ManualCalibrationModal: React.FC<ManualCalibrationModalProps> = ({
  isOpen,
  frameIds,
  filterDisplay,
  currentFlatSetId,
  currentDarkSetId,
  currentBiasSetId,
  useBiasForDarkOptimization,
  onApply,
  onClose,
  onRefresh,
}) => {
  const { notify } = useNotifications();
  const [activeTab, setActiveTab] = useState<TabType>('flat');
  const [lightParams, setLightParams] = useState<LightFrameParameters | null>(null);
  const [flatSets, setFlatSets] = useState<CalibrationSetWithScore[]>([]);
  const [darkSets, setDarkSets] = useState<CalibrationSetWithScore[]>([]);
  const [biasSets, setBiasSets] = useState<CalibrationSetWithScore[]>([]);
  const [showAll, setShowAll] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [resetting, setResetting] = useState(false);

  // Date range filter (only used when showAll is true)
  const [filterDateFrom, setFilterDateFrom] = useState<string>('');
  const [filterDateTo, setFilterDateTo] = useState<string>('');

  // Selections
  const [selectedFlatId, setSelectedFlatId] = useState<number | null>(currentFlatSetId ?? null);
  const [selectedDarkId, setSelectedDarkId] = useState<number | null>(currentDarkSetId ?? null);
  const [selectedBiasId, setSelectedBiasId] = useState<number | null>(currentBiasSetId ?? null);

  // Load data when modal opens
  useEffect(() => {
    if (isOpen && frameIds.length > 0) {
      loadData();
    }
  }, [isOpen, frameIds, showAll]);

  // Reset selections when data loads from backend (use backend data as source of truth)
  useEffect(() => {
    if (isOpen && lightParams) {
      // Use backend data as source of truth for current selections
      setSelectedFlatId(lightParams.current_flat_set_id ?? null);
      setSelectedDarkId(lightParams.current_dark_set_id ?? null);
      setSelectedBiasId(lightParams.current_bias_set_id ?? null);
    }
  }, [isOpen, lightParams]);

  const loadData = async () => {
    setLoading(true);
    setError(null);

    try {
      // Load light frame parameters
      const params = await api.invoke<LightFrameParameters>('get_light_frame_parameters', {
        frameIds,
      });
      setLightParams(params);

      // Load calibration sets for each type
      const [flats, darks, biases] = await Promise.all([
        api.invoke<CalibrationSetWithScore[]>('get_calibration_sets_for_manual_selection', {
          frameIds,
          calibrationType: 'flat',
          showAll,
        }),
        api.invoke<CalibrationSetWithScore[]>('get_calibration_sets_for_manual_selection', {
          frameIds,
          calibrationType: 'dark',
          showAll,
        }),
        useBiasForDarkOptimization
          ? api.invoke<CalibrationSetWithScore[]>('get_calibration_sets_for_manual_selection', {
              frameIds,
              calibrationType: 'bias',
              showAll,
            })
          : Promise.resolve([]),
      ]);

      setFlatSets(flats);
      setDarkSets(darks);
      setBiasSets(biases);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  // Use backend data as source of truth for detecting changes. Normalized to
  // `null` so an omitted prop can never read as a change against a `null`
  // selection (the selections themselves are always `number | null`).
  const currentFlatFromBackend = lightParams?.current_flat_set_id ?? currentFlatSetId ?? null;
  const currentDarkFromBackend = lightParams?.current_dark_set_id ?? currentDarkSetId ?? null;
  const currentBiasFromBackend = lightParams?.current_bias_set_id ?? currentBiasSetId ?? null;

  // Only forward slots the user actually changed: untouched slots come back as
  // `null` (parent skips them), an explicit deselect comes back as `'clear'`
  // (parent clears that type's link).
  const handleApply = () => {
    onApply(
      pick(selectedFlatId, currentFlatFromBackend),
      pick(selectedDarkId, currentDarkFromBackend),
      pick(selectedBiasId, currentBiasFromBackend),
    );
  };

  const hasChanges =
    selectedFlatId !== currentFlatFromBackend ||
    selectedDarkId !== currentDarkFromBackend ||
    selectedBiasId !== currentBiasFromBackend;

  // Undo for manual_assign_calibration: clears every is_manual_override link
  // for these frames (all types) so auto-find is free to reassign them.
  const handleResetToAutomatic = async () => {
    if (frameIds.length === 0 || resetting) return;
    setResetting(true);
    try {
      const count = await api.invoke<number>('clear_manual_calibration_override', {
        frameIds,
        calibrationType: null,
      });
      onClose();
      onRefresh?.();
      notify({
        title: 'Calibration reset to automatic',
        detail: count === 1
          ? '1 manual override cleared — auto-find can now reassign it.'
          : `${count} manual overrides cleared — auto-find can now reassign them.`,
        kind: 'generic',
        tone: 'success',
      });
    } catch (e) {
      console.error('Failed to clear manual calibration override:', e);
      notify({
        title: 'Failed to reset calibration',
        detail: e instanceof Error ? e.message : String(e),
        kind: 'generic',
        tone: 'warning',
        hasErrors: true,
      });
    } finally {
      setResetting(false);
    }
  };

  if (!isOpen) return null;

  const formatTemp = (temp: number | null | undefined) => {
    if (temp === null || temp === undefined) return 'N/A';
    return `${temp.toFixed(1)}°C`;
  };

  const pad = (n: number) => String(n).padStart(2, '0');
  const fmtDt = (iso: string) => {
    const d = new Date(iso);
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  };
  const formatDateRange = (range: [string, string] | null | undefined) => {
    if (!range) return 'N/A';
    const s = new Date(range[0]);
    const e = new Date(range[1]);
    if (s.toDateString() === e.toDateString()) {
      return `${fmtDt(range[0])} – ${pad(e.getHours())}:${pad(e.getMinutes())}:${pad(e.getSeconds())}`;
    }
    return `${fmtDt(range[0])} – ${fmtDt(range[1])}`;
  };

  const formatMatchScore = (score: number) => {
    const percent = Math.round(score * 100);
    if (percent >= 80) return { label: 'Excellent', color: 'text-success' };
    if (percent >= 60) return { label: 'Good', color: 'text-accent' };
    if (percent >= 40) return { label: 'Fair', color: 'text-warning' };
    return { label: 'Poor', color: 'text-orange' };
  };

  // Calibration set row component
  const CalibrationSetRow = ({
    setWithScore,
    isSelected,
    isCurrent,
    onSelect,
    type,
  }: {
    setWithScore: CalibrationSetWithScore;
    isSelected: boolean;
    isCurrent: boolean;
    onSelect: () => void;
    type: TabType;
  }) => {
    const { set, match_score, match_details } = setWithScore;
    const scoreInfo = formatMatchScore(match_score);

    const fmtDateJsx = (iso: string) => {
      const d = new Date(iso);
      return <>{d.getFullYear()}-{pad(d.getMonth() + 1)}-<span className="font-bold">{pad(d.getDate())}</span> {pad(d.getHours())}:{pad(d.getMinutes())}:{pad(d.getSeconds())}</>;
    };
    const dateRange = (() => {
      if (!set.date_start) return <>{set.date_display}</>;
      if (!set.date_end || set.date_start === set.date_end) return fmtDateJsx(set.date_start);
      const s = new Date(set.date_start);
      const e = new Date(set.date_end);
      if (s.toDateString() === e.toDateString()) {
        return <>{fmtDateJsx(set.date_start)} – {pad(e.getHours())}:{pad(e.getMinutes())}:{pad(e.getSeconds())}</>;
      }
      return <>{fmtDateJsx(set.date_start)} – {fmtDateJsx(set.date_end)}</>;
    })();

    // Determine if cal set is older or younger than light frames
    const calDateLabel = (() => {
      if (!lightParams?.date_range || !set.date_start) return '';
      const calDate = new Date(set.date_start).getTime();
      const lightMid = (new Date(lightParams.date_range[0]).getTime() + new Date(lightParams.date_range[1]).getTime()) / 2;
      return calDate < lightMid ? 'old' : 'young';
    })();

    // B4: warn when a flat set spans an unusually wide time window. Sky flats
    // shot at dusk and dawn ~10-12 hours apart can merge into a single group
    // when the user widens `time_cluster_minutes`, hiding the fact that two
    // physically different atmospheric conditions are being averaged. The
    // chip surfaces this without changing scoring or selection.
    const longSpanHours = (() => {
      if (type !== 'flat' || !set.date_start || !set.date_end) return 0;
      const s = new Date(set.date_start).getTime();
      const e = new Date(set.date_end).getTime();
      if (!Number.isFinite(s) || !Number.isFinite(e) || e <= s) return 0;
      return (e - s) / 3_600_000;
    })();
    const isLongSpan = longSpanHours > 6;

    return (
      <div
        onClick={onSelect}
        className={`p-3 rounded-lg border-2 cursor-pointer transition-all ${
          isSelected
            ? 'border-accent bg-accent-muted/20'
            : 'border-border hover:border-border bg-surface-elevated/30'
        }`}
      >
        {/* Top row: date range (prominent) + score */}
        <div className="flex items-start justify-between mb-1.5">
          <div className="flex items-center gap-1.5 min-w-0">
            <Calendar className="w-3.5 h-3.5 text-content-muted flex-shrink-0" />
            <span className={`text-sm truncate ${match_details.date_diff_days > 30 ? 'text-warning' : 'text-content'}`}>
              {dateRange}
            </span>
            {match_details.date_diff_days > 0 && (
              <span className={`text-xs font-medium flex-shrink-0 ${match_details.date_diff_days > 30 ? 'text-warning' : 'text-content-muted'}`}>
                {match_details.date_diff_days}d {calDateLabel}
              </span>
            )}
          </div>
          <div className={`text-xs font-medium flex-shrink-0 ml-2 ${scoreInfo.color}`}>
            {Math.round(match_score * 100)}%
          </div>
        </div>

        {/* Second row: filter + camera + badges */}
        <div className="flex items-center gap-2 mb-1.5 flex-wrap">
          {isSelected && <Check className="w-3.5 h-3.5 text-accent flex-shrink-0" />}
          {type === 'flat' && set.filter && (
            <span className={`text-sm font-semibold ${match_details.filter_match ? 'text-accent' : 'text-warning'}`}>
              {set.filter}
            </span>
          )}
          <span className={`text-sm ${match_details.instrume_match ? 'text-content-secondary' : 'text-warning'}`}>
            {set.instrume || 'Unknown'}
          </span>
          {set.exptime !== null && (
            <span className="text-sm text-content-secondary">{set.exptime}s</span>
          )}
          {isCurrent && (
            <span className="text-[10px] px-1.5 py-px rounded bg-surface-hover text-content-secondary">Current</span>
          )}
          {set.is_master && (
            <span className="text-[10px] px-1.5 py-px rounded bg-warning/20 text-warning">Master</span>
          )}
          {isLongSpan && (
            <span
              className="text-[10px] px-1.5 py-px rounded bg-warning/20 text-warning"
              title={`This flat group spans ${longSpanHours.toFixed(1)} hours — verify it isn't merging dawn and dusk frames captured under different atmospheric conditions.`}
            >
              Long span ({longSpanHours.toFixed(0)}h)
            </span>
          )}
        </div>

        {/* Third row: secondary params */}
        <div className="flex items-center gap-3 text-xs text-content-muted">
          <span>{set.frame_count} frames</span>
          <span className={match_details.binning_match ? '' : 'text-warning'}>{set.binning || '—'}</span>
          {set.gain !== null && (
            <span className={match_details.gain_match ? '' : 'text-warning'}>G{set.gain}</span>
          )}
          <span>
            {formatTemp(set.ccd_temp)}
            {match_details.temp_diff !== null && match_details.temp_diff > 2 && (
              <span className="text-warning"> Δ{match_details.temp_diff.toFixed(1)}°</span>
            )}
          </span>
        </div>
      </div>
    );
  };

  // Get current sets for active tab
  const getCurrentSets = () => {
    switch (activeTab) {
      case 'flat':
        return flatSets;
      case 'dark':
        return darkSets;
      case 'bias':
        return biasSets;
    }
  };

  const getSelectedId = () => {
    switch (activeTab) {
      case 'flat':
        return selectedFlatId;
      case 'dark':
        return selectedDarkId;
      case 'bias':
        return selectedBiasId;
    }
  };

  const getCurrentId = () => {
    // Use backend data as source of truth
    switch (activeTab) {
      case 'flat':
        return lightParams?.current_flat_set_id ?? currentFlatSetId;
      case 'dark':
        return lightParams?.current_dark_set_id ?? currentDarkSetId;
      case 'bias':
        return lightParams?.current_bias_set_id ?? currentBiasSetId;
    }
  };

  const handleSelect = (setId: number) => {
    switch (activeTab) {
      case 'flat':
        setSelectedFlatId(selectedFlatId === setId ? null : setId);
        break;
      case 'dark':
        setSelectedDarkId(selectedDarkId === setId ? null : setId);
        break;
      case 'bias':
        setSelectedBiasId(selectedBiasId === setId ? null : setId);
        break;
    }
  };

  const rawSets = getCurrentSets();
  const selectedId = getSelectedId();
  const currentId = getCurrentId();

  // Apply date filter when showAll is enabled
  const filterSetsByDate = (sets: CalibrationSetWithScore[]) => {
    if (!showAll || (!filterDateFrom && !filterDateTo)) return sets;
    return sets.filter(s => {
      if (!s.set.date_start) return true;
      const setDate = new Date(s.set.date_start);
      if (filterDateFrom && setDate < new Date(filterDateFrom)) return false;
      if (filterDateTo && setDate > new Date(filterDateTo + 'T23:59:59')) return false;
      return true;
    });
  };

  const sets = filterSetsByDate(rawSets);
  const isFiltered = showAll && (filterDateFrom || filterDateTo) && sets.length !== rawSets.length;

  return (
    <div className="fixed inset-0 bg-black bg-opacity-60 flex items-center justify-center z-50 overflow-y-auto">
      <div className="bg-surface-elevated rounded-lg w-full max-w-5xl mx-4 my-8 border border-border shadow-xl flex flex-col max-h-[90vh]">
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-border">
          <div>
            <h2 className="text-xl font-semibold text-content">Manual Calibration Selection</h2>
            <p className="text-sm text-content-muted mt-1">
              Select calibration sets for {frameIds.length} {filterDisplay} frame
              {frameIds.length !== 1 ? 's' : ''}
            </p>
          </div>
          <button
            onClick={onClose}
            className="p-2 hover:bg-surface-hover rounded-lg transition-colors"
          >
            <X className="w-5 h-5 text-content-muted" />
          </button>
        </div>

        {/* Content */}
        <div className="flex flex-1 overflow-hidden">
          {/* Left Panel - Light Frame Parameters */}
          <div className="w-72 border-r border-border p-4 bg-surface overflow-y-auto">
            <h3 className="font-medium text-content mb-4">Light Frame Parameters</h3>

            {loading && !lightParams ? (
              <div className="text-content-muted text-sm">Loading...</div>
            ) : error ? (
              <div className="text-error text-sm">{error}</div>
            ) : lightParams ? (
              <div className="space-y-3 text-sm">
                <div className="flex items-center gap-2">
                  <Calendar className="w-4 h-4 text-content-muted" />
                  <span className="text-content-muted">Dates:</span>
                </div>
                <div className="text-content text-sm ml-6">
                  {formatDateRange(lightParams.date_range)}
                </div>
                <div className="flex items-center gap-2">
                  <Hash className="w-4 h-4 text-content-muted" />
                  <span className="text-content-muted">Frames:</span>
                  <span className="text-content">{lightParams.frame_count}</span>
                </div>
                <div className="flex items-center gap-2">
                  <Camera className="w-4 h-4 text-content-muted" />
                  <span className="text-content-muted">Camera:</span>
                  <span className="text-content">{lightParams.instrume || 'Unknown'}</span>
                </div>
                <div className="flex items-center gap-2">
                  <Aperture className="w-4 h-4 text-content-muted" />
                  <span className="text-content-muted">Filter:</span>
                  <span className="text-content">{lightParams.filter || 'No Filter'}</span>
                </div>
                <div className="flex items-center gap-2">
                  <Hash className="w-4 h-4 text-content-muted" />
                  <span className="text-content-muted">Binning:</span>
                  <span className="text-content">{lightParams.binning || 'N/A'}</span>
                </div>
                {lightParams.gain !== null && (
                  <div className="flex items-center gap-2">
                    <span className="text-content-muted ml-6">Gain:</span>
                    <span className="text-content">{lightParams.gain}</span>
                  </div>
                )}
                {lightParams.offset !== null && (
                  <div className="flex items-center gap-2">
                    <span className="text-content-muted ml-6">Offset:</span>
                    <span className="text-content">{lightParams.offset}</span>
                  </div>
                )}
                <div className="flex items-center gap-2">
                  <Thermometer className="w-4 h-4 text-content-muted" />
                  <span className="text-content-muted">Avg Temp:</span>
                  <span className="text-content">{formatTemp(lightParams.avg_ccd_temp)}</span>
                </div>
                <div className="flex items-center gap-2">
                  <Clock className="w-4 h-4 text-content-muted" />
                  <span className="text-content-muted">Avg Exp:</span>
                  <span className="text-content">
                    {lightParams.avg_exptime?.toFixed(1) ?? 'N/A'}s
                  </span>
                </div>

                {/* Current Selections - use backend data as source of truth */}
                <div className="pt-4 border-t border-border mt-4">
                  <h4 className="text-content-muted text-xs uppercase mb-2">Current Links</h4>
                  <div className="space-y-1 text-xs">
                    <div className="flex items-center gap-2">
                      {currentFlatFromBackend ? (
                        <CheckCircle className="w-3 h-3 text-success" />
                      ) : (
                        <AlertTriangle className="w-3 h-3 text-orange" />
                      )}
                      <span className="text-content-muted">Flat:</span>
                      <span className="text-content">
                        {currentFlatFromBackend ? `Set #${currentFlatFromBackend}` : 'None'}
                      </span>
                    </div>
                    <div className="flex items-center gap-2">
                      {currentDarkFromBackend ? (
                        <CheckCircle className="w-3 h-3 text-success" />
                      ) : (
                        <AlertTriangle className="w-3 h-3 text-orange" />
                      )}
                      <span className="text-content-muted">Dark:</span>
                      <span className="text-content">
                        {currentDarkFromBackend ? `Set #${currentDarkFromBackend}` : 'None'}
                      </span>
                    </div>
                    {useBiasForDarkOptimization && (
                      <div className="flex items-center gap-2">
                        {currentBiasFromBackend ? (
                          <CheckCircle className="w-3 h-3 text-success" />
                        ) : (
                          <AlertTriangle className="w-3 h-3 text-orange" />
                        )}
                        <span className="text-content-muted">Bias:</span>
                        <span className="text-content">
                          {currentBiasFromBackend ? `Set #${currentBiasFromBackend}` : 'None'}
                        </span>
                      </div>
                    )}
                  </div>
                </div>
              </div>
            ) : null}
          </div>

          {/* Right Panel - Calibration Sets */}
          <div className="flex-1 flex flex-col overflow-hidden">
            {/* Tabs */}
            <div className="flex border-b border-border">
              <button
                onClick={() => setActiveTab('flat')}
                className={`px-6 py-3 font-medium text-sm transition-colors ${
                  activeTab === 'flat'
                    ? 'text-accent border-b-2 border-accent bg-surface-elevated'
                    : 'text-content-muted hover:text-content'
                }`}
              >
                Flats
                {flatSets.length > 0 && (
                  <span className="ml-2 px-1.5 py-0.5 text-xs rounded bg-surface-hover">
                    {flatSets.length}
                  </span>
                )}
              </button>
              <button
                onClick={() => setActiveTab('dark')}
                className={`px-6 py-3 font-medium text-sm transition-colors ${
                  activeTab === 'dark'
                    ? 'text-purple border-b-2 border-purple bg-surface-elevated'
                    : 'text-content-muted hover:text-content'
                }`}
              >
                Darks
                {darkSets.length > 0 && (
                  <span className="ml-2 px-1.5 py-0.5 text-xs rounded bg-surface-hover">
                    {darkSets.length}
                  </span>
                )}
              </button>
              {useBiasForDarkOptimization && (
                <button
                  onClick={() => setActiveTab('bias')}
                  className={`px-6 py-3 font-medium text-sm transition-colors ${
                    activeTab === 'bias'
                      ? 'text-success border-b-2 border-success bg-surface-elevated'
                      : 'text-content-muted hover:text-content'
                  }`}
                >
                  Bias
                  {biasSets.length > 0 && (
                    <span className="ml-2 px-1.5 py-0.5 text-xs rounded bg-surface-hover">
                      {biasSets.length}
                    </span>
                  )}
                </button>
              )}

              {/* Show All Toggle and Date Filter */}
              <div className="ml-auto flex items-center gap-3 px-4">
                {/* Date filter (only when showAll is enabled) */}
                {showAll && (
                  <div className="flex items-center gap-2 text-sm">
                    <span className="text-content-muted">From:</span>
                    <input
                      type="date"
                      value={filterDateFrom}
                      onChange={(e) => setFilterDateFrom(e.target.value)}
                      className="bg-surface-hover text-content rounded px-2 py-1 text-xs border border-border focus:border-accent focus:outline-none"
                    />
                    <span className="text-content-muted">To:</span>
                    <input
                      type="date"
                      value={filterDateTo}
                      onChange={(e) => setFilterDateTo(e.target.value)}
                      className="bg-surface-hover text-content rounded px-2 py-1 text-xs border border-border focus:border-accent focus:outline-none"
                    />
                    {(filterDateFrom || filterDateTo) && (
                      <button
                        onClick={() => { setFilterDateFrom(''); setFilterDateTo(''); }}
                        className="text-content-muted hover:text-content text-xs px-1"
                        title="Clear date filter"
                      >
                        Clear
                      </button>
                    )}
                  </div>
                )}
                <button
                  onClick={() => setShowAll(!showAll)}
                  className={`flex items-center gap-2 px-3 py-1.5 rounded text-sm transition-colors ${
                    showAll
                      ? 'bg-info-muted text-info'
                      : 'bg-surface-hover text-content-muted hover:text-content'
                  }`}
                >
                  {showAll ? <Eye className="w-4 h-4" /> : <EyeOff className="w-4 h-4" />}
                  {showAll ? 'Showing All' : 'Show All'}
                </button>
              </div>
            </div>

            {/* Sets List */}
            <div className="flex-1 overflow-y-auto p-4">
              {loading ? (
                <div className="flex items-center justify-center h-full text-content-muted">
                  Loading calibration sets...
                </div>
              ) : error ? (
                <div className="flex items-center justify-center h-full text-error">
                  {error}
                </div>
              ) : sets.length === 0 ? (
                <div className="flex flex-col items-center justify-center h-full text-content-muted">
                  <p>No {activeTab} calibration sets found.</p>
                  {!showAll && (
                    <p className="text-sm mt-2">
                      Try enabling "Show All" to see incompatible sets.
                    </p>
                  )}
                </div>
              ) : (
                <div className="space-y-3">
                  {/* Show filter count when filtering is active */}
                  {isFiltered && (
                    <div className="text-xs text-content-muted mb-2">
                      Showing {sets.length} of {rawSets.length} sets
                    </div>
                  )}
                  {sets.map((setWithScore) => (
                    <CalibrationSetRow
                      key={setWithScore.set.id}
                      setWithScore={setWithScore}
                      isSelected={selectedId === setWithScore.set.id}
                      isCurrent={currentId === setWithScore.set.id}
                      onSelect={() => setWithScore.set.id && handleSelect(setWithScore.set.id)}
                      type={activeTab}
                    />
                  ))}
                </div>
              )}
            </div>
          </div>
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between px-6 py-4 border-t border-border bg-surface">
          <div className="flex items-center gap-4">
            <button
              onClick={handleResetToAutomatic}
              disabled={frameIds.length === 0 || resetting}
              title="Clear manual overrides for these frames so auto-find can reassign calibration"
              className="flex items-center gap-1.5 px-3 py-1.5 text-sm text-error hover:bg-error-muted rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              {resetting ? (
                <Loader2 className="w-3.5 h-3.5 animate-spin" />
              ) : (
                <RotateCcw className="w-3.5 h-3.5" />
              )}
              Reset to automatic
            </button>
            <div className="text-sm text-content-muted">
              {hasChanges ? (
                <span className="text-warning">You have unsaved changes</span>
              ) : (
                'Select calibration sets to apply'
              )}
            </div>
          </div>
          <div className="flex gap-3">
            <button
              onClick={onClose}
              className="px-4 py-2 bg-surface-hover text-content rounded hover:bg-surface-hover transition-colors"
            >
              Cancel
            </button>
            <button
              onClick={handleApply}
              disabled={!hasChanges}
              className={`px-4 py-2 rounded transition-colors ${
                hasChanges
                  ? 'bg-accent text-surface hover:bg-accent-hover'
                  : 'bg-surface-hover text-content-muted cursor-not-allowed'
              }`}
            >
              Apply Selection
            </button>
          </div>
        </div>
      </div>
    </div>
  );
};
