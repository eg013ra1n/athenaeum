import { Plus, RefreshCw, Star } from 'lucide-react';
import type { ScanRootWithAvailability, ArchiveRoot, ArchivedFrameSetSummary } from '../../types/helpers';
import type { FolderOverview } from '../../types/models';
import { ROLE_META, ROLE_ORDER, KIND_META, type RailSelection, type RoleKind, type AddableKind } from './roleMeta';
import { basename, parentPath, formatBytes } from './format';

interface FolderRailProps {
  scanRoots: ScanRootWithAvailability[];
  archiveRoots: ArchiveRoot[];
  archivedSets: ArchivedFrameSetSummary[];
  overview: FolderOverview | null;
  missingCounts: Record<number, number>;
  selection: RailSelection | null;
  onSelect: (sel: RailSelection) => void;
  onAdd: (preselect?: AddableKind) => void;
  onRescan: (rootId: number) => void;
  isScanning: (rootId: number) => boolean;
  scanPercent: (rootId: number) => number | null;
}

const isSel = (sel: RailSelection | null, other: RailSelection) =>
  !!sel && sel.type === other.type &&
  (sel.type === 'placeholder' ? sel.kind === (other as { kind: RoleKind }).kind : sel.id === (other as { id: number }).id);

function GroupHeader({ label }: { label: string }) {
  return <div className="px-2 mt-4 mb-1 first:mt-0 text-[10px] font-bold uppercase tracking-wider text-content-muted">{label}</div>;
}

function ScanRow({ root, sub, tint, Icon, selected, onClick, onRescan, scanning, percent, missing }: {
  root: ScanRootWithAvailability; sub: string; tint: string;
  Icon: React.ComponentType<{ size?: number; className?: string }>;
  selected: boolean; onClick: () => void; onRescan: () => void;
  scanning: boolean; percent: number | null; missing: number;
}) {
  const offline = !root.is_available;
  return (
    <div
      onClick={onClick}
      className={`flex items-center gap-2 px-2 py-1.5 rounded-lg cursor-pointer transition ${selected ? 'bg-surface-hover shadow-[inset_2px_0_0] shadow-accent' : 'hover:bg-surface-hover/50'}`}
    >
      <Icon size={16} className={`${tint} shrink-0 ${offline ? 'opacity-50' : ''}`} />
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-1.5 text-sm font-semibold text-content truncate">
          <span className="truncate">{basename(root.path)}</span>
          {missing > 0 && (
            <span className="shrink-0 px-1.5 rounded-full text-[10px] font-semibold bg-orange/20 text-orange border border-orange/40">{missing} missing</span>
          )}
          {offline && (
            <span className="shrink-0 px-1.5 rounded-full text-[10px] font-semibold bg-error-muted text-error border border-error/40">offline</span>
          )}
        </div>
        <div className="text-[11px] text-content-muted truncate">
          {scanning ? `scanning…${percent != null ? ` ${Math.round(percent)}%` : ''}` : sub}
        </div>
      </div>
      <button
        onClick={(e) => { e.stopPropagation(); if (!offline && !scanning) onRescan(); }}
        disabled={offline || scanning}
        title={offline ? 'Folder is offline' : 'Rescan this folder'}
        className={`p-1 rounded shrink-0 transition ${offline ? 'opacity-30 cursor-not-allowed text-content-muted' : scanning ? 'cursor-not-allowed text-content-muted' : 'text-content-muted hover:text-accent hover:bg-surface-hover'}`}
      >
        <RefreshCw size={14} className={scanning ? 'animate-spin text-accent' : ''} />
      </button>
    </div>
  );
}

