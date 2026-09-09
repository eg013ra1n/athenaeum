import { useEffect, useRef, useState } from 'react';
import { api } from '../../api';
import type { ObservingGoal, ObservingProgress } from '../../types/models';
import { useNotifications } from '../../contexts/NotificationContext';
import { ObservingGoalForm } from './ObservingGoalForm';
import { ObservingProgressTable } from './ObservingProgressTable';

/** Edit per-filter catalog goals and review counts for the current field. */
export function ObservingGoalsPanel({ frameSetId }: { frameSetId: number }) {
  const [rows, setRows] = useState<ObservingProgress[]>([]);
  const [draft, setDraft] = useState<ObservingGoal | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  // Discard obsolete results when the field changes or another refresh starts.
  const generation = useRef(0);
  const { notify } = useNotifications();
  const report = (error: unknown) => {
    console.error('[ObservingGoals]', error);
    setError(String(error));
  };
  const load = async () => {
    const token = ++generation.current;
    setBusy(true);
    setError('');
    try {
      const result = await api.invoke<ObservingProgress[]>('get_observing_progress', {
        frameSetId,
      });
      if (token === generation.current) setRows(result);
    } catch (error) {
      console.error('[ObservingGoals]', error);
      if (token === generation.current) setError(String(error));
    } finally {
      if (token === generation.current) setBusy(false);
    }
  };
  useEffect(() => {
    void load();
    return () => {
      generation.current++;
    };
  }, [frameSetId]);
  const mutate = async (command: string, args: Record<string, unknown>, title: string) => {
    setBusy(true);
    setError('');
    try {
      await api.invoke(command, args);
      setDraft(null);
      await load();
      notify({
        title,
        kind: 'generic',
        tone: 'success',
        detail: 'Observing goals updated in this catalog.',
      });
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  };
  const create = (filter = ''): ObservingGoal => ({
    frameSetId,
    filter,
    revision: 0,
    targetSeconds: 3600,
    requireAnalysis: false,
    maxFwhmPx: null,
    maxEccentricity: null,
    rejectTrailed: false,
  });
  return (
    <details className="bg-surface-elevated border border-border rounded-lg p-3 mb-4">
      <summary className="cursor-pointer font-semibold">
        Observing goals & integration progress
      </summary>
      <p className="text-xs text-content-muted my-2">
        Set a goal for each field/filter. Counts use one representative per confirmed exposure
        within this field; integrated products and black-holed files are excluded. Unconfirmed
        versions can still count separately.
      </p>
      <p className="text-xs text-content-muted mb-2">
        Quality applies to the representative’s saved analysis, without choosing a better-scoring
        version. Catalog measurements may be outdated; refresh after analysis or membership changes.
        Overlapping fields must not be summed. Integration completion does not measure sky-area
        coverage.
      </p>
      <div className="flex gap-4 text-sm">
        <button disabled={busy} onClick={() => setDraft(create())} className="text-accent">
          Add filter goal
        </button>
        <button disabled={busy} onClick={() => void load()} className="text-accent">
          {busy ? 'Loading…' : 'Refresh progress'}
        </button>
      </div>
      {error && (
        <p role="alert" className="text-error text-sm">
          {error}
        </p>
      )}
      {draft && (
        <ObservingGoalForm
          goal={draft}
          busy={busy}
          onChange={setDraft}
          onCancel={() => setDraft(null)}
          onSave={() => void mutate('save_observing_goal', { goal: draft }, 'Observing goal saved')}
        />
      )}
      <ObservingProgressTable
        rows={rows}
        busy={busy}
        onEdit={row => setDraft(row.goal ?? create(row.filter))}
        onRemove={goal =>
          void mutate(
            'delete_observing_goal',
            { frameSetId, filter: goal.filter, revision: goal.revision },
            'Observing goal removed',
          )
        }
      />
    </details>
  );
}
