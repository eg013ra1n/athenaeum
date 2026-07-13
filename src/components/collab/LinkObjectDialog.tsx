import { useCallback, useEffect, useState } from 'react';
import { Check, Link2, X } from 'lucide-react';
import { api } from '../../api';
import type { LinkSuggestion } from '../../types/models';

/**
 * Link/unlink frame sets to a project (spec §7 — linking is explicit, never
 * automatic). Suggestions are ranked by the backend: within-radius first, then
 * ascending distance from the project target.
 */
export default function LinkObjectDialog({
  projectId,
  onClose,
  onChanged,
}: {
  projectId: string;
  onClose: () => void;
  onChanged: () => void;
}) {
  const [suggestions, setSuggestions] = useState<LinkSuggestion[]>([]);
  const [busy, setBusy] = useState<number | null>(null);

  const load = useCallback(async () => {
    try {
      setSuggestions(
        await api.invoke<LinkSuggestion[]>('list_collab_link_suggestions', { projectId }),
      );
    } catch (err) {
      console.error('[projects] link suggestions failed:', err);
    }
  }, [projectId]);

  useEffect(() => {
    void load();
  }, [load]);

  const toggle = async (s: LinkSuggestion) => {
    setBusy(s.framesSetId);
    try {
      await api.invoke('set_collab_link', {
        projectId,
        framesSetId: s.framesSetId,
        linked: !s.alreadyLinked,
      });
      await load();
      onChanged();
    } catch (err) {
      console.error('[projects] link toggle failed:', err);
    } finally {
      setBusy(null);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
      onClick={onClose}
    >
      <div
        className="max-h-[80vh] w-[34rem] overflow-auto rounded-lg border border-border bg-surface p-4"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-3 flex items-center gap-2">
          <Link2 size={16} className="text-content-secondary" />
          <h2 className="font-medium text-content">Link an object</h2>
          <button
            onClick={onClose}
            className="ml-auto text-content-muted transition-colors hover:text-content"
            aria-label="Close"
          >
            <X size={16} />
          </button>
        </div>
        <p className="mb-3 text-xs text-content-muted">
          Frame sets nearest the project target come first. Linking is a catalog-only
          choice — each frame is still checked against the project&apos;s quality gate.
        </p>
        <ul className="space-y-1">
          {suggestions.map((s) => (
            <li
              key={s.framesSetId}
              className="flex items-center gap-2 rounded border border-border px-3 py-2 text-sm"
            >
              <span className="truncate text-content">{s.name ?? `Set #${s.framesSetId}`}</span>
              <span className="flex-shrink-0 text-xs text-content-muted">
                {s.lightCount} lights
              </span>
              {s.withinRadius ? (
                <span className="flex-shrink-0 rounded bg-accent/20 px-1.5 py-0.5 text-xs text-accent">
                  on target
                </span>
              ) : s.distanceDeg != null ? (
                <span className="flex-shrink-0 text-xs text-content-muted">
                  {s.distanceDeg.toFixed(1)}° away
                </span>
              ) : (
                <span className="flex-shrink-0 text-xs text-content-muted">no center</span>
              )}
              <button
                onClick={() => void toggle(s)}
                disabled={busy === s.framesSetId}
                className={`ml-auto flex-shrink-0 inline-flex items-center gap-1 rounded px-2 py-1 text-xs transition-colors disabled:opacity-50 ${
                  s.alreadyLinked
                    ? 'border border-border text-content-secondary hover:bg-surface-hover'
                    : 'bg-accent text-surface hover:bg-accent-hover'
                }`}
              >
                {s.alreadyLinked ? (
                  <>
                    <Check size={12} /> Linked
                  </>
                ) : (
                  'Link'
                )}
              </button>
            </li>
          ))}
          {suggestions.length === 0 && (
            <li className="py-2 text-sm text-content-muted">No frame sets to link yet.</li>
          )}
        </ul>
      </div>
    </div>
  );
}
