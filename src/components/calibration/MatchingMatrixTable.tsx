import {
  MatchMode,
  ParameterConfig,
  SourceTypeConfig,
  CalibrationTypeConfig,
  CONFIGURABLE_PARAMETERS,
  getParameterLabel,
} from "../../types/calibration-config";

interface MatchingMatrixTableProps {
  sourceType: "lights" | "flats" | "darks";
  sourceConfig: SourceTypeConfig;
  onParameterUpdate: (
    sourceType: "lights" | "flats" | "darks",
    calibrationType: "flat" | "darkflat" | "dark" | "bias",
    parameter: string,
    updates: Partial<ParameterConfig>
  ) => void;
}

const calibrationTypes: Record<string, ("flat" | "darkflat" | "dark" | "bias")[]> = {
  lights: ["flat", "dark", "bias"],
  flats: ["darkflat", "dark", "bias"],
  darks: ["bias"],
};

export default function MatchingMatrixTable({
  sourceType,
  sourceConfig,
  onParameterUpdate,
}: MatchingMatrixTableProps) {
  const types = calibrationTypes[sourceType] || [];

  const getModeColor = (mode: MatchMode): string => {
    switch (mode) {
      case MatchMode.Exact:
        return "bg-green-600/30 text-green-300 border-green-600/50";
      case MatchMode.Warning:
        return "bg-yellow-600/30 text-yellow-300 border-yellow-600/50";
      case MatchMode.Ignore:
        return "bg-gray-700/30 text-gray-400 border-gray-600/50";
      default:
        return "bg-gray-700/30 text-gray-400 border-gray-600/50";
    }
  };

  const renderModeCell = (
    calibrationType: "flat" | "darkflat" | "dark" | "bias",
    parameter: string,
    paramConfig: ParameterConfig
  ) => {
    const handleModeChange = (e: React.ChangeEvent<HTMLSelectElement>) => {
      const newMode = e.target.value as MatchMode;
      onParameterUpdate(sourceType, calibrationType, parameter, { mode: newMode });
    };

    const handleThresholdChange = (e: React.ChangeEvent<HTMLInputElement>) => {
      const value = parseFloat(e.target.value);
      if (!isNaN(value)) {
        onParameterUpdate(sourceType, calibrationType, parameter, {
          warning_threshold: value,
        });
      }
    };

    return (
      <td key={`${calibrationType}-${parameter}`} className="p-2 border border-gray-700">
        <div className="flex flex-col gap-1">
          <select
            value={paramConfig.mode}
            onChange={handleModeChange}
            className={`w-full px-2 py-1 rounded text-xs border ${getModeColor(
              paramConfig.mode
            )}`}
          >
            <option value={MatchMode.Exact}>Exact</option>
            <option value={MatchMode.Warning}>Warning</option>
            <option value={MatchMode.Ignore}>-</option>
          </select>
          {paramConfig.mode === MatchMode.Warning && (
            <input
              type="number"
              value={paramConfig.warning_threshold || ""}
              onChange={handleThresholdChange}
              placeholder="Threshold"
              step="0.1"
              className="w-full px-2 py-1 bg-gray-700 border border-gray-600 rounded text-xs text-gray-100"
            />
          )}
        </div>
      </td>
    );
  };

  const getTypeConfig = (
    calibrationType: "flat" | "darkflat" | "dark" | "bias"
  ): CalibrationTypeConfig | null => {
    return sourceConfig[calibrationType] || null;
  };

  return (
    <div className="overflow-x-auto">
      <table className="w-full border-collapse text-sm">
        <thead>
          <tr className="bg-gray-700/50">
            <th className="p-2 border border-gray-700 text-left font-medium">
              Type
            </th>
            {CONFIGURABLE_PARAMETERS.map((param) => (
              <th
                key={param}
                className="p-2 border border-gray-700 text-center font-medium"
              >
                {getParameterLabel(param)}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {types.map((calibType) => {
            const typeConfig = getTypeConfig(calibType);
            if (!typeConfig) return null;

            return (
              <tr key={calibType} className="hover:bg-gray-700/30">
                <td className="p-2 border border-gray-700 font-medium capitalize">
                  {calibType === "darkflat" ? "DarkFlat" : calibType}
                </td>
                {CONFIGURABLE_PARAMETERS.map((param) => {
                  const paramConfig =
                    typeConfig[param as keyof CalibrationTypeConfig];
                  return renderModeCell(calibType, param, paramConfig);
                })}
              </tr>
            );
          })}
        </tbody>
      </table>
      <p className="text-xs text-gray-500 mt-2">
        <strong>Exact</strong> = Must match exactly | <strong>Warning</strong> =
        Match but warn if threshold exceeded | <strong>-</strong> = Ignore
      </p>
    </div>
  );
}
