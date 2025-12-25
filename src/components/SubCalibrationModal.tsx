import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
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
  // For flat: tabs are darkflat, dark, bias
  // For dark: tab is bias only
  const [activeTab, setActiveTab] = useState<SubCalTabType>(sourceType === 'flat' ? 'dark' : 'bias');
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

  const loadData = async () => {
    setLoading(true);
    setError(null);

    try {
      // Load calibration set parameters
      const params = await invoke<CalibrationSetParameters>('get_calibration_set_parameters', {
        setId: sourceSetId,
      });
      setSetParams(params);

      if (sourceType === 'flat') {
        // Load DarkFlat, Dark, and Bias sets for flat sub-calibration
        const [darkflats, darks, biases] = await Promise.all([
          invoke<CalibrationSetWithScore[]>('get_subcalibration_sets_for_manual_selection', {
            setId: sourceSetId,
            calibrationType: 'darkflat',
            showAll,
          }),
          invoke<CalibrationSetWithScore[]>('get_subcalibration_sets_for_manual_selection', {
            setId: sourceSetId,
            calibrationType: 'dark',
            showAll,
          }),
          invoke<CalibrationSetWithScore[]>('get_subcalibration_sets_for_manual_selection', {
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
        const biases = await invoke<CalibrationSetWithScore[]>('get_subcalibration_sets_for_manual_selection', {
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
      await invoke('clear_subcalibration_override', {
        sourceSetId,
        calibrationType: null,  // Clear all
      });

      // Apply new selections
      if (sourceType === 'flat') {
        // For flats, only one of DarkFlat/Dark/Bias should be selected (priority)
        if (selectedDarkFlatId) {
          await invoke('manual_assign_subcalibration', {
            sourceSetId,
            calibrationSetId: selectedDarkFlatId,
            calibrationType: 'DarkFlat',
          });
        } else if (selectedDarkId) {
          await invoke('manual_assign_subcalibration', {
            sourceSetId,
            calibrationSetId: selectedDarkId,
            calibrationType: 'Dark',
          });
        } else if (selectedBiasId) {
          await invoke('manual_assign_subcalibration', {
            sourceSetId,
            calibrationSetId: selectedBiasId,
            calibrationType: 'Bias',
          });
        }
      } else {
        // For darks, only Bias
        if (selectedBiasId) {
          await invoke('manual_assign_subcalibration', {
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

  const formatMatchScore = (score: number) => {
    const percent = Math.round(score * 100);
    if (percent >= 80) return { label: 'Excellent', color: 'text-green-400' };
    if (percent >= 60) return { label: 'Good', color: 'text-blue-400' };
    if (percent >= 40) return { label: 'Fair', color: 'text-yellow-400' };
    return { label: 'Poor', color: 'text-orange-400' };
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
    const { set, match_score, match_details } = setWithScore;
    const scoreInfo = formatMatchScore(match_score);

    return (
      <div
        onClick={onSelect}
        className={`p-3 rounded-lg border-2 cursor-pointer transition-all ${
          isSelected
            ? 'border-blue-500 bg-blue-900/20'
            : 'border-gray-700 hover:border-gray-600 bg-gray-800/30'
        }`}
      >
        <div className="flex items-start justify-between mb-2">
          <div className="flex items-center gap-2">
            {isSelected && <Check className="w-4 h-4 text-blue-400" />}
            <span className="font-medium text-gray-200">
              Set #{set.id}
              {set.exptime !== null && ` (${set.exptime}s)`}
            </span>
            {isCurrent && (
              <span className="text-xs px-2 py-0.5 rounded bg-gray-700 text-gray-300">Current</span>
            )}
          </div>
          <div className={`text-sm font-medium ${scoreInfo.color}`}>
            {Math.round(match_score * 100)}% {scoreInfo.label}
          </div>
        </div>

        <div className="grid grid-cols-2 gap-2 text-sm">
          <div className="flex items-center gap-2 text-gray-400">
            <Camera className="w-3 h-3" />
            <span className={match_details.instrume_match ? 'text-gray-300' : 'text-orange-400'}>
              {set.instrume || 'Unknown'}
              {!match_details.instrume_match && ' (mismatch)'}
            </span>
          </div>
          <div className="flex items-center gap-2 text-gray-400">
            <Hash className="w-3 h-3" />
            <span className={match_details.binning_match ? 'text-gray-300' : 'text-orange-400'}>
              {set.binning || 'N/A'}
              {!match_details.binning_match && ' (mismatch)'}
            </span>
          </div>
          <div className="flex items-center gap-2 text-gray-400">
            <Thermometer className="w-3 h-3" />
            <span className="text-gray-300">
              {formatTemp(set.ccd_temp)}
              {match_details.temp_diff !== null && (
                <span className={match_details.temp_diff > 2 ? 'text-orange-400' : 'text-gray-500'}>
                  {' '}
                  ({match_details.temp_diff > 0 ? '+' : ''}
                  {match_details.temp_diff?.toFixed(1)}°C)
                </span>
              )}
            </span>
          </div>
          <div className="flex items-center gap-2 text-gray-400">
            <Calendar className="w-3 h-3" />
            <span className={match_details.date_diff_days > 30 ? 'text-orange-400' : 'text-gray-300'}>
              {set.date_start ? new Date(set.date_start).toLocaleDateString('en-GB') : set.date_display}
              {match_details.date_diff_days > 0 && ` (${match_details.date_diff_days}d)`}
            </span>
          </div>
          {set.gain !== null && (
            <div className="flex items-center gap-2 text-gray-400">
              <span className="text-gray-500 text-xs">Gain:</span>
              <span className={match_details.gain_match ? 'text-gray-300' : 'text-orange-400'}>
                {set.gain}
                {!match_details.gain_match && ' (mismatch)'}
              </span>
            </div>
          )}
          <div className="flex items-center gap-2 text-gray-400">
            <span className="text-gray-500 text-xs">Frames:</span>
            <span className="text-gray-300">{set.frame_count}</span>
          </div>
        </div>
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
    switch (activeTab) {
      case 'darkflat':
        setSelectedDarkFlatId(selectedDarkFlatId === setId ? null : setId);
        break;
      case 'dark':
        setSelectedDarkId(selectedDarkId === setId ? null : setId);
        break;
      case 'bias':
        setSelectedBiasId(selectedBiasId === setId ? null : setId);
        break;
    }
  };

  const sets = getCurrentSets();
  const selectedId = getSelectedId();
  const currentId = getCurrentId();

  return (
    <div className="fixed inset-0 bg-black bg-opacity-60 flex items-center justify-center z-50 overflow-y-auto">
      <div className="bg-gray-800 rounded-lg w-full max-w-5xl mx-4 my-8 border border-gray-700 shadow-xl flex flex-col max-h-[90vh]">
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-gray-700">
          <div>
            <h2 className="text-xl font-semibold text-gray-100">
              Sub-Calibration for {sourceType === 'flat' ? 'Flat' : 'Dark'} Set #{sourceSetId}
            </h2>
            <p className="text-sm text-gray-400 mt-1">
              Select {sourceType === 'flat' ? 'Dark/DarkFlat/Bias' : 'Bias'} calibration for this set
            </p>
          </div>
          <button
            onClick={onClose}
            className="p-2 hover:bg-gray-700 rounded-lg transition-colors"
          >
            <X className="w-5 h-5 text-gray-400" />
          </button>
        </div>

        {/* Content */}
        <div className="flex flex-1 overflow-hidden">
          {/* Left Panel - Source Set Parameters */}
          <div className="w-72 border-r border-gray-700 p-4 bg-gray-850 overflow-y-auto">
            <h3 className="font-medium text-gray-200 mb-4">
              {sourceType === 'flat' ? 'Flat' : 'Dark'} Set Parameters
            </h3>

            {loading && !setParams ? (
              <div className="text-gray-400 text-sm">Loading...</div>
            ) : error ? (
              <div className="text-red-400 text-sm">{error}</div>
            ) : setParams ? (
              <div className="space-y-3 text-sm">
                <div className="flex items-center gap-2">
                  <Camera className="w-4 h-4 text-gray-500" />
                  <span className="text-gray-400">Camera:</span>
                  <span className="text-gray-200">{setParams.instrume || 'Unknown'}</span>
                </div>
                {setParams.filter && (
                  <div className="flex items-center gap-2">
                    <Aperture className="w-4 h-4 text-gray-500" />
                    <span className="text-gray-400">Filter:</span>
                    <span className="text-gray-200">{setParams.filter}</span>
                  </div>
                )}
                <div className="flex items-center gap-2">
                  <Hash className="w-4 h-4 text-gray-500" />
                  <span className="text-gray-400">Binning:</span>
                  <span className="text-gray-200">{setParams.binning || 'N/A'}</span>
                </div>
                {setParams.gain !== null && (
                  <div className="flex items-center gap-2">
                    <span className="text-gray-400 ml-6">Gain:</span>
                    <span className="text-gray-200">{setParams.gain}</span>
                  </div>
                )}
                {setParams.offset !== null && (
                  <div className="flex items-center gap-2">
                    <span className="text-gray-400 ml-6">Offset:</span>
                    <span className="text-gray-200">{setParams.offset}</span>
                  </div>
                )}
                <div className="flex items-center gap-2">
                  <Thermometer className="w-4 h-4 text-gray-500" />
                  <span className="text-gray-400">Temp:</span>
                  <span className="text-gray-200">{formatTemp(setParams.ccd_temp)}</span>
                </div>
                {setParams.exptime !== null && (
                  <div className="flex items-center gap-2">
                    <Clock className="w-4 h-4 text-gray-500" />
                    <span className="text-gray-400">Exposure:</span>
                    <span className="text-gray-200">{setParams.exptime}s</span>
                  </div>
                )}
                <div className="flex items-center gap-2">
                  <Calendar className="w-4 h-4 text-gray-500" />
                  <span className="text-gray-400">Date:</span>
                </div>
                <div className="text-gray-200 text-xs ml-6">
                  {setParams.date_start?.substring(0, 10) || 'N/A'}
                </div>
                <div className="flex items-center gap-2">
                  <Hash className="w-4 h-4 text-gray-500" />
                  <span className="text-gray-400">Frames:</span>
                  <span className="text-gray-200">{setParams.frame_count}</span>
                </div>

                {/* Current Sub-Calibration Links */}
                <div className="pt-4 border-t border-gray-700 mt-4">
                  <h4 className="text-gray-400 text-xs uppercase mb-2">Current Sub-Cal Links</h4>
                  <div className="space-y-1 text-xs">
                    {sourceType === 'flat' && (
                      <>
                        <div className="flex items-center gap-2">
                          {setParams.current_darkflat_set_id ? (
                            <CheckCircle className="w-3 h-3 text-green-400" />
                          ) : (
                            <AlertTriangle className="w-3 h-3 text-gray-500" />
                          )}
                          <span className="text-gray-400">DarkFlat:</span>
                          <span className="text-gray-200">
                            {setParams.current_darkflat_set_id ? `Set #${setParams.current_darkflat_set_id}` : 'None'}
                          </span>
                        </div>
                        <div className="flex items-center gap-2">
                          {setParams.current_dark_set_id ? (
                            <CheckCircle className="w-3 h-3 text-green-400" />
                          ) : (
                            <AlertTriangle className="w-3 h-3 text-gray-500" />
                          )}
                          <span className="text-gray-400">Dark:</span>
                          <span className="text-gray-200">
                            {setParams.current_dark_set_id ? `Set #${setParams.current_dark_set_id}` : 'None'}
                          </span>
                        </div>
                      </>
                    )}
                    <div className="flex items-center gap-2">
                      {setParams.current_bias_set_id ? (
                        <CheckCircle className="w-3 h-3 text-green-400" />
                      ) : (
                        <AlertTriangle className="w-3 h-3 text-gray-500" />
                      )}
                      <span className="text-gray-400">Bias:</span>
                      <span className="text-gray-200">
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
            <div className="flex border-b border-gray-700">
              {sourceType === 'flat' && (
                <>
                  <button
                    onClick={() => setActiveTab('darkflat')}
                    className={`px-6 py-3 font-medium text-sm transition-colors ${
                      activeTab === 'darkflat'
                        ? 'text-indigo-400 border-b-2 border-indigo-400 bg-gray-750'
                        : 'text-gray-400 hover:text-gray-200'
                    }`}
                  >
                    DarkFlats
                    {darkflatSets.length > 0 && (
                      <span className="ml-2 px-1.5 py-0.5 text-xs rounded bg-gray-700">
                        {darkflatSets.length}
                      </span>
                    )}
                  </button>
                  <button
                    onClick={() => setActiveTab('dark')}
                    className={`px-6 py-3 font-medium text-sm transition-colors ${
                      activeTab === 'dark'
                        ? 'text-purple-400 border-b-2 border-purple-400 bg-gray-750'
                        : 'text-gray-400 hover:text-gray-200'
                    }`}
                  >
                    Darks
                    {darkSets.length > 0 && (
                      <span className="ml-2 px-1.5 py-0.5 text-xs rounded bg-gray-700">
                        {darkSets.length}
                      </span>
                    )}
                  </button>
                </>
              )}
              <button
                onClick={() => setActiveTab('bias')}
                className={`px-6 py-3 font-medium text-sm transition-colors ${
                  activeTab === 'bias'
                    ? 'text-emerald-400 border-b-2 border-emerald-400 bg-gray-750'
                    : 'text-gray-400 hover:text-gray-200'
                }`}
              >
                Bias
                {biasSets.length > 0 && (
                  <span className="ml-2 px-1.5 py-0.5 text-xs rounded bg-gray-700">
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
                      ? 'bg-blue-900/30 text-blue-300'
                      : 'bg-gray-700 text-gray-400 hover:text-gray-200'
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
                <div className="flex items-center justify-center h-full text-gray-400">
                  Loading calibration sets...
                </div>
              ) : error ? (
                <div className="flex items-center justify-center h-full text-red-400">
                  {error}
                </div>
              ) : sets.length === 0 ? (
                <div className="flex flex-col items-center justify-center h-full text-gray-400">
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
        <div className="flex items-center justify-between px-6 py-4 border-t border-gray-700 bg-gray-850">
          <div className="text-sm text-gray-400">
            {hasChanges ? (
              <span className="text-yellow-400">You have unsaved changes</span>
            ) : (
              'Select sub-calibration to apply'
            )}
          </div>
          <div className="flex gap-3">
            <button
              onClick={onClose}
              className="px-4 py-2 bg-gray-700 text-gray-200 rounded hover:bg-gray-600 transition-colors"
            >
              Cancel
            </button>
            <button
              onClick={handleApply}
              disabled={!hasChanges || saving}
              className={`px-4 py-2 rounded transition-colors ${
                hasChanges && !saving
                  ? 'bg-blue-600 text-white hover:bg-blue-700'
                  : 'bg-gray-600 text-gray-400 cursor-not-allowed'
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
