import React, { useState, useEffect } from 'react';
import { api } from '../api';
import { MatchVerdict } from './calibration/MatchVerdict';
import {
  X,
  Camera,
  Thermometer,
  Clock,
  Check,
  Eye,
  EyeOff,
  Calendar,
  Hash,
  AlertTriangle,
  CheckCircle,
  Aperture,
} from 'lucide-react';
import type {
  CalibrationSetParameters,
  CalibrationSetWithScore,
} from '../types/models';

interface SubCalibrationModalProps {
  isOpen: boolean;
  sourceSetId: number;
  sourceType: 'flat' | 'dark';
  onApply: () => void;  // Just refresh, selections saved via command
  onClose: () => void;
}

type SubCalTabType = 'darkflat' | 'dark' | 'bias';

export const SubCalibrationModal: React.FC<SubCalibrationModalProps> = ({
  isOpen,
  sourceSetId,
  sourceType,
  onApply,
  onClose,
}) => {
  // For flat sources, the auto-link engine uses the fallback chain
  // DarkFlat (preferred) → Dark (fallback) → Bias (last resort) — see
  // configurable_matcher::find_calibration_with_fallback. We mirror that order
  // visually here so the user sees what auto-link would have picked first.
  // Default activeTab is set after data loads (see effect below) so we land on
  // the highest-priority tab that has at least one candidate.
  const [activeTab, setActiveTab] = useState<SubCalTabType>(sourceType === 'flat' ? 'darkflat' : 'bias');
  const [hasAutoSelectedTab, setHasAutoSelectedTab] = useState(false);
  const [setParams, setSetParams] = useState<CalibrationSetParameters | null>(null);
  const [darkflatSets, setDarkflatSets] = useState<CalibrationSetWithScore[]>([]);
  const [darkSets, setDarkSets] = useState<CalibrationSetWithScore[]>([]);
  const [biasSets, setBiasSets] = useState<CalibrationSetWithScore[]>([]);
  const [showAll, setShowAll] = useState(false);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Selections
  const [selectedDarkFlatId, setSelectedDarkFlatId] = useState<number | null>(null);
  const [selectedDarkId, setSelectedDarkId] = useState<number | null>(null);
  const [selectedBiasId, setSelectedBiasId] = useState<number | null>(null);

  // Load data when modal opens
  useEffect(() => {
    if (isOpen && sourceSetId) {
      loadData();
    }
  }, [isOpen, sourceSetId, showAll]);

  // Reset selections when data loads from backend
  useEffect(() => {
    if (isOpen && setParams) {
      setSelectedDarkFlatId(setParams.current_darkflat_set_id ?? null);
      setSelectedDarkId(setParams.current_dark_set_id ?? null);
      setSelectedBiasId(setParams.current_bias_set_id ?? null);
    }
  }, [isOpen, setParams]);

  // Reset the "has auto-selected" flag when the modal closes so the next open
  // re-runs the priority defaulting against fresh data.
  useEffect(() => {
    if (!isOpen) setHasAutoSelectedTab(false);
  }, [isOpen]);

  // Default to the highest-priority tab that has at least one candidate, but
  // only once per modal open and only for flat sources (dark sources have a
  // single Bias tab). If the user has already linked a sub-cal, prefer the tab
  // matching that link so they land on what's currently selected.
  useEffect(() => {
    if (!isOpen || sourceType !== 'flat' || hasAutoSelectedTab || !setParams) return;
    if (loading) return; // wait until data has loaded

    const linkedTab: SubCalTabType | null =
      setParams.current_darkflat_set_id ? 'darkflat' :
      setParams.current_dark_set_id ? 'dark' :
      setParams.current_bias_set_id ? 'bias' : null;

    const firstNonEmpty: SubCalTabType | null =
      darkflatSets.length > 0 ? 'darkflat' :
      darkSets.length > 0 ? 'dark' :
      biasSets.length > 0 ? 'bias' : null;

    const next = linkedTab ?? firstNonEmpty;
    if (next) setActiveTab(next);
    setHasAutoSelectedTab(true);
  }, [isOpen, sourceType, hasAutoSelectedTab, setParams, loading, darkflatSets.length, darkSets.length, biasSets.length]);

  const loadData = async () => {
    setLoading(true);
    setError(null);

    try {
      // Load calibration set parameters
      const params = await api.invoke<CalibrationSetParameters>('get_calibration_set_parameters', {
        setId: sourceSetId,
      });
      setSetParams(params);

      if (sourceType === 'flat') {
        // Load DarkFlat, Dark, and Bias sets for flat sub-calibration
        const [darkflats, darks, biases] = await Promise.all([
          api.invoke<CalibrationSetWithScore[]>('get_subcalibration_sets_for_manual_selection', {
            setId: sourceSetId,
            calibrationType: 'darkflat',
            showAll,
          }),
          api.invoke<CalibrationSetWithScore[]>('get_subcalibration_sets_for_manual_selection', {
            setId: sourceSetId,
            calibrationType: 'dark',
            showAll,
          }),
          api.invoke<CalibrationSetWithScore[]>('get_subcalibration_sets_for_manual_selection', {
            setId: sourceSetId,
            calibrationType: 'bias',
            showAll,
          }),
        ]);

        setDarkflatSets(darkflats);
        setDarkSets(darks);
        setBiasSets(biases);
      } else {
        // Dark source - only load Bias sets
        const biases = await api.invoke<CalibrationSetWithScore[]>('get_subcalibration_sets_for_manual_selection', {
          setId: sourceSetId,
          calibrationType: 'bias',
          showAll,
        });
        setBiasSets(biases);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  const handleApply = async () => {
    setSaving(true);
    setError(null);

    try {
      // Clear existing sub-calibration links first
      await api.invoke('clear_subcalibration_override', {
        sourceSetId,
        calibrationType: null,  // Clear all
      });

      // Apply new selections
      if (sourceType === 'flat') {
        // For flats, only one of DarkFlat/Dark/Bias should be selected (priority)
        if (selectedDarkFlatId) {
          await api.invoke('manual_assign_subcalibration', {
            sourceSetId,
            calibrationSetId: selectedDarkFlatId,
            calibrationType: 'DarkFlat',
          });
        } else if (selectedDarkId) {
          await api.invoke('manual_assign_subcalibration', {
            sourceSetId,
            calibrationSetId: selectedDarkId,
            calibrationType: 'Dark',
          });
        } else if (selectedBiasId) {
          await api.invoke('manual_assign_subcalibration', {
            sourceSetId,
            calibrationSetId: selectedBiasId,
            calibrationType: 'Bias',
          });
        }
      } else {
        // For darks, only Bias
        if (selectedBiasId) {
          await api.invoke('manual_assign_subcalibration', {
            sourceSetId,
            calibrationSetId: selectedBiasId,
            calibrationType: 'Bias',
          });
        }
      }

      onApply();
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  // Determine if any selection changed
  const hasChanges = (() => {
    if (!setParams) return false;
    if (sourceType === 'flat') {
      return (
        selectedDarkFlatId !== (setParams.current_darkflat_set_id ?? null) ||
        selectedDarkId !== (setParams.current_dark_set_id ?? null) ||
        selectedBiasId !== (setParams.current_bias_set_id ?? null)
      );
    }
    return selectedBiasId !== (setParams.current_bias_set_id ?? null);
  })();

  if (!isOpen) return null;

  const formatTemp = (temp: number | null | undefined) => {
    if (temp === null || temp === undefined) return 'N/A';
    return `${temp.toFixed(1)}°C`;
  };

  /** The percentage is CLOSENESS (date / temperature / exposure proximity),
   *  which an incompatible set can score highly — so an incompatible one is
   *  never dressed in a "good match" colour. Its own reason is spelled out by
   *  <MatchVerdict>. */
  const formatMatchScore = (score: number, compatible: boolean) => {
    if (!compatible) return { label: 'Not a match', color: 'text-content-muted' };
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
  }: {
    setWithScore: CalibrationSetWithScore;
    isSelected: boolean;
    isCurrent: boolean;
    onSelect: () => void;
  }) => {
    const { set, match_score, match_details, compatible, parameters } = setWithScore;
    const scoreInfo = formatMatchScore(match_score, compatible);

    return (
      <div
        onClick={onSelect}
        className={`p-3 rounded-lg border-2 cursor-pointer transition-all ${
          isSelected
            ? 'border-accent bg-accent-muted/20'
            : 'border-border hover:border-border bg-surface-elevated/30'
        }`}
      >
        <div className="flex items-start justify-between mb-2">
          <div className="flex items-center gap-2">
            {isSelected && <Check className="w-4 h-4 text-accent" />}
            <span className="font-medium text-content">
              Set #{set.id}
              {set.exptime !== null && ` (${set.exptime}s)`}
            </span>
            {isCurrent && (
              <span className="text-xs px-2 py-0.5 rounded bg-surface-hover text-content-secondary">Current</span>
            )}
          </div>
          <div className={`text-sm font-medium ${scoreInfo.color}`}>
            {Math.round(match_score * 100)}% {scoreInfo.label}
          </div>
        </div>

        <div className="grid grid-cols-2 gap-2 text-sm">
          <div className="flex items-center gap-2 text-content-muted">
            <Camera className="w-3 h-3" />
            <span className={match_details.instrume_match ? 'text-content-secondary' : 'text-orange'}>
              {set.instrume || 'Unknown'}
              {!match_details.instrume_match && ' (mismatch)'}
            </span>
          </div>
          <div className="flex items-center gap-2 text-content-muted">
            <Hash className="w-3 h-3" />
            <span className={match_details.binning_match ? 'text-content-secondary' : 'text-orange'}>
              {set.binning || 'N/A'}
              {!match_details.binning_match && ' (mismatch)'}
            </span>
          </div>
          <div className="flex items-center gap-2 text-content-muted">
            <Thermometer className="w-3 h-3" />
            <span className="text-content-secondary">
              {formatTemp(set.ccd_temp)}
              {match_details.temp_diff !== null && (
                <span className={match_details.temp_diff > 2 ? 'text-orange' : 'text-content-muted'}>
                  {' '}
                  ({match_details.temp_diff > 0 ? '+' : ''}
                  {match_details.temp_diff?.toFixed(1)}°C)
                </span>
              )}
            </span>
          </div>
          <div className="flex items-center gap-2 text-content-muted">
            <Calendar className="w-3 h-3" />
            <span className={match_details.date_diff_days > 30 ? 'text-orange' : 'text-content-secondary'}>
              {set.date_start ? new Date(set.date_start).toLocaleDateString('en-GB') : set.date_display}
              {match_details.date_diff_days > 0 && ` (${match_details.date_diff_days}d)`}
            </span>
          </div>
          {set.gain !== null && (
            <div className="flex items-center gap-2 text-content-muted">
              <span className="text-content-muted text-xs">Gain:</span>
              <span className={match_details.gain_match ? 'text-content-secondary' : 'text-orange'}>
                {set.gain}
                {!match_details.gain_match && ' (mismatch)'}
              </span>
            </div>
          )}
          <div className="flex items-center gap-2 text-content-muted">
            <span className="text-content-muted text-xs">Frames:</span>
            <span className="text-content-secondary">{set.frame_count}</span>
          </div>
        </div>

        <MatchVerdict compatible={compatible} parameters={parameters} />
      </div>
    );
  };

  // Get current sets for active tab
  const getCurrentSets = () => {
    switch (activeTab) {
      case 'darkflat':
        return darkflatSets;
      case 'dark':
        return darkSets;
      case 'bias':
        return biasSets;
    }
  };

  const getSelectedId = () => {
    switch (activeTab) {
      case 'darkflat':
        return selectedDarkFlatId;
      case 'dark':
        return selectedDarkId;
      case 'bias':
        return selectedBiasId;
    }
  };

  const getCurrentId = () => {
    if (!setParams) return null;
    switch (activeTab) {
      case 'darkflat':
        return setParams.current_darkflat_set_id;
      case 'dark':
        return setParams.current_dark_set_id;
      case 'bias':
        return setParams.current_bias_set_id;
    }
  };

  const handleSelect = (setId: number) => {
    // For flats, only one of DarkFlat/Dark/Bias may be set as sub-cal at a time
    // (handleApply uses a priority chain that would otherwise let a pre-loaded
    // higher-priority selection silently win over the user's tab change).
    const isFlat = sourceType === 'flat';
    switch (activeTab) {
      case 'darkflat': {
        const next = selectedDarkFlatId === setId ? null : setId;
        setSelectedDarkFlatId(next);
        if (isFlat && next !== null) {
          setSelectedDarkId(null);
          setSelectedBiasId(null);
        }
        break;
      }
      case 'dark': {
        const next = selectedDarkId === setId ? null : setId;
        setSelectedDarkId(next);
        if (isFlat && next !== null) {
          setSelectedDarkFlatId(null);
          setSelectedBiasId(null);
        }
        break;
      }
      case 'bias': {
        const next = selectedBiasId === setId ? null : setId;
        setSelectedBiasId(next);
        if (isFlat && next !== null) {
          setSelectedDarkFlatId(null);
          setSelectedDarkId(null);
        }
        break;
      }
    }
  };

  const sets = getCurrentSets();
  const selectedId = getSelectedId();
  const currentId = getCurrentId();

  return (
    <div className="fixed inset-0 bg-black bg-opacity-60 flex items-center justify-center z-50 overflow-y-auto">
      <div className="bg-surface-elevated rounded-lg w-full max-w-5xl mx-4 my-8 border border-border shadow-xl flex flex-col max-h-[90vh]">
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-border">
          <div>
            <h2 className="text-xl font-semibold text-content">
              Sub-Calibration for {sourceType === 'flat' ? 'Flat' : 'Dark'} Set #{sourceSetId}
            </h2>
            <p className="text-sm text-content-muted mt-1">
              Select {sourceType === 'flat' ? 'Dark/DarkFlat/Bias' : 'Bias'} calibration for this set
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
          {/* Left Panel - Source Set Parameters */}
          <div className="w-72 border-r border-border p-4 bg-surface overflow-y-auto">
            <h3 className="font-medium text-content mb-4">
              {sourceType === 'flat' ? 'Flat' : 'Dark'} Set Parameters
            </h3>

            {loading && !setParams ? (
              <div className="text-content-muted text-sm">Loading...</div>
            ) : error ? (
              <div className="text-error text-sm">{error}</div>
            ) : setParams ? (
              <div className="space-y-3 text-sm">
                <div className="flex items-center gap-2">
                  <Camera className="w-4 h-4 text-content-muted" />
                  <span className="text-content-muted">Camera:</span>
                  <span className="text-content">{setParams.instrume || 'Unknown'}</span>
                </div>
                {setParams.filter && (
                  <div className="flex items-center gap-2">
                    <Aperture className="w-4 h-4 text-content-muted" />
                    <span className="text-content-muted">Filter:</span>
                    <span className="text-content">{setParams.filter}</span>
                  </div>
                )}
                <div className="flex items-center gap-2">
                  <Hash className="w-4 h-4 text-content-muted" />
                  <span className="text-content-muted">Binning:</span>
                  <span className="text-content">{setParams.binning || 'N/A'}</span>
                </div>
                {setParams.gain !== null && (
                  <div className="flex items-center gap-2">
                    <span className="text-content-muted ml-6">Gain:</span>
                    <span className="text-content">{setParams.gain}</span>
                  </div>
                )}
                {setParams.offset !== null && (
                  <div className="flex items-center gap-2">
                    <span className="text-content-muted ml-6">Offset:</span>
                    <span className="text-content">{setParams.offset}</span>
                  </div>
                )}
                <div className="flex items-center gap-2">
                  <Thermometer className="w-4 h-4 text-content-muted" />
                  <span className="text-content-muted">Temp:</span>
                  <span className="text-content">{formatTemp(setParams.ccd_temp)}</span>
                </div>
                {setParams.exptime !== null && (
                  <div className="flex items-center gap-2">
                    <Clock className="w-4 h-4 text-content-muted" />
                    <span className="text-content-muted">Exposure:</span>
                    <span className="text-content">{setParams.exptime}s</span>
                  </div>
                )}
                <div className="flex items-center gap-2">
                  <Calendar className="w-4 h-4 text-content-muted" />
                  <span className="text-content-muted">Date:</span>
                </div>
                <div className="text-content text-xs ml-6">
                  {setParams.date_start?.substring(0, 10) || 'N/A'}
                </div>
                <div className="flex items-center gap-2">
                  <Hash className="w-4 h-4 text-content-muted" />
                  <span className="text-content-muted">Frames:</span>
                  <span className="text-content">{setParams.frame_count}</span>
                </div>

                {/* Current Sub-Calibration Links */}
                <div className="pt-4 border-t border-border mt-4">
                  <h4 className="text-content-muted text-xs uppercase mb-2">Current Sub-Cal Links</h4>
                  <div className="space-y-1 text-xs">
                    {sourceType === 'flat' && (
                      <>
                        <div className="flex items-center gap-2">
                          {setParams.current_darkflat_set_id ? (
                            <CheckCircle className="w-3 h-3 text-success" />
                          ) : (
                            <AlertTriangle className="w-3 h-3 text-content-muted" />
                          )}
                          <span className="text-content-muted">DarkFlat:</span>
                          <span className="text-content">
                            {setParams.current_darkflat_set_id ? `Set #${setParams.current_darkflat_set_id}` : 'None'}
                          </span>
                        </div>
                        <div className="flex items-center gap-2">
                          {setParams.current_dark_set_id ? (
                            <CheckCircle className="w-3 h-3 text-success" />
                          ) : (
                            <AlertTriangle className="w-3 h-3 text-content-muted" />
                          )}
                          <span className="text-content-muted">Dark:</span>
                          <span className="text-content">
                            {setParams.current_dark_set_id ? `Set #${setParams.current_dark_set_id}` : 'None'}
                          </span>
                        </div>
                      </>
                    )}
                    <div className="flex items-center gap-2">
                      {setParams.current_bias_set_id ? (
                        <CheckCircle className="w-3 h-3 text-success" />
                      ) : (
                        <AlertTriangle className="w-3 h-3 text-content-muted" />
                      )}
                      <span className="text-content-muted">Bias:</span>
                      <span className="text-content">
                        {setParams.current_bias_set_id ? `Set #${setParams.current_bias_set_id}` : 'None'}
                      </span>
                    </div>
                  </div>
                </div>
              </div>
            ) : null}
          </div>

          {/* Right Panel - Sub-Calibration Sets */}
          <div className="flex-1 flex flex-col overflow-hidden">
            {/* Tabs */}
            <div className="flex border-b border-border">
              {sourceType === 'flat' && (
                <>
                  <button
                    onClick={() => setActiveTab('darkflat')}
                    title="Auto-link prefers DarkFlat for calibrating Flats"
                    className={`px-6 py-3 font-medium text-sm transition-colors ${
                      activeTab === 'darkflat'
                        ? 'text-info border-b-2 border-info bg-surface-elevated'
                        : 'text-content-muted hover:text-content'
                    }`}
                  >
                    DarkFlats
                    <span className="ml-2 px-1.5 py-0.5 text-[10px] uppercase tracking-wide rounded bg-info-muted text-info">
                      Preferred
                    </span>
                    {darkflatSets.length > 0 && (
                      <span className="ml-2 px-1.5 py-0.5 text-xs rounded bg-surface-hover">
                        {darkflatSets.length}
                      </span>
                    )}
                  </button>
                  <button
                    onClick={() => setActiveTab('dark')}
                    title="Used when no DarkFlat is available"
                    className={`px-6 py-3 font-medium text-sm transition-colors ${
                      activeTab === 'dark'
                        ? 'text-purple border-b-2 border-purple bg-surface-elevated'
                        : 'text-content-muted hover:text-content'
                    }`}
                  >
                    Darks
                    <span className="ml-2 px-1.5 py-0.5 text-[10px] uppercase tracking-wide rounded bg-surface-hover text-content-muted">
                      Fallback
                    </span>
                    {darkSets.length > 0 && (
                      <span className="ml-2 px-1.5 py-0.5 text-xs rounded bg-surface-hover">
                        {darkSets.length}
                      </span>
                    )}
                  </button>
                </>
              )}
              <button
                onClick={() => setActiveTab('bias')}
                title={sourceType === 'flat' ? 'Last-resort fallback when no Dark/DarkFlat is available' : 'Bias sub-calibration for Dark frames'}
                className={`px-6 py-3 font-medium text-sm transition-colors ${
                  activeTab === 'bias'
                    ? 'text-success border-b-2 border-success bg-surface-elevated'
                    : 'text-content-muted hover:text-content'
                }`}
              >
                Bias
                {sourceType === 'flat' && (
                  <span className="ml-2 px-1.5 py-0.5 text-[10px] uppercase tracking-wide rounded bg-surface-hover text-content-muted">
                    Last resort
                  </span>
                )}
                {biasSets.length > 0 && (
                  <span className="ml-2 px-1.5 py-0.5 text-xs rounded bg-surface-hover">
                    {biasSets.length}
                  </span>
                )}
              </button>

              {/* Show All Toggle */}
              <div className="ml-auto flex items-center gap-3 px-4">
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
                  {sets.map((setWithScore) => (
                    <CalibrationSetRow
                      key={setWithScore.set.id}
                      setWithScore={setWithScore}
                      isSelected={selectedId === setWithScore.set.id}
                      isCurrent={currentId === setWithScore.set.id}
                      onSelect={() => setWithScore.set.id && handleSelect(setWithScore.set.id)}
                    />
                  ))}
                </div>
              )}
            </div>
          </div>
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between px-6 py-4 border-t border-border bg-surface">
          <div className="text-sm text-content-muted">
            {hasChanges ? (
              <span className="text-warning">You have unsaved changes</span>
            ) : (
              'Select sub-calibration to apply'
            )}
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
              disabled={!hasChanges || saving}
              className={`px-4 py-2 rounded transition-colors ${
                hasChanges && !saving
                  ? 'bg-accent text-surface hover:bg-accent-hover'
                  : 'bg-surface-hover text-content-muted cursor-not-allowed'
              }`}
            >
              {saving ? 'Saving...' : 'Apply Selection'}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
};
