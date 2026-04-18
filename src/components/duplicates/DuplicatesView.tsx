import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { CheckCircle2, Loader2 } from 'lucide-react';
import { api } from '../../api';
import { useBulkMoveToBlackHole } from '../../hooks/useBulkMoveToBlackHole';
import type { DuplicateGroup, ScanRootWithAvailability, ScanRoot } from '../../types/models';
import { ConfirmDialog } from '../ConfirmDialog';
import { AlertDialog } from '../AlertDialog';
import { DuplicatesToolbar, type SortMode } from './DuplicatesToolbar';
import { DuplicateGroupCard } from './DuplicateGroupCard';
import { buildAutoDeletes, computePlan, groupKey } from './keepRules';

const MASTER_ROOT_SETTING_KEY = 'duplicates.master_scan_root_id';

interface DuplicatesViewProps {
  /** Already-loaded duplicate groups (from useDuplicates in the parent). */
  duplicates: DuplicateGroup[];
  loading: boolean;
  error: string | null;
  /** Force-reload the group list. */
  refresh: () => Promise<void> | void;
}

export const DuplicatesView: React.FC<DuplicatesViewProps> = ({
  duplicates,
  loading,
  error,
  refresh,
}) => {
  const [scanRoots, setScanRoots] = useState<ScanRootWithAvailability[]>([]);
  const [masterRootId, setMasterRootId] = useState<number | null>(null);
  /** Per-group set of file_ids marked for deletion. Bootstrapped from the
   *  master-root rule whenever the rule or the group list changes; users can
   *  toggle individual checkboxes from there. */
  const [deletesByGroup, setDeletesByGroup] = useState<Map<string, Set<number>>>(new Map());
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [sortMode, setSortMode] = useState<SortMode>('wasted-desc');

  const [confirmOpen, setConfirmOpen] = useState(false);
  const [alert, setAlert] = useState<{ title: string; message: string; kind: 'error' | 'info' | 'warning' } | null>(null);

  const bulkMove = useBulkMoveToBlackHole();

  // ── Load scan roots + master-root setting on mount ─────────────────────
  // (The groups themselves are owned by the parent and passed in as a prop.)

  useEffect(() => {
    (async () => {
      try {
        const roots = await api.invoke<ScanRoot[]>('get_scan_roots');
        const availability = await api.invoke<[number, boolean][]>('check_all_scan_roots_availability');
        const availMap = new Map(availability);
        setScanRoots(
          roots.map(r => ({ ...r, is_available: availMap.get(r.id!) ?? false })),
        );
      } catch (err) {
        console.error('Failed to load scan roots:', err);
      }
    })();
  }, []);

  useEffect(() => {
    (async () => {
      try {
        const raw = await api.invoke<string>('get_setting', {
          key: MASTER_ROOT_SETTING_KEY,
          defaultValue: '',
        });
        if (raw) {
          const n = Number(raw);
          if (Number.isFinite(n) && n > 0) setMasterRootId(n);
        }
      } catch (err) {
        console.error('Failed to load master-root setting:', err);
      }
    })();
  }, []);

  // ── Auto-populate deletions when rule or data changes ──────────────────
  // Whenever the master-root rule or the underlying group list changes we
  // reseed the per-group deletion sets. Manual check/uncheck by the user
  // edits these sets in place and survives until the next reseed.

  useEffect(() => {
    setDeletesByGroup(buildAutoDeletes(duplicates, masterRootId));
  }, [duplicates, masterRootId]);

  const handleMasterRootChange = useCallback((id: number | null) => {
    setMasterRootId(id);
    api
      .invoke('set_setting', {
        key: MASTER_ROOT_SETTING_KEY,
        value: id == null ? '' : String(id),
      })
      .catch((err) => console.error('Failed to save master-root setting:', err));
  }, []);

  // ── Derived state ──────────────────────────────────────────────────────

  const plan = useMemo(
    () => computePlan(duplicates, deletesByGroup),
    [duplicates, deletesByGroup],
  );

  const sortedGroups = useMemo(() => {
    const arr = [...duplicates];
    switch (sortMode) {
      case 'count-desc':
        arr.sort((a, b) => b.file_count - a.file_count || b.size - a.size);
        break;
      case 'size-desc':
        arr.sort((a, b) => b.size - a.size);
        break;
      case 'wasted-desc':
      default:
        arr.sort(
          (a, b) => b.size * (b.file_count - 1) - a.size * (a.file_count - 1),
        );
    }
    return arr;
  }, [duplicates, sortMode]);

  // ── Handlers ───────────────────────────────────────────────────────────

  const handleToggleDelete = useCallback((group: DuplicateGroup, fileId: number) => {
    const key = groupKey(group);
    setDeletesByGroup((prev) => {
      const next = new Map(prev);
      const set = new Set(next.get(key) ?? []);
      if (set.has(fileId)) set.delete(fileId);
      else set.add(fileId);
      next.set(key, set);
      return next;
    });
  }, []);

  const handleResetToRule = useCallback(() => {
    setDeletesByGroup(buildAutoDeletes(duplicates, masterRootId));
  }, [duplicates, masterRootId]);

  const handleToggleExpanded = useCallback((group: DuplicateGroup) => {
    const key = groupKey(group);
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  const handleExpandAll = useCallback(() => {
    setExpanded(new Set(duplicates.map((g) => groupKey(g))));
  }, [duplicates]);

  const handleCollapseAll = useCallback(() => {
    setExpanded(new Set());
  }, []);

  const handleConfirmMove = useCallback(async () => {
    setConfirmOpen(false);
    try {
      const result = await bulkMove.start(plan.deleteIds, 'duplicates');
      await refresh();
      // Refresh reseeds the deletion map via the effect above.
      if (result.failed.length > 0) {
        setAlert({
          title: 'Completed with errors',
          message: `Moved ${result.moved} of ${plan.deleteIds.length} files. ${result.failed.length} failed — check the console for details.`,
          kind: 'error',
        });
      } else {
        setAlert({
          title: 'Done',
          message: `Moved ${result.moved} file${result.moved === 1 ? '' : 's'} to the Black Hole.`,
          kind: 'info',
        });
      }
    } catch (err) {
      setAlert({
        title: 'Move failed',
        message: typeof err === 'string' ? err : (err as Error).message ?? String(err),
        kind: 'error',
      });
    }
  }, [bulkMove, plan.deleteIds, refresh]);

  // ── Sticky toolbar height measurement ──────────────────────────────────
  // The inline progress banner can change the toolbar's total height — we
  // don't currently use it for a sticky table header, but measuring costs
  // nothing and lets future iterations offset content cleanly.
  const toolbarRef = useRef<HTMLDivElement>(null);

  // ── Render ─────────────────────────────────────────────────────────────

  return (
    <div className="bg-surface-elevated rounded-lg p-4 flex flex-col gap-3">
      <div ref={toolbarRef} className="sticky top-0 z-20 bg-surface-elevated -mx-4 -mt-4 px-4 pt-4 rounded-t-lg">
        <DuplicatesToolbar
          loading={loading || bulkMove.isRunning}
          onRefresh={() => refresh()}
          scanRoots={scanRoots}
          masterRootId={masterRootId}
          onMasterRootChange={handleMasterRootChange}
          sortMode={sortMode}
          onSortChange={setSortMode}
          totalGroups={duplicates.length}
          groupsWithDeletions={plan.groupsWithDeletions}
          groupsWithAllDeleted={plan.groupsWithAllDeleted}
          deleteCount={plan.deleteIds.length}
          bytesToFree={plan.bytesToFree}
          onResetToRule={handleResetToRule}
          anyExpanded={expanded.size > 0}
          onExpandAll={handleExpandAll}
          onCollapseAll={handleCollapseAll}
          onMoveToBlackHole={() => setConfirmOpen(true)}
          moveDisabled={plan.deleteIds.length === 0 || bulkMove.isRunning}
        />

        {/* Inline progress banner — sits inside the sticky header so users
            can always see it while scrolling. */}
        {bulkMove.isRunning && (
          <div className="mt-2 p-3 rounded-lg border border-border bg-surface space-y-2">
            <div className="flex items-center justify-between text-sm">
              <div className="flex items-center gap-2 text-content-secondary min-w-0">
                <Loader2 size={14} className="animate-spin flex-shrink-0 text-error" />
                <span className="truncate">
                  Moving to Black Hole…
                  {bulkMove.progress?.currentFile && (
                    <span className="ml-2 text-content-muted font-mono text-xs">
                      {bulkMove.progress.currentFile}
                    </span>
                  )}
                </span>
              </div>
              {bulkMove.progress && (
                <span className="text-content-muted flex-shrink-0 ml-3">
                  {bulkMove.progress.current.toLocaleString()} / {bulkMove.progress.total.toLocaleString()}
                </span>
              )}
            </div>
            <div className="h-1.5 w-full rounded-full bg-surface-hover overflow-hidden">
              <div
                className="h-full rounded-full bg-error transition-[width] duration-100 ease-linear"
                style={{ width: `${bulkMove.progress?.percent ?? 0}%` }}
              />
            </div>
          </div>
        )}
      </div>

      {/* Error banner */}
      {error && (
        <div className="p-3 bg-error-muted border border-error/50 rounded">
          <p className="text-error text-sm">Error: {String(error)}</p>
        </div>
      )}

      {/* Body */}
      {loading ? (
        <div className="flex items-center justify-center py-12">
          <Loader2 className="animate-spin mr-2" size={24} />
          <span className="text-content-muted">Loading duplicates…</span>
        </div>
      ) : duplicates.length === 0 ? (
        <div className="text-content-muted text-center py-12">
          <CheckCircle2 className="mx-auto mb-3 text-success" size={48} />
          <p className="font-semibold mb-1">No duplicates found!</p>
          <p className="text-sm">All your files are unique.</p>
        </div>
      ) : (
        <div className="flex flex-col gap-2">
          {sortedGroups.map((group) => {
            const key = groupKey(group);
            const deletes = deletesByGroup.get(key) ?? new Set<number>();
            return (
              <DuplicateGroupCard
                key={key}
                group={group}
                deletes={deletes}
                masterRootId={masterRootId}
                isExpanded={expanded.has(key)}
                onToggleExpanded={() => handleToggleExpanded(group)}
                onToggleDelete={(fileId) => handleToggleDelete(group, fileId)}
              />
            );
          })}
        </div>
      )}

      <ConfirmDialog
        isOpen={confirmOpen}
        title="Move to Black Hole"
        message={(() => {
          if (plan.deleteIds.length === 0) return 'Nothing to move.';
          const base = `Move ${plan.deleteIds.length.toLocaleString()} file${plan.deleteIds.length === 1 ? '' : 's'} in ${plan.groupsWithDeletions} group${plan.groupsWithDeletions === 1 ? '' : 's'} to the Black Hole? This is reversible — you can restore from the Black Hole tab.`;
          if (plan.groupsWithAllDeleted > 0) {
            return `${base}\n\nWarning: ${plan.groupsWithAllDeleted} group${plan.groupsWithAllDeleted === 1 ? ' has' : 's have'} every copy marked for deletion — no copy will remain.`;
          }
          return base;
        })()}
        confirmText="Move"
        confirmDanger
        onConfirm={handleConfirmMove}
        onCancel={() => setConfirmOpen(false)}
      />

      {alert && (
        <AlertDialog
          isOpen={true}
          title={alert.title}
          message={alert.message}
          variant={alert.kind}
          onClose={() => setAlert(null)}
        />
      )}
    </div>
  );
};
