import { BehavioralOptions } from "../../types/calibration-config";

interface BehavioralOptionsPanelProps {
  sourceType: string;
  options?: BehavioralOptions;
  onUpdate: (sourceType: string, field: string, value: boolean | string[]) => void;
  showFallbackInfo?: boolean;
}

export default function BehavioralOptionsPanel({
  sourceType,
  options,
  onUpdate,
  showFallbackInfo = false,
}: BehavioralOptionsPanelProps) {
  // Don't render if no options apply to this source type
  // (only darks and flats have behavioral options)
  const hasOptions = sourceType === "darks" || sourceType === "flats";
  if (!hasOptions) return null;

  const safeOptions: BehavioralOptions = options || {
    use_bias_for_dark_optimization: false,
    use_bias_if_no_darks: true,
    fallback_chain: [],
  };

  return (
    <div className="bg-gray-800/50 rounded-lg p-4">
      <h4 className="text-sm font-medium text-gray-300 mb-3">Options</h4>
      <div className="space-y-3">
        {sourceType === "darks" && (
          <label className="flex items-center gap-3 cursor-pointer">
            <input
              type="checkbox"
              checked={safeOptions.use_bias_for_dark_optimization}
              onChange={(e) =>
                onUpdate(
                  sourceType,
                  "use_bias_for_dark_optimization",
                  e.target.checked
                )
              }
              className="w-4 h-4 rounded border-gray-600 bg-gray-700 text-blue-600 focus:ring-2 focus:ring-blue-500"
            />
            <div>
              <span className="text-sm text-gray-200">
                Use BIAS for Dark Optimization
              </span>
              <p className="text-xs text-gray-500">
                Link Bias sets as sub-calibration to Dark sets
              </p>
            </div>
          </label>
        )}

        {sourceType === "flats" && (
          <label className="flex items-center gap-3 cursor-pointer">
            <input
              type="checkbox"
              checked={safeOptions.use_bias_if_no_darks}
              onChange={(e) =>
                onUpdate(sourceType, "use_bias_if_no_darks", e.target.checked)
              }
              className="w-4 h-4 rounded border-gray-600 bg-gray-700 text-blue-600 focus:ring-2 focus:ring-blue-500"
            />
            <div>
              <span className="text-sm text-gray-200">
                Use BIAS if no Darks Found
              </span>
              <p className="text-xs text-gray-500">
                Fallback to Bias if Dark calibration is not available
              </p>
            </div>
          </label>
        )}

        {showFallbackInfo && safeOptions.fallback_chain.length > 0 && (
          <div className="mt-2 p-2 bg-gray-700/50 rounded text-xs text-gray-400">
            <span className="font-medium">Fallback Chain:</span>{" "}
            {safeOptions.fallback_chain.map((type, i) => (
              <span key={type}>
                <span className="capitalize">{type}</span>
                {i < safeOptions.fallback_chain.length - 1 && " -> "}
              </span>
            ))}
            <p className="mt-1 text-gray-500">
              For Flats, the system will try DarkFlat first, then Dark, then
              Bias
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
