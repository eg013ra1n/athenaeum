import { CheckCircle2, AlertTriangle, RotateCw } from 'lucide-react';
import type { LightFrameReadiness } from '../../types/models';

/** Human label for a single dark/flat/bias link classification. */
function linkLabel(v: string): string {
  switch (v) {
    case 'master': return 'master';
    case 'rawSet': return 'raw set (master will be built)';
    default: return 'missing';
  }
}

/** Tooltip describing which masters back a frame's calibration status. Bias is
 *  intentionally described as optional — the raw-master-dark convention means a
 *  frame with a dark + flat master and no bias link is still fully ready. */
export function lightCalTooltip(frame: LightFrameReadiness): string {
  return [
    `Dark: ${linkLabel(frame.dark)}`,
    `Flat: ${linkLabel(frame.flat)}`,
    `Bias: ${linkLabel(frame.bias)} (optional — raw master darks already include bias)`,
  ].join('\n');
}

/**
 * Compact per-frame light-calibration status pill, fed by the parent's readiness
 * fetch (one call per set view, not per frame). Renders nothing meaningful when
 * no readiness row exists for the frame (shows an em dash placeholder).
 */
export function LightCalStatusBadge({ frame }: { frame: LightFrameReadiness | undefined }) {
  if (!frame) {
    return <span className="text-content-muted" title="No calibration status">—</span>;
  }

  const title = lightCalTooltip(frame);

  switch (frame.status) {
    case 'calibrated':
      return (
        <span
          className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-xs font-medium bg-success/15 text-success border border-success/40"
          title={title}
        >
          <CheckCircle2 size={11} />
          Calibrated
        </span>
      );
    case 'stale':
      return (
        <span
          className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-xs font-medium bg-warning/15 text-warning border border-warning/40"
          title={`Calibration is out of date — re-run to refresh.\n${title}`}
        >
          <RotateCw size={11} />
          Stale
        </span>
      );
    case 'partial':
      return (
        <span
          className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-xs font-medium bg-orange/15 text-orange border border-orange/40"
          title={`Calibrated with the masters that exist (some steps missing).\n${title}`}
        >
          <AlertTriangle size={11} />
          Partial
        </span>
      );
    default: // 'notCalibrated'
      return (
        <span
          className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-xs font-medium text-content-muted border border-border"
          title={`Not calibrated yet.\n${title}`}
        >
          Not calibrated
        </span>
      );
  }
}
