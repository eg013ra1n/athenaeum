import { AlertTriangle } from 'lucide-react';

// Spec 2026-10-04 §5.2: advisory calibration sentences from export readiness;
// shown while the mode is Calibrated lights (export and frame-set send).
export function CalibrationWarnings({ warnings }: { warnings: string[] }) {
  if (warnings.length === 0) return null;
  return (
    <ul className="space-y-1" aria-label="Calibration warnings">
      {warnings.map((w, i) => (
        <li key={i} className="flex items-start gap-2 text-xs text-warning">
          <AlertTriangle size={12} className="mt-0.5 shrink-0" /> <span>{w}</span>
        </li>
      ))}
    </ul>
  );
}
