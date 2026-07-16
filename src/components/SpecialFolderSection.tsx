import { useEffect, useState, useCallback } from 'react';
import { FolderOpen, Trash2, ExternalLink, type LucideIcon } from 'lucide-react';
import { api } from '../api';
import { pickDirectory, revealItemInDir } from '../api/desktop';
import { isTauri } from '../utils/platform';
import { FolderBrowserModal } from './FolderBrowserModal';
import { useNotifications, type NotificationKind } from '../contexts/NotificationContext';
import type { ScanRoot } from '../types/models';

interface SpecialFolderSectionProps {
  /** Section heading, e.g. "Sync Incoming Folder" / "Collaboration Folder". */
  title: string;
  /** One-line purpose text under the heading. */
  description: string;
  /** Scan-root kind this folder is designated as. The backend stores the folder
   *  as its OWN dedicated scan root of this kind (no nested/settings layer). */
  kind: 'sync_incoming' | 'collaboration';
  /** Heading icon (lucide component). */
  icon: LucideIcon;
  /** Notification panel icon bucket for surfaced errors. */
  notifyKind: NotificationKind;
  /** `api.invoke` command names for the get/set/clear triple (Task 4). */
  getCommand: string;
  setCommand: string;
  clearCommand: string;
  /** Current scan roots — the designated folder is added as a dedicated root of
   *  `kind`, so it also appears under Monitored Directories. */
  scanRoots: ScanRoot[];
  /** Called after set/clear so the parent refreshes the scan-root list. */
  onRootsChanged: () => void;
  /** Optional DOM id on the section root, so other views can deep-link + scroll
   *  to this designator (e.g. the `/transfers` app-data warning strip). */
  id?: string;
}

/**
 * Map the two dead-end backend Conflict messages the `set_*` command can return
 * into an actionable instruction. Everything else falls through to the verbatim
 * backend text — an unknown error is never hidden.
 *
 * - the per-kind uniqueness Conflict ("…already exists — only one is allowed"):
 *   the folder is already designated, so tell the user to remove it first;
 * - the scan-root overlap Conflict ("…is a subdirectory of…" in either
 *   direction, or "…already being monitored"): the picked folder is
 *   inside/contains a monitored directory, so tell them to pick one outside
 *   their monitored directories.
 */
function friendlyConflict(msg: string): string {
  if (msg.includes('subdirectory') || msg.includes('already being monitored')) {
    return 'This folder is inside (or contains) a monitored directory — pick a folder outside your monitored directories.';
  }
  if (msg.includes('already exists') || msg.includes('only one is allowed')) {
    return 'Remove the current folder first (trash icon), then choose the new one.';
  }
  return msg;
}

/**
 * Generic "special folder" designator section for the File Manager's Monitored
 * Directories tab — a parameterized clone of {@link CalibrationFolderSection}
 * used for the sync-incoming and collaboration folders (Stage 1.5 sync
 * hardening, Task 6).
 *
 * Backed by the Task 4 `get_*`/`set_*`/`clear_*` triple:
 * - `get` returns the designated folder (or `null`);
 * - `set` designates a folder — it becomes a dedicated scan root of `kind`, so
 *   it must live OUTSIDE every existing monitored directory (the backend
 *   rejects an overlapping/nested folder with a Conflict; surfaced verbatim);
 * - `clear` DEMOTES the folder back to a normal monitored directory (never
 *   deletes it) — files on disk are untouched.
 *
 * Web mode has no native folder picker (`pickDirectory()` returns `null`) —
 * falls back to the in-app `FolderBrowserModal`, same branch the calibration
 * section uses.
 */
