import { useEffect, useRef, useState } from 'react';
import { api } from '../../api';
import type { EquipmentCandidate, EquipmentEvidence, EquipmentProfile } from '../../types/models';
import { useNotifications } from '../../contexts/NotificationContext';
import { EquipmentProfileForm, emptyProfile } from './EquipmentProfileForm';
import { EquipmentEvidenceTable } from './EquipmentEvidenceTable';

/** Review saved scale evidence; only an explicit confirmation creates a match. */
export function EquipmentProfilesPanel({ cameras }: { cameras: string[] }) {
  const [profiles, setProfiles] = useState<EquipmentProfile[]>([]);
  const [draft, setDraft] = useState<EquipmentProfile | null>(null);
  const [camera, setCamera] = useState('');
  const [rows, setRows] = useState<EquipmentEvidence[]>([]);
  const [cursor, setCursor] = useState(0);
  const [history, setHistory] = useState<number[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [reviewed, setReviewed] = useState(false);
  // Camera changes and newer page requests invalidate pending evidence responses.
  const generation = useRef(0);
  const { notify } = useNotifications();
  const report = (error: unknown) => {
    console.error('[EquipmentProfiles]', error);
    setError(String(error));
  };
  const loadProfiles = async () =>
    setProfiles(await api.invoke<EquipmentProfile[]>('get_equipment_profiles'));
  useEffect(() => {
    let cancelled = false;
    api
      .invoke<EquipmentProfile[]>('get_equipment_profiles')
      .then(value => {
        if (!cancelled) setProfiles(value);
      })
      .catch(error => {
        console.error('[EquipmentProfiles]', error);
        if (!cancelled) setError(String(error));
      });
    return () => {
      cancelled = true;
      generation.current++;
    };
  }, []);
  const loadEvidence = async (nextCursor = cursor) => {
    if (!camera) return;
    const token = ++generation.current;
    setBusy(true);
    setError('');
    setRows([]);
    setReviewed(false);
    try {
      const result = await api.invoke<EquipmentEvidence[]>('get_equipment_evidence', {
        camera,
        afterId: nextCursor,
      });
      if (token === generation.current) {
        setRows(result);
        setCursor(nextCursor);
        setReviewed(true);
      }
    } catch (error) {
      console.error('[EquipmentProfiles]', error);
      if (token === generation.current) setError(String(error));
    } finally {
      if (token === generation.current) setBusy(false);
    }
  };
  const mutate = async (command: string, args: Record<string, unknown>, title: string) => {
    setBusy(true);
    setError('');
    try {
      await api.invoke(command, args);
      await loadProfiles();
      if (camera) await loadEvidence();
      notify({
        title,
        detail: 'Saved in this catalog. Original image headers and plate solves are unchanged.',
        kind: 'generic',
        tone: 'success',
      });
      return true;
    } catch (error) {
      report(error);
      return false;
    } finally {
      setBusy(false);
    }
  };
  const confirm = (row: EquipmentEvidence, candidate: EquipmentCandidate) =>
    void mutate(
      'confirm_equipment_match',
      {
        frameId: row.frameId,
        profileId: candidate.profile.id,
        revision: candidate.profile.revision,
        solvedAt: row.solvedAt,
        scale: row.solvedScale,
      },
      'Equipment match confirmed',
    );
  return (
    <section className="bg-surface-elevated border border-border rounded-lg p-4 mb-5">
      <div className="flex items-center justify-between">
        <h3 className="font-semibold">Optical configurations & solved-scale review</h3>
        <button disabled={busy} onClick={() => setDraft(emptyProfile())} className="text-accent">
          Add configuration
        </button>
      </div>
      <p className="text-xs text-content-muted mt-2">
        Compare saved plate solves with your telescope, reducer/flattener and camera. Scale
        agreement is evidence, not unique identification. Only Confirm match records an association;
        nothing is assigned automatically.
      </p>
      {error && (
        <p role="alert" className="text-error text-sm py-2">
          {error}
        </p>
      )}
      {draft && (
        <EquipmentProfileForm
          value={draft}
          cameras={cameras}
          busy={busy}
          onChange={setDraft}
          onCancel={() => setDraft(null)}
          onSave={async () => {
            if (
              await mutate(
                'save_equipment_profile',
                { profile: draft },
                'Equipment configuration saved',
              )
            )
              setDraft(null);
          }}
        />
      )}
      <div className="space-y-1 my-3">
        {profiles.map(profile => (
          <div key={profile.id} className="flex gap-3 items-center text-sm">
            <span className="flex-1">
              #{profile.id} {profile.name} · {profile.telescope} · {profile.camera} ·{' '}
              {profile.focalLengthMm} mm × {profile.opticalMultiplier} · {profile.binning}×
              {profile.binning}
            </span>
            <button disabled={busy} onClick={() => setDraft(profile)} className="text-accent">
              Edit
            </button>
            <button
              disabled={busy}
              onClick={() =>
                void mutate(
                  'delete_equipment_profile',
                  { id: profile.id, revision: profile.revision },
                  'Configuration and its confirmations removed',
                )
              }
              title="Remove this configuration and its catalog confirmations"
              className="text-content-muted"
            >
              Remove configuration
            </button>
          </div>
        ))}
        {!profiles.length && (
          <p className="text-xs text-content-muted">
            Add the optical configurations you want to compare. No equipment names or dimensions are
            inferred for you.
          </p>
        )}
      </div>
      <div className="flex items-center gap-3 text-sm flex-wrap">
        <label>
          Camera{' '}
          <select
            value={camera}
            disabled={busy}
            onChange={e => {
              generation.current++;
              setCamera(e.target.value);
              setCursor(0);
              setHistory([]);
              setRows([]);
              setReviewed(false);
            }}
            className="bg-surface border border-border rounded p-1 ml-2"
          >
            <option value="">Choose camera</option>
            {[...new Set([...cameras, ...profiles.map(p => p.camera)])].map(name => (
              <option key={name}>{name}</option>
            ))}
          </select>
        </label>
        <button
          disabled={busy || !camera}
          onClick={() => void loadEvidence()}
          className="text-accent disabled:opacity-50"
        >
          {busy ? 'Loading…' : 'Review saved solves'}
        </button>
      </div>
      {reviewed && (
        <>
          <p className="text-xs text-content-muted mt-3">
            {rows.length} solved files on this page. Resampling, drizzle and uncertain header
            binning can invalidate a physical match; identical-scale configurations remain separate
            candidates.
          </p>
          <EquipmentEvidenceTable
            rows={rows}
            busy={busy}
            onConfirm={confirm}
            onClear={row =>
              void mutate(
                'clear_equipment_match',
                { frameId: row.frameId },
                'Equipment confirmation cleared',
              )
            }
          />
          <div className="flex gap-3 text-sm mt-2">
            <button
              disabled={busy || !history.length}
              onClick={() => {
                const previous = history[history.length - 1];
                setHistory(history.slice(0, -1));
                void loadEvidence(previous);
              }}
            >
              Previous
            </button>
            <button
              disabled={busy || rows.length < 100}
              onClick={() => {
                setHistory([...history, cursor]);
                void loadEvidence(rows[rows.length - 1].frameId);
              }}
            >
              Next
            </button>
          </div>
        </>
      )}
    </section>
  );
}
