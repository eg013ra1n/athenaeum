import { Seg } from '../ui';
import { NumericField } from './NumericField';
import type { StackSubject } from './subject';
import type { ExposureMode, GroupingConfig, PlanGroup } from '../../types/stacking';

function formatExposure(totalExposureS: number): string {
  if (totalExposureS <= 0) return '0m';
  const hours = totalExposureS / 3600;
  if (hours >= 1) return `${hours.toFixed(1)}h`;
  const minutes = totalExposureS / 60;
  return `${minutes.toFixed(0)}m`;
}

/** Mirrors `stacking::register::SCALE_TOLERANCE` — the same tolerance the
 *  plan-time pixel-scale warning and the per-frame registration gate both
 *  read from the backend constant; there is no wire copy of the number
 *  itself, so it is pinned here. */
const SCALE_TOLERANCE = 1.25;

/** `0.78"` for a solve-measured group scale, `~0.78"` (with an explanatory
 *  title) when the group's own value rests on at least one header-implied
 *  member instead of a measured plate solve, `—` when the group has no
 *  scale at all. */
function formatScale(pixelScaleArcsec: number | null, scaleSource: PlanGroup['scaleSource']): { text: string; title?: string } {
  if (pixelScaleArcsec == null) return { text: '—' };
  const value = `${pixelScaleArcsec.toFixed(2)}"`;
  if (scaleSource === 'header') {
    return { text: `~${value}`, title: "from the header's focal length and pixel size" };
  }
  return { text: value };
}

/** Whether a group's own scale sits outside `[1 / SCALE_TOLERANCE,
 *  SCALE_TOLERANCE]` of the resolved reference — the same test the backend
 *  plan-time warning uses to decide whether to warn at all. */
function isFarFromReference(scaleRatioToReference: number | null): boolean {
  if (scaleRatioToReference == null) return false;
  return scaleRatioToReference > SCALE_TOLERANCE || scaleRatioToReference < 1 / SCALE_TOLERANCE;
}

const MODES: { value: ExposureMode; label: string }[] = [
  { value: 'seconds', label: 'Seconds' },
  { value: 'stops', label: 'Stops' },
  { value: 'equivalent', label: 'Equivalent t/N²' },
  { value: 'none', label: 'None' },
];

// The floors `resolve_config` holds the two thresholds to (`MIN_EXPOSURE_TOLERANCE_SEC`, `MIN_STOPS_WIDTH` in
// `stacking/config.rs`): below them two classes would print the same token. The field holds to them too, so a
// typed 0 reaches the config as the floor.
const MIN_EXPOSURE_TOLERANCE_SEC = 0.01;
const MIN_STOPS_WIDTH = 0.1;

const RECALIBRATE_HINT =
  "The tolerance is part of the calibration hash: changing it re-calibrates this set's lights on the next run.";
const EQUIVALENT_HINT =
  "Classes by t / N², N = FOCALLEN / APTDIA from each frame's header. Frames without both are grouped by seconds; the plan names how many.";

interface ExposureGroupingControlProps {
  grouping: GroupingConfig;
  disabled?: boolean;
  subjectKind: StackSubject['kind'];
  onPatchGrouping: (patch: Partial<GroupingConfig>) => void;
}

/** How a group's exposure classes are formed (spec §8, D1): the mode and that mode's threshold. A change patches
 *  `grouping`; the tab re-plans as for any config edit. */
function ExposureGroupingControl({ grouping, disabled, subjectKind, onPatchGrouping }: ExposureGroupingControlProps) {
  const { exposureMode: mode } = grouping;
  return (
    <div className="mb-2 space-y-1.5">
      <div className="flex flex-wrap items-end gap-x-3 gap-y-2">
        <div className="text-xs">
          <span className="block text-xs text-content-secondary mb-1">Exposure grouping</span>
          <Seg
            ariaLabel="Exposure grouping"
            options={MODES}
            value={mode}
            disabled={disabled}
            onChange={(m) => {
              if (m !== mode) onPatchGrouping({ exposureMode: m });
            }}
          />
        </div>
        {mode === 'seconds' && (
          <div className="w-28">
            <NumericField
              label="Tolerance (s)"
              value={grouping.exposureToleranceSec}
              onCommit={(n) => onPatchGrouping({ exposureToleranceSec: n })}
              min={MIN_EXPOSURE_TOLERANCE_SEC}
              step={0.5}
              disabled={disabled}
            />
          </div>
        )}
        {(mode === 'stops' || mode === 'equivalent') && (
          <div className="w-28">
            <NumericField
              label="Width (stops)"
              value={grouping.stopsWidth}
              onCommit={(n) => onPatchGrouping({ stopsWidth: n })}
              min={MIN_STOPS_WIDTH}
              step={0.1}
              disabled={disabled}
            />
          </div>
        )}
      </div>
      {/* Only a frame set's tolerance enters its calibration hash (`calibration_subtree`); a project's Prepare hash
          has no grouping in it. */}
      {subjectKind === 'frameSet' && mode === 'seconds' && (
        <p className="text-[11px] text-content-muted">{RECALIBRATE_HINT}</p>
      )}
      {mode === 'equivalent' && <p className="text-[11px] text-content-muted">{EQUIVALENT_HINT}</p>}
    </div>
  );
}

