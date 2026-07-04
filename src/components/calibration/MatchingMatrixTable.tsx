import type {
  MatchMode,
  ParameterConfig,
  SourceTypeConfig,
  CalibrationTypeConfig,
} from "../../types/calibration-config";
import {
  MatchModeValues,
  CONFIGURABLE_PARAMETERS,
  getParameterLabel,
  supportsWarningMode,
  validateThresholds,
} from "../../types/helpers";
import { Lock, AlertTriangle } from "lucide-react";

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

  const getModeColor = (mode: MatchMode, locked: boolean): string => {
    if (locked) {
      return "bg-surface-hover/40 text-content-muted border-border/50";
    }
    switch (mode) {
      case MatchModeValues.Exact:
        return "bg-success/30 text-success border-success/50";
      case MatchModeValues.Warning:
        return "bg-warning/30 text-warning border-warning/50";
      case MatchModeValues.Ignore:
        return "bg-surface-hover/30 text-content-muted border-border/50";
      default:
        return "bg-surface-hover/30 text-content-muted border-border/50";
    }
  };

  const renderModeCellContent = (
    calibrationType: "flat" | "darkflat" | "dark" | "bias",
    parameter: string,
    paramConfig: ParameterConfig
  ) => {
    const isLocked = paramConfig.locked;
    const canWarn = paramConfig.supports_warning || supportsWarningMode(parameter);
    const validationError = validateThresholds(paramConfig);

    const handleModeChange = (e: React.ChangeEvent<HTMLSelectElement>) => {
      if (isLocked) return;
      const newMode = e.target.value as MatchMode;
      onParameterUpdate(sourceType, calibrationType, parameter, { mode: newMode });
    };

    const handleWarningThresholdChange = (e: React.ChangeEvent<HTMLInputElement>) => {
      const value = parseFloat(e.target.value);
      if (!isNaN(value) && value >= 0) {
        onParameterUpdate(sourceType, calibrationType, parameter, {
          warning_threshold: value,
        });
      }
    };

    const handleMatchingThresholdChange = (e: React.ChangeEvent<HTMLInputElement>) => {
      const value = parseFloat(e.target.value);
      if (!isNaN(value) && value >= 0) {
        onParameterUpdate(sourceType, calibrationType, parameter, {
          matching_threshold: value,
        });
      }
    };

    // Build mode options based on parameter type
    // Using symbols: = (Exact), ≈ (Warning), - (Ignore)
    const getModeOptions = () => {
      if (isLocked) {
        return <option value={MatchModeValues.Exact}>=</option>;
      }
      if (canWarn) {
        return (
          <>
            <option value={MatchModeValues.Exact}>=</option>
            <option value={MatchModeValues.Warning}>≈</option>
            <option value={MatchModeValues.Ignore}>-</option>
          </>
        );
      }
      // Exact or disabled (no warning option)
      return (
        <>
          <option value={MatchModeValues.Exact}>=</option>
          <option value={MatchModeValues.Ignore}>-</option>
        </>
      );
    };

    return (
      <div className="flex flex-col gap-1">
        <div className="relative">
          <select
            value={paramConfig.mode}
            onChange={handleModeChange}
            disabled={isLocked}
            className={`w-full px-2 py-1 rounded text-xs border ${getModeColor(
              paramConfig.mode,
              isLocked
            )} ${isLocked ? "cursor-not-allowed opacity-75" : ""}`}
          >
            {getModeOptions()}
          </select>
          {isLocked && (
            <Lock
              size={10}
              className="absolute right-6 top-1/2 -translate-y-1/2 text-content-muted"
            />
          )}
        </div>
        {paramConfig.mode === MatchModeValues.Warning && (
          <div className="flex flex-col gap-1">
            <input
              type="number"
              value={paramConfig.warning_threshold ?? ""}
              onChange={handleWarningThresholdChange}
              placeholder="Warn"
              step="0.1"
              min="0"
              title="Warning threshold - triggers warning display"
              className="w-full px-1.5 py-0.5 bg-orange-950/40 border border-orange-700/50 rounded text-xs text-orange-200 placeholder-orange-400/50"
            />
            <input
              type="number"
              value={paramConfig.matching_threshold ?? ""}
              onChange={handleMatchingThresholdChange}
              placeholder="Max"
              step="0.1"
              min="0"
              title="Matching threshold - rejects match if exceeded"
              className="w-full px-1.5 py-0.5 bg-rose-950/40 border border-rose-700/50 rounded text-xs text-rose-200 placeholder-rose-400/50"
            />
            {validationError && (
              <div className="flex items-center gap-1 text-error text-xs">
                <AlertTriangle size={10} />
                <span>Warn &lt;= Max</span>
              </div>
            )}
          </div>
        )}
      </div>
    );
  };

  const getTypeConfig = (
    calibrationType: "flat" | "darkflat" | "dark" | "bias"
  ): CalibrationTypeConfig | null => {
    return sourceConfig[calibrationType] || null;
  };

  // Column width classes for specific parameters
  const getColumnWidth = (param: string): string => {
    switch (param) {
      case "filter":
        return "w-[85px]"; // Filter needs more space
      case "exptime":
        return "w-[75px]"; // Exposure can be narrower
      case "ccd_temp":
        return "w-[85px]"; // CCD Temp needs space for threshold inputs
      case "focallen":
        return "w-[85px]"; // Focal Length needs space for threshold inputs
      default:
        return ""; // Auto-size for other columns
    }
  };

  return (
    <div className="overflow-x-auto">
      <div className="rounded-lg border border-border overflow-hidden">
        <table className="w-full border-collapse text-sm table-fixed">
          <thead>
            <tr className="bg-surface-hover/50">
              <th className="p-2 border-b border-r border-border text-left font-medium w-[70px]">
                Type
              </th>
              {CONFIGURABLE_PARAMETERS.map((param, idx) => (
                <th
                  key={param}
                  className={`p-2 border-b border-border text-center font-medium ${
                    idx < CONFIGURABLE_PARAMETERS.length - 1 ? "border-r" : ""
                  } ${getColumnWidth(param)}`}
                >
                  <span className="flex items-center justify-center gap-1">
                    {getParameterLabel(param)}
                  </span>
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {types.map((calibType, rowIdx) => {
              const typeConfig = getTypeConfig(calibType);
              if (!typeConfig) return null;
              const isLastRow = rowIdx === types.length - 1;

              return (
                <tr key={calibType} className="hover:bg-surface-hover/30">
                  <td className={`p-2 border-r border-border font-medium capitalize ${
                    !isLastRow ? "border-b" : ""
                  }`}>
                    {calibType === "darkflat" ? "DarkFlat" : calibType}
                  </td>
                  {CONFIGURABLE_PARAMETERS.map((param, idx) => {
                    const paramConfig =
                      typeConfig[param as keyof CalibrationTypeConfig];
                    const isLastCol = idx === CONFIGURABLE_PARAMETERS.length - 1;
                    return (
                      <td
                        key={`${calibType}-${param}`}
                        className={`p-2 ${!isLastRow ? "border-b" : ""} ${
                          !isLastCol ? "border-r" : ""
                        } border-border`}
                      >
                        {renderModeCellContent(calibType, param, paramConfig)}
                      </td>
                    );
                  })}
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>

      {/* Legend and Explanation */}
      <div className="mt-3 p-3 bg-surface-elevated/50 rounded-lg border border-border space-y-3">
        {/* Compact legend */}
        <div className="flex flex-wrap gap-x-6 gap-y-2 text-xs text-content-muted">
          <div className="flex items-center gap-2">
            <span className="w-6 h-6 flex items-center justify-center bg-success/30 text-success border border-success/50 rounded font-bold">=</span>
            <span>Exact match</span>
          </div>
          <div className="flex items-center gap-2">
            <span className="w-6 h-6 flex items-center justify-center bg-warning/30 text-warning border border-warning/50 rounded font-bold">≈</span>
            <span>Threshold</span>
          </div>
          <div className="flex items-center gap-2">
            <span className="w-6 h-6 flex items-center justify-center bg-surface-hover/30 text-content-muted border border-border/50 rounded font-bold">-</span>
            <span>Ignored</span>
          </div>
        </div>

        {/* Detailed explanation */}
        <div className="text-xs text-content-muted space-y-1 pt-2 border-t border-border">
          <p>
            <span className="text-success font-medium">=</span> <strong>Exact</strong>:
            Parameters must match exactly. Equipment parameters (Camera, Binning, Gain, Offset)
            default to Exact match. Set to Ignore only if you intentionally mix equipment configurations.
          </p>
          <p>
            <span className="text-warning font-medium">≈</span> <strong>Threshold</strong>:
            Matches within the <span className="text-rose-300">Max</span> threshold, but shows a warning if the <span className="text-orange-300">Warn</span> threshold
            is exceeded. Match is rejected if outside <span className="text-rose-300">Max</span>.
          </p>
          <p>
            <span className="text-content-muted font-medium">-</span> <strong>Ignored</strong>:
            Parameter is not checked during matching (any value accepted).
          </p>
        </div>
      </div>
    </div>
  );
}
