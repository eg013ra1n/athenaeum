import { useEffect, useState } from 'react';
import { AlertTriangle, AlertCircle, ChevronDown, ChevronRight, Loader2 } from 'lucide-react';
import { api } from '../../api';
import { MissingFilesPanel } from '../MissingFilesPanel';
import type { MissingFileRecord } from '../../types/helpers';

interface MissingFilesDisclosureProps {
  rootId: number;
  missingCount: number;
  /** The catalog changed under the list (delete / relocate) — the host refreshes its counts. */
  onMissingChanged: () => void;
}

/**
 * The "N files missing from disk" disclosure of a folder's Needs-attention
 * section: the count as a collapsible header, and the MissingFilesPanel
 * (recheck / locate / ignore / delete from database) loaded on first open.
 *
 * Shared by the monitored and the role inspectors. The Calibration Library
 * used to get the rail badge and nothing to act on it, so a master whose file
 * was gone could not be purged from Folders at all — and purging is what
 * un-supersedes its raw set (`delete_missing_files` →
 * `relinking::delete_orphaned_files`). Callers gate rendering: nothing to show
 * while offline (every action here mutates the catalog, spec §5.4), with no
 * missing files, or for an unpersisted root.
 */
export function MissingFilesDisclosure({ rootId, missingCount, onMissingChanged }: MissingFilesDisclosureProps) {
  const [open, setOpen] = useState(false);
  const [files, setFiles] = useState<MissingFileRecord[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Inspectors are remounted per selection (FoldersTab's `selectionKey`), so this
  // reset is belt-and-braces for a host that reuses the component across roots.
  useEffect(() => { setOpen(false); setFiles(null); setError(null); }, [rootId]);

  const load = async () => {
    try {
      const list = await api.invoke<MissingFileRecord[]>('get_missing_files', { rootId });
      setFiles(list);
      setError(null);
    } catch (e) {
      console.error('[MissingFilesDisclosure] get_missing_files failed:', e);
      setError(String(e));
    }
  };

  const panelId = `missing-files-panel-${rootId}`;
  return (
    <div className="rounded-lg border border-orange/40 bg-surface">
      <button onClick={() => { const next = !open; setOpen(next); if (next && !files) void load(); }}
        aria-expanded={open} aria-controls={panelId}
        className="w-full flex items-center gap-2 p-2.5 text-left text-sm text-orange hover:bg-orange/10 rounded-lg transition">
        {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
        <AlertTriangle size={14} /> {missingCount} file{missingCount !== 1 ? 's' : ''} missing from disk
      </button>
      {open && (
        <div id={panelId}>
          {error
            ? <div className="p-3 flex items-center gap-2 text-xs text-error">
                <AlertCircle size={12} className="shrink-0" />
                <span className="flex-1 min-w-0 break-all">Could not load the missing-file list — {error}</span>
                <button onClick={() => { setError(null); void load(); }}
                  className="shrink-0 px-2 py-0.5 rounded border border-error/50 hover:bg-error-muted transition">Retry</button>
              </div>
            : files
              ? <div className="p-2"><MissingFilesPanel rootId={rootId} missingFiles={files} onRefresh={() => { void load(); onMissingChanged(); }} /></div>
              : <div className="p-3 text-xs text-content-muted flex items-center gap-2"><Loader2 size={12} className="animate-spin" /> loading…</div>}
        </div>
      )}
    </div>
  );
}
