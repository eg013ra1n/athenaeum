import { CheckCircle2, AlertTriangle, RotateCw } from 'lucide-react';
import type { LightFrameReadiness, LightCalDetails, LightCalParams } from '../../types/models';
import { formatTimestamp } from '../../utils/dateFormatting';

/** Human label for a single dark/flat/bias link classification. */
function linkLabel(v: string): string {
  switch (v) {
    case 'master': return 'master';
    case 'rawSet': return 'raw set (master will be built)';
    default: return 'missing';
  }
}

/** Human label for a flat-normalization statistic wire string. */
function flatNormModeLabel(mode: string): string {
  return mode === 'pixinsightTrimmed' ? 'full-frame trimmed mean' : 'central-third mean';
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
 * Recipe tooltip from the tracking-row truth (spec §12.1): the CALSTAT actually
 * recorded, the applied master filenames, the normalization mode + whether it
 * was applied, the Advanced params from `cal_params` (parsed — camelCase JSON,
 * `'{}'` on pre-feature rows), the calibrated-at timestamp, and a stale flag.
 */
export function lightCalRecipeTooltip(detail: LightCalDetails): string {
  let trim: number | undefined;
  let pedestal: number | undefined;
  try {
    const p = JSON.parse(detail.calParams) as Partial<LightCalParams>;
    if (typeof p.trimFraction === 'number') trim = p.trimFraction;
    if (typeof p.pedestalDn === 'number') pedestal = p.pedestalDn;
  } catch {
    /* '{}' or malformed → omit the param lines */
  }

  const lines: string[] = [
    `CALSTAT: ${detail.calstat || '—'}`,
    `Dark: ${detail.darkMaster ?? '—'}`,
    `Flat: ${detail.flatMaster ?? '—'}`,
    `Bias: ${detail.biasMaster ?? '—'}`,
    detail.flatNormApplied
      ? `Normalization: ${flatNormModeLabel(detail.flatNormMode)} (applied)`
      : 'Normalization: off (plain flat division)',
  ];
  if (pedestal != null && pedestal > 0) lines.push(`Pedestal: ${pedestal} DN`);
  if (trim != null && detail.flatNormApplied && detail.flatNormMode === 'pixinsightTrimmed') {
    lines.push(`Trim: ${trim} per tail`);
  }
  lines.push(`Calibrated: ${formatTimestamp(detail.calibratedAt)}`);
  lines.push(`Engine v${detail.engineVersion}`);
  if (detail.stale) lines.push('⚠ Out of date — re-run to refresh');
  return lines.join('\n');
}

/**
 * Compact per-frame light-calibration status pill, fed by the parent's readiness
 * fetch (one call per set view, not per frame). When a `detail` tracking row is
 * available, the hover tooltip exposes the full applied recipe (spec §12.1);
 * otherwise it falls back to the link-classification tooltip. Renders an em-dash
 * placeholder when neither is present.
 */
export function LightCalStatusBadge({
  frame,
  detail,
}: {
  frame: LightFrameReadiness | undefined;
  detail?: LightCalDetails;
}) {
  if (!frame && !detail) {
    return <span className="text-content-muted" title="No calibration status">—</span>;
  }

  // Recipe (tracking-row) tooltip wins when present; otherwise link classification.
  const baseTitle = detail
    ? lightCalRecipeTooltip(detail)
    : frame
      ? lightCalTooltip(frame)
      : '';
  const cursor = detail ? 'cursor-help' : '';

  // Status visuals come from readiness; when there's no readiness row but a
  // recipe exists, derive from the recipe's stale flag.
  const status = frame?.status ?? (detail?.stale ? 'stale' : 'calibrated');

  switch (status) {
    case 'calibrated':
      return (
        <span
          className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-xs font-medium bg-success/15 text-success border border-success/40 ${cursor}`}
          title={baseTitle}
        >
          <CheckCircle2 size={11} />
          Calibrated
        </span>
      );
    case 'stale':
      return (
        <span
          className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-xs font-medium bg-warning/15 text-warning border border-warning/40 ${cursor}`}
          title={`Calibration is out of date — re-run to refresh.\n${baseTitle}`}
        >
          <RotateCw size={11} />
          Stale
        </span>
      );
    case 'partial':
      return (
        <span
          className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-xs font-medium bg-orange/15 text-orange border border-orange/40 ${cursor}`}
          title={`Calibrated with the masters that exist (some steps missing).\n${baseTitle}`}
        >
          <AlertTriangle size={11} />
          Partial
        </span>
      );
    default: // 'notCalibrated'
      return (
        <span
          className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-xs font-medium text-content-muted border border-border ${cursor}`}
          title={`Not calibrated yet.\n${baseTitle}`}
        >
          Not calibrated
        </span>
      );
  }
}
