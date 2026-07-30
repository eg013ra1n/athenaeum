import { useEffect, useRef, useState } from 'react';
import { X, Check, Info, AlertCircle, FolderOpen, Loader2 } from 'lucide-react';
import { api } from '../../api';
import { pickDirectory } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { addArchiveRoot } from '../../api/archive';
import { FolderBrowserModal } from '../FolderBrowserModal';
import { useNotifications } from '../../contexts/NotificationContext';
import type { ScanRoot, FolderCandidateVerdict } from '../../types/models';
import { ROLE_META, ROLE_ORDER, KIND_META, metaForKind, verdictMessage, type AddableKind, type RoleKind } from './roleMeta';

interface AddFolderDialogProps {
  isOpen: boolean;
  /** Pre-select a type (e.g. a role's "Set up…" row) and jump to step 2. */
  preselect?: AddableKind;
  scanRoots: ScanRoot[];
  /** Effective calibration-library dir when it is settings-only (covered) — no scan-root row exists for it. */
  coveredCalibrationDir?: string | null;
  onClose: () => void;
  onAdded: () => void;
}

export function AddFolderDialog({ isOpen, preselect, scanRoots, coveredCalibrationDir, onClose, onAdded }: AddFolderDialogProps) {
  const { notify } = useNotifications();
  const [kind, setKind] = useState<AddableKind | null>(null);
  const [pickedPath, setPickedPath] = useState<string | null>(null);
  const [verdict, setVerdict] = useState<FolderCandidateVerdict | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showBrowser, setShowBrowser] = useState(false);
  /** Bumped on every validate/reset; a late reply whose ticket moved must not paint. */
  const validateSeq = useRef(0);

  useEffect(() => {
    if (isOpen) {
      validateSeq.current++;
      setKind(preselect ?? null);
      setPickedPath(null);
      setVerdict(null);
      setError(null);
      setShowBrowser(false);
      setBusy(false);
    }
  }, [isOpen, preselect]);

  if (!isOpen) return null;

  const takenRolePath = (role: RoleKind): string | undefined => {
    const fromRoots = scanRoots.find((r) => r.kind === role)?.path;
    // A "covered" calibration library (inside a monitored root) is settings-only — it has no
    // scan-root row, so the effective dir is the only evidence the role is already taken.
    if (role === 'calibration_library') return fromRoots ?? coveredCalibrationDir ?? undefined;
    return fromRoots;
  };

  const validate = async (candidateKind: AddableKind, path: string) => {
    const seq = ++validateSeq.current;
    setError(null);
    setPickedPath(path);
    setVerdict(null);
    try {
      const v = await api.invoke<FolderCandidateVerdict>('validate_folder_candidate', { kind: candidateKind, path });
      if (seq !== validateSeq.current) return; // superseded by a newer pick/reset
      setVerdict(v);
    } catch (e) {
      console.error('[AddFolderDialog] validate failed:', e);
      if (seq !== validateSeq.current) return;
      setError(typeof e === 'string' ? e : String(e));
    }
  };

  const pick = async () => {
    if (!kind) return;
    if (!isTauri) { setShowBrowser(true); return; }
    try {
      const picked = await pickDirectory();
      if (picked && typeof picked === 'string') await validate(kind, picked);
    } catch (e) {
      console.error('[AddFolderDialog] pick failed:', e);
      setError(typeof e === 'string' ? e : String(e));
    }
  };

  const confirmAdd = async () => {
    if (!kind || !pickedPath || !verdict?.ok) return;
    setBusy(true);
    setError(null);
    try {
      if (kind === 'normal') {
        await api.invoke('add_scan_root', { path: pickedPath });
      } else if (kind === 'archive') {
        await addArchiveRoot(pickedPath, null);
      } else {
        await api.invoke<string>(ROLE_META[kind].setCommand, { path: pickedPath });
      }
      onAdded();
      onClose();
    } catch (e) {
      // Backend stays authoritative — surface its message verbatim (TOCTOU
      // between validate and add is possible and must not be hidden).
      const msg = typeof e === 'string' ? e : String(e);
      console.error('[AddFolderDialog] add failed:', e);
      setError(msg);
      notify({ title: 'Could not add folder', detail: msg, kind: 'files', tone: 'warning', hasErrors: true });
    } finally {
      setBusy(false);
    }
  };

  const meta = kind ? metaForKind(kind) : null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50" onClick={onClose}>
      <div className="w-[560px] max-w-[92vw] bg-surface-elevated border border-border rounded-xl p-5" onClick={(e) => e.stopPropagation()}>
        <div className="flex items-center justify-between mb-4">
          <h3 className="text-lg font-semibold text-content">Add Folder</h3>
          <button onClick={onClose} className="p-1 rounded text-content-muted hover:text-content hover:bg-surface-hover transition"><X size={18} /></button>
        </div>

        {/* Step 1 — type picker */}
        {!kind && (
          <div className="space-y-1">
            {(['normal', 'archive'] as const).map((k) => (
              <button key={k} onClick={() => setKind(k)} className="w-full flex items-start gap-3 p-3 rounded-lg text-left hover:bg-surface-hover transition">
                <KindIcon k={k} />
                <span><span className="block text-sm font-semibold text-content">{KIND_META[k].label}</span>
                <span className="block text-xs text-content-muted">{KIND_META[k].purpose}</span></span>
              </button>
            ))}
            <div className="pt-2 pb-1 px-3 text-[10px] font-bold uppercase tracking-wider text-content-muted">Assign a role — one of each</div>
            {ROLE_ORDER.map((role) => {
              const taken = takenRolePath(role);
              const rm = ROLE_META[role];
              return (
                <button key={role} onClick={() => !taken && setKind(role)} disabled={!!taken}
                  className={`w-full flex items-start gap-3 p-3 rounded-lg text-left transition ${taken ? 'opacity-45 cursor-not-allowed' : 'hover:bg-surface-hover'}`}>
                  <rm.icon size={18} className={`${rm.tint} mt-0.5 shrink-0`} />
                  <span className="min-w-0">
                    <span className="block text-sm font-semibold text-content">{rm.label} {taken && <Check size={12} className="inline text-success" />}</span>
                    <span className="block text-xs text-content-muted truncate">{taken ? `already set → ${taken}` : rm.purpose}</span>
                  </span>
                </button>
              );
            })}
          </div>
        )}

        {/* Step 2 — rule, pick, inline validation */}
        {kind && meta && (
          <div className="space-y-3">
            <div className="flex items-center gap-2 text-sm font-semibold text-content">
              <KindIcon k={kind} /> {meta.label}
              <button onClick={() => { validateSeq.current++; setKind(null); setPickedPath(null); setVerdict(null); setError(null); }} className="ml-auto text-xs text-accent hover:underline">change type</button>
            </div>
            <div className="flex items-start gap-2 p-3 rounded-lg bg-surface border border-accent/30 text-xs text-content-muted">
              <Info size={14} className="text-accent shrink-0 mt-0.5" />
              <span>{meta.placementRule}</span>
            </div>
            <button onClick={pick} disabled={busy} className="flex items-center gap-2 px-4 py-2 bg-surface-hover hover:brightness-110 rounded-lg text-sm text-content transition">
              <FolderOpen size={16} /> {pickedPath ? 'Pick a different folder…' : 'Choose folder…'}
            </button>
            {pickedPath && (
              <div className="p-3 rounded-lg bg-surface border border-border">
                <div className="font-mono text-xs text-content break-all">{pickedPath}</div>
                {verdict === null && !error && <div className="mt-1 text-xs text-content-muted flex items-center gap-1"><Loader2 size={12} className="animate-spin" /> checking…</div>}
                {verdict?.ok && (
                  <div className="mt-1 text-xs text-success flex items-center gap-1">
                    <Check size={12} />
                    {verdict.placement === 'covered'
                      ? 'Inside a monitored folder — stored as the library destination; the parent folder keeps scanning it.'
                      : verdict.placement === 'standalone'
                        ? 'Standalone — becomes its own scanned library folder.'
                        : 'Looks good.'}
                  </div>
                )}
                {verdict && !verdict.ok && (
                  <div className="mt-1 text-xs text-error flex items-start gap-1">
                    <AlertCircle size={12} className="shrink-0 mt-0.5" /> {verdictMessage(verdict.reason, verdict.conflicting_path)}
                  </div>
                )}
              </div>
            )}
            {error && <div className="text-xs text-error">{error}</div>}
            <div className="flex justify-end gap-2 pt-1">
              <button onClick={onClose} className="px-4 py-2 rounded-lg text-sm text-content-muted hover:bg-surface-hover transition">Cancel</button>
              <button onClick={confirmAdd} disabled={busy || !verdict?.ok}
                className="px-4 py-2 rounded-lg text-sm font-semibold bg-accent hover:bg-accent-hover text-surface transition disabled:opacity-50 disabled:cursor-not-allowed">
                {busy ? 'Adding…' : 'Add'}
              </button>
            </div>
          </div>
        )}

        <FolderBrowserModal
          isOpen={showBrowser}
          scope="scan"
          onSelect={(path) => {
            setShowBrowser(false);
            if (kind) void validate(kind, path);
            else console.error('[AddFolderDialog] onSelect with no kind — dropping', path);
          }}
          onClose={() => setShowBrowser(false)}
        />
      </div>
    </div>
  );
}

function KindIcon({ k }: { k: AddableKind }) {
  const m = metaForKind(k);
  const Icon = m.icon;
  return <Icon size={18} className={`${m.tint} mt-0.5 shrink-0`} />;
}
