import { ClusteringConfig, ScoringConfig } from "../../types/calibration-config";

interface ClusteringParametersPanelProps {
  // Partial, not Record: the generated CalibrationMatchingConfig types this
  // as a HashMap-backed optional-index map ({ [key in string]?: T }), which
  // is the accurate shape (a key can be genuinely absent) — Record<string, T>
  // over-promised every key was always present.
  clustering: Partial<Record<string, ClusteringConfig>>;
  scoring: ScoringConfig;
  onClusteringUpdate: (
    calibrationType: string,
    field: string,
    value: number
  ) => void;
  onScoringUpdate: (field: string, value: number) => void;
}

const calibrationTypes = ["flat", "dark", "bias", "darkflat"];

// Flats cluster by minutes (session-based), others cluster by days
const usesDaysForTimeCluster = (type: string) => type !== "flat";
const MINUTES_PER_DAY = 1440;

export default function ClusteringParametersPanel({
  clustering,
  scoring,
  onClusteringUpdate,
  onScoringUpdate,
}: ClusteringParametersPanelProps) {
  const getClusteringConfig = (type: string): ClusteringConfig => {
    // Defaults: flat = 30 days max age, 30 min cluster; dark/bias/darkflat = 365 days max age, 30 days cluster
    const isFlat = type === "flat";
    const defaultMaxAge = isFlat ? 30 : 365;
    const defaultTimeCluster = isFlat ? 30 : 43200; // 30 min for flat, 30 days (43200 min) for others
    return clustering[type] || { max_age_days: defaultMaxAge, time_cluster_minutes: defaultTimeCluster, temp_threshold_celsius: 2.0 };
  };

  return (
    <div className="space-y-6">
      {/* Clustering per calibration type */}
      <div>
        <h4 className="text-sm font-medium text-content-secondary mb-4">
          Clustering Settings per Calibration Type
        </h4>
        <div className="overflow-x-auto">
          <table className="w-full border-collapse text-sm">
            <thead>
              <tr className="bg-surface-hover/50">
                <th className="p-3 border border-border text-left font-medium">
                  Calibration Type
                </th>
                <th className="p-3 border border-border text-center font-medium">
                  Max Age (days)
                </th>
                <th className="p-3 border border-border text-center font-medium">
                  Time Cluster
                </th>
                <th className="p-3 border border-border text-center font-medium">
                  Temp Threshold (°C)
                </th>
              </tr>
            </thead>
            <tbody>
              {calibrationTypes.map((type) => {
                const config = getClusteringConfig(type);
                return (
                  <tr key={type} className="hover:bg-surface-hover/30">
                    <td className="p-3 border border-border font-medium capitalize">
                      {type === "darkflat" ? "DarkFlat" : type}
                    </td>
                    <td className="p-3 border border-border">
                      <input
                        type="number"
                        value={config.max_age_days}
                        onChange={(e) =>
                          onClusteringUpdate(
                            type,
                            "max_age_days",
                            parseInt(e.target.value) || 30
                          )
                        }
                        min="1"
                        className="w-full px-3 py-1 bg-surface-hover border border-border rounded text-content text-center"
                      />
                    </td>
                    <td className="p-3 border border-border">
                      <div className="flex items-center gap-2">
                        <input
                          type="number"
                          value={
                            usesDaysForTimeCluster(type)
                              ? Math.round(config.time_cluster_minutes / MINUTES_PER_DAY)
                              : config.time_cluster_minutes
                          }
                          onChange={(e) => {
                            const inputValue = parseInt(e.target.value) || 1;
                            const minutes = usesDaysForTimeCluster(type)
                              ? inputValue * MINUTES_PER_DAY
                              : inputValue;
                            onClusteringUpdate(type, "time_cluster_minutes", minutes);
                          }}
                          min="1"
                          className="flex-1 px-3 py-1 bg-surface-hover border border-border rounded text-content text-center"
                        />
                        <span className="text-xs text-content-muted w-12">
                          {usesDaysForTimeCluster(type) ? "days" : "min"}
                        </span>
                      </div>
                    </td>
                    <td className="p-3 border border-border">
                      <input
                        type="number"
                        value={config.temp_threshold_celsius}
                        onChange={(e) =>
                          onClusteringUpdate(
                            type,
                            "temp_threshold_celsius",
                            parseFloat(e.target.value) || 2.0
                          )
                        }
                        min="0.1"
                        step="0.1"
                        className="w-full px-3 py-1 bg-surface-hover border border-border rounded text-content text-center"
                      />
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
        <p className="text-xs text-content-muted mt-2">
          <strong>Max Age</strong> = Only consider frames within this many days
          |{" "}
          <strong>Time Cluster</strong> = Group frames captured within this time
          window (minutes for Flats, days for Darks/Bias/DarkFlats)
          |{" "}
          <strong>Temp Threshold</strong> = Split cluster if temperature differs by more than this value
        </p>
      </div>

      {/* Scoring parameters */}
      <div>
        <h4 className="text-sm font-medium text-content-secondary mb-4">
          Scoring Parameters
        </h4>
        <p className="text-xs text-content-muted mb-4">
          When linking calibration sets to frames, candidates are scored based on date proximity, temperature match, and exposure time proximity.
          These settings control how temperature and exposure time affect the scoring.
        </p>
        <div className="bg-surface-elevated/50 rounded-lg p-4 space-y-4">
          {/* Temperature Match Weight */}
          <div>
            <div className="flex items-center gap-4">
              <label className="text-sm text-content-secondary min-w-48">
                Temperature Match Weight
              </label>
              <input
                type="range"
                min="0"
                max="1"
                step="0.1"
                value={scoring.temperature_match_weight}
                onChange={(e) =>
                  onScoringUpdate(
                    "temperature_match_weight",
                    parseFloat(e.target.value)
                  )
                }
                className="flex-1"
              />
              <span className="text-sm text-content w-12 text-right">
                {scoring.temperature_match_weight.toFixed(1)}
              </span>
            </div>
            <p className="text-xs text-content-muted mt-1">
              How much temperature affects the score (0 = ignore temperature, 1 = full weight). Default: 0.3
            </p>
          </div>

          {/* Temperature Scale */}
          <div>
            <div className="flex items-center gap-4">
              <label className="text-sm text-content-secondary min-w-48">
                Temperature Sensitivity (°C)
              </label>
              <input
                type="range"
                min="1"
                max="10"
                step="0.5"
                value={scoring.temperature_scale}
                onChange={(e) =>
                  onScoringUpdate(
                    "temperature_scale",
                    parseFloat(e.target.value)
                  )
                }
                className="flex-1"
              />
              <span className="text-sm text-content w-12 text-right">
                {scoring.temperature_scale.toFixed(1)}
              </span>
            </div>
            <p className="text-xs text-content-muted mt-1">
              At this temperature difference, the temp score drops to 50%. Higher = more tolerant. Default: 2.0°C
            </p>
          </div>

          {/* Exposure Match Weight */}
          <div>
            <div className="flex items-center gap-4">
              <label className="text-sm text-content-secondary min-w-48">
                Exposure Match Weight
              </label>
              <input
                type="range"
                min="0"
                max="1"
                step="0.1"
                value={scoring.exposure_match_weight}
                onChange={(e) =>
                  onScoringUpdate(
                    "exposure_match_weight",
                    parseFloat(e.target.value)
                  )
                }
                className="flex-1"
              />
              <span className="text-sm text-content w-12 text-right">
                {scoring.exposure_match_weight.toFixed(1)}
              </span>
            </div>
            <p className="text-xs text-content-muted mt-1">
              How much exposure time affects the score (0 = ignore, 1 = full weight). Default: 0.4
            </p>
          </div>

          {/* Exposure Scale */}
          <div>
            <div className="flex items-center gap-4">
              <label className="text-sm text-content-secondary min-w-48">
                Exposure Sensitivity (s)
              </label>
              <input
                type="range"
                min="0.5"
                max="10"
                step="0.5"
                value={scoring.exposure_scale}
                onChange={(e) =>
                  onScoringUpdate(
                    "exposure_scale",
                    parseFloat(e.target.value)
                  )
                }
                className="flex-1"
              />
              <span className="text-sm text-content w-12 text-right">
                {scoring.exposure_scale.toFixed(1)}
              </span>
            </div>
            <p className="text-xs text-content-muted mt-1">
              At this exposure difference, the exposure score drops to 50%. Higher = more tolerant. Default: 1.0s
            </p>
          </div>
        </div>
      </div>
    </div>
  );
}
