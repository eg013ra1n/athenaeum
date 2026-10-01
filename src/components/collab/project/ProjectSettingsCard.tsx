import { useState } from 'react';
import { api } from '../../../api';
import { useNotifications } from '../../../contexts/NotificationContext';
import { Button, Card, Chip, ProgressBar, Seg, StatusDot } from '../../ui';
import { Checkbox } from '../../settings/Checkbox';
import { formatTimestamp } from '../../../utils/dateFormatting';
import { deviceLabel } from './usePublishing';
import { describeLastRun, STAGE_TITLE } from './publishRunText';
import type { PublishRunState } from './useCollabPublishRun';
import type { ProjectCard, PublishMode } from '../../../types/models';

const MODES: { value: PublishMode; label: string; help: string }[] = [
  { value: 'manual', label: 'Manual', help: 'Nothing runs on its own. You calibrate, review and publish from My frames.' },
  {
    value: 'autoCalibrate',
    label: 'Auto-calibrate',
    help: 'New passing frames are calibrated after scans, analysis or new masters, then wait in To review until you publish them. Runs while Athenaeum is open and signed in to the hub.',
  },
  {
    value: 'automatic',
    label: 'Fully automatic',
    help: 'New passing frames are calibrated and published with no review. For a remote rig nobody watches. Runs while Athenaeum is open and signed in to the hub.',
  },
];

const HEADING = 'mb-1.5 text-[11px] uppercase tracking-[.04em] text-content-faint';
const HELP = 'mt-1 text-[12px] leading-[1.45] text-content-faint';

/** Spec 2026-10-01 §7.3 — the project's local settings, each with visible help. */
export default function ProjectSettingsCard({
  card,
  canReceive,
  run,
  liveState,
  onChanged,
  onSwitchHere,
  switchBusy,
  onOpenMyFrames,
}: {
  card: ProjectCard;
  canReceive: boolean;
  run: PublishRunState;
  liveState: string | null;
  onChanged: () => void;
  onSwitchHere: () => void;
  switchBusy: boolean;
  onOpenMyFrames: () => void;
}) {
  const { notify } = useNotifications();
  const [busy, setBusy] = useState<'mode' | 'replicate' | null>(null);

  const write = async (which: 'mode' | 'replicate', cmd: string, args: Record<string, unknown>) => {
    setBusy(which);
    try {
      await api.invoke(cmd, { projectId: card.projectId, ...args });
      onChanged();
    } catch (err) {
      console.error(`[projects] ${cmd} failed:`, err);
      notify({
        title: which === 'mode' ? 'Could not change the publishing mode' : 'Could not change auto-replicate',
        detail: err instanceof Error ? err.message : String(err),
        kind: 'project',
        tone: 'warning',
        hasErrors: true,
      });
    } finally {
      setBusy(null);
    }
  };

  const mode = MODES.find((m) => m.value === card.publishMode) ?? MODES[0]!;
  const paused = card.publishMode !== 'manual' && liveState === 'off';
  const r = run.running;

  return (
    <section aria-label="Project settings">
      <Card title="Project settings" subtitle="this device only">
        <div className="pb-2.5">
          <h3 className={HEADING}>Publishing device</h3>
          <div className="flex flex-wrap items-center gap-2 text-[13px] text-content">
            {card.publishingHere ? (
              <>
                <StatusDot state="live" /> This device · {deviceLabel(card.publishingDevice?.name)}
              </>
            ) : card.publishingDevice ? (
              <>
                {deviceLabel(card.publishingDevice.name)}
                <Button size="sm" onClick={onSwitchHere} disabled={switchBusy}>
                  Publish from this device
                </Button>
              </>
            ) : (
              'Nobody is publishing to this project yet'
            )}
          </div>
          <p className={HELP}>
            One device per account publishes to a project. Frames on your other devices are not published from them.
          </p>
        </div>

        <div className="border-t border-line py-2.5">
          <h3 className={HEADING}>Publishing</h3>
          <Seg
            options={MODES.map((m) => ({ value: m.value, label: m.label }))}
            value={card.publishMode}
            onChange={(v) => {
              if (v !== card.publishMode && busy === null) void write('mode', 'set_project_publish_mode', { mode: v });
            }}
          />
          <p className={HELP}>{mode.help}</p>
          <div className="mt-2 rounded-md border border-line bg-surface px-2.5 py-2 text-[12px] text-content-muted">
            {r ? (
              <>
                <div className="flex items-center gap-2">
                  <Chip tone="info">{r.trigger}</Chip>
                  <b className="font-semibold text-content">
                    {STAGE_TITLE[r.stage]} {r.current} / {r.total}
                  </b>
                  <span className="flex-1" />
                  <Button variant="link" size="sm" onClick={onOpenMyFrames}>
                    Open My frames →
                  </Button>
                </div>
                <div className="my-1.5">
                  <ProgressBar percent={r.total > 0 ? (100 * r.current) / r.total : 0} />
                </div>
                {r.currentFile && <div className="truncate font-mono text-[11.5px]">{r.currentFile}</div>}
              </>
            ) : paused ? (
              'Paused — collaboration is off'
            ) : run.last ? (
              <LastRunLine last={run.last} />
            ) : (
              'No run yet'
            )}
          </div>
        </div>

        {canReceive && (
          <div className="border-t border-line pt-2.5">
            <h3 className={HEADING}>Auto-replicate</h3>
            <Checkbox
              role="switch"
              checked={card.autoReplicate}
              disabled={busy !== null}
              label={card.autoReplicate ? 'On' : 'Off'}
              onChange={(v) => void write('replicate', 'set_project_auto_replicate', { enabled: v })}
            />
            <p className={HELP}>
              New approved contributions download to this device automatically. Every member who holds a frame helps
              distribute it.
            </p>
          </div>
        )}
      </Card>
    </section>
  );
}

function LastRunLine({ last }: { last: NonNullable<PublishRunState['last']> }) {
  const d = describeLastRun(last);
  return (
    <span>
      Last run ·{' '}
      <b className={d.tone === 'error' ? 'font-semibold text-error' : 'font-semibold text-content'}>{d.text}</b>
      {' · '}
      {formatTimestamp(last.finishedAt, { seconds: true })} · {last.trigger}
    </span>
  );
}
