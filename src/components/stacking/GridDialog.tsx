// The canonical grid's editor (wave 6c Task 7, spec §6.3): six numbers, the schematic of the draft as it is edited,
// and a save the hub may refuse. Opened by the grid card for an editor only; the card owns the command.

import { useState } from 'react';
import { Button } from '../ui/Button';
import { DialogShell } from '../ui/DialogShell';
import { KV } from '../ui/KV';
import { useNotifications } from '../../contexts/NotificationContext';
import { NumericField } from './NumericField';
import { GridSchematic, type GridSchematicProps } from './GridSchematic';
import { fieldOfView } from './gridProjection';
import type { CanonicalGrid } from '../../types/models';

export interface GridDialogProps {
  initial: CanonicalGrid;
  /** The grid is already confirmed: changing it makes every processor's next run rewrite its registered frames. */
  warnRewrite: boolean;
  schematic: Pick<GridSchematicProps, 'target' | 'footprints' | 'framesNotDrawn' | 'reference'>;
  /** Saves the grid; a rejection keeps the dialog open with its message. */
  onSave: (grid: CanonicalGrid) => Promise<void>;
  onClose: () => void;
}

const inRange = (n: number, lo: number, hi: number) => Number.isFinite(n) && n >= lo && n <= hi;
const isPixelCount = (n: number) => Number.isInteger(n) && n >= 64 && n <= 65536;

/** What is wrong with `g`, in plain words, or `null`. Mirrors `collab::grid::CanonicalGrid::validate` and the hub:
 *  `scaleArcsec` in (0, 100], `widthPx` and `heightPx` whole numbers in [64, 65536], `crval1` in [0, 360), `crval2` in
 *  [-90, 90], `rotationDeg` finite. The first field out of bounds is the one named. */
export function validateGrid(g: CanonicalGrid): string | null {
  if (!(Number.isFinite(g.scaleArcsec) && g.scaleArcsec > 0 && g.scaleArcsec <= 100)) {
    return 'The scale must be above 0 and at most 100 ″/px.';
  }
  if (!isPixelCount(g.widthPx)) return 'The width must be a whole number of pixels from 64 to 65536.';
  if (!isPixelCount(g.heightPx)) return 'The height must be a whole number of pixels from 64 to 65536.';
  if (!(Number.isFinite(g.crval1) && g.crval1 >= 0 && g.crval1 < 360)) return 'The centre RA must be from 0 up to, but not including, 360°.';
  if (!inRange(g.crval2, -90, 90)) return 'The centre Dec must be from -90° to 90°.';
  if (!Number.isFinite(g.rotationDeg)) return 'The rotation must be a number of degrees.';
  return null;
}

/** A draft the schematic can project: a positive scale and size and finite angles. Mid-typing values outside the
 *  hub's bounds still draw (the picture follows the keys); only one that cannot be projected falls back to the plane
 *  at the target. */
function drawable(g: CanonicalGrid): boolean {
  return [g.scaleArcsec, g.widthPx, g.heightPx, g.crval1, g.crval2, g.rotationDeg].every(Number.isFinite) &&
    g.scaleArcsec > 0 && g.widthPx > 0 && g.heightPx > 0;
}

/** An `api.invoke` rejection as text: the backend sends a string, the web transport an `Error`. */
export function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

export function GridDialog({ initial, warnRewrite, schematic, onSave, onClose }: GridDialogProps) {
  const { notify } = useNotifications();
  const [draft, setDraft] = useState<CanonicalGrid>(initial);
  const [busy, setBusy] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  const invalid = validateGrid(draft);
  const problem = invalid ?? saveError;

  const edit = (patch: Partial<CanonicalGrid>) => {
    setDraft((d) => ({ ...d, ...patch }));
    // The refusal was about the grid as it was; an edit makes it stale.
    setSaveError(null);
  };

  const handleSave = async () => {
    if (invalid !== null || busy) return;
    setBusy(true);
    setSaveError(null);
    try {
      await onSave(draft);
    } catch (err) {
      // A 403 (the capability is gone), a 400 (the hub's bounds) or a grid another member confirmed first: stay
      // open, say so, and let the next `collab-grid-changed` bring the newer plan.
      console.error('[GridDialog] set_collab_project_grid failed:', err);
      const detail = errorText(err);
      setSaveError(detail);
      setBusy(false);
      notify({ title: 'Grid not saved', detail, tone: 'warning', hasErrors: true });
      return;
    }
    setBusy(false);
    onClose();
  };

  return (
    <DialogShell
      size="md"
      title={warnRewrite ? 'Change the canonical grid' : 'Set the canonical grid'}
      onClose={onClose}
      busy={busy}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={() => void handleSave()} disabled={invalid !== null || busy}>
            Save
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <div className="grid grid-cols-3 gap-x-3 gap-y-2">
          <NumericField label="Scale (″/px)" value={draft.scaleArcsec} onCommit={(n) => edit({ scaleArcsec: n })} step={0.01} disabled={busy} />
          <NumericField label="Width (px)" value={draft.widthPx} onCommit={(n) => edit({ widthPx: n })} step={64} disabled={busy} />
          <NumericField label="Height (px)" value={draft.heightPx} onCommit={(n) => edit({ heightPx: n })} step={64} disabled={busy} />
          <NumericField label="Centre RA (°)" value={draft.crval1} onCommit={(n) => edit({ crval1: n })} step={0.01} disabled={busy} />
          <NumericField label="Centre Dec (°)" value={draft.crval2} onCommit={(n) => edit({ crval2: n })} step={0.01} disabled={busy} />
          <NumericField label="Rotation (°)" value={draft.rotationDeg} onCommit={(n) => edit({ rotationDeg: n })} step={1} disabled={busy} />
        </div>

        {problem !== null && (
          <p role="alert" className="text-xs text-error">
            {problem}
          </p>
        )}

        <GridSchematic grid={drawable(draft) ? draft : null} {...schematic} />
        <KV items={[['Field of view', fieldOfView(draft)]]} />

        {warnRewrite && (
          <p className="rounded border border-warning/40 bg-warning-muted px-2.5 py-1.5 text-[12px] text-content-secondary">
            Changing the grid makes every processor&apos;s next run rewrite its registered frames. Star matching is kept.
          </p>
        )}
      </div>
    </DialogShell>
  );
}
