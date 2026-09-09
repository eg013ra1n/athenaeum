import { formatTimestamp } from '../../utils/dateFormatting';
import type { EquipmentCandidate, EquipmentEvidence } from '../../types/models';
import { FileLocationActions } from '../FileLocationActions';

export function EquipmentEvidenceTable({
  rows,
  busy,
  onConfirm,
  onClear,
}: {
  rows: EquipmentEvidence[];
  busy: boolean;
  onConfirm: (row: EquipmentEvidence, candidate: EquipmentCandidate) => void;
  onClear: (row: EquipmentEvidence) => void;
}) {
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-xs text-left">
        <thead>
          <tr className="border-b border-border">
            <th className="p-2">Solved frame</th>
            <th className="p-2">Evidence</th>
            <th className="p-2">Compatible configurations</th>
            <th className="p-2">Review</th>
          </tr>
        </thead>
        <tbody>
          {rows.map(row => (
            <tr key={row.frameId} className="border-b border-border">
              <td className="p-2 max-w-xs">
                <span className="break-all" title={row.path}>
                  {row.filename}
                </span>
                <FileLocationActions compact paths={[row.path]} />
              </td>
              <td className="p-2">
                {row.solvedScale.toFixed(4)} arcsec/px
                <br />
                {row.binningX ?? '?'}×{row.binningY ?? '?'} binning
                <br />
                <span className="text-content-muted">{formatTimestamp(row.solvedAt)}</span>
              </td>
              <td className="p-2">
                {row.candidates.length ? (
                  row.candidates.map(candidate => (
                    <div
                      key={candidate.profile.id}
                      className="flex items-center justify-between gap-3 py-1"
                    >
                      <span>
                        {candidate.profile.name}: {candidate.expectedScale.toFixed(4)} arcsec/px ·{' '}
                        {candidate.differencePercent.toFixed(2)}% difference
                      </span>
                      <button
                        disabled={busy}
                        onClick={() => onConfirm(row, candidate)}
                        className="text-accent hover:underline disabled:opacity-50"
                      >
                        Confirm match
                      </button>
                    </div>
                  ))
                ) : (
                  <span className="text-content-muted">
                    No compatible profile. Check camera, binning, resampling and tolerance.
                  </span>
                )}
              </td>
              <td className="p-2">
                {row.confirmedProfileId != null ? (
                  <>
                    <span className={row.confirmationStale ? 'text-warning' : 'text-success'}>
                      {row.confirmationStale ? 'Needs review' : 'Confirmed'} · #
                      {row.confirmedProfileId}
                    </span>
                    <button
                      disabled={busy}
                      onClick={() => onClear(row)}
                      className="block text-content-muted hover:text-content"
                    >
                      Clear confirmation
                    </button>
                  </>
                ) : (
                  'Unconfirmed'
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
