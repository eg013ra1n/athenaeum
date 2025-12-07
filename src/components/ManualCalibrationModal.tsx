import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
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
} from 'lucide-react';
import type {
  LightFrameParameters,
  CalibrationSetWithScore,
} from '../types/models';

interface ManualCalibrationModalProps {
  isOpen: boolean;
  frameIds: number[];
  filterDisplay: string;
  currentFlatSetId?: number | null;
  currentDarkSetId?: number | null;
  currentBiasSetId?: number | null;
  useBiasForDarkOptimization: boolean;
  onApply: (flatSetId: number | null, darkSetId: number | null, biasSetId: number | null) => void;
  onClose: () => void;
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
}) => {
  const [activeTab, setActiveTab] = useState<TabType>('flat');
  const [lightParams, setLightParams] = useState<LightFrameParameters | null>(null);
  const [flatSets, setFlatSets] = useState<CalibrationSetWithScore[]>([]);
  const [darkSets, setDarkSets] = useState<CalibrationSetWithScore[]>([]);
  const [biasSets, setBiasSets] = useState<CalibrationSetWithScore[]>([]);
  const [showAll, setShowAll] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

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

  // Reset selections when modal opens with new data
  useEffect(() => {
    if (isOpen) {
      setSelectedFlatId(currentFlatSetId ?? null);
      setSelectedDarkId(currentDarkSetId ?? null);
      setSelectedBiasId(currentBiasSetId ?? null);
    }
  }, [isOpen, currentFlatSetId, currentDarkSetId, currentBiasSetId]);

  const loadData = async () => {
    setLoading(true);
    setError(null);

    try {
      // Load light frame parameters
      const params = await invoke<LightFrameParameters>('get_light_frame_parameters', {
        frameIds,
      });
      setLightParams(params);

      // Load calibration sets for each type
      const [flats, darks, biases] = await Promise.all([
        invoke<CalibrationSetWithScore[]>('get_calibration_sets_for_manual_selection', {
          frameIds,
          calibrationType: 'flat',
          showAll,
        }),
        invoke<CalibrationSetWithScore[]>('get_calibration_sets_for_manual_selection', {
          frameIds,
          calibrationType: 'dark',
          showAll,
        }),
        useBiasForDarkOptimization
          ? invoke<CalibrationSetWithScore[]>('get_calibration_sets_for_manual_selection', {
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

  const handleApply = () => {
    onApply(selectedFlatId, selectedDarkId, selectedBiasId);
  };

  const hasChanges =
    selectedFlatId !== currentFlatSetId ||
    selectedDarkId !== currentDarkSetId ||
    selectedBiasId !== currentBiasSetId;

  if (!isOpen) return null;

  const formatTemp = (temp: number | null | undefined) => {
    if (temp === null || temp === undefined) return 'N/A';
    return `${temp.toFixed(1)}°C`;
  };

  const formatDateRange = (range: [string, string] | null | undefined) => {
    if (!range) return 'N/A';
    const start = range[0].substring(0, 10);
    const end = range[1].substring(0, 10);
    return start === end ? start : `${start} - ${end}`;
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
              {type === 'flat' && set.filter ? `${set.filter}` : type.charAt(0).toUpperCase() + type.slice(1)}
              {set.exptime !== null && type !== 'flat' && ` (${set.exptime}s)`}
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
              {set.date_display}
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
    switch (activeTab) {
      case 'flat':
        return currentFlatSetId;
      case 'dark':
        return currentDarkSetId;
      case 'bias':
        return currentBiasSetId;
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

  const sets = getCurrentSets();
  const selectedId = getSelectedId();
  const currentId = getCurrentId();

  return (
    <div className="fixed inset-0 bg-black bg-opacity-60 flex items-center justify-center z-50 overflow-y-auto">
      <div className="bg-gray-800 rounded-lg w-full max-w-5xl mx-4 my-8 border border-gray-700 shadow-xl flex flex-col max-h-[90vh]">
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-gray-700">
          <div>
            <h2 className="text-xl font-semibold text-gray-100">Manual Calibration Selection</h2>
            <p className="text-sm text-gray-400 mt-1">
              Select calibration sets for {frameIds.length} {filterDisplay} frame
              {frameIds.length !== 1 ? 's' : ''}
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
          {/* Left Panel - Light Frame Parameters */}
          <div className="w-72 border-r border-gray-700 p-4 bg-gray-850 overflow-y-auto">
            <h3 className="font-medium text-gray-200 mb-4">Light Frame Parameters</h3>

            {loading && !lightParams ? (
              <div className="text-gray-400 text-sm">Loading...</div>
            ) : error ? (
              <div className="text-red-400 text-sm">{error}</div>
            ) : lightParams ? (
              <div className="space-y-3 text-sm">
                <div className="flex items-center gap-2">
                  <Camera className="w-4 h-4 text-gray-500" />
                  <span className="text-gray-400">Camera:</span>
                  <span className="text-gray-200">{lightParams.instrume || 'Unknown'}</span>
                </div>
                <div className="flex items-center gap-2">
                  <Aperture className="w-4 h-4 text-gray-500" />
                  <span className="text-gray-400">Filter:</span>
                  <span className="text-gray-200">{lightParams.filter || 'No Filter'}</span>
                </div>
                <div className="flex items-center gap-2">
                  <Hash className="w-4 h-4 text-gray-500" />
                  <span className="text-gray-400">Binning:</span>
                  <span className="text-gray-200">{lightParams.binning || 'N/A'}</span>
                </div>
                {lightParams.gain !== null && (
                  <div className="flex items-center gap-2">
                    <span className="text-gray-400 ml-6">Gain:</span>
                    <span className="text-gray-200">{lightParams.gain}</span>
                  </div>
                )}
                {lightParams.offset !== null && (
                  <div className="flex items-center gap-2">
                    <span className="text-gray-400 ml-6">Offset:</span>
                    <span className="text-gray-200">{lightParams.offset}</span>
                  </div>
                )}
                <div className="flex items-center gap-2">
                  <Thermometer className="w-4 h-4 text-gray-500" />
                  <span className="text-gray-400">Avg Temp:</span>
                  <span className="text-gray-200">{formatTemp(lightParams.avg_ccd_temp)}</span>
                </div>
                <div className="flex items-center gap-2">
                  <Clock className="w-4 h-4 text-gray-500" />
                  <span className="text-gray-400">Avg Exp:</span>
                  <span className="text-gray-200">
                    {lightParams.avg_exptime?.toFixed(1) ?? 'N/A'}s
                  </span>
                </div>
                <div className="flex items-center gap-2">
                  <Calendar className="w-4 h-4 text-gray-500" />
                  <span className="text-gray-400">Dates:</span>
                </div>
                <div className="text-gray-200 text-xs ml-6">
                  {formatDateRange(lightParams.date_range)}
                </div>
                <div className="flex items-center gap-2">
                  <Hash className="w-4 h-4 text-gray-500" />
                  <span className="text-gray-400">Frames:</span>
                  <span className="text-gray-200">{lightParams.frame_count}</span>
                </div>

                {/* Current Selections */}
                <div className="pt-4 border-t border-gray-700 mt-4">
                  <h4 className="text-gray-400 text-xs uppercase mb-2">Current Links</h4>
                  <div className="space-y-1 text-xs">
                    <div className="flex items-center gap-2">
                      {currentFlatSetId ? (
                        <CheckCircle className="w-3 h-3 text-green-400" />
                      ) : (
                        <AlertTriangle className="w-3 h-3 text-orange-400" />
                      )}
                      <span className="text-gray-400">Flat:</span>
                      <span className="text-gray-200">
                        {currentFlatSetId ? `Set #${currentFlatSetId}` : 'None'}
                      </span>
                    </div>
                    <div className="flex items-center gap-2">
                      {currentDarkSetId ? (
                        <CheckCircle className="w-3 h-3 text-green-400" />
                      ) : (
                        <AlertTriangle className="w-3 h-3 text-orange-400" />
                      )}
                      <span className="text-gray-400">Dark:</span>
                      <span className="text-gray-200">
                        {currentDarkSetId ? `Set #${currentDarkSetId}` : 'None'}
                      </span>
                    </div>
                    {useBiasForDarkOptimization && (
                      <div className="flex items-center gap-2">
                        {currentBiasSetId ? (
                          <CheckCircle className="w-3 h-3 text-green-400" />
                        ) : (
                          <AlertTriangle className="w-3 h-3 text-orange-400" />
                        )}
                        <span className="text-gray-400">Bias:</span>
                        <span className="text-gray-200">
                          {currentBiasSetId ? `Set #${currentBiasSetId}` : 'None'}
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
            <div className="flex border-b border-gray-700">
              <button
                onClick={() => setActiveTab('flat')}
                className={`px-6 py-3 font-medium text-sm transition-colors ${
                  activeTab === 'flat'
                    ? 'text-blue-400 border-b-2 border-blue-400 bg-gray-750'
                    : 'text-gray-400 hover:text-gray-200'
                }`}
              >
                Flats
                {flatSets.length > 0 && (
                  <span className="ml-2 px-1.5 py-0.5 text-xs rounded bg-gray-700">
                    {flatSets.length}
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
              {useBiasForDarkOptimization && (
                <button
                  onClick={() => setActiveTab('bias')}
                  className={`px-6 py-3 font-medium text-sm transition-colors ${
                    activeTab === 'bias'
                      ? 'text-green-400 border-b-2 border-green-400 bg-gray-750'
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
              )}

              {/* Show All Toggle */}
              <div className="ml-auto flex items-center px-4">
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
                      type={activeTab}
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
              'Select calibration sets to apply'
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
              disabled={!hasChanges}
              className={`px-4 py-2 rounded transition-colors ${
                hasChanges
                  ? 'bg-blue-600 text-white hover:bg-blue-700'
                  : 'bg-gray-600 text-gray-400 cursor-not-allowed'
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
