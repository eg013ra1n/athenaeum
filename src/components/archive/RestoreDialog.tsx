import { useEffect, useState } from 'react';
import { Folder } from 'lucide-react';
import { getRestoreSuggestions, startRestoreOperation } from '../../api/archive';
import { pickDirectory } from '../../api/desktop';
import type { ArchivedFrameSetSummary } from '../../types/helpers';
import { Button, DialogShell, Select, TextInput } from '../ui';

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
  const [error, setError] = useState<string | null>(null);

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
        setError(`Restore options not loaded: ${e}`);
      })
      .finally(() => setLoading(false));
  }, [item.operation_id]);

  async function pickCustom() {
    try {
      const picked = await pickDirectory();
      if (picked) {
        setCustomPath(picked);
        setMode('custom');
      }
    } catch (e) {
      console.error('[RestoreDialog] folder picker failed:', e);
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
      setError('Pick a target folder first.');
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await startRestoreOperation(item.operation_id, target, overwrite, keepZip);
      // Hand off to parent: dialog closes, progress widget appears in its place.
      onStarted(item.operation_id);
    } catch (e) {
      console.error('restore failed', e);
      setError(`Restore failed: ${e}`);
      setBusy(false);
    }
  }

  const target = effectiveTarget();
  const name = item.name ?? `Frame Set #${item.frames_set_id}`;
  const option = (active: boolean) =>
    `flex items-start gap-2 rounded border px-2.5 py-2 cursor-pointer transition-colors ${active ? 'border-accent bg-accent/5' : 'border-border hover:bg-surface-hover'}`;

  return (
    <DialogShell
      title={`Unarchive ${name}`}
      size="md"
      busy={busy}
      onClose={onCancel}
      footer={
        <>
          <Button onClick={onCancel} disabled={busy}>Cancel</Button>
          <Button variant="primary" onClick={() => void start()} disabled={loading || busy || !target} data-autofocus>
            {busy ? 'Restoring…' : 'Restore'}
          </Button>
        </>
      }
    >
      {loading ? (
        <p role="status">Loading restore options…</p>
      ) : (
        <div className="space-y-3">
          <div>
            <div className="mb-1 text-content-secondary">Restore to</div>
            <div className="space-y-1.5">
              {/* Original location */}
              <label className={option(mode === 'original')}>
                <input
                  type="radio"
                  name="restore-mode"
                  checked={mode === 'original'}
                  onChange={() => setMode('original')}
                  disabled={!originalPath}
                  className="mt-0.5 accent-accent"
                />
                <div className="min-w-0 flex-1">
                  <div className="font-medium text-content">Original location</div>
                  <div className="mt-0.5 break-all font-mono text-[11px]">
                    {originalPath ?? 'Could not determine original location'}
                  </div>
                  {originalPath && !originalExists && (
                    <div className="mt-1 text-warning">
                      This path no longer exists on disk — pick another option below or create it manually first.
                    </div>
                  )}
                </div>
              </label>

              {/* Existing scan root */}
              {scanRoots.length > 0 && (
                <label className={option(mode === 'scan_root')}>
                  <input
                    type="radio"
                    name="restore-mode"
                    checked={mode === 'scan_root'}
                    onChange={() => setMode('scan_root')}
                    className="mt-0.5 accent-accent"
                  />
                  <div className="min-w-0 flex-1">
                    <div className="font-medium text-content">Existing scan root</div>
                    <Select
                      value={selectedScanRoot}
                      onChange={(e) => { setSelectedScanRoot(e.target.value); setMode('scan_root'); }}
                      disabled={mode !== 'scan_root'}
                      className="mt-1 w-full font-mono disabled:opacity-50"
                    >
                      {scanRoots.map((r) => (
                        <option key={r} value={r}>{r}</option>
                      ))}
                    </Select>
                  </div>
                </label>
              )}

              {/* Custom folder */}
              <label className={option(mode === 'custom')}>
                <input
                  type="radio"
                  name="restore-mode"
                  checked={mode === 'custom'}
                  onChange={() => setMode('custom')}
                  className="mt-0.5 accent-accent"
                />
                <div className="min-w-0 flex-1">
                  <div className="font-medium text-content">Custom folder</div>
                  <div className="mt-1 flex gap-2">
                    <TextInput
                      value={customPath}
                      onChange={(e) => { setCustomPath(e.target.value); setMode('custom'); }}
                      placeholder="Choose a folder…"
                      title={customPath || undefined}
                      className="min-w-0 flex-1 font-mono"
                    />
                    <Button onClick={() => void pickCustom()}>
                      <Folder size={12} aria-hidden="true" />
                      Browse
                    </Button>
                  </div>
                </div>
              </label>
            </div>
          </div>

          <div className="space-y-1.5">
            <label className="flex items-center gap-2 cursor-pointer text-content-secondary">
              <input
                type="checkbox"
                checked={overwrite}
                onChange={(e) => setOverwrite(e.target.checked)}
                className="accent-accent"
              />
              Overwrite existing files at target
            </label>
            <label className="flex items-center gap-2 cursor-pointer text-content-secondary">
              <input
                type="checkbox"
                checked={keepZip}
                onChange={(e) => setKeepZip(e.target.checked)}
                className="accent-accent"
              />
              Keep zip file after restore (default: delete)
            </label>
          </div>

          {error && <p role="alert" className="break-words text-error">{error}</p>}
        </div>
      )}
    </DialogShell>
  );
}
