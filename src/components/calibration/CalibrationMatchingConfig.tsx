import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Save,
  RefreshCw,
  AlertCircle,
  CheckCircle,
  ChevronDown,
  ChevronRight,
} from "lucide-react";
import {
  CalibrationMatchingConfig as ConfigType,
  CalibrationTypeConfig,
  MasterPreference,
  ParameterConfig,
} from "../../types/calibration-config";
import MatchingMatrixTable from "./MatchingMatrixTable";
import BehavioralOptionsPanel from "./BehavioralOptionsPanel";
import ClusteringParametersPanel from "./ClusteringParametersPanel";

export default function CalibrationMatchingConfig() {
  const [config, setConfig] = useState<ConfigType | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);

  // Accordion state for source type sections
  const [expandedSections, setExpandedSections] = useState<
    Record<string, boolean>
  >({
    lights: true,
    flats: false,
    darks: false,
    clustering: false,
    warnings: false,
    scoring: false,
    preferences: false,
  });

  useEffect(() => {
    loadConfig();
  }, []);

  const loadConfig = async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await invoke<ConfigType>("get_calibration_matching_config");
      setConfig(result);
    } catch (err) {
      setError(String(err));
      console.error("Failed to load calibration config:", err);
    } finally {
      setLoading(false);
    }
  };

  const handleSave = async () => {
    if (!config) return;

    try {
      setSaving(true);
      setError(null);
      setSuccess(false);

      await invoke("set_calibration_matching_config", { config });

      setSuccess(true);
      setTimeout(() => setSuccess(false), 3000);
    } catch (err) {
      setError(String(err));
      console.error("Failed to save calibration config:", err);
    } finally {
      setSaving(false);
    }
  };

  const handleReset = async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await invoke<ConfigType>("reset_calibration_matching_config");
      setConfig(result);
    } catch (err) {
      setError(String(err));
      console.error("Failed to reset calibration config:", err);
    } finally {
      setLoading(false);
    }
  };

  const toggleSection = (section: string) => {
    setExpandedSections((prev) => ({
      ...prev,
      [section]: !prev[section],
    }));
  };

  const updateParameterConfig = (
    sourceType: "lights" | "flats" | "darks",
    calibrationType: "flat" | "darkflat" | "dark" | "bias",
    parameter: string,
    updates: Partial<ParameterConfig>
  ) => {
    if (!config) return;

    setConfig((prev) => {
      if (!prev) return prev;

      const sourceConfig = { ...prev[sourceType] };
      const typeConfig = sourceConfig[calibrationType];

      if (typeConfig) {
        sourceConfig[calibrationType] = {
          ...typeConfig,
          [parameter]: {
            ...typeConfig[parameter as keyof CalibrationTypeConfig],
            ...updates,
          },
        };
      }

      return {
        ...prev,
        [sourceType]: sourceConfig,
      };
    });
  };

  const updateBehavioralOptions = (
    sourceType: string,
    field: string,
    value: boolean | string[]
  ) => {
    if (!config) return;

    setConfig((prev) => {
      if (!prev) return prev;

      const newBehavioralOptions = { ...prev.behavioral_options };
      if (!newBehavioralOptions[sourceType]) {
        newBehavioralOptions[sourceType] = {
          use_bias_for_dark_optimization: false,
          use_bias_if_no_darks: false,
          fallback_chain: [],
        };
      }

      newBehavioralOptions[sourceType] = {
        ...newBehavioralOptions[sourceType],
        [field]: value,
      };

      return {
        ...prev,
        behavioral_options: newBehavioralOptions,
      };
    });
  };

  const updateClusteringConfig = (
    calibrationType: string,
    field: string,
    value: number
  ) => {
    if (!config) return;

    setConfig((prev) => {
      if (!prev) return prev;

      const newClustering = { ...prev.clustering };
      if (!newClustering[calibrationType]) {
        newClustering[calibrationType] = {
          max_age_days: 30,
          time_cluster_minutes: 30,
          temp_threshold_celsius: 2.0,
        };
      }

      newClustering[calibrationType] = {
        ...newClustering[calibrationType],
        [field]: value,
      };

      return {
        ...prev,
        clustering: newClustering,
      };
    });
  };

  const updateMasterPreference = (
    calibrationType: string,
    preference: MasterPreference
  ) => {
    if (!config) return;

    setConfig((prev) => {
      if (!prev) return prev;

      return {
        ...prev,
        master_preferences: {
          ...prev.master_preferences,
          [calibrationType]: preference,
        },
      };
    });
  };

  const updateScoringConfig = (field: string, value: number) => {
    if (!config) return;

    setConfig((prev) => {
      if (!prev) return prev;

      return {
        ...prev,
        scoring: {
          ...prev.scoring,
          [field]: value,
        },
      };
    });
  };

  const updateWarningConfig = (field: string, value: number) => {
    if (!config) return;

    setConfig((prev) => {
      if (!prev) return prev;

      return {
        ...prev,
        warnings: {
          ...prev.warnings,
          [field]: value,
        },
      };
    });
  };

  if (loading) {
    return (
      <div className="text-center py-8 text-gray-400">
        <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-blue-500 mx-auto"></div>
        <p className="mt-4">Loading calibration config...</p>
      </div>
    );
  }

  if (!config) {
    return (
      <div className="p-4 bg-red-900/20 border border-red-800 rounded-lg">
        <p className="text-red-400">Failed to load calibration configuration</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {error && (
        <div className="p-4 bg-red-900/20 border border-red-800 rounded-lg flex items-start gap-3">
          <AlertCircle className="text-red-500 flex-shrink-0 mt-0.5" size={20} />
          <div className="flex-1">
            <p className="font-medium text-red-400">Error</p>
            <p className="text-sm text-red-300">{error}</p>
          </div>
        </div>
      )}

      {success && (
        <div className="p-4 bg-green-900/20 border border-green-800 rounded-lg flex items-start gap-3">
          <CheckCircle
            className="text-green-500 flex-shrink-0 mt-0.5"
            size={20}
          />
          <div className="flex-1">
            <p className="font-medium text-green-400">
              Configuration saved successfully
            </p>
          </div>
        </div>
      )}

      {/* Lights Section */}
      <div className="bg-gray-750 rounded-lg border border-gray-700">
        <button
          onClick={() => toggleSection("lights")}
          className="w-full px-4 py-3 flex items-center justify-between hover:bg-gray-700/50 rounded-t-lg"
        >
          <span className="font-semibold text-lg">For Lights</span>
          {expandedSections.lights ? (
            <ChevronDown size={20} />
          ) : (
            <ChevronRight size={20} />
          )}
        </button>
        {expandedSections.lights && (
          <div className="px-4 pb-4 space-y-4">
            <BehavioralOptionsPanel
              sourceType="lights"
              options={config.behavioral_options.lights}
              onUpdate={updateBehavioralOptions}
            />
            <MatchingMatrixTable
              sourceType="lights"
              sourceConfig={config.lights}
              onParameterUpdate={updateParameterConfig}
            />
          </div>
        )}
      </div>

      {/* Flats Section */}
      <div className="bg-gray-750 rounded-lg border border-gray-700">
        <button
          onClick={() => toggleSection("flats")}
          className="w-full px-4 py-3 flex items-center justify-between hover:bg-gray-700/50 rounded-t-lg"
        >
          <span className="font-semibold text-lg">For Flats</span>
          {expandedSections.flats ? (
            <ChevronDown size={20} />
          ) : (
            <ChevronRight size={20} />
          )}
        </button>
        {expandedSections.flats && (
          <div className="px-4 pb-4 space-y-4">
            <BehavioralOptionsPanel
              sourceType="flats"
              options={config.behavioral_options.flats}
              onUpdate={updateBehavioralOptions}
              showFallbackInfo
            />
            <MatchingMatrixTable
              sourceType="flats"
              sourceConfig={config.flats}
              onParameterUpdate={updateParameterConfig}
            />
          </div>
        )}
      </div>

      {/* Darks Section */}
      <div className="bg-gray-750 rounded-lg border border-gray-700">
        <button
          onClick={() => toggleSection("darks")}
          className="w-full px-4 py-3 flex items-center justify-between hover:bg-gray-700/50 rounded-t-lg"
        >
          <span className="font-semibold text-lg">For Darks</span>
          {expandedSections.darks ? (
            <ChevronDown size={20} />
          ) : (
            <ChevronRight size={20} />
          )}
        </button>
        {expandedSections.darks && (
          <div className="px-4 pb-4 space-y-4">
            <BehavioralOptionsPanel
              sourceType="darks"
              options={config.behavioral_options.darks}
              onUpdate={updateBehavioralOptions}
            />
            <MatchingMatrixTable
              sourceType="darks"
              sourceConfig={config.darks}
              onParameterUpdate={updateParameterConfig}
            />
          </div>
        )}
      </div>

      {/* Clustering Parameters */}
      <div className="bg-gray-750 rounded-lg border border-gray-700">
        <button
          onClick={() => toggleSection("clustering")}
          className="w-full px-4 py-3 flex items-center justify-between hover:bg-gray-700/50 rounded-t-lg"
        >
          <span className="font-semibold text-lg">
            Clustering Parameters & Thresholds
          </span>
          {expandedSections.clustering ? (
            <ChevronDown size={20} />
          ) : (
            <ChevronRight size={20} />
          )}
        </button>
        {expandedSections.clustering && (
          <div className="px-4 pb-4">
            <ClusteringParametersPanel
              clustering={config.clustering}
              scoring={config.scoring}
              onClusteringUpdate={updateClusteringConfig}
              onScoringUpdate={updateScoringConfig}
            />
          </div>
        )}
      </div>

      {/* Warning Thresholds */}
      <div className="bg-gray-750 rounded-lg border border-gray-700">
        <button
          onClick={() => toggleSection("warnings")}
          className="w-full px-4 py-3 flex items-center justify-between hover:bg-gray-700/50 rounded-t-lg"
        >
          <span className="font-semibold text-lg">Warning Thresholds</span>
          {expandedSections.warnings ? (
            <ChevronDown size={20} />
          ) : (
            <ChevronRight size={20} />
          )}
        </button>
        {expandedSections.warnings && (
          <div className="px-4 pb-4">
            <p className="text-sm text-gray-400 mb-4">
              Configure warning thresholds for calibration frame matching.
            </p>
            <div className="space-y-4">
              <div>
                <label className="block text-sm font-medium text-gray-300 mb-2">
                  Temperature Delta (°C)
                </label>
                <input
                  type="number"
                  value={config.warnings.temp_delta_celsius}
                  onChange={(e) =>
                    updateWarningConfig(
                      "temp_delta_celsius",
                      parseFloat(e.target.value) || 2.0
                    )
                  }
                  step="0.1"
                  min="0"
                  className="w-full bg-gray-700 border border-gray-600 rounded px-3 py-2 text-gray-100"
                />
                <p className="text-xs text-gray-500 mt-2">
                  Maximum temperature difference for calibration matching warning
                </p>
              </div>

              <div>
                <label className="block text-sm font-medium text-gray-300 mb-2">
                  Flat Date Warning (days)
                </label>
                <input
                  type="number"
                  value={config.warnings.flat_date_warning_days}
                  onChange={(e) =>
                    updateWarningConfig(
                      "flat_date_warning_days",
                      parseInt(e.target.value) || 30
                    )
                  }
                  min="1"
                  className="w-full bg-gray-700 border border-gray-600 rounded px-3 py-2 text-gray-100"
                />
                <p className="text-xs text-gray-500 mt-2">
                  Warn if flat frames are older than this many days
                </p>
              </div>

              <div>
                <label className="block text-sm font-medium text-gray-300 mb-2">
                  Dark Date Warning (days)
                </label>
                <input
                  type="number"
                  value={config.warnings.dark_date_warning_days}
                  onChange={(e) =>
                    updateWarningConfig(
                      "dark_date_warning_days",
                      parseInt(e.target.value) || 365
                    )
                  }
                  min="1"
                  className="w-full bg-gray-700 border border-gray-600 rounded px-3 py-2 text-gray-100"
                />
                <p className="text-xs text-gray-500 mt-2">
                  Warn if dark frames are older than this many days
                </p>
              </div>

              <div>
                <label className="block text-sm font-medium text-gray-300 mb-2">
                  DarkFlat Date Warning (days)
                </label>
                <input
                  type="number"
                  value={config.warnings.darkflat_date_warning_days}
                  onChange={(e) =>
                    updateWarningConfig(
                      "darkflat_date_warning_days",
                      parseInt(e.target.value) || 365
                    )
                  }
                  min="1"
                  className="w-full bg-gray-700 border border-gray-600 rounded px-3 py-2 text-gray-100"
                />
                <p className="text-xs text-gray-500 mt-2">
                  Warn if darkflat frames are older than this many days
                </p>
              </div>
            </div>
          </div>
        )}
      </div>

      {/* Scoring Configuration */}
      <div className="bg-gray-750 rounded-lg border border-gray-700">
        <button
          onClick={() => toggleSection("scoring")}
          className="w-full px-4 py-3 flex items-center justify-between hover:bg-gray-700/50 rounded-t-lg"
        >
          <span className="font-semibold text-lg">Scoring Configuration</span>
          {expandedSections.scoring ? (
            <ChevronDown size={20} />
          ) : (
            <ChevronRight size={20} />
          )}
        </button>
        {expandedSections.scoring && (
          <div className="px-4 pb-4">
            <p className="text-sm text-gray-400 mb-4">
              Configure how calibration matches are scored and ranked.
            </p>
            <div className="space-y-4">
              <div>
                <label className="block text-sm font-medium text-gray-300 mb-2">
                  Temperature Match Weight (0.0-1.0)
                </label>
                <input
                  type="number"
                  value={config.scoring.temperature_match_weight}
                  onChange={(e) =>
                    updateScoringConfig(
                      "temperature_match_weight",
                      parseFloat(e.target.value) || 0.3
                    )
                  }
                  step="0.1"
                  min="0"
                  max="1"
                  className="w-full bg-gray-700 border border-gray-600 rounded px-3 py-2 text-gray-100"
                />
                <p className="text-xs text-gray-500 mt-2">
                  Weight for temperature proximity in scoring (0=ignore, 1=maximum weight)
                </p>
              </div>

              <div>
                <label className="block text-sm font-medium text-gray-300 mb-2">
                  Temperature Scale (°C)
                </label>
                <input
                  type="number"
                  value={config.scoring.temperature_scale}
                  onChange={(e) =>
                    updateScoringConfig(
                      "temperature_scale",
                      parseFloat(e.target.value) || 2.0
                    )
                  }
                  step="0.5"
                  min="0.1"
                  className="w-full bg-gray-700 border border-gray-600 rounded px-3 py-2 text-gray-100"
                />
                <p className="text-xs text-gray-500 mt-2">
                  Temperature scaling factor for scoring formula (default 2.0°C)
                </p>
              </div>
            </div>
          </div>
        )}
      </div>

      {/* Master Preferences */}
      <div className="bg-gray-750 rounded-lg border border-gray-700">
        <button
          onClick={() => toggleSection("preferences")}
          className="w-full px-4 py-3 flex items-center justify-between hover:bg-gray-700/50 rounded-t-lg"
        >
          <span className="font-semibold text-lg">Master Preferences</span>
          {expandedSections.preferences ? (
            <ChevronDown size={20} />
          ) : (
            <ChevronRight size={20} />
          )}
        </button>
        {expandedSections.preferences && (
          <div className="px-4 pb-4">
            <p className="text-sm text-gray-400 mb-4">
              Choose whether to prefer Master calibration frames or frame sets
              when both are available.
            </p>
            <div className="grid grid-cols-2 gap-4">
              {["flat", "dark", "bias", "darkflat"].map((type) => (
                <div key={type}>
                  <label className="block text-sm font-medium text-gray-300 mb-2 capitalize">
                    {type}
                  </label>
                  <select
                    value={
                      config.master_preferences[type] ||
                      MasterPreference.NoPreference
                    }
                    onChange={(e) =>
                      updateMasterPreference(type, e.target.value as MasterPreference)
                    }
                    className="w-full bg-gray-700 border border-gray-600 rounded px-3 py-2 text-gray-100 text-sm"
                  >
                    <option value={MasterPreference.NoPreference}>
                      No Preference
                    </option>
                    <option value={MasterPreference.PreferMaster}>
                      Prefer Master
                    </option>
                    <option value={MasterPreference.PreferFrameset}>
                      Prefer Frameset
                    </option>
                  </select>
                </div>
              ))}
            </div>
          </div>
        )}
      </div>

      {/* Action Buttons */}
      <div className="flex items-center gap-4 pt-4">
        <button
          onClick={handleSave}
          disabled={saving}
          className="flex items-center gap-2 px-6 py-2 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-600 disabled:cursor-not-allowed text-white rounded-lg transition-colors"
        >
          <Save size={18} />
          {saving ? "Saving..." : "Save Configuration"}
        </button>

        <button
          onClick={handleReset}
          disabled={loading}
          className="flex items-center gap-2 px-6 py-2 bg-gray-600 hover:bg-gray-700 disabled:bg-gray-700 disabled:cursor-not-allowed text-white rounded-lg transition-colors"
        >
          <RefreshCw size={18} />
          Reset to Defaults
        </button>
      </div>
    </div>
  );
}
