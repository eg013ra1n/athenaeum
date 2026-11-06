// The project's canonical grid, above the pipeline board (wave 6c Task 7, spec §6.3): the one registration target
// every processor's run shares. An editor (`canEdit`: `project.edit`) proposes, confirms and changes it here; every
// other member reads it, or waits for the coordinator to confirm one.

import { useEffect, useState } from 'react';
import { api } from '../../api';
import { Button } from '../ui/Button';
import { Card } from '../ui/Card';
import { KV } from '../ui/KV';
import { useNotifications } from '../../contexts/NotificationContext';
import { GridDialog, errorText } from './GridDialog';
import { GridSchematic } from './GridSchematic';
import { gridValuesLine } from './gridProjection';
import type { CanonicalGrid } from '../../types/models';
import type { PlanGrid, PlanGridTarget } from '../../types/stacking';

export interface GridCardProps {
  projectId: string;
  grid: PlanGrid;
  /** The grid was saved: the tab re-plans (the feed's `collab-grid-changed` re-plans again, harmlessly). */
  onSaved: () => void;
}

interface OpenDialog {
  initial: CanonicalGrid;
  warnRewrite: boolean;
}

/** `Edit…` with no proposal starts from the project's target: its centre, 1″/px, 4096 × 4096, no rotation. */
function gridFromTarget(target: PlanGridTarget): CanonicalGrid {
  return {
    scaleArcsec: 1,
    widthPx: 4096,
    heightPx: 4096,
    crval1: ((target.raDeg % 360) + 360) % 360,
    crval2: Math.min(90, Math.max(-90, target.decDeg)),
    rotationDeg: 0,
  };
}

export function GridCard({ projectId, grid, onSaved }: GridCardProps) {
  const { notify } = useNotifications();
  const [dialog, setDialog] = useState<OpenDialog | null>(null);
  const [busy, setBusy] = useState(false);
  const [proposalError, setProposalError] = useState<string | null>(null);

  const { proposal, current, canEdit } = grid;
  const confirmed = grid.state === 'confirmed' && current !== null;
  // The grid the schematic draws: the confirmed one, else the proposal.
  const shown = confirmed ? current : proposal;

  // A refusal of `Use proposal` is about the card as it was: a grid someone else confirmed makes it stale.
  useEffect(() => {
    if (confirmed) setProposalError(null);
  }, [confirmed]);

  const save = async (g: CanonicalGrid) => {
    await api.invoke<CanonicalGrid>('set_collab_project_grid', { projectId, grid: g });
    // Any save that went through, from the dialog too, settles the refusal the card was showing.
    setProposalError(null);
    onSaved();
  };

  const applyProposal = async () => {
    if (proposal === null || busy) return;
    setBusy(true);
    setProposalError(null);
    try {
      await save(proposal);
    } catch (err) {
      console.error('[GridCard] set_collab_project_grid failed:', err);
      const detail = errorText(err);
      setProposalError(detail);
      notify({ title: 'Grid not saved', detail, tone: 'warning', hasErrors: true });
    } finally {
      setBusy(false);
    }
  };

  const facts: [string, string][] = [];
  if (confirmed) {
    facts.push(['Grid', gridValuesLine(current)]);
  } else if (proposal !== null) {
    facts.push(['Proposal', gridValuesLine(proposal)]);
    if (canEdit) facts.push([
        'Frames',
        grid.framesInside === 1
          ? '1 accepted frame lies at least half inside'
          : `${grid.framesInside} accepted frames lie at least half inside`,
      ]);
  }

  return (
    <Card title={confirmed ? 'Canonical grid' : 'Canonical grid — not confirmed'}>
      <div className="space-y-2.5">
        {!confirmed && !canEdit && (
          <p className="text-[12px] text-content-muted">Waiting for the coordinator to confirm the grid</p>
        )}
        {!confirmed && canEdit && proposal === null && <p className="text-[12px] text-content-muted">No proposal yet.</p>}
        {facts.length > 0 && <KV items={facts} />}

        <GridSchematic grid={shown} target={grid.target} footprints={grid.footprints} framesNotDrawn={grid.framesNotDrawn} />

        {proposalError !== null && (
          <p role="alert" className="text-xs text-error">
            {proposalError}
          </p>
        )}

        {canEdit && (
          <div className="flex flex-wrap justify-end gap-2">
            {confirmed ? (
              <Button onClick={() => setDialog({ initial: current, warnRewrite: true })} disabled={busy}>
                Change…
              </Button>
            ) : (
              <>
                {proposal !== null && (
                  <Button variant="primary" onClick={() => void applyProposal()} disabled={busy}>
                    Use proposal
                  </Button>
                )}
                <Button
                  onClick={() => setDialog({ initial: proposal ?? gridFromTarget(grid.target), warnRewrite: false })}
                  disabled={busy}
                >
                  Edit…
                </Button>
              </>
            )}
          </div>
        )}
      </div>

      {dialog !== null && (
        <GridDialog
          initial={dialog.initial}
          // A dialog opened from `Edit…` warns too once the grid behind it has been confirmed by someone else.
          warnRewrite={dialog.warnRewrite || confirmed}
          schematic={{ target: grid.target, footprints: grid.footprints, framesNotDrawn: grid.framesNotDrawn }}
          onSave={save}
          onClose={() => setDialog(null)}
        />
      )}
    </Card>
  );
}
