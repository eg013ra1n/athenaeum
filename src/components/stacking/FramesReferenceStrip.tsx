// The two-line reference strip above the Frames table (spec §6.5). Line 1 names the frame the selected run registered
// onto; line 2 names the reference the next run will use. Both come from data the caller already holds, so the strip
// needs no request of its own.
import type { ReactNode } from 'react';
import { Star, XCircle } from 'lucide-react';
import type { PlanReference, StackingRunDetail } from '../../types/stacking';

export interface FramesReferenceStripProps {
  /** The selected run; `null` when no run is selected (line 1 is then left out). */
  runDetail: StackingRunDetail | null;
  /** The plan's reference for the next run; `null` while no plan is loaded (read as Auto). */
  planReference: PlanReference | null;
  /** The display name of a run group, from its group key. */
  groupLabel: (groupKey: string) => string;
  /** The file name of a run frame id; `null` when the caller cannot name it. */
  filenameOf: (runFrameId: number) => string | null;
}

const SEP = ' · ';
const OFF_DISK_TITLE = 'The reference file is not on this device';

/** Whether the run registered every group onto its own reference. A run whose summary is not written yet carries the
 *  geometry in its stored config; an unreadable config reads as co-registered, with a warning. */
function isNativeGeometry(runDetail: StackingRunDetail): boolean {
  if (runDetail.summary !== null) return runDetail.summary.config.registration.geometry === 'native';
  try {
    const config = JSON.parse(runDetail.run.configJson) as { registration?: { geometry?: string } };
    return config.registration?.geometry === 'native';
  } catch (err) {
    console.warn('[FramesReferenceStrip] run config unreadable:', err);
    return false;
  }
}

const File = ({ name }: { name: string }) => <span className="font-mono">{name}</span>;

/** The parts of line 1 after the run id, in order; `null` for a run with no reference to name. */
function runLineParts(
  runDetail: StackingRunDetail,
  groupLabel: (groupKey: string) => string,
  filenameOf: (runFrameId: number) => string | null,
): ReactNode[] {
  if (isNativeGeometry(runDetail)) return ['per group'];

  const { summary, groups, frames, run } = runDetail;
  const reference = summary?.reference ?? null;
  // A finished run names its reference in the summary; a run still in flight only has the row marked `reference`.
  const refRow =
    reference === null
      ? frames.find((f) => f.regStatus === 'reference')
      : reference.frameId === null
        ? undefined
        : frames.find((f) => f.frameId === reference.frameId);
  const frameId = reference === null ? (refRow?.frameId ?? null) : reference.frameId;
  const fileName =
    reference?.filename ?? (frameId === null ? null : (filenameOf(frameId) ?? `#${frameId}`));
  const groupKey = refRow ? groups.find((g) => g.id === refRow.groupId)?.groupKey : undefined;

  const parts: ReactNode[] = [
    fileName === null ? (reference === null ? 'not picked yet' : 'not recorded') : <File key="file" name={fileName} />,
  ];
  if (groupKey !== undefined) parts.push(groupLabel(groupKey));
  if (reference?.weight != null) parts.push(`weight ${reference.weight.toFixed(3)}`);
  parts.push(reference?.mode ?? run.referenceMode);
  if (reference?.switchedFrom != null) {
    const previous = filenameOf(reference.switchedFrom);
    parts.push(
      <span key="switched">
        two-pass moved it from {previous === null ? `#${reference.switchedFrom}` : <File name={previous} />}
      </span>,
    );
  }
  return parts;
}

const joined = (parts: ReactNode[]): ReactNode[] =>
  parts.flatMap((part, i) => (i === 0 ? [part] : [SEP, part]));

export function FramesReferenceStrip({ runDetail, planReference, groupLabel, filenameOf }: FramesReferenceStripProps) {
  const nextFile =
    planReference !== null && planReference.mode === 'manual'
      ? (planReference.filename ?? (planReference.frameId === null ? null : `#${planReference.frameId}`))
      : null;

  return (
    <div className="bg-surface-hover/40 rounded-lg px-3 py-2 text-xs space-y-1">
      {runDetail !== null && (
        <div data-testid="reference-line-run" className="flex items-center gap-1.5 text-content">
          <Star size={12} className="shrink-0 text-accent" aria-hidden="true" />
          <span className="min-w-0 break-words">
            Registration reference, run #{runDetail.run.id}:{' '}
            {joined(runLineParts(runDetail, groupLabel, filenameOf))}
          </span>
        </div>
      )}
      <div data-testid="reference-line-next" className="flex items-center gap-1.5 text-content-muted">
        {runDetail !== null && <span className="w-3 shrink-0" aria-hidden="true" />}
        <span className="min-w-0 break-words">
          {nextFile === null ? (
            'Next run: auto, picked at run time'
          ) : (
            <>
              Next run: <File name={nextFile} />
              {SEP}manual
            </>
          )}
        </span>
        {nextFile !== null && planReference !== null && !planReference.onDisk && (
          <span title={OFF_DISK_TITLE} className="shrink-0 text-error">
            <XCircle size={12} aria-hidden="true" />
          </span>
        )}
      </div>
    </div>
  );
}
