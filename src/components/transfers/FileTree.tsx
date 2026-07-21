import { useMemo, useState } from 'react';
import { ChevronDown, ChevronRight, Folder, FileIcon } from 'lucide-react';
import { formatBytes, outcomeChipClass, outcomeLabel, fileStateChipClass } from './presentation';
import type { TransferFileEntry } from '../../types/models';

interface FileTreeProps {
  entries: TransferFileEntry[];
  /** Live per-file bytes keyed by forward-slash relPath (from `sync-file-progress`). */
  liveOverlay: Map<string, { bytesDone: number; bytesTotal: number }> | undefined;
  /** The owning batch is still moving — un-settled files show a live bar instead of a chip. */
  active: boolean;
}

interface DirNode {
  name: string;
  /** Directory path (joined segments) — the collapse key. */
  path: string;
  dirs: Map<string, DirNode>;
  files: TransferFileEntry[];
}

function newDir(name: string, path: string): DirNode {
  return { name, path, dirs: new Map(), files: [] };
}

/** Build a directory tree from the entries' forward-slash `relPath`s (§D2). A
 *  batch with no `/` in any path degenerates to a flat root — the render below
 *  then shows a plain file list, no folder rows. */
function buildTree(entries: TransferFileEntry[]): DirNode {
  const root = newDir('', '');
  for (const entry of entries) {
    const segments = entry.relPath.split('/').filter(Boolean);
    if (segments.length <= 1) {
      root.files.push(entry);
      continue;
    }
    let node = root;
    const dirSegs = segments.slice(0, -1);
    let acc = '';
    for (const seg of dirSegs) {
      acc = acc ? `${acc}/${seg}` : seg;
      let child = node.dirs.get(seg);
      if (!child) {
        child = newDir(seg, acc);
        node.dirs.set(seg, child);
      }
      node = child;
    }
    node.files.push(entry);
  }
  return root;
}

export function FileTree({ entries, liveOverlay, active }: FileTreeProps) {
  const tree = useMemo(() => buildTree(entries), [entries]);
  // All folders open by default (torrent-client default) — collapsing is opt-in.
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());

  const toggle = (path: string) =>
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });

  if (entries.length === 0) {
    return <p className="px-1 py-3 text-xs text-content-muted">No file detail yet.</p>;
  }

  return (
    <ul className="space-y-0.5 py-1">
      <DirChildren
        node={tree}
        depth={0}
        collapsed={collapsed}
        toggle={toggle}
        liveOverlay={liveOverlay}
        active={active}
      />
    </ul>
  );
}

interface DirChildrenProps {
  node: DirNode;
  depth: number;
  collapsed: Set<string>;
  toggle: (path: string) => void;
  liveOverlay: Map<string, { bytesDone: number; bytesTotal: number }> | undefined;
  active: boolean;
}

function DirChildren({ node, depth, collapsed, toggle, liveOverlay, active }: DirChildrenProps) {
  // Directories first (sorted), then files (sorted) — stable, predictable order.
  const dirs = [...node.dirs.values()].sort((a, b) => a.name.localeCompare(b.name));
  const files = [...node.files].sort((a, b) => a.name.localeCompare(b.name));
  return (
    <>
      {dirs.map((dir) => {
        const isCollapsed = collapsed.has(dir.path);
        return (
          <li key={`d:${dir.path}`}>
            <button
              type="button"
              onClick={() => toggle(dir.path)}
              className="flex w-full items-center gap-1.5 rounded px-1 py-1 text-left text-xs text-content-secondary transition-colors hover:bg-surface-hover"
              style={{ paddingLeft: `${depth * 14 + 4}px` }}
            >
              {isCollapsed ? (
                <ChevronRight size={13} className="shrink-0 text-content-muted" />
              ) : (
                <ChevronDown size={13} className="shrink-0 text-content-muted" />
              )}
              <Folder size={13} className="shrink-0 text-accent" />
              <span className="truncate font-medium">{dir.name}</span>
            </button>
            {!isCollapsed && (
              <ul className="space-y-0.5">
                <DirChildren
                  node={dir}
                  depth={depth + 1}
                  collapsed={collapsed}
                  toggle={toggle}
                  liveOverlay={liveOverlay}
                  active={active}
                />
              </ul>
            )}
          </li>
        );
      })}
      {files.map((f) => (
        <FileRow key={`f:${f.relPath}`} entry={f} depth={depth} liveOverlay={liveOverlay} active={active} />
      ))}
    </>
  );
}

interface FileRowProps {
  entry: TransferFileEntry;
  depth: number;
  liveOverlay: Map<string, { bytesDone: number; bytesTotal: number }> | undefined;
  active: boolean;
}

function FileRow({ entry, depth, liveOverlay, active }: FileRowProps) {
  const live = liveOverlay?.get(entry.relPath);
  const doneBytes = live?.bytesDone ?? entry.bytesDone ?? 0;
  const totalBytes = live?.bytesTotal ?? entry.bytesTotal;
  const fraction = totalBytes > 0 ? Math.min(1, doneBytes / totalBytes) : 0;

  // A live bar shows while the batch is moving and this file has no settled
  // outcome yet; otherwise the file shows its outcome/state chip.
  const showBar = active && entry.outcome == null;

  return (
    <li
      className="flex items-center gap-2 rounded px-1 py-1 text-xs"
      style={{ paddingLeft: `${depth * 14 + 22}px` }}
    >
      <FileIcon size={12} className="shrink-0 text-content-muted" />
      <span className="min-w-0 flex-1 truncate text-content-secondary" title={entry.relPath}>
        {entry.name}
      </span>
      <span className="shrink-0 text-content-muted tabular-nums">{formatBytes(entry.bytesTotal)}</span>
      {showBar ? (
        <div className="h-1 w-24 shrink-0 overflow-hidden rounded-full bg-surface-hover">
          <div
            className="h-full rounded-full bg-accent transition-all"
            style={{ width: `${Math.round(fraction * 100)}%` }}
          />
        </div>
      ) : entry.outcome ? (
        <span
          className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium ${outcomeChipClass(entry.outcome)}`}
          title={entry.error ?? entry.outcome}
        >
          {outcomeLabel(entry.outcome)}
        </span>
      ) : entry.state ? (
        <span
          className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium ${fileStateChipClass(entry.state)}`}
        >
          {entry.state}
        </span>
      ) : null}
    </li>
  );
}
