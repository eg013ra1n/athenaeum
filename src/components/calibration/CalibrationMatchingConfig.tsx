import { useState, useEffect } from "react";
import { api } from '../../api';
import {
  Save,
  RefreshCw,
  AlertCircle,
  CheckCircle,
  ChevronDown,
  ChevronRight,
} from "lucide-react";
import type {
  CalibrationMatchingConfig as ConfigType,
  CalibrationTypeConfig,
  MasterPreference,
  ParameterConfig,
} from "../../types/calibration-config";
import { MasterPreferenceValues } from "../../types/helpers";
import type { CameraStats } from "../../types/models";
import { useNotifications } from "../../contexts/NotificationContext";
import MatchingMatrixTable from "./MatchingMatrixTable";
import BehavioralOptionsPanel from "./BehavioralOptionsPanel";
import ClusteringParametersPanel from "./ClusteringParametersPanel";

export default function CalibrationMatchingConfig() {
  const { notify } = useNotifications();
  const [config, setConfig] = useState<ConfigType | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [refreshingAll, setRefreshingAll] = useState(false);
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
      const result = await api.invoke<ConfigType>("get_calibration_matching_config");
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

      await api.invoke("set_calibration_matching_config", { config });

      setSuccess(true);
      setTimeout(() => setSuccess(false), 3000);
    } catch (err) {
      setError(String(err));
      console.error("Failed to save calibration config:", err);
    } finally {
      setSaving(false);
    }
  };

  const handleRefreshAllCameras = async () => {
    try {
      setRefreshingAll(true);
      const cameras = await api.invoke<CameraStats[]>("get_equipment_cameras");
      let newSets = 0;
      for (const cam of cameras) {
        const result = await api.invoke<{ sets_created: number }>(
          "refresh_calibration_library_for_camera",
          { instrume: cam.instrume },
        );
        newSets += result.sets_created;
      }
      notify({
        title: `Calibration sets refreshed — ${cameras.length} camera${cameras.length === 1 ? "" : "s"}`,
        detail: newSets > 0 ? `${newSets} new sets after re-clustering` : "No regrouping needed",
        kind: "generic",
        tone: "success",
      });
    } catch (err) {
      console.error("[CalibrationMatchingConfig] refresh all cameras failed:", err);
      notify({
        title: "Calibration refresh failed",
        detail: String(err),
        kind: "generic",
        tone: "warning",
        hasErrors: true,
      });
    } finally {
      setRefreshingAll(false);
    }
  };

  const handleReset = async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await api.invoke<ConfigType>("reset_calibration_matching_config");
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
          use_bias_if_no_darks: true,
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
        // Defaults: flat = 30 days max age, 30 min cluster; dark/bias/darkflat = 365 days max age, 30 days cluster
        const isFlat = calibrationType === "flat";
        const defaultMaxAge = isFlat ? 30 : 365;
        const defaultTimeCluster = isFlat ? 30 : 43200; // 30 min for flat, 30 days (43200 min) for others
        newClustering[calibrationType] = {
          max_age_days: defaultMaxAge,
          time_cluster_minutes: defaultTimeCluster,
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
      <div className="text-center py-8 text-content-muted">
        <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-accent mx-auto"></div>
        <p className="mt-4">Loading calibration config...</p>
      </div>
    );
  }

  if (!config) {
    return (
      <div className="p-4 bg-error-muted border border-error/50 rounded-lg">
        <p className="text-error">Failed to load calibration configuration</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {error && (
        <div className="p-4 bg-error-muted border border-error/50 rounded-lg flex items-start gap-3">
          <AlertCircle className="text-error flex-shrink-0 mt-0.5" size={20} />
          <div className="flex-1">
            <p className="font-medium text-error">Error</p>
            <p className="text-sm text-error/80">{error}</p>
          </div>
        </div>
      )}

      {success && (
        <div className="p-4 bg-success-muted border border-success/50 rounded-lg flex items-start gap-3">
          <CheckCircle
            className="text-success flex-shrink-0 mt-0.5"
            size={20}
          />
          <div className="flex-1">
            <p className="font-medium text-success">
              Configuration saved successfully
            </p>
          </div>
        </div>
      )}

      {/* Lights Section */}
      <div className="bg-surface rounded-lg border border-border">
        <button
          onClick={() => toggleSection("lights")}
          className="w-full px-4 py-3 flex items-center justify-between hover:bg-surface-hover/50 rounded-t-lg"
        >
          <span className="font-semibold text-lg">For Lights</span>
          {expandedSections.lights ? (
            <ChevronDown size={20} />
          ) : (
            <ChevronRight size={20} />
          )}
        </button>
        {expandedSections.lights && (
          <div className="px-4 pt-2 pb-4 space-y-4">
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
      <div className="bg-surface rounded-lg border border-border">
        <button
          onClick={() => toggleSection("flats")}
          className="w-full px-4 py-3 flex items-center justify-between hover:bg-surface-hover/50 rounded-t-lg"
        >
          <span className="font-semibold text-lg">For Flats</span>
          {expandedSections.flats ? (
            <ChevronDown size={20} />
          ) : (
            <ChevronRight size={20} />
          )}
        </button>
        {expandedSections.flats && (
          <div className="px-4 pt-2 pb-4 space-y-4">
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
      <div className="bg-surface rounded-lg border border-border">
        <button
          onClick={() => toggleSection("darks")}
          className="w-full px-4 py-3 flex items-center justify-between hover:bg-surface-hover/50 rounded-t-lg"
        >
          <span className="font-semibold text-lg">For Darks</span>
          {expandedSections.darks ? (
            <ChevronDown size={20} />
          ) : (
            <ChevronRight size={20} />
          )}
        </button>
        {expandedSections.darks && (
          <div className="px-4 pt-2 pb-4 space-y-4">
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
      <div className="bg-surface rounded-lg border border-border">
        <button
          onClick={() => toggleSection("clustering")}
          className="w-full px-4 py-3 flex items-center justify-between hover:bg-surface-hover/50 rounded-t-lg"
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
          <div className="px-4 pt-2 pb-4">
            <ClusteringParametersPanel
              clustering={config.clustering}
              scoring={config.scoring}
              onClusteringUpdate={updateClusteringConfig}
              onScoringUpdate={updateScoringConfig}
            />
            <div className="mt-4 pt-4 border-t border-border flex items-center justify-between gap-4">
              <p className="text-xs text-content-muted">
                Clustering changes apply to newly scanned frames. To regroup the
                already-cataloged calibration frames with these settings, save
                first, then refresh all cameras. Masters and superseded sets are
                left untouched.
              </p>
              <button
                onClick={handleRefreshAllCameras}
                disabled={refreshingAll || saving}
                title="Re-cluster the existing calibration sets of every camera using the saved clustering settings"
                className="flex items-center gap-2 shrink-0 rounded-lg border border-border bg-surface-hover px-3 py-1.5 text-sm hover:brightness-110 disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <RefreshCw size={14} className={refreshingAll ? "animate-spin" : ""} />
                {refreshingAll ? "Refreshing…" : "Refresh All Calibration Sets"}
              </button>
            </div>
          </div>
        )}
      </div>

      {/* Date Warning Thresholds */}
      <div className="bg-surface rounded-lg border border-border">
        <button
          onClick={() => toggleSection("warnings")}
          className="w-full px-4 py-3 flex items-center justify-between hover:bg-surface-hover/50 rounded-t-lg"
        >
          <span className="font-semibold text-lg">Date Warning Thresholds</span>
          {expandedSections.warnings ? (
            <ChevronDown size={20} />
          ) : (
            <ChevronRight size={20} />
          )}
        </button>
        {expandedSections.warnings && (
          <div className="px-4 pt-2 pb-4">
            <p className="text-sm text-content-muted mb-4">
              Warn when calibration frames are older than these thresholds.
            </p>
            <div className="space-y-4">
              <div>
                <label className="block text-sm font-medium text-content-secondary mb-2">
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
                  className="w-full bg-surface-hover border border-border rounded px-3 py-2 text-content"
                />
                <p className="text-xs text-content-muted mt-2">
                  Warn if flat frames are older than this many days
                </p>
              </div>

              <div>
                <label className="block text-sm font-medium text-content-secondary mb-2">
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
                  className="w-full bg-surface-hover border border-border rounded px-3 py-2 text-content"
                />
                <p className="text-xs text-content-muted mt-2">
                  Warn if dark frames are older than this many days
                </p>
              </div>

              <div>
                <label className="block text-sm font-medium text-content-secondary mb-2">
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
                  className="w-full bg-surface-hover border border-border rounded px-3 py-2 text-content"
                />
                <p className="text-xs text-content-muted mt-2">
                  Warn if darkflat frames are older than this many days
                </p>
              </div>
            </div>
          </div>
        )}
      </div>

      {/* Master Preferences */}
      <div className="bg-surface rounded-lg border border-border">
        <button
          onClick={() => toggleSection("preferences")}
          className="w-full px-4 py-3 flex items-center justify-between hover:bg-surface-hover/50 rounded-t-lg"
        >
          <span className="font-semibold text-lg">Master Preferences</span>
          {expandedSections.preferences ? (
            <ChevronDown size={20} />
          ) : (
            <ChevronRight size={20} />
          )}
        </button>
        {expandedSections.preferences && (
          <div className="px-4 pt-2 pb-4">
            <p className="text-sm text-content-muted mb-4">
              Choose whether to prefer Master calibration frames or frame sets
              when both are available.
            </p>
            <div className="grid grid-cols-2 gap-4">
              {["flat", "dark", "bias", "darkflat"].map((type) => (
                <div key={type}>
                  <label className="block text-sm font-medium text-content-secondary mb-2 capitalize">
                    {type}
                  </label>
                  <select
                    value={
                      config.master_preferences[type] ||
                      MasterPreferenceValues.NoPreference
                    }
                    onChange={(e) =>
                      updateMasterPreference(type, e.target.value as MasterPreference)
                    }
                    className="w-full bg-surface-hover border border-border rounded px-3 py-2 text-content text-sm"
                  >
                    <option value={MasterPreferenceValues.NoPreference}>
                      No Preference
                    </option>
                    <option value={MasterPreferenceValues.PreferMaster}>
                      Prefer Master
                    </option>
                    <option value={MasterPreferenceValues.PreferFrameset}>
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
          className="flex items-center gap-2 px-6 py-2 bg-accent hover:bg-accent-hover disabled:bg-surface-hover disabled:cursor-not-allowed text-white rounded-lg transition-colors"
        >
          <Save size={18} />
          {saving ? "Saving..." : "Save Configuration"}
        </button>

        <button
          onClick={handleReset}
          disabled={loading}
          className="flex items-center gap-2 px-6 py-2 bg-surface-hover hover:brightness-110 disabled:opacity-50 disabled:cursor-not-allowed text-white rounded-lg transition-colors"
        >
          <RefreshCw size={18} />
          Reset to Defaults
        </button>
      </div>
    </div>
  );
}
