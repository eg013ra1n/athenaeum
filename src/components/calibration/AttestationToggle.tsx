import { useState } from 'react';
import { api } from '../../api';
import { Checkbox } from '../settings/Checkbox';
import { ConfirmDialog } from '../ConfirmDialog';
import { useNotifications } from '../../contexts/NotificationContext';

interface AttestationToggleProps {
  framesSetId: number;
  calibratedExternally: boolean;
  /** Count of calibration sets currently linked to this set's lights — used
   *  only to word the confirm shown when turning attestation ON. */
  linkedCalibrationSets: number;
  onChanged: () => void;
}

/**
 * Spec 2026-09-28 §6.2 — the attestation checkbox on the Calibration tab.
 * Turning it ON with linked calibration sets confirms first (those masters
 * are ignored by projects once attested); turning it OFF needs no confirm.
 */
export function AttestationToggle({ framesSetId, calibratedExternally, linkedCalibrationSets, onChanged }: AttestationToggleProps) {
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
      <Checkbox
        checked={calibratedExternally}
        onChange={handleChange}
        disabled={busy}
        label="Calibrated by an external tool"
        description="The lights of this set are already calibrated (dark and flat applied) as single-channel frames — mono or CFA, not debayered RGB. Collaboration projects take them as they are; nothing in the app calibrates them again."
      />
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
