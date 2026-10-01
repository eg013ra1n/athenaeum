import { useEffect, useState } from 'react';
import { Button, Chip, ProgressBar } from '../../ui';
import { formatTimestamp } from '../../../utils/dateFormatting';
import { describeLastRun, MODE_LABEL, STAGE_TITLE } from './publishRunText';
import { STEP_ORDER, stepIndex, type PublishRunState } from './useCollabPublishRun';
import type { Segment } from './MyFramesTab';
import type { PublishRunKind, PublishStage } from '../../../types/models';

interface Step { label: string; stages: PublishStage[] }
const QUEUED: Step = { label: 'Queued', stages: ['queued'] };
const CALIBRATE: Step = { label: 'Calibrate', stages: ['calibrating'] };
const SEED: Step = { label: 'Seed', stages: ['seeding'] };
const ANNOUNCE: Step = { label: 'Announce', stages: ['announcing', 'versions'] };
/** Plan F9 — the steps each run kind shows. */
const STEPS: Record<PublishRunKind, Step[]> = {
  calibrate: [QUEUED, CALIBRATE],
  publish: [SEED, ANNOUNCE],
  republish: [QUEUED, CALIBRATE, SEED, ANNOUNCE],
  auto: [QUEUED, CALIBRATE, SEED, ANNOUNCE],
};
const WHY: Record<PublishRunKind, string> = {
  calibrate: 'you clicked Calibrate', publish: 'you clicked Publish', republish: 'you clicked Republish', auto: 'started automatically',
};
const ACTION: Record<Segment, string> = { review: 'Review →', published: 'Open Published →', held: 'Open Held back →', ready: 'Open Ready →' };

function formatElapsed(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, '0')}s`;
}

/** Spec 2026-10-01 §8.1 — the publish run of this project, above the segments. */
export default function PublishRunPanel({ run, onOpenSegment }: { run: PublishRunState; onOpenSegment: (s: Segment) => void }) {
  const r = run.running;
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!r) return undefined;
    const t = setInterval(() => setNow(Date.now()), 1000); // a display tick, not a poll
    return () => clearInterval(t);
  }, [r?.publishRunId]); // eslint-disable-line react-hooks/exhaustive-deps

  if (r) {
    const auto = r.trigger === 'auto';
    const queued = r.stage === 'queued';
    const title = queued
      ? STAGE_TITLE.queued
      : auto && r.mode
        ? `${MODE_LABEL[r.mode]} · ${STAGE_TITLE[r.stage].toLowerCase()} ${r.current} of ${r.total}`
        : `${STAGE_TITLE[r.stage]} ${r.current} of ${r.total}`;
    const at = STEP_ORDER[Math.max(0, run.reached)];
    return (
      <section aria-label="Publish run" className="mb-2.5 rounded-md border border-line bg-surface-elevated px-3 py-2.5">
        <div className="flex flex-wrap items-center gap-2">
          <Chip tone={auto ? 'pur' : 'info'}>{auto ? 'auto' : 'manual'}</Chip>
          <b className="text-[13px] font-semibold text-content">{title}</b>
          <span className="text-[12px] text-content-faint">{WHY[r.kind]}</span>
          <span className="flex-1" />
          <Button size="sm" onClick={() => void run.cancel()} disabled={run.cancelBusy}>Cancel</Button>
        </div>
        <ol className="mt-2 flex flex-wrap items-center gap-1.5 text-[12px]">
          {STEPS[r.kind].map((st, i, all) => {
            const active = st.stages.includes(at);
            const done = !active && Math.max(...st.stages.map(stepIndex)) < run.reached;
            return (
              <li key={st.label} aria-current={active ? 'step' : undefined}
                className={active ? 'font-semibold text-accent' : done ? 'text-success' : 'text-content-faint'}>
                {done ? `✓ ${st.label}` : active ? `${st.label} ${r.current} / ${r.total}` : st.label}
                {i < all.length - 1 && <span className="ml-1.5 text-content-faint">→</span>}
              </li>
            );
          })}
        </ol>
        <div className="my-1.5"><ProgressBar percent={r.total > 0 ? (100 * r.current) / r.total : 0} /></div>
        <div className="flex flex-wrap gap-3 text-[11.5px] text-content-muted">
          {r.currentFile && <span className="truncate font-mono">{r.currentFile}</span>}
          <span>{formatElapsed((now - Date.parse(r.startedAt)) / 1000)} elapsed</span>
          {queued && <span>one compute slot · other runs wait</span>}
        </div>
      </section>
    );
  }
  if (!run.last) return null;
  const d = describeLastRun(run.last);
  return (
    <section aria-label="Last publish run" className="mb-2.5 flex flex-wrap items-center gap-2 rounded-md border border-line px-3 py-2 text-[12px] text-content-muted">
      <Chip tone={d.tone === 'ok' ? 'ok' : d.tone === 'warn' ? 'warn' : 'err'}>{run.last.outcome}</Chip>
      <span className={d.tone === 'error' ? 'text-error' : undefined}>
        {d.text} · {formatTimestamp(run.last.finishedAt, { seconds: true })} · {run.last.trigger}
      </span>
      {d.segment && <Button variant="link" size="sm" onClick={() => onOpenSegment(d.segment!)}>{ACTION[d.segment]}</Button>}
    </section>
  );
}
