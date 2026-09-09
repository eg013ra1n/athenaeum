import type { ObservingGoal } from '../../types/models';

export function ObservingGoalForm({
  goal,
  busy,
  onChange,
  onSave,
  onCancel,
}: {
  goal: ObservingGoal;
  busy: boolean;
  onChange: (goal: ObservingGoal) => void;
  onSave: () => void;
  onCancel: () => void;
}) {
  const change = (patch: Partial<ObservingGoal>) => onChange({ ...goal, ...patch });
  const optionalNumber = (value: string) => (value === '' ? null : Number(value));
  return (
    <form
      onSubmit={e => {
        e.preventDefault();
        onSave();
      }}
      className="border border-border rounded p-3 my-3"
    >
      <fieldset disabled={busy} className="flex flex-wrap items-end gap-3 text-sm">
        <label>
          Exact filter name
          <input
            aria-label="Exact filter name"
            value={goal.filter}
            readOnly={goal.revision > 0}
            maxLength={256}
            onChange={e => change({ filter: e.target.value })}
            placeholder="Blank = unknown filter"
            className="block bg-surface border border-border rounded p-1"
          />
        </label>
        <label>
          Target hours
          <input
            aria-label="Target hours"
            type="number"
            min="0.000001"
            max={1e9 / 3600}
            step="any"
            required
            value={goal.targetSeconds / 3600}
            onChange={e => change({ targetSeconds: Number(e.target.value) * 3600 })}
            className="block bg-surface border border-border rounded p-1 w-28"
          />
        </label>
        <label>
          <input
            type="checkbox"
            checked={goal.requireAnalysis}
            onChange={e =>
              change(
                e.target.checked
                  ? { requireAnalysis: true }
                  : {
                      requireAnalysis: false,
                      maxFwhmPx: null,
                      maxEccentricity: null,
                      rejectTrailed: false,
                    },
              )
            }
          />{' '}
          Require valid star analysis
        </label>
        {goal.requireAnalysis && (
          <>
            <label>
              Max FWHM (pixels)
              <input
                aria-label="Max FWHM (pixels)"
                type="number"
                min="0.000001"
                step="any"
                value={goal.maxFwhmPx ?? ''}
                onChange={e => change({ maxFwhmPx: optionalNumber(e.target.value) })}
                className="block bg-surface border border-border rounded p-1 w-28"
              />
            </label>
            <label>
              Max eccentricity
              <input
                aria-label="Max eccentricity"
                type="number"
                min="0"
                max="1"
                step="any"
                value={goal.maxEccentricity ?? ''}
                onChange={e => change({ maxEccentricity: optionalNumber(e.target.value) })}
                className="block bg-surface border border-border rounded p-1 w-28"
              />
            </label>
            <label>
              <input
                type="checkbox"
                checked={goal.rejectTrailed}
                onChange={e => change({ rejectTrailed: e.target.checked })}
              />{' '}
              Reject trail flag
            </label>
          </>
        )}
        <button type="submit" className="text-accent">
          Save goal
        </button>
        <button type="button" onClick={onCancel}>
          Cancel
        </button>
      </fieldset>
      <p className="text-xs text-content-muted mt-2">
        Blank quality limits apply no threshold. FWHM is in image pixels: compare sampling/binning
        before choosing a limit. Missing or invalid analysis stays unknown when analysis is
        required.
      </p>
    </form>
  );
}
