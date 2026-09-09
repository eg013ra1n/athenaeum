import type { ObservingGoal, ObservingProgress } from '../../types/models';

export function ObservingProgressTable({
  rows,
  busy,
  onEdit,
  onRemove,
}: {
  rows: ObservingProgress[];
  busy: boolean;
  onEdit: (row: ObservingProgress) => void;
  onRemove: (goal: ObservingGoal) => void;
}) {
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-sm mt-3">
        <thead>
          <tr className="text-left text-content-muted">
            <th>Filter</th>
            <th>Accepted / rejected / unknown</th>
            <th>Accepted integration</th>
            <th>Goal</th>
            <th>Completion</th>
            <th>Policy</th>
            <th>Actions</th>
          </tr>
        </thead>
        <tbody>
          {rows.map(row => (
            <tr key={row.filter} className="border-t border-border">
              <td className="py-2">{row.filter || 'Unknown filter'}</td>
              <td>
                {row.accepted} / {row.rejected} / {row.unknown}
              </td>
              <td>{(row.acceptedSeconds / 3600).toFixed(2)} h</td>
              <td>{row.goal ? `${(row.goal.targetSeconds / 3600).toFixed(2)} h` : 'Not set'}</td>
              <td>
                {row.goal
                  ? `${((100 * row.acceptedSeconds) / row.goal.targetSeconds).toFixed(1)}%`
                  : '—'}
              </td>
              <td>
                {row.goal?.requireAnalysis ? (
                  <>
                    Valid analysis
                    {row.goal.maxFwhmPx !== null && ` · FWHM ≤ ${row.goal.maxFwhmPx} px`}
                    {row.goal.maxEccentricity !== null &&
                      ` · eccentricity ≤ ${row.goal.maxEccentricity}`}
                    {row.goal.rejectTrailed && ' · no trail flag'}
                  </>
                ) : (
                  'Positive exposure time; no quality cut'
                )}
              </td>
              <td className="whitespace-nowrap">
                <button disabled={busy} onClick={() => onEdit(row)} className="text-accent mr-3">
                  {row.goal ? 'Edit goal' : 'Set goal'}
                </button>
                {row.goal && (
                  <button
                    disabled={busy}
                    onClick={() => onRemove(row.goal!)}
                    className="text-content-muted"
                  >
                    Remove goal
                  </button>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {!rows.length && (
        <p className="text-content-muted text-sm mt-2">
          No eligible catalog lights or saved goals in this field. Add a filter goal to track
          missing observations.
        </p>
      )}
    </div>
  );
}