export function FolderRail({
  scanRoots, archiveRoots, archivedSets, overview, missingCounts,
  selection, onSelect, onAdd, onRescan, isScanning, scanPercent,
}: FolderRailProps) {
  const monitored = scanRoots
    .filter((r) => r.kind === 'normal')
    .sort((a, b) => basename(a.path).localeCompare(basename(b.path)));
  const roleRoots = new Map(scanRoots.filter((r) => r.kind !== 'normal').map((r) => [r.kind as RoleKind, r]));
  const sortedArchive = [...archiveRoots].sort((a, b) =>
    a.is_default === b.is_default ? basename(a.path).localeCompare(basename(b.path)) : a.is_default ? -1 : 1);

  const archiveRow = (root: ArchiveRoot) => overview?.archive_roots.find((a) => a.archive_root_id === root.id);
  const setCount = (root: ArchiveRoot) =>
    archiveRow(root)?.set_count ?? archivedSets.filter((s) => (s.archive_root_path ?? '') === root.path).length;
  const archiveBytes = (root: ArchiveRoot) => archiveRow(root)?.total_zip_bytes ?? 0;

  return (
    <div className="w-[300px] shrink-0 bg-surface-elevated rounded-lg p-3 overflow-y-auto">
      <button
        onClick={() => onAdd()}
        className="w-full flex items-center justify-center gap-2 px-3 py-2 mb-1 bg-accent hover:bg-accent-hover text-surface font-semibold rounded-lg transition"
      >
        <Plus size={16} /> Add Folder
      </button>

      <GroupHeader label="Monitored" />
      {monitored.length === 0 && <p className="px-2 text-xs text-content-muted">No monitored folders yet.</p>}
      {monitored.map((root) => (
        <ScanRow
          key={root.id}
          root={root}
          sub={parentPath(root.path)}
          tint={KIND_META.normal.tint}
          Icon={KIND_META.normal.icon}
          selected={isSel(selection, { type: 'scan', id: root.id! })}
          onClick={() => onSelect({ type: 'scan', id: root.id! })}
          onRescan={() => onRescan(root.id!)}
          scanning={root.id ? isScanning(root.id) : false}
          percent={root.id ? scanPercent(root.id) : null}
          missing={root.id ? (missingCounts[root.id] ?? 0) : 0}
        />
      ))}

      <GroupHeader label="Special roles" />
      {ROLE_ORDER.map((kind) => {
        const meta = ROLE_META[kind];
        const root = roleRoots.get(kind);
        if (root) {
          return (
            <ScanRow
              key={kind}
              root={root}
              sub={`${meta.label} · ${parentPath(root.path)}`}
              tint={meta.tint}
              Icon={meta.icon}
              selected={isSel(selection, { type: 'scan', id: root.id! })}
              onClick={() => onSelect({ type: 'scan', id: root.id! })}
              onRescan={() => onRescan(root.id!)}
              scanning={root.id ? isScanning(root.id) : false}
              percent={root.id ? scanPercent(root.id) : null}
              missing={root.id ? (missingCounts[root.id] ?? 0) : 0}
            />
          );
        }
        const selected = isSel(selection, { type: 'placeholder', kind });
        return (
          <div
            key={kind}
            onClick={() => onSelect({ type: 'placeholder', kind })}
            className={`flex items-center gap-2 px-2 py-1.5 rounded-lg border border-dashed border-border cursor-pointer transition ${selected ? 'bg-surface-hover' : 'hover:bg-surface-hover/50'}`}
          >
            <meta.icon size={16} className={`${meta.tint} opacity-60 shrink-0`} />
            <div className="flex-1 min-w-0">
              <div className="text-sm text-content-muted truncate">{meta.label}</div>
              <div className="text-[11px] text-content-muted/70 truncate">{meta.purpose}</div>
            </div>
            <button
              onClick={(e) => { e.stopPropagation(); onAdd(kind); }}
              className="shrink-0 px-2 py-1 rounded bg-surface-hover text-xs text-accent hover:brightness-110 transition"
            >
              Set up…
            </button>
          </div>
        );
      })}

      <GroupHeader label="Archive destinations" />
      {sortedArchive.length === 0 && <p className="px-2 text-xs text-content-muted">No archive folders yet.</p>}
      {sortedArchive.map((root) => {
        const selected = isSel(selection, { type: 'archive', id: root.id });
        const bytes = archiveBytes(root);
        return (
          <div
            key={root.id}
            onClick={() => onSelect({ type: 'archive', id: root.id })}
            className={`flex items-center gap-2 px-2 py-1.5 rounded-lg cursor-pointer transition ${selected ? 'bg-surface-hover shadow-[inset_2px_0_0] shadow-accent' : 'hover:bg-surface-hover/50'}`}
          >
            <KIND_META.archive.icon size={16} className={`${KIND_META.archive.tint} shrink-0`} />
            <div className="flex-1 min-w-0">
              <div className="flex items-center gap-1.5 text-sm font-semibold text-content truncate">
                <span className="truncate">{basename(root.path)}</span>
                {root.is_default && <Star size={12} className="text-warning shrink-0" fill="currentColor" />}
              </div>
              <div className="text-[11px] text-content-muted truncate">
                {parentPath(root.path)} · {setCount(root)} sets{bytes > 0 ? ` · ${formatBytes(bytes)}` : ''}
              </div>
            </div>
          </div>
        );
      })}
    </div>
  );
}
