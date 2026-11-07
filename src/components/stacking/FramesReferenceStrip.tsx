// The reference strip above the Frames table (spec §6.5), two labelled rows: the frame the selected run aligned every
// other frame onto, with its group, weight, how it was chosen and a second-pass move each on a line of their own; then
// the frame the next run will align onto. Both come from data the caller already holds, so the strip needs no request
// of its own.
import { useMemo, type ReactNode } from 'react';
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
const NO_FRAME_TITLE = 'No reference frame is chosen';
/** A run that will not change any more: its summary, if it has none, is never coming. */
const TERMINAL_STATUSES = ['done', 'failed', 'cancelled'];

/** Whether a run registered every group onto its own reference, from the geometry its summary records or, for a run
 *  whose summary is not written yet, from its stored config; an unreadable config reads as co-registered, with a
 *  warning. */
function isNativeGeometry(summaryGeometry: string | null, configJson: string): boolean {
  if (summaryGeometry !== null) return summaryGeometry === 'native';
  try {
    const config = JSON.parse(configJson) as { registration?: { geometry?: string } };
    return config.registration?.geometry === 'native';
  } catch (err) {
    console.warn('[FramesReferenceStrip] run config unreadable:', err);
    return false;
  }
}

const FileName = ({ name }: { name: string }) => <span className="font-mono text-content">{name}</span>;

/** How a reference was chosen, in words: the run's and the plan's `auto` / `manual`. */
const howChosen = (mode: string) => (mode === 'manual' ? 'pinned by hand' : 'chosen automatically');

/** A value inside a sentence of the strip, set apart from the words around it. */
const Value = ({ children }: { children: ReactNode }) => <span className="text-content">{children}</span>;

/** What the selected run aligned onto, split into the strip's lines. */
interface RunReference {
  /** The frame, or why none is named. */
  frame: ReactNode;
  /** The second line, a sentence about the frame: the group it is in, its weight, how it was chosen. */
  about: ReactNode;
  /** The frame the second pass moved the reference away from; `null` when it did not move it. */
  movedFrom: string | null;
}

/** "This frame is in group <g>, weight <w>, chosen automatically", with whichever of the group and the weight are
 *  known; a reference with no frame named gets only how it is chosen. The group's own title keeps its `·`, so the
 *  sentence separates its facts with commas. */
function aboutTheFrame(named: boolean, group: string | null, weight: number | null, how: string): ReactNode {
  if (!named) return how;
  const w = weight === null ? null : <Value>{weight.toFixed(3)}</Value>;
  if (group !== null) {
    return (
      <>
        This frame is in group <Value>{group}</Value>
        {w !== null && <>, weight {w}</>}, {how}
      </>
    );
  }
  return w === null ? `This frame was ${how}` : <>This frame has weight {w}, {how}</>;
}

function runReference(
  runDetail: StackingRunDetail,
  groupLabel: (groupKey: string) => string,
  filenameOf: (runFrameId: number) => string | null,
): RunReference {
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

  // No reference named: a run still going has not picked one yet; a run that has ended never will.
  const notNamed = reference === null && !TERMINAL_STATUSES.includes(run.status) ? 'not picked yet' : 'not recorded';
  const switchedFrom = reference?.switchedFrom ?? null;
  return {
    frame: fileName === null ? notNamed : <FileName name={fileName} />,
    about: aboutTheFrame(
      fileName !== null,
      groupKey === undefined ? null : groupLabel(groupKey),
      reference?.weight ?? null,
      howChosen(reference?.mode ?? run.referenceMode),
    ),
    movedFrom: switchedFrom === null ? null : (filenameOf(switchedFrom) ?? `#${switchedFrom}`),
  };
}

/** One labelled row of the strip; `display: contents` keeps the label and the value in the strip's two columns. */
function Row({ testId, label, title, icon, children }: {
  testId: string;
  label: string;
  title: string;
  icon: ReactNode;
  children: ReactNode;
}) {
  return (
    <div data-testid={testId} className="contents">
      <dt className="self-start flex items-center gap-1.5 text-content-muted whitespace-nowrap" title={title}>
        <span className="w-3 shrink-0" aria-hidden="true">{icon}</span>
        {label}
      </dt>
      <dd className="m-0 min-w-0 space-y-0.5 break-words text-content-muted">{children}</dd>
    </div>
  );
}

export function FramesReferenceStrip({ runDetail, planReference, groupLabel, filenameOf }: FramesReferenceStripProps) {
  const summaryGeometry = runDetail?.summary?.config.registration.geometry ?? null;
  const configJson = runDetail?.run.configJson ?? null;
  const nativeGeometry = useMemo(
    () => (configJson === null ? false : isNativeGeometry(summaryGeometry, configJson)),
    [summaryGeometry, configJson],
  );

  const nextManual = planReference !== null && planReference.mode === 'manual';
  const nextFile = nextManual
    ? (planReference.filename ?? (planReference.frameId === null ? null : `#${planReference.frameId}`))
    : null;
  const nextWithoutFrame = nextManual && nextFile === null;
  // The error mark beside line 2: a manual reference that cannot be used.
  const markLabel = nextWithoutFrame
    ? NO_FRAME_TITLE
    : nextFile !== null && planReference !== null && !planReference.onDisk
      ? OFF_DISK_TITLE
      : null;

  const run = runDetail === null || nativeGeometry ? null : runReference(runDetail, groupLabel, filenameOf);

  return (
    <dl className="bg-surface-hover/40 rounded-lg px-3 py-2 text-xs m-0 grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1.5">
      {runDetail !== null && (
        <Row
          testId="reference-line-run"
          label={`Run #${runDetail.run.id} aligned onto`}
          title={`The registration reference: every frame of run #${runDetail.run.id} was aligned onto it`}
          icon={<Star size={12} className="text-accent" />}
        >
          {run === null ? (
            <div className="text-content">each group&rsquo;s own frame, named in its header</div>
          ) : (
            <>
              <div>{run.frame}</div>
              <div>{run.about}</div>
              {run.movedFrom !== null && (
                <div>
                  The second pass moved it here from <FileName name={run.movedFrom} />, which sat further from the
                  median framing
                </div>
              )}
            </>
          )}
        </Row>
      )}
      <Row
        testId="reference-line-next"
        label="Next run aligns onto"
        title="The frame the next run will align every frame onto"
        icon={null}
      >
        <div className="flex items-center gap-1.5">
          <span className="min-w-0">
            {nextWithoutFrame ? (
              'a frame pinned by hand, but none is chosen'
            ) : nextFile === null ? (
              'a frame it picks automatically when it starts'
            ) : (
              <>
                <FileName name={nextFile} />
                {SEP}
                {howChosen('manual')}
              </>
            )}
          </span>
          {markLabel !== null && (
            <span title={markLabel} role="img" aria-label={markLabel} className="shrink-0 text-error">
              <XCircle size={12} aria-hidden="true" />
            </span>
          )}
        </div>
      </Row>
    </dl>
  );
}
