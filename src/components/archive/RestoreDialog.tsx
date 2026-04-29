import { useEffect, useState } from 'react';
import { getRestoreSuggestions, startRestoreOperation } from '../../api/archive';
import { pickDirectory } from '../../api/desktop';
import type { ArchivedFrameSetSummary } from '../../types/archive';

interface Props {
  item: ArchivedFrameSetSummary;
  onCancel: () => void;
  /** Called immediately after the restore worker is dispatched, with the
   *  operation_id so the parent page can mount a progress widget. The actual
   *  restore continues asynchronously. */
  onStarted: (operationId: number) => void;
}

type Mode = 'original' | 'scan_root' | 'custom';

export function RestoreDialog({ item, onCancel, onStarted }: Props) {
  const [mode, setMode] = useState<Mode>('original');
  const [originalPath, setOriginalPath] = useState<string | null>(null);
  const [originalExists, setOriginalExists] = useState(false);
  const [scanRoots, setScanRoots] = useState<string[]>([]);
  const [selectedScanRoot, setSelectedScanRoot] = useState<string>('');
  const [customPath, setCustomPath] = useState<string>('');
  const [overwrite, setOverwrite] = useState(false);
  const [keepZip, setKeepZip] = useState(false);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!item.operation_id) {
      setLoading(false);
      return;
    }
    getRestoreSuggestions(item.operation_id)
      .then((s) => {
        setOriginalPath(s.suggested_original_path);
        setOriginalExists(s.suggested_original_exists);
        setScanRoots(s.scan_roots);
        // Default mode: original if it exists; else first scan root; else custom
        if (s.suggested_original_path && s.suggested_original_exists) {
          setMode('original');
        } else if (s.scan_roots.length > 0) {
          setMode('scan_root');
          setSelectedScanRoot(s.scan_roots[0]);
        } else {
          setMode('custom');
        }
      })
      .catch((e) => {
        console.error('Failed to load restore suggestions', e);
      })
      .finally(() => setLoading(false));
  }, [item.operation_id]);

  async function pickCustom() {
    const picked = await pickDirectory();
    if (picked) {
      setCustomPath(picked);
      setMode('custom');
    }
  }

  function effectiveTarget(): string {
    if (mode === 'original' && originalPath) return originalPath;
    if (mode === 'scan_root') return selectedScanRoot;
    if (mode === 'custom') return customPath;
    return '';
  }

  async function start() {
    if (!item.operation_id) return;
    const target = effectiveTarget();
    if (!target) {
      alert('Pick a target folder first.');
      return;
    }
    setBusy(true);
    try {
      await startRestoreOperation(item.operation_id, target, overwrite, keepZip);
      // Hand off to parent: dialog closes, progress widget appears in its place.
      onStarted(item.operation_id);
    } catch (e) {
      console.error('restore failed', e);
      alert(`Restore failed: ${e}`);
      setBusy(false);
    }
  }

  if (loading) {
    return (
      <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-50 p-4">
        <div className="bg-surface-elevated border border-border rounded-lg shadow-2xl p-6 max-w-lg w-full">
          <p className="text-sm text-content-muted">Loading restore options…</p>
        </div>
      </div>
    );
  }

  const target = effectiveTarget();

  return (
    <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-50 p-4">
      <div className="bg-surface-elevated border border-border rounded-lg shadow-2xl p-6 max-w-xl w-full">
        <h2 className="text-lg font-semibold mb-2 text-content">Restore archive</h2>
        <p className="text-sm text-content-muted mb-4">
          {item.name ?? `Frame Set #${item.frames_set_id}`}
        </p>

        <label className="block text-sm font-medium text-content mb-2">Restore target</label>

        <div className="space-y-2 mb-4">
          {/* Original location */}
          <label className={`flex items-start gap-2 p-2 rounded-lg border ${mode === 'original' ? 'border-accent bg-accent/5' : 'border-border'} cursor-pointer hover:bg-surface-hover transition-colors`}>
            <input
              type="radio"
              name="restore-mode"
              checked={mode === 'original'}
              onChange={() => setMode('original')}
              disabled={!originalPath}
              className="mt-0.5 accent-accent"
            />
            <div className="flex-1">
              <div className="text-sm text-content font-medium">Original location</div>
              <div className="text-xs text-content-muted font-mono mt-0.5 break-all">
                {originalPath ?? 'Could not determine original location'}
              </div>
              {originalPath && !originalExists && (
                <div className="text-xs text-warning mt-1">
                  This path no longer exists on disk — pick another option below or create it manually first.
                </div>
              )}
            </div>
          </label>

          {/* Existing scan root */}
          {scanRoots.length > 0 && (
            <label className={`flex items-start gap-2 p-2 rounded-lg border ${mode === 'scan_root' ? 'border-accent bg-accent/5' : 'border-border'} cursor-pointer hover:bg-surface-hover transition-colors`}>
              <input
                type="radio"
                name="restore-mode"
                checked={mode === 'scan_root'}
                onChange={() => setMode('scan_root')}
                className="mt-0.5 accent-accent"
              />
              <div className="flex-1">
                <div className="text-sm text-content font-medium">Existing scan root</div>
                <select
                  value={selectedScanRoot}
                  onChange={(e) => { setSelectedScanRoot(e.target.value); setMode('scan_root'); }}
                  disabled={mode !== 'scan_root'}
                  className="mt-1 w-full px-2 py-1 bg-surface-hover border border-border rounded text-xs font-mono text-content disabled:opacity-50"
                >
                  {scanRoots.map((r) => (
                    <option key={r} value={r}>{r}</option>
                  ))}
                </select>
              </div>
            </label>
          )}

          {/* Custom folder */}
          <label className={`flex items-start gap-2 p-2 rounded-lg border ${mode === 'custom' ? 'border-accent bg-accent/5' : 'border-border'} cursor-pointer hover:bg-surface-hover transition-colors`}>
            <input
              type="radio"
              name="restore-mode"
              checked={mode === 'custom'}
              onChange={() => setMode('custom')}
              className="mt-0.5 accent-accent"
            />
            <div className="flex-1">
              <div className="text-sm text-content font-medium">Custom folder</div>
              <div className="flex gap-2 mt-1">
                <input
                  type="text"
                  value={customPath}
                  onChange={(e) => { setCustomPath(e.target.value); setMode('custom'); }}
                  placeholder="Choose a folder…"
                  className="flex-1 px-2 py-1 bg-surface-hover border border-border rounded text-xs font-mono text-content"
                />
                <button
                  type="button"
                  onClick={pickCustom}
                  className="px-3 py-1 border border-border rounded text-xs text-content hover:bg-surface-hover transition-colors"
                >
                  Browse…
                </button>
              </div>
            </div>
          </label>
        </div>

        <label className="flex items-center gap-2 text-sm text-content mb-2 cursor-pointer">
          <input
            type="checkbox"
            checked={overwrite}
            onChange={(e) => setOverwrite(e.target.checked)}
            className="accent-accent"
          />
          Overwrite existing files at target
        </label>
        <label className="flex items-center gap-2 text-sm text-content mb-5 cursor-pointer">
          <input
            type="checkbox"
            checked={keepZip}
            onChange={(e) => setKeepZip(e.target.checked)}
            className="accent-accent"
          />
          Keep zip file after restore (default: delete)
        </label>

        <div className="flex justify-end gap-2">
          <button
            onClick={onCancel}
            disabled={busy}
            className="px-4 py-1.5 rounded-lg border border-border text-sm text-content hover:bg-surface-hover transition-colors disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            onClick={start}
            disabled={busy || !target}
            className="px-4 py-1.5 rounded-lg bg-accent text-white text-sm disabled:opacity-50 disabled:cursor-not-allowed hover:brightness-110 transition-colors"
          >
            {busy ? 'Restoring…' : 'Restore'}
          </button>
        </div>
      </div>
    </div>
  );
}
