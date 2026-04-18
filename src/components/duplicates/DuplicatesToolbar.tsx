import React from 'react';
import { AlertTriangle, ChevronsDownUp, ChevronsUpDown, RefreshCw, RotateCcw, Shield, Trash2 } from 'lucide-react';
import type { ScanRootWithAvailability } from '../../types/models';

export type SortMode = 'wasted-desc' | 'count-desc' | 'size-desc';

interface DuplicatesToolbarProps {
  loading: boolean;
  onRefresh: () => void;

  scanRoots: ScanRootWithAvailability[];
  masterRootId: number | null;
  onMasterRootChange: (id: number | null) => void;

  sortMode: SortMode;
  onSortChange: (mode: SortMode) => void;

  totalGroups: number;
  /** Number of groups with at least one file marked for deletion. */
  groupsWithDeletions: number;
  /** Number of groups where every copy is marked — surface as a warning. */
  groupsWithAllDeleted: number;
  deleteCount: number;
  bytesToFree: number;

  /** Reseed every group's deletion set from the current rule, dropping any
   *  manual check/uncheck the user has done. */
  onResetToRule: () => void;

  /** True when at least one group is currently expanded. Controls the
   *  expand/collapse-all button's mode. */
  anyExpanded: boolean;
  onExpandAll: () => void;
  onCollapseAll: () => void;

  onMoveToBlackHole: () => void;
  moveDisabled: boolean;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(1)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  const gb = mb / 1024;
  return `${gb.toFixed(2)} GB`;
}

export const DuplicatesToolbar: React.FC<DuplicatesToolbarProps> = ({
  loading,
  onRefresh,
  scanRoots,
  masterRootId,
  onMasterRootChange,
  sortMode,
  onSortChange,
  totalGroups,
  groupsWithDeletions,
  groupsWithAllDeleted,
  deleteCount,
  bytesToFree,
  onResetToRule,
  anyExpanded,
  onExpandAll,
  onCollapseAll,
  onMoveToBlackHole,
  moveDisabled,
}) => {
  return (
    <div className="flex items-center gap-3 flex-wrap pb-3 border-b border-border">
      <button
        onClick={onRefresh}
        disabled={loading}
        title="Refresh"
        className="h-7 px-2.5 inline-flex items-center gap-1 text-xs font-medium rounded-lg border border-border bg-surface-hover hover:bg-surface-elevated text-content-secondary hover:text-content transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
      >
        <RefreshCw size={12} className={loading ? 'animate-spin' : ''} />
        {loading ? 'Loading…' : 'Refresh'}
      </button>

      <span className="text-border h-7 flex items-center">|</span>

      {/* Master root selector */}
      <div className="flex items-center gap-2">
        <Shield size={14} className="text-accent" />
        <label className="text-xs text-content-muted" htmlFor="master-root-select">
          Master root:
        </label>
        <select
          id="master-root-select"
          value={masterRootId ?? ''}
          onChange={(e) => onMasterRootChange(e.target.value === '' ? null : Number(e.target.value))}
          className="h-7 bg-surface-hover border border-border rounded-lg px-2 text-xs focus:outline-none focus:ring-2 focus:ring-accent max-w-[320px]"
        >
          <option value="">— None (manual) —</option>
          {scanRoots.map((root) => (
            <option key={root.id!} value={root.id!}>
              {root.path}
            </option>
          ))}
        </select>
      </div>

      <span className="text-border h-7 flex items-center">|</span>

      {/* Sort */}
      <div className="flex items-center gap-2">
        <label className="text-xs text-content-muted" htmlFor="sort-mode-select">
          Sort:
        </label>
        <select
          id="sort-mode-select"
          value={sortMode}
          onChange={(e) => onSortChange(e.target.value as SortMode)}
          className="h-7 bg-surface-hover border border-border rounded-lg px-2 text-xs focus:outline-none focus:ring-2 focus:ring-accent"
        >
          <option value="wasted-desc">Wasted space</option>
          <option value="count-desc">File count</option>
          <option value="size-desc">File size</option>
        </select>
      </div>

      <button
        onClick={anyExpanded ? onCollapseAll : onExpandAll}
        title={anyExpanded ? 'Collapse all groups' : 'Expand all groups'}
        className="h-7 px-2.5 inline-flex items-center gap-1 text-xs font-medium rounded-lg border border-border bg-surface-hover hover:bg-surface-elevated text-content-secondary hover:text-content transition-colors"
      >
        {anyExpanded ? <ChevronsDownUp size={12} /> : <ChevronsUpDown size={12} />}
        {anyExpanded ? 'Collapse all' : 'Expand all'}
      </button>

      <span className="text-border h-7 flex items-center">|</span>

      {/* Stats */}
      <div className="flex-1 min-w-[200px] text-xs text-content-secondary">
        <span className="text-content">{groupsWithDeletions}</span>
        <span className="text-content-muted"> / {totalGroups} groups planned</span>
        <span className="mx-2 text-border">·</span>
        <span className="text-error">{deleteCount.toLocaleString()}</span>
        <span className="text-content-muted"> files to delete</span>
        <span className="mx-2 text-border">·</span>
        <span className="text-success">{formatBytes(bytesToFree)}</span>
        <span className="text-content-muted"> will free</span>
        {groupsWithAllDeleted > 0 && (
          <>
            <span className="mx-2 text-border">·</span>
            <span className="inline-flex items-center gap-1 text-warning">
              <AlertTriangle size={11} />
              {groupsWithAllDeleted} group{groupsWithAllDeleted === 1 ? '' : 's'} will lose every copy
            </span>
          </>
        )}
      </div>

      <button
        onClick={onResetToRule}
        title="Reset every group to the master-root rule's defaults"
        className="h-7 px-2.5 inline-flex items-center gap-1 text-xs font-medium rounded-lg border border-border bg-surface-hover hover:bg-surface-elevated text-content-secondary hover:text-content transition-colors"
      >
        <RotateCcw size={12} />
        Reset to rule
      </button>

      <button
        onClick={onMoveToBlackHole}
        disabled={moveDisabled}
        className="h-7 px-3 inline-flex items-center gap-1.5 text-xs font-semibold rounded-lg bg-error hover:bg-error/90 text-white transition-colors disabled:bg-surface-hover disabled:text-content-muted disabled:cursor-not-allowed"
      >
        <Trash2 size={12} />
        {deleteCount > 0
          ? `Move ${deleteCount.toLocaleString()} files (${groupsWithDeletions} groups) to Black Hole`
          : 'Move to Black Hole'}
      </button>
    </div>
  );
};
