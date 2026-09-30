import { useCallback, useEffect, useState } from 'react';
import { Link2 } from 'lucide-react';
import { api } from '../../api';
import { useNotifications } from '../../contexts/NotificationContext';
import { Button, Chip, DialogShell } from '../ui';
import type { LinkSuggestion, LinkedSetView } from '../../types/models';

/**
 * Link/unlink frame sets to a project (spec §7 — linking is explicit, never
 * automatic). Suggestions are ranked by the backend: within-radius first, then
 * ascending distance from the project target.
 */
export default function LinkObjectDialog({
  projectId,
  links,
  onClose,
  onChanged,
}: {
  projectId: string;
  /** The project's current links — listed read-only above the suggestions
   *  (each row carries Unlink). */
  links: LinkedSetView[];
  onClose: () => void;
  onChanged: () => void;
}) {
  const { notify } = useNotifications();
  const [suggestions, setSuggestions] = useState<LinkSuggestion[]>([]);
  const [busy, setBusy] = useState<number | null>(null);

  const load = useCallback(async () => {
    try {
      setSuggestions(
        (await api.invoke<LinkSuggestion[] | null>('list_collab_link_suggestions', { projectId })) ?? [],
      );
    } catch (err) {
      console.error('[projects] link suggestions failed:', err);
    }
  }, [projectId]);

  useEffect(() => {
    void load();
  }, [load]);

  const setLinked = async (framesSetId: number, linked: boolean) => {
    setBusy(framesSetId);
    try {
      await api.invoke('set_collab_link', { projectId, framesSetId, linked });
      await load();
      onChanged();
    } catch (err) {
      console.error('[projects] link toggle failed:', err);
      notify({
        title: linked ? 'Could not link the frame set' : 'Could not unlink the frame set',
        detail: err instanceof Error ? err.message : String(err),
        kind: 'project',
        tone: 'warning',
        hasErrors: true,
      });
    } finally {
      setBusy(null);
    }
  };

  return (
    <DialogShell
      title={<span className="inline-flex items-center gap-2"><Link2 size={14} className="text-content-secondary" />Link an object</span>}
      size="md"
      onClose={onClose}
      footer={<Button onClick={onClose} data-autofocus>Done</Button>}
    >
      <p className="mb-3 text-[11.5px] text-content-faint">
        Frame sets nearest the project target come first. Linking is a catalog-only
        choice — each frame is still checked against the project&apos;s quality gate.
      </p>
      <div className="max-h-[60vh] overflow-auto">
        {links.length > 0 && (
          <div className="mb-3">
            <div className="mb-1 font-medium text-content-secondary">Linked objects</div>
            <ul className="space-y-1">
              {links.map((l) => (
                <li
                  key={l.framesSetId}
                  className="flex items-center gap-2 rounded border border-border px-3 py-2"
                >
                  <span className="truncate text-content">{l.name ?? `Set #${l.framesSetId}`}</span>
                  <span className="flex-shrink-0 text-[11.5px] text-content-faint">· {l.lightCount} lights</span>
                  {l.withinRadius ? (
                    <Chip tone="ok">on target</Chip>
                  ) : (
                    <Chip tone="warn">outside the target</Chip>
                  )}
                  <Button
                    variant="link"
                    size="sm"
                    className="ml-auto"
                    disabled={busy === l.framesSetId}
                    onClick={() => void setLinked(l.framesSetId, false)}
                  >
                    Unlink
                  </Button>
                </li>
              ))}
            </ul>
          </div>
        )}
        <ul className="space-y-1">
          {suggestions.filter((s) => !s.alreadyLinked).map((s) => (
            <li
              key={s.framesSetId}
              className="flex items-center gap-2 rounded border border-border px-3 py-2"
            >
              <span className="truncate text-content">{s.name ?? `Set #${s.framesSetId}`}</span>
              <span className="flex-shrink-0 text-[11.5px] text-content-faint">
                {s.lightCount} lights
              </span>
              {s.withinRadius ? (
                <Chip tone="ok">on target</Chip>
              ) : s.distanceDeg != null ? (
                <span className="flex-shrink-0 text-[11.5px] text-content-faint">
                  {s.distanceDeg.toFixed(1)}° away
                </span>
              ) : (
                <span className="flex-shrink-0 text-[11.5px] text-content-faint">no center</span>
              )}
              <Button
                variant="primary"
                size="sm"
                className="ml-auto flex-shrink-0"
                onClick={() => void setLinked(s.framesSetId, true)}
                disabled={busy === s.framesSetId}
              >
                Link
              </Button>
            </li>
          ))}
          {suggestions.every((s) => s.alreadyLinked) && (
            <li className="py-2 text-content-faint">No frame sets to link yet.</li>
          )}
        </ul>
      </div>
    </DialogShell>
  );
}
