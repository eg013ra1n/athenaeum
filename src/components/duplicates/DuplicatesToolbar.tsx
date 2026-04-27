import React from 'react';
import {
  AlertTriangle,
  CheckCircle2,
  ChevronsDownUp,
  ChevronsUpDown,
  RefreshCw,
  RotateCcw,
  ShieldCheck,
  Trash2,
} from 'lucide-react';
import type { VerifyPhase } from '../../hooks/useDeepVerify';

export type SortMode = 'wasted-desc' | 'count-desc' | 'size-desc';

interface DuplicatesToolbarProps {
  loading: boolean;
  onRefresh: () => void;

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

  // ── Deep-verify props ─────────────────────────────────────────────────────
  verifyPhase: VerifyPhase;
  /** True when a clean verify pass completed (no mismatches, no errors, no cancellation). */
  verifyClean: boolean;
  onDeepVerify: () => void;
  onCancelVerify: () => void;
  /** Disabled when there are no groups at all, verify is already running, or
   *  a bulk move is in progress. */
  deepVerifyDisabled: boolean;
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
  verifyPhase,
  verifyClean,
  onDeepVerify,
  onCancelVerify,
  deepVerifyDisabled,
}) => {
  const verifyRunning = verifyPhase === 'running';
  const effectiveMoveDisabled = moveDisabled;

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
        title="Reapply the rule chain to every group, dropping any manual checkbox edits"
        className="h-7 px-2.5 inline-flex items-center gap-1 text-xs font-medium rounded-lg border border-border bg-surface-hover hover:bg-surface-elevated text-content-secondary hover:text-content transition-colors"
      >
        <RotateCcw size={12} />
        Reapply rules
      </button>

      {/* Verify all duplicates button */}
      {verifyRunning ? (
        <button
          onClick={onCancelVerify}
          title="Cancel verification"
          aria-label="Cancel verification"
          className="h-7 px-2.5 inline-flex items-center gap-1 text-xs font-medium rounded-lg border border-warning/60 bg-warning-muted hover:bg-warning/20 text-warning transition-colors"
        >
          <ShieldCheck size={12} className="animate-pulse" />
          Cancel verify
        </button>
      ) : (
        <button
          onClick={onDeepVerify}
          disabled={deepVerifyDisabled}
          title="Run a byte-by-byte comparison on every file in every group to confirm true duplicates"
          aria-label="Verify all duplicates"
          className="h-7 px-2.5 inline-flex items-center gap-1 text-xs font-medium rounded-lg border border-border bg-surface-hover hover:bg-surface-elevated text-content-secondary hover:text-content transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
        >
          <ShieldCheck size={12} />
          Verify all duplicates
        </button>
      )}

      {/* Move button */}
      <button
        onClick={onMoveToBlackHole}
        disabled={effectiveMoveDisabled}
        className={`h-7 px-3 inline-flex items-center gap-1.5 text-xs font-semibold rounded-lg transition-colors ${
          effectiveMoveDisabled
            ? 'bg-surface-hover text-content-muted cursor-not-allowed'
            : 'bg-error hover:bg-error/90 text-white'
        }`}
      >
        {verifyClean ? (
          <CheckCircle2 size={12} className="text-green-300" />
        ) : (
          <Trash2 size={12} />
        )}
        {deleteCount > 0
          ? `Move ${deleteCount.toLocaleString()} files to Black Hole`
          : 'Move to Black Hole'}
      </button>
    </div>
  );
};
