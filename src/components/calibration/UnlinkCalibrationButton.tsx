import { useState } from 'react';
import { Unlink } from 'lucide-react';
import { api } from '../../api';
import { Button, DialogShell } from '../ui';
import { Checkbox } from '../settings/Checkbox';
import { useNotifications } from '../../contexts/NotificationContext';

type LinkType = 'Flat' | 'Dark' | 'Bias';
const LINK_TYPES: LinkType[] = ['Flat', 'Dark', 'Bias'];

interface UnlinkCalibrationButtonProps {
  /** Light frames of the selected groups; empty = nothing selected. */
  selectedFrameIds: number[];
  selectedGroupCount: number;
  /** Every (non-Black-Hole) light frame of the frame set: the scope when nothing is selected. */
  allFrameIds: number[];
  disabled?: boolean;
  onUnlinked: () => void;
}

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? '' : 's'}`;

/**
 * Removes calibration links from lights — auto-matched and manual alike —
 * from the selected light groups, or from every light of the frame set when
 * nothing is selected. Flat sets' own pre-calibration is never touched.
 */
export function UnlinkCalibrationButton({ selectedFrameIds, selectedGroupCount, allFrameIds, disabled, onUnlinked }: UnlinkCalibrationButtonProps) {
  const { notify } = useNotifications();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [types, setTypes] = useState<Set<LinkType>>(new Set(LINK_TYPES));

  const selected = selectedFrameIds.length > 0;
  const frameIds = selected ? selectedFrameIds : allFrameIds;
  const scope = selected
    ? `the ${plural(selectedGroupCount, 'selected light group')} (${plural(frameIds.length, 'frame')})`
    : `every light of this frame set (${plural(frameIds.length, 'frame')})`;

  const openDialog = () => {
    setTypes(new Set(LINK_TYPES));
    setOpen(true);
  };

  const toggleType = (t: LinkType, on: boolean) =>
    setTypes(prev => {
      const next = new Set(prev);
      if (on) next.add(t);
      else next.delete(t);
      return next;
    });

  const unlink = async () => {
    setBusy(true);
    try {
      const removed = await api.invoke<number>('unlink_calibration_for_frames', {
        frameIds,
        calibrationTypes: LINK_TYPES.filter(t => types.has(t)),
      });
      setOpen(false);
      notify({
        title: removed > 0 ? `${plural(removed, 'calibration link')} removed` : 'No calibration links to remove',
        detail: removed > 0
          ? 'Find Calibration can link the automatic matches again.'
          : 'The chosen lights had no links of those types.',
        kind: 'calibration',
        tone: removed > 0 ? 'success' : 'info',
      });
      onUnlinked();
    } catch (err) {
      console.error('[calibration] unlink_calibration_for_frames failed:', err);
      notify({
        title: 'Could not remove the calibration links',
        detail: err instanceof Error ? err.message : String(err),
        kind: 'calibration',
        tone: 'warning',
      });
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <button
        onClick={openDialog}
        disabled={disabled || frameIds.length === 0}
        className="h-7 inline-flex items-center gap-1.5 px-3 text-xs font-medium rounded-lg transition-colors bg-surface-elevated text-content-secondary hover:text-content hover:bg-surface-hover border border-border/50 disabled:opacity-50 disabled:cursor-not-allowed"
        title={selected ? 'Remove calibration links from the selected light groups' : 'Remove calibration links from every light of this frame set'}
      >
        <Unlink size={12} />
        {selected ? `Unlink selected (${selectedGroupCount})` : 'Unlink all'}
      </button>

      {open && (
        <DialogShell
          title="Remove calibration links?"
          onClose={() => setOpen(false)}
          busy={busy}
          footer={
            <>
              <Button onClick={() => setOpen(false)} disabled={busy} data-autofocus>Cancel</Button>
              <Button variant="dangerPrimary" onClick={unlink} disabled={busy || types.size === 0}>
                {busy ? 'Removing…' : 'Unlink'}
              </Button>
            </>
          }
        >
          <div className="flex flex-col gap-3">
            <p>Removes the chosen links from {scope}, automatic and manual alike.</p>
            <div className="flex items-center gap-4">
              {LINK_TYPES.map(t => (
                <Checkbox key={t} checked={types.has(t)} onChange={on => toggleType(t, on)} label={t} disabled={busy} />
              ))}
            </div>
            <p className="text-xs text-content-muted">
              Find Calibration can link the automatic matches again; a manual choice is not restored. The flats' own
              pre-calibration is not touched.
            </p>
          </div>
        </DialogShell>
      )}
    </>
  );
}
