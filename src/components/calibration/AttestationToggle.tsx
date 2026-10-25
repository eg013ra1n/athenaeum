import { useState } from 'react';
import { Lock } from 'lucide-react';
import { api } from '../../api';
import { ConfirmDialog } from '../ConfirmDialog';
import { useNotifications } from '../../contexts/NotificationContext';

interface AttestationToggleProps {
  framesSetId: number;
  calibratedExternally: boolean;
  /** Count of calibration sets currently linked to this set's lights — used
   *  only to word the confirm shown when turning attestation ON. */
  linkedCalibrationSets: number;
  onChanged: () => void;
  disabled?: boolean;
}

/**
 * Spec 2026-09-28 §6.2 — the attestation switch in the Calibration tab's
 * toolbar. ON locks the tab's calibration controls (the lights are taken as
 * they are; linked sets stay but are ignored); OFF clears the flag and gives
 * them back. Turning it ON with linked calibration sets confirms first;
 * turning it OFF needs no confirm.
 */
export function AttestationToggle({ framesSetId, calibratedExternally, linkedCalibrationSets, onChanged, disabled }: AttestationToggleProps) {
  const { notify } = useNotifications();
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [busy, setBusy] = useState(false);

  const setAttested = async (attested: boolean) => {
    setBusy(true);
    try {
      await api.invoke('set_frame_set_attestation', { framesSetId, attested });
      onChanged();
    } catch (err) {
      console.error('[collab] set_frame_set_attestation failed:', err);
      notify({
        title: 'Could not change the attestation',
        detail: err instanceof Error ? err.message : String(err),
        kind: 'calibration',
        tone: 'warning',
      });
    } finally {
      setBusy(false);
    }
  };

  const handleChange = (checked: boolean) => {
    if (checked && linkedCalibrationSets > 0) {
      setConfirmOpen(true);
      return;
    }
    void setAttested(checked);
  };

  return (
    <>
      <button
        type="button"
        role="switch"
        aria-checked={calibratedExternally}
        onClick={() => handleChange(!calibratedExternally)}
        disabled={busy || disabled}
        className={`h-7 inline-flex items-center gap-2 px-3 text-xs font-medium rounded-lg border transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${
          calibratedExternally
            ? 'bg-warning-muted text-warning border-warning/50'
            : 'bg-surface-elevated text-content-secondary hover:text-content hover:bg-surface-hover border-border/50'
        }`}
        title="The lights of this set are already calibrated (dark and flat applied). Mono, raw-mosaic (CFA) and already-debayered RGB lights are all accepted: a mosaic is debayered at stacking (debayer on), RGB lights stack as colour. Stacking and collaboration projects take the lights as they are — no masters are asked for and nothing in the app calibrates them again."
      >
        <span
          aria-hidden
          className={`relative inline-block h-3.5 w-6 rounded-full transition-colors ${calibratedExternally ? 'bg-warning' : 'bg-border'}`}
        >
          <span
            className={`absolute top-0.5 h-2.5 w-2.5 rounded-full bg-surface transition-all ${calibratedExternally ? 'left-3' : 'left-0.5'}`}
          />
        </span>
        {calibratedExternally && <Lock size={11} />}
        Calibrated externally
      </button>
      <ConfirmDialog
        isOpen={confirmOpen}
        title="Attest external calibration?"
        message={`This set has ${linkedCalibrationSets} linked calibration sets. Attesting it means projects take the lights as they are and ignore those masters. Attest?`}
        confirmText="Attest"
        onConfirm={() => {
          setConfirmOpen(false);
          void setAttested(true);
        }}
        onCancel={() => setConfirmOpen(false)}
      />
    </>
  );
}
