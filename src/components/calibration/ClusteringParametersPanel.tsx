import { ClusteringConfig, ScoringConfig } from "../../types/calibration-config";

interface ClusteringParametersPanelProps {
  clustering: Record<string, ClusteringConfig>;
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
    return clustering[type] || { max_age_days: 30, time_cluster_minutes: 30, temp_threshold_celsius: 2.0 };
  };

  return (
    <div className="space-y-6">
      {/* Clustering per calibration type */}
      <div>
        <h4 className="text-sm font-medium text-gray-300 mb-4">
          Clustering Settings per Calibration Type
        </h4>
        <div className="overflow-x-auto">
          <table className="w-full border-collapse text-sm">
            <thead>
              <tr className="bg-gray-700/50">
                <th className="p-3 border border-gray-700 text-left font-medium">
                  Calibration Type
                </th>
                <th className="p-3 border border-gray-700 text-center font-medium">
                  Max Age (days)
                </th>
                <th className="p-3 border border-gray-700 text-center font-medium">
                  Time Cluster
                </th>
                <th className="p-3 border border-gray-700 text-center font-medium">
                  Temp Threshold (°C)
                </th>
              </tr>
            </thead>
            <tbody>
              {calibrationTypes.map((type) => {
                const config = getClusteringConfig(type);
                return (
                  <tr key={type} className="hover:bg-gray-700/30">
                    <td className="p-3 border border-gray-700 font-medium capitalize">
                      {type === "darkflat" ? "DarkFlat" : type}
                    </td>
                    <td className="p-3 border border-gray-700">
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
                        className="w-full px-3 py-1 bg-gray-700 border border-gray-600 rounded text-gray-100 text-center"
                      />
                    </td>
                    <td className="p-3 border border-gray-700">
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
                          className="flex-1 px-3 py-1 bg-gray-700 border border-gray-600 rounded text-gray-100 text-center"
                        />
                        <span className="text-xs text-gray-400 w-12">
                          {usesDaysForTimeCluster(type) ? "days" : "min"}
                        </span>
                      </div>
                    </td>
                    <td className="p-3 border border-gray-700">
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
                        className="w-full px-3 py-1 bg-gray-700 border border-gray-600 rounded text-gray-100 text-center"
                      />
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
        <p className="text-xs text-gray-500 mt-2">
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
        <h4 className="text-sm font-medium text-gray-300 mb-4">
          Scoring Parameters
        </h4>
        <div className="bg-gray-800/50 rounded-lg p-4">
          <div className="flex items-center gap-4">
            <label className="text-sm text-gray-300 min-w-48">
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
            <span className="text-sm text-gray-100 w-12 text-right">
              {scoring.temperature_match_weight.toFixed(1)}
            </span>
          </div>
          <p className="text-xs text-gray-500 mt-2">
            Weight given to temperature proximity when scoring calibration
            matches (0.0 = ignore, 1.0 = full weight). Typical value is 0.3.
          </p>
        </div>
      </div>
    </div>
  );
}
