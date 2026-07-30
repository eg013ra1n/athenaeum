import { useEffect, useState } from 'react';
import { RefreshCw, ExternalLink, AlertTriangle, AlertCircle, ChevronDown, ChevronRight, Loader2, CheckCircle2, Info } from 'lucide-react';
import { api } from '../../api';
import { revealItemInDir } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { formatTimestamp } from '../../utils/dateFormatting';
import { MissingFilesPanel } from '../MissingFilesPanel';
import { SwitchRow } from './SwitchRow';
import { basename, formatBytes } from './format';
import type { ScanRootWithAvailability, MissingFileRecord, ScanResult } from '../../types/helpers';
import type { RelinkResult, ScanRootOverview } from '../../types/models';

interface MonitoredInspectorProps {
  root: ScanRootWithAvailability;
  overview: ScanRootOverview | undefined;
  missingCount: number;
  scanResult: ScanResult | null;
  isScanning: boolean;
  relinking: boolean;
  relinkResult: RelinkResult | null;
  onScan: () => void;
  onRelink: () => void;
  onShowScanDetails: () => void;
  onToggleDuplicates: (v: boolean) => void;
  onToggleUniqueCamera: (v: boolean) => void;
  onToggleMonitor: (v: boolean) => void;
  onRemove: () => void;
  onMissingChanged: () => void;
}