export function SpecialFolderSection({
  title,
  description,
  kind,
  icon: Icon,
  notifyKind,
  getCommand,
  setCommand,
  clearCommand,
  scanRoots,
  onRootsChanged,
  id,
}: SpecialFolderSectionProps) {
  const { notify } = useNotifications();
  const [dir, setDir] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showFolderBrowser, setShowFolderBrowser] = useState(false);

  const load = useCallback(async () => {
    try {
      setLoading(true);
      const d = await api.invoke<string | null>(getCommand);
      setDir(d);
    } catch (e) {
      const msg = typeof e === 'string' ? e : String(e);
      setError(msg);
      console.error(`[SpecialFolderSection:${kind}] ${getCommand} failed:`, e);
    } finally {
      setLoading(false);
    }
  }, [getCommand, kind]);

  useEffect(() => { void load(); }, [load]);

  const designate = useCallback(async (path: string) => {
    setError(null);
    setBusy(true);
    try {
      await api.invoke<string>(setCommand, { path });
      await load();
      onRootsChanged();
    } catch (e) {
      // Remap the two dead-end backend Conflicts (per-kind uniqueness, scan-root
      // overlap) into actionable instructions; anything else falls back to the
      // verbatim backend text (never hide an unknown error). Log the raw error.
      const raw = typeof e === 'string' ? e : String(e);
      const msg = friendlyConflict(raw);
      setError(msg);
      console.error(`[SpecialFolderSection:${kind}] ${setCommand} failed:`, e);
      notify({
        title: `Could not set ${title}`,
        detail: msg,
        kind: notifyKind,
        tone: 'warning',
        hasErrors: true,
      });
    } finally {
      setBusy(false);
    }
  }, [setCommand, load, onRootsChanged, kind, title, notifyKind, notify]);

  const choose = async () => {
    if (!isTauri) {
      setShowFolderBrowser(true);
      return;
    }
    const picked = await pickDirectory();
    if (!picked || typeof picked !== 'string') return;
    await designate(picked);
  };

  const handleRemove = async () => {
    if (!window.confirm(
      `Stop using this folder as the ${title}? It stays a monitored directory — files already on disk are untouched — and you can designate a folder again at any time.`
    )) return;
    setError(null);
    setBusy(true);
    try {
      await api.invoke(clearCommand);
      await load();
      onRootsChanged();
    } catch (e) {
      const msg = typeof e === 'string' ? e : String(e);
      setError(msg);
      console.error(`[SpecialFolderSection:${kind}] ${clearCommand} failed:`, e);
      notify({
        title: `Could not clear ${title}`,
        detail: msg,
        kind: notifyKind,
        tone: 'warning',
        hasErrors: true,
      });
    } finally {
      setBusy(false);
    }
  };

  const isDedicatedRoot = dir
    ? scanRoots.some(r => r.kind === kind && r.path === dir)
    : false;

  return (
    <div className="mt-8" id={id}>
      <div className="flex items-center justify-between mb-4">
        <div>
          <h3 className="text-xl font-semibold flex items-center gap-2">
            <Icon size={20} />
            {title}
          </h3>
          <p className="text-sm text-content-muted mt-1">{description}</p>
          <p className="text-xs text-content-muted mt-1">
            Pick a folder outside your monitored directories.
          </p>
        </div>
        <button
          onClick={choose}
          disabled={busy || loading}
          className="flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover rounded-lg transition disabled:opacity-50 disabled:cursor-not-allowed whitespace-nowrap"
        >
          <FolderOpen size={18} />
          {dir ? 'Change Folder' : 'Choose Folder'}
        </button>
      </div>

      {loading ? (
        <div className="bg-surface-elevated rounded-lg p-6 text-center text-content-muted text-sm">
          Loading folder…
        </div>
      ) : dir ? (
        <div className="bg-surface-elevated rounded-lg border border-border p-3 flex items-center gap-3">
          <Icon size={16} className="text-content-muted shrink-0" />
          <div className="flex-1 min-w-0">
            <div className="font-mono text-sm text-content truncate">{dir}</div>
            <div className="text-xs text-content-muted">
              {isDedicatedRoot
                ? 'dedicated folder (listed under Monitored Directories above)'
                : 'designated folder'}
            </div>
          </div>
          {isTauri && (
            <button
              onClick={() => revealItemInDir(dir).catch(e => alert(`Failed: ${e}`))}
              disabled={busy}
              title="Reveal folder in file manager"
              className="p-1.5 rounded text-content-muted hover:text-accent hover:bg-surface-hover transition-colors"
            >
              <ExternalLink size={16} />
            </button>
          )}
          <button
            onClick={handleRemove}
            disabled={busy}
            title={`Stop using this folder as the ${title}`}
            className="p-1.5 rounded text-content-muted hover:text-error hover:bg-surface-hover transition-colors disabled:opacity-50"
          >
            <Trash2 size={16} />
          </button>
        </div>
      ) : (
        <div className="bg-surface-elevated rounded-lg p-6 text-center">
          <p className="text-content-muted text-sm">
            No folder configured yet &mdash; click &ldquo;Choose Folder&rdquo; to set one.
          </p>
        </div>
      )}
      {error && <div className="text-xs text-error mt-2">{error}</div>}

      {/* Web mode: in-app folder browser (no native picker there) */}
      <FolderBrowserModal
        isOpen={showFolderBrowser}
        scope="scan"
        onSelect={path => {
          setShowFolderBrowser(false);
          void designate(path);
        }}
        onClose={() => setShowFolderBrowser(false)}
      />
    </div>
  );
}
