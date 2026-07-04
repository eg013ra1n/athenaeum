import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { CheckCircle2, Loader2, ShieldCheck, XCircle } from 'lucide-react';
import { api } from '../../api';
import { useBulkMoveToBlackHole } from '../../hooks/useBulkMoveToBlackHole';
import { useDeepVerify, groupVerifyStatus } from '../../hooks/useDeepVerify';
import type { DuplicateGroup, ScanRoot } from '../../types/models';
import type { ScanRootWithAvailability, RuleChain, RuleKind } from '../../types/helpers';
import { ConfirmDialog } from '../ConfirmDialog';
import { AlertDialog } from '../AlertDialog';
import { DuplicatesToolbar, type SortMode } from './DuplicatesToolbar';
import { DuplicateGroupCard } from './DuplicateGroupCard';
import {
  buildAutoDeletes,
  computePlan,
  defaultRuleChain,
  groupKey,
  parseRuleChain,
  serializeRuleChain,
} from './keepRules';
import { RuleChainPanel } from './RuleChainPanel';

const MASTER_ROOT_SETTING_KEY = 'duplicates.master_scan_root_id';
const RULE_CHAIN_SETTING_KEY = 'duplicates.rule_chain';

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
  /** The active rule chain. Loaded from settings on mount, persisted on every
   *  change. Null while the initial settings load is in flight; the auto-deletes
   *  effect skips reseeding until it settles. */
  const [chain, setChain] = useState<RuleChain | null>(null);
  /** Per-group set of file_ids marked for deletion. Bootstrapped from the
   *  rule chain whenever the chain or the group list changes; users can
   *  toggle individual checkboxes from there. */
  const [deletesByGroup, setDeletesByGroup] = useState<Map<string, Set<number>>>(new Map());
  /** group key → kind of the rule that produced the auto-marks for that group. */
  const [firedRuleByGroup, setFiredRuleByGroup] = useState<Map<string, RuleKind>>(new Map());
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [sortMode, setSortMode] = useState<SortMode>('wasted-desc');

  const [confirmOpen, setConfirmOpen] = useState(false);
  const [alert, setAlert] = useState<{ title: string; message: string; kind: 'error' | 'info' | 'warning' } | null>(null);

  const bulkMove = useBulkMoveToBlackHole();
  const deepVerify = useDeepVerify();

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

  // ── Load the rule chain from settings ──────────────────────────────────
  // Migration: if no chain is stored but the legacy master-root setting is,
  // build a default chain seeded with that root id. The legacy setting is left
  // in place so older builds continue to work after a downgrade.

  useEffect(() => {
    (async () => {
      try {
        const chainRaw = await api.invoke<string>('get_setting', {
          key: RULE_CHAIN_SETTING_KEY,
          defaultValue: '',
        });
        const parsed = parseRuleChain(chainRaw);
        if (parsed) {
          setChain(parsed);
          return;
        }

        // No chain yet — seed defaults, migrating master_root setting if any.
        let seededRootId: number | null = null;
        try {
          const masterRaw = await api.invoke<string>('get_setting', {
            key: MASTER_ROOT_SETTING_KEY,
            defaultValue: '',
          });
          if (masterRaw) {
            const n = Number(masterRaw);
            if (Number.isFinite(n) && n > 0) seededRootId = n;
          }
        } catch (err) {
          console.error('Failed to read master-root setting:', err);
        }

        const fresh = defaultRuleChain(seededRootId);
        setChain(fresh);
        api
          .invoke('set_setting', {
            key: RULE_CHAIN_SETTING_KEY,
            value: serializeRuleChain(fresh),
          })
          .catch((err) =>
            console.error('Failed to save default rule chain:', err),
          );
      } catch (err) {
        console.error('Failed to load rule chain setting:', err);
        setChain(defaultRuleChain(null));
      }
    })();
  }, []);

  // ── Auto-populate deletions when chain or data changes ────────────────
  // Whenever the chain or the underlying group list changes we reseed the
  // per-group deletion sets. Manual check/uncheck by the user edits these sets
  // in place and survives until the next reseed. When the plan changes, clear
  // any previous verify results so stale badges don't mislead the user.

  useEffect(() => {
    if (chain == null) return;
    const { deletes, firedRules } = buildAutoDeletes(duplicates, chain);
    setDeletesByGroup(deletes);
    setFiredRuleByGroup(firedRules);
    deepVerify.reset();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [duplicates, chain]);

  const persistChain = useCallback((next: RuleChain) => {
    api
      .invoke('set_setting', {
        key: RULE_CHAIN_SETTING_KEY,
        value: serializeRuleChain(next),
      })
      .catch((err) => console.error('Failed to save rule chain:', err));
  }, []);

  const handleChainChange = useCallback(
    (next: RuleChain) => {
      setChain(next);
      persistChain(next);
      // Side-effect: keep the legacy master-root setting in sync so other parts
      // of the app that still read it (or a downgraded build) see the latest
      // value.
      const masterRoot = next.find(
        (r): r is Extract<RuleChain[number], { kind: 'master_root' }> =>
          r.kind === 'master_root',
      );
      const idStr =
        masterRoot && masterRoot.config.rootId != null
          ? String(masterRoot.config.rootId)
          : '';
      api
        .invoke('set_setting', {
          key: MASTER_ROOT_SETTING_KEY,
          value: idStr,
        })
        .catch((err) =>
          console.error('Failed to mirror master-root setting:', err),
        );
    },
    [persistChain],
  );

  const handleResetChainToDefaults = useCallback(() => {
    // Preserve the master-root id from the current chain (so resetting the
    // chain doesn't silently lose the user's primary library selection).
    const currentMasterRoot = chain?.find((r) => r.kind === 'master_root');
    const seedId =
      currentMasterRoot && currentMasterRoot.kind === 'master_root'
        ? currentMasterRoot.config.rootId
        : null;
    handleChainChange(defaultRuleChain(seedId));
  }, [chain, handleChainChange]);

  /** Master root currently configured by the master_root rule (if any).
   *  Drives in-card path highlighting that already used `masterRootId`. */
  const masterRootId = useMemo<number | null>(() => {
    if (!chain) return null;
    const r = chain.find((rule) => rule.kind === 'master_root');
    if (!r || r.kind !== 'master_root') return null;
    return r.enabled ? r.config.rootId : null;
  }, [chain]);

  /** Active path-contains patterns when the rule is enabled — used to
   *  highlight matched substrings inside a file's path in the group card. */
  const pathContainsPatterns = useMemo<string[]>(() => {
    if (!chain) return [];
    const r = chain.find((rule) => rule.kind === 'path_contains');
    if (!r || r.kind !== 'path_contains' || !r.enabled) return [];
    return r.config.patterns
      .map((p) => p.trim())
      .filter((p) => p.length > 0);
  }, [chain]);

  const pathContainsCaseSensitive = useMemo<boolean>(() => {
    if (!chain) return false;
    const r = chain.find((rule) => rule.kind === 'path_contains');
    if (!r || r.kind !== 'path_contains') return false;
    return r.config.caseSensitive;
  }, [chain]);

  /** Per-rule-kind count of groups currently picked by that rule. Surfaces in
   *  the rule chain panel so the user can see what each rule actually does. */
  const ruleCounts = useMemo<Partial<Record<RuleKind, number>>>(() => {
    const out: Partial<Record<RuleKind, number>> = {};
    for (const kind of firedRuleByGroup.values()) {
      out[kind] = (out[kind] ?? 0) + 1;
    }
    return out;
  }, [firedRuleByGroup]);

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

  // ── Deep-verify derived values ─────────────────────────────────────────

  const verifyResults = deepVerify.state.results;

  /**
   * Groups with mismatch files filtered out, and groups that drop below
   * 2 files after filtering removed entirely.
   *
   * Filtering is purely a display concern — the backend catalog is untouched.
   * Refreshing re-fetches the full unfiltered list.
   */
  const verifiedGroups = useMemo<typeof sortedGroups>(() => {
    if (verifyResults.size === 0) return sortedGroups;

    const mismatchIds = new Set<number>();
    for (const [id, result] of verifyResults) {
      if (result.status === 'mismatch') mismatchIds.add(id);
    }
    if (mismatchIds.size === 0) return sortedGroups;

    const filtered: typeof sortedGroups = [];
    for (const group of sortedGroups) {
      const keepFileIds = new Set(
        group.files.filter((f) => !mismatchIds.has(f.fileId)).map((f) => f.fileId),
      );
      if (keepFileIds.size < 2) continue; // group no longer has duplicates — hide it

      const files = group.files.filter((f) => keepFileIds.has(f.fileId));
      const fileIds = group.file_ids.filter((id) => keepFileIds.has(id));
      const filePaths = group.file_paths.filter((_, i) => keepFileIds.has(group.file_ids[i]));

      filtered.push({
        ...group,
        files,
        file_ids: fileIds,
        file_paths: filePaths,
        file_count: files.length,
      });
    }
    return filtered;
  }, [sortedGroups, verifyResults]);

  // Counts for the summary banner.
  const mismatchFileCount = useMemo(() => {
    let count = 0;
    for (const result of verifyResults.values()) {
      if (result.status === 'mismatch') count += 1;
    }
    return count;
  }, [verifyResults]);

  const hiddenGroupCount = useMemo(
    () => sortedGroups.length - verifiedGroups.length,
    [sortedGroups, verifiedGroups],
  );

  const verifyClean = useMemo(
    () =>
      deepVerify.state.phase === 'done' &&
      mismatchFileCount === 0 &&
      (deepVerify.state.summary?.errors ?? 0) === 0 &&
      (deepVerify.state.summary?.total ?? 0) > 0,
    [deepVerify.state, mismatchFileCount],
  );

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
    // Changing the plan invalidates previous verify results.
    deepVerify.reset();
  }, [deepVerify]);

  const handleResetToRule = useCallback(() => {
    if (chain == null) return;
    const { deletes, firedRules } = buildAutoDeletes(duplicates, chain);
    setDeletesByGroup(deletes);
    setFiredRuleByGroup(firedRules);
    deepVerify.reset();
  }, [duplicates, chain, deepVerify]);

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

  const handleDeepVerify = useCallback(() => {
    deepVerify.start(sortedGroups);
  }, [deepVerify, sortedGroups]);

  const handleCancelVerify = useCallback(() => {
    deepVerify.cancel();
  }, [deepVerify]);

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

  // ── Verify summary alert (shown on completion) ─────────────────────────

  const prevVerifyPhaseRef = useRef(deepVerify.state.phase);
  useEffect(() => {
    const prev = prevVerifyPhaseRef.current;
    const next = deepVerify.state.phase;
    prevVerifyPhaseRef.current = next;

    // Fire the summary dialog when the run transitions to done/cancelled.
    if ((prev === 'running') && (next === 'done' || next === 'cancelled')) {
      const s = deepVerify.state.summary;
      if (!s) return;

      if (next === 'cancelled') {
        setAlert({
          title: 'Verification cancelled',
          message: `Checked ${s.total} of ${deepVerify.state.progress?.total ?? s.total} files before cancellation.\nVerified: ${s.verified}  ·  Mismatches: ${s.mismatches}  ·  Errors: ${s.errors}`,
          kind: 'warning',
        });
        return;
      }

      if (s.mismatches > 0 || s.errors > 0) {
        const lines: string[] = [
          `Checked ${s.total} file${s.total === 1 ? '' : 's'}.`,
          `Verified: ${s.verified}  ·  Removed (false duplicates): ${s.mismatches}  ·  Errors: ${s.errors}`,
        ];
        if (s.mismatches > 0) {
          const groupWord = hiddenGroupCount === 1 ? 'group' : 'groups';
          lines.push(
            `\nRemoved ${s.mismatches} false-duplicate file${s.mismatches === 1 ? '' : 's'} from the list` +
            (hiddenGroupCount > 0 ? `; ${hiddenGroupCount} ${groupWord} no longer have duplicates and were hidden.` : '.') +
            ' These files are NOT identical to their group anchor.',
          );
        }
        if (s.errors > 0) {
          lines.push('\nFiles with errors could not be read — they remain visible. Check file accessibility before proceeding.');
        }
        setAlert({
          title: 'Verification complete — review required',
          message: lines.join('\n'),
          kind: s.mismatches > 0 ? 'warning' : 'error',
        });
      } else {
        setAlert({
          title: 'All files verified',
          message: `Checked ${s.total} file${s.total === 1 ? '' : 's'}. All duplicates are byte-identical to their group anchor. Safe to proceed.`,
          kind: 'info',
        });
      }
    }
  }, [deepVerify.state.phase, deepVerify.state.summary, deepVerify.state.progress?.total]);

  // ── Auto-remove mismatched files from the deletion plan ────────────────
  // When a file comes back as 'mismatch', immediately remove it from the
  // plan so it's not accidentally deleted.

  useEffect(() => {
    if (verifyResults.size === 0) return;
    let changed = false;
    setDeletesByGroup((prev) => {
      const next = new Map(prev);
      for (const group of duplicates) {
        const key = groupKey(group);
        const currentSet = next.get(key);
        if (!currentSet || currentSet.size === 0) continue;
        const filtered = new Set<number>();
        for (const fileId of currentSet) {
          const r = verifyResults.get(fileId);
          if (r?.status === 'mismatch') {
            changed = true;
            // Skip — remove from deletion plan.
          } else {
            filtered.add(fileId);
          }
        }
        if (filtered.size !== currentSet.size) {
          next.set(key, filtered);
        }
      }
      return changed ? next : prev;
    });
  }, [verifyResults, duplicates]);

  // ── Sticky toolbar height measurement ──────────────────────────────────
  const toolbarRef = useRef<HTMLDivElement>(null);

  // ── Render ─────────────────────────────────────────────────────────────

  const verifyPhase = deepVerify.state.phase;
  const verifyProgress = deepVerify.state.progress;
  const verifyRunning = verifyPhase === 'running';

  return (
    <div className="bg-surface-elevated rounded-lg p-4 flex flex-col gap-3">
      <div ref={toolbarRef} className="sticky top-0 z-20 bg-surface-elevated -mx-4 -mt-4 px-4 pt-4 rounded-t-lg">
        <DuplicatesToolbar
          loading={loading || bulkMove.isRunning}
          onRefresh={() => refresh()}
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
          verifyPhase={verifyPhase}
          verifyClean={verifyClean}
          onDeepVerify={handleDeepVerify}
          onCancelVerify={handleCancelVerify}
          deepVerifyDisabled={duplicates.length === 0 || verifyRunning || bulkMove.isRunning}
        />

        {/* Rule chain panel — controls auto-marking */}
        {chain != null && (
          <div className="mt-2">
            <RuleChainPanel
              chain={chain}
              scanRoots={scanRoots}
              ruleCounts={ruleCounts}
              totalGroups={duplicates.length}
              onChange={handleChainChange}
              onResetToDefaults={handleResetChainToDefaults}
            />
          </div>
        )}

        {/* Bulk-move inline progress banner */}
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

        {/* Deep-verify inline progress banner */}
        {verifyRunning && verifyProgress && (
          <div className="mt-2 p-3 rounded-lg border border-info/40 bg-info-muted/30 space-y-2">
            <div className="flex items-center justify-between text-sm">
              <div className="flex items-center gap-2 text-content-secondary min-w-0">
                <ShieldCheck size={14} className="animate-pulse flex-shrink-0 text-info" />
                <span className="truncate">
                  Verifying all duplicates…
                  {verifyProgress.currentFile && (
                    <span className="ml-2 text-content-muted font-mono text-xs">
                      {verifyProgress.currentFile}
                    </span>
                  )}
                </span>
              </div>
              <span className="text-content-muted flex-shrink-0 ml-3">
                {verifyProgress.current.toLocaleString()} / {verifyProgress.total.toLocaleString()}
              </span>
            </div>
            <div className="h-1.5 w-full rounded-full bg-surface-hover overflow-hidden">
              <div
                className="h-full rounded-full bg-info transition-[width] duration-100 ease-linear"
                style={{ width: `${verifyProgress.percent}%` }}
              />
            </div>
          </div>
        )}

        {/* False-duplicate removal banner — persistent after a verify pass with mismatches */}
        {mismatchFileCount > 0 && !verifyRunning && (
          <div className="mt-2 p-3 rounded-lg border border-warning/50 bg-warning-muted flex items-start gap-2">
            <XCircle size={15} className="text-warning flex-shrink-0 mt-0.5" />
            <p className="text-xs text-warning leading-relaxed">
              Removed {mismatchFileCount} false-duplicate file{mismatchFileCount === 1 ? '' : 's'} from the list
              {hiddenGroupCount > 0
                ? `; ${hiddenGroupCount} group${hiddenGroupCount === 1 ? '' : 's'} no longer have duplicates and were hidden.`
                : '.'}
              {' '}These files match the sampling hash but differ in content — they are <strong>not</strong> true duplicates.
            </p>
          </div>
        )}

        {/* Clean verify confirmation banner */}
        {verifyClean && !verifyRunning && (
          <div className="mt-2 p-3 rounded-lg border border-success/40 bg-surface flex items-center gap-2">
            <CheckCircle2 size={15} className="text-success flex-shrink-0" />
            <p className="text-xs text-success">
              All {deepVerify.state.summary?.total} file{deepVerify.state.summary?.total === 1 ? '' : 's'} verified byte-identical. Safe to move to Black Hole.
            </p>
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
        <div className="bg-surface rounded-lg border border-border divide-y divide-border/40 overflow-hidden">
          {verifiedGroups.map((group) => {
            const key = groupKey(group);
            const deletes = deletesByGroup.get(key) ?? new Set<number>();
            const firedRule = firedRuleByGroup.get(key);
            const vStatus = groupVerifyStatus(group, verifyResults);
            return (
              <DuplicateGroupCard
                key={key}
                group={group}
                deletes={deletes}
                masterRootId={masterRootId}
                firedRuleKind={firedRule}
                pathContainsPatterns={pathContainsPatterns}
                pathContainsCaseSensitive={pathContainsCaseSensitive}
                isExpanded={expanded.has(key)}
                onToggleExpanded={() => handleToggleExpanded(group)}
                onToggleDelete={(fileId) => handleToggleDelete(group, fileId)}
                verifyStatus={vStatus === 'unverified' ? undefined : vStatus}
                fileVerifyResults={verifyResults.size > 0 ? verifyResults : undefined}
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
          const parts = [base];
          if (plan.groupsWithAllDeleted > 0) {
            parts.push(`Warning: ${plan.groupsWithAllDeleted} group${plan.groupsWithAllDeleted === 1 ? ' has' : 's have'} every copy marked for deletion — no copy will remain.`);
          }
          if (!verifyClean && deepVerify.state.phase !== 'idle') {
            parts.push('Note: not all files have been verified byte-identical. Proceed anyway?');
          }
          return parts.join('\n\n');
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
