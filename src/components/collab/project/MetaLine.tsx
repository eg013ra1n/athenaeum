import { useState } from 'react';
import { Monitor } from 'lucide-react';
import { api } from '../../../api';
import { useNotifications } from '../../../contexts/NotificationContext';
import { Button } from '../../ui';
import { deviceLabel } from './usePublishing';
import type { ProjectCard } from '../../../types/models';

/**
 * The project header's second row (spec 2026-09-30 §8 row 2): the publishing
 * device (A6), then the two per-project preferences as text toggles —
 * "Auto-publish on/off" and, for a member who receives, "Auto-replicate
 * on/off"; their explanations live in the tooltips.
 *
 * Both are LOCAL preferences (`set_project_auto_publish` /
 * `set_project_auto_replicate`; the hub never learns of them). A toggle writes,
 * then `onChanged` re-reads the card, so the word shown is always the stored
 * one — never optimistic. A failed write logs, then notifies; the word stays.
 *
 * `null` publishing device = nobody yet (the next device that publishes
 * becomes it) — never "this device".
 */
export default function MetaLine({
  card,
  canReceive,
  onChanged,
  onSwitchHere,
  switchBusy,
}: {
  card: ProjectCard;
  /** Auto-replication is role-gated in core exactly like the Library tab. */
  canReceive: boolean;
  onChanged: () => void;
  onSwitchHere: () => void;
  switchBusy: boolean;
}) {
  const { notify } = useNotifications();
  const [busy, setBusy] = useState<'publish' | 'replicate' | null>(null);

  const flip = async (which: 'publish' | 'replicate', enabled: boolean) => {
    setBusy(which);
    try {
      await api.invoke(which === 'publish' ? 'set_project_auto_publish' : 'set_project_auto_replicate', {
        projectId: card.projectId,
        enabled,
      });
      onChanged();
    } catch (err) {
      console.error(`[projects] set auto-${which} failed:`, err);
      notify({
        title: `Could not change auto-${which === 'publish' ? 'publish' : 'replicate'}`,
        detail: err instanceof Error ? err.message : String(err),
        kind: 'project',
        tone: 'warning',
        hasErrors: true,
      });
    } finally {
      setBusy(null);
    }
  };

  const toggle =
    'text-content-faint underline-offset-2 hover:text-accent hover:underline disabled:cursor-not-allowed disabled:opacity-45';

  return (
    <div className="mt-1.5 flex flex-wrap items-center gap-1.5 text-[12px] leading-[1.4] text-content-faint">
      <Monitor size={11} aria-hidden className="shrink-0" />
      <span className="min-w-0 break-words">
        {card.publishingHere
          ? 'Publishing from this device'
          : card.publishingDevice
            ? `Publishing from ${deviceLabel(card.publishingDevice.name)}`
            : 'Nobody is publishing to this project yet'}
      </span>
      {!card.publishingHere && card.publishingDevice && (
        <Button variant="link" size="sm" onClick={onSwitchHere} disabled={switchBusy}>
          Publish from here
        </Button>
      )}
      <span aria-hidden>·</span>
      <button
        type="button"
        className={toggle}
        disabled={busy !== null}
        onClick={() => void flip('publish', !card.autoPublish)}
        title="Passing frames publish automatically as scans, analysis and links change."
      >
        Auto-publish {card.autoPublish ? 'on' : 'off'}
      </button>
      {canReceive && (
        <>
          <span aria-hidden>·</span>
          <button
            type="button"
            className={toggle}
            disabled={busy !== null}
            onClick={() => void flip('replicate', !card.autoReplicate)}
            title="New approved contributions download automatically. Every member who has a frame helps distribute it."
          >
            Auto-replicate {card.autoReplicate ? 'on' : 'off'}
          </button>
        </>
      )}
    </div>
  );
}
