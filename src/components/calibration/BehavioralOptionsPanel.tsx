import { BehavioralOptions } from "../../types/calibration-config";
import { Checkbox } from "../settings/Checkbox";

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
    <div className="bg-surface-elevated/50 rounded-lg p-4">
      <h4 className="text-sm font-medium text-content-secondary mb-3">Options</h4>
      <div className="space-y-3">
        {sourceType === "darks" && (
          <Checkbox
            checked={safeOptions.use_bias_for_dark_optimization}
            onChange={(checked) =>
              onUpdate(sourceType, "use_bias_for_dark_optimization", checked)
            }
            label="Use BIAS for Dark Optimization"
            description="Link Bias sets as sub-calibration to Dark sets"
          />
        )}

        {sourceType === "flats" && (
          <Checkbox
            checked={safeOptions.use_bias_if_no_darks}
            onChange={(checked) => onUpdate(sourceType, "use_bias_if_no_darks", checked)}
            label="Use BIAS if no Darks Found"
            description="Fallback to Bias if Dark calibration is not available"
          />
        )}

        {showFallbackInfo && safeOptions.fallback_chain.length > 0 && (
          <div className="mt-2 p-2 bg-surface-hover/50 rounded text-xs text-content-muted">
            <span className="font-medium">Fallback Chain:</span>{" "}
            {safeOptions.fallback_chain.map((type, i) => (
              <span key={type}>
                <span className="capitalize">{type}</span>
                {i < safeOptions.fallback_chain.length - 1 && " -> "}
              </span>
            ))}
            <p className="mt-1 text-content-muted">
              For Flats, the system will try DarkFlat first, then Dark, then
              Bias
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