export function MonitoredInspector(props: MonitoredInspectorProps) {
  const { root, overview, missingCount, scanResult, isScanning, relinking, relinkResult } = props;
  const offline = !root.is_available;
  const [missingOpen, setMissingOpen] = useState(false);
  const [missingFiles, setMissingFiles] = useState<MissingFileRecord[] | null>(null);
  const [missingError, setMissingError] = useState<string | null>(null);
  const [errorsOpen, setErrorsOpen] = useState(false);
  const displayErrors = scanResult?.errors ?? root.last_scan_errors ?? [];
  // Missing-file actions (recheck / delete / relocate) mutate the catalog, so they are
  // offline read-only per spec §5.4. `null` also covers an unpersisted root (id === null),
  // which nothing can be fetched for. The parse-error log below stays visible offline.
  const missingRootId = !offline && missingCount > 0 ? root.id : null;

  useEffect(() => { setMissingOpen(false); setMissingFiles(null); setMissingError(null); setErrorsOpen(false); }, [root.id]);

  const loadMissing = async () => {
    if (root.id == null) return;
    try {
      const files = await api.invoke<MissingFileRecord[]>('get_missing_files', { rootId: root.id });
      setMissingFiles(files);
      setMissingError(null);
    } catch (e) {
      console.error('[MonitoredInspector] get_missing_files failed:', e);
      setMissingError(String(e));
    }
  };

  return (
    <div className="flex-1 min-w-0 bg-surface-elevated rounded-lg p-5 overflow-y-auto">
      {/* Header */}
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2 text-lg font-bold text-content">
            <span className="truncate">{basename(root.path)}</span>
            <span className="px-2 py-0.5 rounded-full text-[10px] font-semibold bg-surface-hover text-content-muted border border-border">Monitored</span>
          </div>
          <div className="flex items-center gap-2 font-mono text-xs text-content-muted">
            <span className="truncate">{root.path}</span>
            {isTauri && !offline && (
              <button onClick={() => revealItemInDir(root.path).catch((e) => console.error('[MonitoredInspector] reveal failed:', e))}
                title="Reveal in file manager" className="p-0.5 rounded hover:text-accent transition"><ExternalLink size={12} /></button>
            )}
          </div>
        </div>
        {!offline && (
          <div className="flex gap-2 shrink-0">
            <button onClick={props.onScan} disabled={isScanning || relinking}
              className="flex items-center gap-2 px-3 py-2 bg-accent hover:bg-accent-hover text-surface font-semibold rounded-lg text-sm transition disabled:opacity-50">
              <RefreshCw size={14} className={isScanning ? 'animate-spin' : ''} /> {isScanning ? 'Scanning…' : 'Scan now'}
            </button>
            <button onClick={props.onRelink} disabled={isScanning || relinking}
              className="px-3 py-2 bg-surface-hover hover:brightness-110 rounded-lg text-sm text-content transition disabled:opacity-50">
              {relinking ? 'Relinking…' : 'Relink…'}
            </button>
          </div>
        )}
      </div>

      {/* Offline banner (spec §5.4) */}
      {offline && (
        <div className="mt-4 p-4 bg-error-muted border border-error/50 rounded-lg flex items-start gap-3">
          <AlertTriangle className="text-error shrink-0 mt-0.5" size={18} />
          <div className="flex-1">
            <p className="text-sm font-semibold text-error">Folder not reachable</p>
            <p className="text-xs text-error/80 mt-0.5 mb-2">
              Drive unmounted, renamed or moved. The catalog still remembers all
              {overview ? ` ${overview.file_count.toLocaleString()}` : ''} files — Relink points them to the new location;
              frame sets, calibration links and tags survive.
            </p>
            <button onClick={props.onRelink} disabled={relinking}
              className="flex items-center gap-2 px-3 py-1.5 bg-error hover:brightness-90 text-surface rounded text-sm transition disabled:opacity-50">
              <RefreshCw size={14} className={relinking ? 'animate-spin' : ''} /> {relinking ? 'Relinking…' : 'Relink — point to new location…'}
            </button>
          </div>
        </div>
      )}

      {/* Relink result */}
      {relinkResult && (
        <div className="mt-4 p-4 bg-surface rounded-lg border border-border">
          <h4 className="text-sm font-semibold text-content flex items-center gap-2 mb-2"><CheckCircle2 className="text-success" size={16} /> Relinking complete</h4>
          <div className="grid grid-cols-3 gap-4 text-sm">
            <div><p className="text-content-muted text-xs">Matched</p><p className="text-lg font-bold text-success">{relinkResult.files_matched}</p></div>
            <div><p className="text-content-muted text-xs">New files</p><p className="text-lg font-bold text-accent">{relinkResult.files_new}</p></div>
            <div><p className="text-content-muted text-xs">Orphaned</p><p className="text-lg font-bold text-warning">{relinkResult.files_orphaned}</p></div>
          </div>
        </div>
      )}

      {/* Stats */}
      <div className="flex flex-wrap gap-2 mt-4">
        <Stat label="files cataloged" value={overview ? overview.file_count.toLocaleString() : '—'} />
        <Stat label="on disk" value={overview ? formatBytes(overview.total_bytes) : '—'} />
        <Stat label="last scan" value={root.last_scan ? formatTimestamp(root.last_scan) : 'never'} />
        <Stat label="watching" value={root.monitor_enabled ? 'background interval' : 'manual only'} />
      </div>

      {/* Last scan result strip */}
      {scanResult && (
        <div className="mt-3 p-3 bg-success-muted border border-success/50 rounded-lg flex items-center justify-between text-sm">
          <span className="flex items-center gap-2 text-success font-semibold"><CheckCircle2 size={14} /> Scan complete — {scanResult.files_processed} processed</span>
          <button onClick={props.onShowScanDetails} title="View scan details" className="p-1 rounded hover:bg-surface-hover transition"><Info size={14} className="text-content-muted" /></button>
        </div>
      )}

      {!offline && (
        <Section title="Behavior">
          <SwitchRow title="Watch for new files" checked={root.monitor_enabled} onChange={props.onToggleMonitor}
            description="Re-scan this folder periodically in the background. The interval is global — Settings → Scanning." />
          <SwitchRow title="Include in duplicate detection" checked={root.find_duplicates} onChange={props.onToggleDuplicates}
            description="Files here are content-hashed and compared against every other folder with this enabled." />
          <SwitchRow title="Treat camera as unique to this folder" checked={root.unique_camera} onChange={props.onToggleUniqueCamera}
            description="Two rigs with the same camera model? Keeps their calibration frames apart. Takes effect after the next scan." />
        </Section>
      )}

      {(missingRootId !== null || displayErrors.length > 0) && (
        <Section title="Needs attention">
          {missingRootId !== null && (
            <div className="rounded-lg border border-orange/40 bg-surface">
              <button onClick={() => { const next = !missingOpen; setMissingOpen(next); if (next && !missingFiles) void loadMissing(); }}
                className="w-full flex items-center gap-2 p-2.5 text-left text-sm text-orange hover:bg-orange/10 rounded-lg transition">
                {missingOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                <AlertTriangle size={14} /> {missingCount} file{missingCount !== 1 ? 's' : ''} missing from disk
              </button>
              {missingOpen && (missingError
                ? <div className="p-3 flex items-center gap-2 text-xs text-error">
                    <AlertCircle size={12} className="shrink-0" />
                    <span className="flex-1 min-w-0 break-all">Could not load the missing-file list — {missingError}</span>
                    <button onClick={() => { setMissingError(null); void loadMissing(); }}
                      className="shrink-0 px-2 py-0.5 rounded border border-error/50 hover:bg-error-muted transition">Retry</button>
                  </div>
                : missingFiles
                  ? <div className="p-2"><MissingFilesPanel rootId={missingRootId} missingFiles={missingFiles} onRefresh={() => { void loadMissing(); props.onMissingChanged(); }} /></div>
                  : <div className="p-3 text-xs text-content-muted flex items-center gap-2"><Loader2 size={12} className="animate-spin" /> loading…</div>)}
            </div>
          )}
          {displayErrors.length > 0 && (
            <div className="rounded-lg border border-error/30 bg-surface mt-2">
              <button onClick={() => setErrorsOpen((v) => !v)}
                className="w-full flex items-center gap-2 p-2.5 text-left text-sm text-error hover:bg-error-muted rounded-lg transition">
                {errorsOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                <AlertCircle size={14} /> {displayErrors.length} file{displayErrors.length !== 1 ? 's' : ''} failed in last scan
              </button>
              {errorsOpen && (
                <div className="px-3 py-2 max-h-40 overflow-y-auto space-y-1">
                  {displayErrors.map((err, i) => <p key={i} className="text-xs text-error/80 font-mono break-all">{err}</p>)}
                </div>
              )}
            </div>
          )}
        </Section>
      )}

      <Section title="Remove">
        <div className="flex items-center gap-3 p-3 rounded-lg border border-error/30 bg-surface">
          <p className="flex-1 text-xs text-content-muted">
            Forgets the folder and its catalog entries (frames, the sets they belong to).{' '}
            <span className="font-semibold text-content">Files on disk are never touched.</span>
          </p>
          <button onClick={props.onRemove}
            className="shrink-0 px-3 py-1.5 rounded-lg border border-error/50 text-error text-sm hover:bg-error-muted transition">
            Remove folder…
          </button>
        </div>
      </Section>
    </div>
  );
}

export function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="px-3 py-1.5 bg-surface rounded-lg">
      <div className="text-sm font-bold text-content">{value}</div>
      <div className="text-[10px] text-content-muted">{label}</div>
    </div>
  );
}

export function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="mt-5 pt-4 border-t border-border">
      <div className="text-[10px] font-bold uppercase tracking-wider text-content-muted mb-2">{title}</div>
      {children}
    </div>
  );
}
