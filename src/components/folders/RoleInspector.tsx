import { AlertTriangle, CheckCircle2, ExternalLink, RefreshCw } from 'lucide-react';
import { revealItemInDir } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { formatTimestamp } from '../../utils/dateFormatting';
import { SwitchRow } from './SwitchRow';
import { Stat, Section } from './MonitoredInspector';
import { basename, formatBytes } from './format';
import { ROLE_META, type RoleKind } from './roleMeta';
import type { ScanRootWithAvailability } from '../../types/helpers';
import type { RelinkResult, ScanRootOverview } from '../../types/models';

interface RoleInspectorProps {
  kind: RoleKind;
  /** The dedicated scan root — null for a covered calibration library (settings-only). */
  root: ScanRootWithAvailability | null;
  /** Effective directory (root path, or the covered library path). */
  dir: string;
  /** Monitored root covering a settings-only calibration library, if any. */
  coveredBy: string | null;
  overview: ScanRootOverview | undefined;
  isScanning: boolean;
  onScan: () => void;
  onChangeFolder: () => void;
  onReleaseRole: () => void;
  onToggleDuplicates: (v: boolean) => void;
  onToggleMonitor: (v: boolean) => void;
  /**
   * Relink (spec §5.4) — a role folder on a drive that went away is just as
   * relinkable as a plain monitored one. Optional: a COVERED calibration
   * library (`root === null`) has no root to relink, and the banner that hosts
   * the button only renders for a non-null offline root anyway.
   */
  relinking?: boolean;
  relinkResult?: RelinkResult | null;
  onRelink?: () => void;
}

export function RoleInspector(props: RoleInspectorProps) {
  const meta = ROLE_META[props.kind];
  const { root, dir, coveredBy, overview, relinking, relinkResult } = props;
  const offline = root ? !root.is_available : false;
  return (
    <div className="flex-1 min-w-0 bg-surface-elevated rounded-lg p-5 overflow-y-auto">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2 text-lg font-bold text-content">
            <span className="truncate">{basename(dir)}</span>
            <span className={`px-2 py-0.5 rounded-full text-[10px] font-semibold ${meta.chip}`}>{meta.label}</span>
            {offline && <span className="px-2 py-0.5 rounded-full text-[10px] font-semibold bg-error-muted text-error border border-error/40">offline</span>}
          </div>
          <div className="flex items-center gap-2 font-mono text-xs text-content-muted">
            <span className="truncate">{dir}</span>
            {isTauri && !offline && (
              <button onClick={() => revealItemInDir(dir).catch((e) => console.error('[RoleInspector] reveal failed:', e))}
                title="Reveal in file manager" aria-label="Reveal in file manager" className="p-0.5 rounded hover:text-accent transition"><ExternalLink size={12} /></button>
            )}
          </div>
        </div>
        {root && !offline && (
          <button onClick={props.onScan} disabled={props.isScanning}
            className="flex items-center gap-2 px-3 py-2 bg-accent hover:bg-accent-hover text-surface font-semibold rounded-lg text-sm transition disabled:opacity-50 shrink-0">
            <RefreshCw size={14} className={props.isScanning ? 'animate-spin' : ''} /> {props.isScanning ? 'Scanning…' : 'Scan now'}
          </button>
        )}
      </div>

      {/* Offline banner + relink (spec §5.4) — same affordance a plain
          monitored folder gets; a role must not make a lost drive unfixable. */}
      {root && offline && (
        <div className="mt-4 p-4 bg-error-muted border border-error/50 rounded-lg flex items-start gap-3">
          <AlertTriangle className="text-error shrink-0 mt-0.5" size={18} />
          <div className="flex-1">
            <p className="text-sm font-semibold text-error">Folder not reachable</p>
            <p className="text-xs text-error/80 mt-0.5 mb-2">
              Drive unmounted, renamed or moved. Relink points the catalog to the new location.
            </p>
            {props.onRelink && (
              <button onClick={props.onRelink} disabled={relinking}
                className="flex items-center gap-2 px-3 py-1.5 bg-error hover:brightness-90 text-surface rounded text-sm transition disabled:opacity-50">
                <RefreshCw size={14} className={relinking ? 'animate-spin' : ''} /> {relinking ? 'Relinking…' : 'Relink — point to new location…'}
              </button>
            )}
          </div>
        </div>
      )}

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

      <div className={`mt-4 p-3 rounded-lg bg-surface border text-xs text-content-muted ${props.kind === 'calibration_library' ? 'border-purple/40' : 'border-border'}`}>
        {meta.explainer}
        {overview && <span className="font-semibold text-content"> {overview.file_count.toLocaleString()} files cataloged.</span>}
      </div>

      <div className="flex flex-wrap gap-2 mt-3">
        <Stat label="placement" value={coveredBy ? `inside ${basename(coveredBy)}` : 'standalone · own scanned folder'} />
        {root?.last_scan && <Stat label="last scan" value={formatTimestamp(root.last_scan)} />}
        {overview && <Stat label="on disk" value={formatBytes(overview.total_bytes)} />}
      </div>

      <Section title="Role">
        <div className="flex items-center gap-3 p-3 rounded-lg border border-border bg-surface">
          <p className="flex-1 text-xs text-content-muted">
            Move the role to a different folder, or release it. Releasing keeps the folder monitored and never touches files.
          </p>
          <button onClick={props.onChangeFolder} className="shrink-0 px-3 py-1.5 rounded-lg bg-surface-hover text-sm text-content hover:brightness-110 transition">Change folder…</button>
          <button onClick={props.onReleaseRole} className="shrink-0 px-3 py-1.5 rounded-lg bg-surface-hover text-sm text-content hover:brightness-110 transition">Release role</button>
        </div>
      </Section>

      {root && !offline && (
        <Section title="Behavior">
          {meta.switches.watch && (
            <SwitchRow title="Watch for new files" checked={root.monitor_enabled} onChange={props.onToggleMonitor}
              description={props.kind === 'calibration_library'
                ? 'Imports masters dropped in from outside. The interval is global — Settings → Scanning.'
                : 'Re-scan this folder periodically in the background. The interval is global — Settings → Scanning.'} />
          )}
          {meta.switches.duplicates && (
            <SwitchRow title="Include in duplicate detection" checked={root.find_duplicates} onChange={props.onToggleDuplicates}
              description="Files here are content-hashed and compared against every other folder with this enabled." />
          )}
        </Section>
      )}
    </div>
  );
}

/** Placeholder-selection state — role not assigned yet (spec §4). */
export function RolePlaceholderInspector({ kind, onSetUp }: { kind: RoleKind; onSetUp: () => void }) {
  const meta = ROLE_META[kind];
  return (
    <div className="flex-1 min-w-0 bg-surface-elevated rounded-lg p-5 flex flex-col items-center justify-center text-center">
      <meta.icon size={36} className={`${meta.tint} opacity-70`} />
      <div className="mt-3 text-base font-bold text-content">{meta.label} — not set</div>
      <p className="mt-1 max-w-sm text-xs text-content-muted">{meta.purpose} {meta.placementRule}</p>
      <button onClick={onSetUp} className="mt-4 px-4 py-2 bg-accent hover:bg-accent-hover text-surface font-semibold rounded-lg text-sm transition">Set up…</button>
    </div>
  );
}