export interface GroupsTableProps {
  groups: PlanGroup[];
  /** The draft config's grouping: what the control shows and patches. */
  grouping: GroupingConfig;
  disabled?: boolean;
  /** Decides whether the tolerance hint applies: only a frame set's calibration hash carries it. */
  subjectKind: StackSubject['kind'];
  onPatchGrouping: (patch: Partial<GroupingConfig>) => void;
}

/** The plan's frame groups (spec §11.1), under the control that decides how their exposure classes are formed. */
export function GroupsTable({ groups, grouping, disabled, subjectKind, onPatchGrouping }: GroupsTableProps) {
  const control = (
    <ExposureGroupingControl
      grouping={grouping}
      disabled={disabled}
      subjectKind={subjectKind}
      onPatchGrouping={onPatchGrouping}
    />
  );

  if (groups.length === 0) {
    return (
      <>
        {control}
        <p className="text-sm text-content-muted px-1">No groups yet.</p>
      </>
    );
  }

  return (
    <div>
      {control}
      <div className="overflow-x-auto">
        <table className="w-full text-xs">
          <thead>
            <tr className="text-content-muted text-left border-b border-border">
              <th className="py-1.5 px-2 font-medium">Key</th>
              <th className="py-1.5 px-2 font-medium">Camera</th>
              <th className="py-1.5 px-2 font-medium">Colour</th>
              <th className="py-1.5 px-2 font-medium">Filter</th>
              <th className="py-1.5 px-2 font-medium">Bin</th>
              <th className="py-1.5 px-2 font-medium">Scale</th>
              <th className="py-1.5 px-2 font-medium">Exp</th>
              <th className="py-1.5 px-2 font-medium">Frames</th>
              <th className="py-1.5 px-2 font-medium">Exposure</th>
              <th className="py-1.5 px-2 font-medium">Cached</th>
            </tr>
          </thead>
          <tbody>
            {groups.map((g) => {
              const scale = formatScale(g.pixelScaleArcsec, g.scaleSource);
              const farRatio = isFarFromReference(g.scaleRatioToReference) ? g.scaleRatioToReference : null;
              return (
                <tr key={g.key} className="border-b border-border/50 last:border-0">
                  <td className="py-1.5 px-2 text-content font-mono">{g.key.split('__').map((part, i) => (<span key={i}>{i > 0 && <>__<wbr /></>}{part}</span>))}</td>
                  <td className="py-1.5 px-2 text-content-secondary">{g.cameras.length > 0 ? g.cameras.join(' + ') : '—'}</td>
                  <td className="py-1.5 px-2 text-content-secondary">{g.colorMode === 'osc' ? 'OSC' : 'Mono'}</td>
                  <td className="py-1.5 px-2 text-content-secondary">{g.filter ?? '—'}</td>
                  <td className="py-1.5 px-2 text-content-secondary tabular-nums">{g.binning}×{g.binning}</td>
                  <td className="py-1.5 px-2 text-content-secondary tabular-nums" title={scale.title}>
                    {scale.text}
                    {farRatio != null && (
                      <span className="ml-1 text-warning">×{farRatio.toFixed(1)}</span>
                    )}
                  </td>
                  <td className="py-1.5 px-2 text-content-secondary tabular-nums">{g.exposureClass}</td>
                  <td className="py-1.5 px-2 text-content-secondary tabular-nums">{g.includedCount}/{g.frameCount}</td>
                  <td className="py-1.5 px-2 text-content-secondary tabular-nums">{formatExposure(g.totalExposureS)}</td>
                  <td
                    className="py-1.5 px-2 text-content-muted tabular-nums whitespace-nowrap"
                    title={`${g.calibratedCached} calibrated frames and ${g.metricsCached} frame measurements cached`}
                  >
                    {g.calibratedCached} cal · {g.metricsCached} meas
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}
