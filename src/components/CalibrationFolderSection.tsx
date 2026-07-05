import { useEffect, useState, useCallback } from 'react';
import { Library, FolderOpen, Trash2, ExternalLink } from 'lucide-react';
import { api } from '../api';
import { pickDirectory, revealItemInDir } from '../api/desktop';
import { isTauri } from '../utils/platform';
import { FolderBrowserModal } from './FolderBrowserModal';
import type { ScanRoot } from '../types/models';

interface CalibrationFolderSectionProps {
  /** Current scan roots — used to render the coverage hint and to refresh
   *  the Monitored Directories list when a standalone folder becomes the
   *  dedicated library root. */
  scanRoots: ScanRoot[];
  /** Called after set/clear so the parent can refresh the scan-root list
   *  (a standalone folder is added as a `calibration_library` root). */
  onRootsChanged?: () => void;
}

/**
 * "Calibration Folder" section — designates where Athenaeum writes the
 * master calibration frames it builds. Sits in the File Manager's Monitored
 * Directories tab, styled after `ArchiveFoldersSection`.
 *
 * Backed by `get/set/clear_calibration_library_dir`:
 * - a folder INSIDE an existing monitored directory is stored as a setting
 *   only (the parent root already provides scan coverage);
 * - a standalone folder is also added as the dedicated
 *   `calibration_library`-kind scan root, so it appears in the Monitored
 *   Directories list above and keeps getting scanned.
 *
 * Web mode has no native folder picker (`pickDirectory()` always returns
 * null there) — falls back to the in-app `FolderBrowserModal`, same branch
 * `ExportTab.tsx` uses for its output-directory picker.
 */
export function CalibrationFolderSection({ scanRoots, onRootsChanged }: CalibrationFolderSectionProps) {
  const [dir, setDir] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showFolderBrowser, setShowFolderBrowser] = useState(false);

  const load = useCallback(async () => {
    try {
      setLoading(true);
      const d = await api.invoke<string | null>('get_calibration_library_dir');
      setDir(d);
    } catch (e) {
      setError(typeof e === 'string' ? e : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { void load(); }, [load]);

  const setLibraryDir = useCallback(async (path: string) => {
    setError(null);
    setBusy(true);
    try {
      await api.invoke<string>('set_calibration_library_dir', { path });
      await load();
      onRootsChanged?.();
    } catch (e) {
      setError(typeof e === 'string' ? e : String(e));
    } finally {
      setBusy(false);
    }
  }, [load, onRootsChanged]);

  const choose = async () => {
    if (!isTauri) {
      setShowFolderBrowser(true);
      return;
    }
    const picked = await pickDirectory();
    if (!picked || typeof picked !== 'string') return;
    await setLibraryDir(picked);
  };

  const handleRemove = async () => {
    if (!window.confirm(
      'Remove the calibration folder setting? Master frames already in the catalog and files on disk are not affected — new master builds will just have no destination until a folder is set again.'
    )) return;
    setError(null);
    setBusy(true);
    try {
      await api.invoke('clear_calibration_library_dir');
      await load();
      onRootsChanged?.();
    } catch (e) {
      setError(typeof e === 'string' ? e : String(e));
    } finally {
      setBusy(false);
    }
  };

  // Coverage hint: which monitored directory contains the folder, if any.
  const coveringRoot = dir
    ? scanRoots.find(r => r.kind !== 'calibration_library' && (dir === r.path || dir.startsWith(r.path.endsWith('/') ? r.path : r.path + '/')))
    : undefined;
  const isDedicatedRoot = dir
    ? scanRoots.some(r => r.kind === 'calibration_library' && r.path === dir)
    : false;

  return (
    <div className="mt-8">
      <div className="flex items-center justify-between mb-4">
        <div>
          <h3 className="text-xl font-semibold flex items-center gap-2">
            <Library size={20} />
            Calibration Folder
          </h3>
          <p className="text-sm text-content-muted mt-1">
            Master calibration frames built by Athenaeum are written here. Pick a folder inside a
            monitored directory, or any other folder &mdash; standalone folders are added as a
            monitored library root so masters dropped in from elsewhere are imported too. A
            standalone (dedicated) folder appears under Monitored Directories above and must be
            removed there if you switch to a different standalone folder later.
          </p>
        </div>
        <button
          onClick={choose}
          disabled={busy || loading}
          className="flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover rounded-lg transition disabled:opacity-50 disabled:cursor-not-allowed whitespace-nowrap"
        >
          <FolderOpen size={18} />
          {dir ? 'Change Folder' : 'Choose Calibration Folder'}
        </button>
      </div>

      {loading ? (
        <div className="bg-surface-elevated rounded-lg p-6 text-center text-content-muted text-sm">
          Loading calibration folder…
        </div>
      ) : dir ? (
        <div className="bg-surface-elevated rounded-lg border border-border p-3 flex items-center gap-3">
          <Library size={16} className="text-content-muted shrink-0" />
          <div className="flex-1 min-w-0">
            <div className="font-mono text-sm text-content truncate">{dir}</div>
            <div className="text-xs text-content-muted">
              {coveringRoot
                ? `inside monitored directory ${coveringRoot.path}`
                : isDedicatedRoot
                  ? 'dedicated library root (listed under Monitored Directories above)'
                  : 'master frame destination'}
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
            title="Remove calibration folder setting"
            className="p-1.5 rounded text-content-muted hover:text-error hover:bg-surface-hover transition-colors disabled:opacity-50"
          >
            <Trash2 size={16} />
          </button>
        </div>
      ) : (
        <div className="bg-surface-elevated rounded-lg p-6 text-center">
          <p className="text-content-muted text-sm">
            No calibration folder configured yet. Master builds need one &mdash; click
            &ldquo;Choose Calibration Folder&rdquo; to set it.
          </p>
        </div>
      )}
      {error && <div className="text-xs text-error mt-2">{error}</div>}

      {/* Web mode: folder browser for the calibration directory */}
      <FolderBrowserModal
        isOpen={showFolderBrowser}
        scope="scan"
        onSelect={path => {
          setShowFolderBrowser(false);
          void setLibraryDir(path);
        }}
        onClose={() => setShowFolderBrowser(false)}
      />
    </div>
  );
}
