import { useCallback, useRef, useState } from 'react';
import { api } from '../api';
import type { DuplicateGroup, VerifyPairResult } from '../types/models';
import { groupKey } from '../components/duplicates/keepRules';

/** Per-file verification outcome. */
export type FileVerifyStatus = 'pending' | 'running' | 'verified' | 'mismatch' | 'error';

export interface FileVerifyResult {
  fileId: number;
  path: string;
  status: FileVerifyStatus;
  /** Human-readable error message (only set when status === 'error'). */
  errorMessage?: string;
}

/**
 * Aggregate verification status for one duplicate group.
 *  - `unverified`  — no verification has been run for this group.
 *  - `running`     — at least one file in this group is currently being checked.
 *  - `verified`    — all non-anchor files checked out identical to the anchor.
 *  - `error`       — at least one file could not be read (I/O failure).
 *  - `partial`     — mix of verified + errored (errors but no mismatches).
 *
 * Note: `mismatch` is no longer a possible group status. Files that fail
 * byte-identical comparison are filtered out of the group entirely before
 * the card renders — so the group itself is never left in a mismatch state.
 */
export type GroupVerifyStatus = 'unverified' | 'running' | 'verified' | 'error' | 'partial';

export interface VerifyProgress {
  current: number;
  total: number;
  percent: number;
  currentFile: string | null;
}

export interface VerifySummary {
  total: number;
  verified: number;
  mismatches: number;
  errors: number;
}

export type VerifyPhase = 'idle' | 'running' | 'done' | 'cancelled';

export interface DeepVerifyState {
  phase: VerifyPhase;
  progress: VerifyProgress | null;
  /** file_id → result */
  results: Map<number, FileVerifyResult>;
  summary: VerifySummary | null;
}

const CONCURRENCY = 3;

/**
 * Drives the deep-verify pass.
 *
 * The caller provides:
 *  - `groups` — all duplicate groups currently displayed.
 *
 * The hook verifies every file in every group regardless of the deletion
 * plan marking.  For each group with ≥ 2 files, it picks `group.files[0]`
 * as the anchor and verifies every other file against it by calling the
 * `verify_files_byte_identical` backend command.
 *
 * Files whose result is `mismatch` are filtered out of the displayed
 * group list by the caller (DuplicatesView).  Files with `error` stay
 * visible so the user can investigate.
 *
 * Verification runs with bounded concurrency (CONCURRENCY = 3).  Cancel
 * stops scheduling new work; in-flight api.invoke calls finish naturally
 * because there's no underlying cancellation mechanism.
 */
export function useDeepVerify() {
  const [state, setState] = useState<DeepVerifyState>({
    phase: 'idle',
    progress: null,
    results: new Map(),
    summary: null,
  });

  // A simple flag the async loop reads to know if it should stop.
  const cancelledRef = useRef(false);
  // Run-generation token. `start` captures the value it incremented to; any
  // later `reset`/`cancel`/`start` bumps it, which tells the still-draining
  // loop of the PREVIOUS run to stop scheduling comparisons and to drop its
  // state updates. Without this, changing the keep rules mid-verify hid the
  // progress UI (reset -> phase 'idle') while the loop kept reading disks for
  // the rest of the queue and finally resurrected a 'done' summary for a run
  // the user had discarded - and reset's old `cancelledRef.current = false`
  // even un-cancelled a cancel pressed just before it.
  const runIdRef = useRef(0);

  const cancel = useCallback(() => {
    cancelledRef.current = true;
  }, []);

  const reset = useCallback(() => {
    runIdRef.current += 1;
    cancelledRef.current = false;
    setState({ phase: 'idle', progress: null, results: new Map(), summary: null });
  }, []);

  const start = useCallback(
    async (groups: DuplicateGroup[]) => {
      const runId = ++runIdRef.current;
      const stale = () => runIdRef.current !== runId;
      cancelledRef.current = false;

      // Build the work list: for every group with ≥ 2 files, pick files[0]
      // as the anchor and enqueue every other file for comparison.
      type WorkItem = {
        fileId: number;
        path: string;
        anchorFileId: number;
        anchorPath: string;
        groupKeyStr: string;
      };

      const workItems: WorkItem[] = [];

      for (const group of groups) {
        if (group.files.length < 2) continue;

        const key = groupKey(group);
        const anchorFileId = group.files[0].fileId;
        const anchorPath = group.files[0].path;

        for (const f of group.files.slice(1)) {
          workItems.push({
            fileId: f.fileId,
            path: f.path,
            anchorFileId,
            anchorPath,
            groupKeyStr: key,
          });
        }
      }

      if (workItems.length === 0) {
        // Nothing to verify (e.g., all groups have every copy marked).
        setState({
          phase: 'done',
          progress: { current: 0, total: 0, percent: 100, currentFile: null },
          results: new Map(),
          summary: { total: 0, verified: 0, mismatches: 0, errors: 0 },
        });
        return;
      }

      const total = workItems.length;

      // Initialise all items as pending.
      const initialResults = new Map<number, FileVerifyResult>(
        workItems.map((item) => [
          item.fileId,
          { fileId: item.fileId, path: item.path, status: 'pending' },
        ]),
      );

      setState({
        phase: 'running',
        progress: { current: 0, total, percent: 0, currentFile: null },
        results: initialResults,
        summary: null,
      });

      // Mutable counters shared across concurrent slots.
      let completedCount = 0;
      let verifiedCount = 0;
      let mismatchCount = 0;
      let errorCount = 0;

      // Process workItems with bounded concurrency.
      let cursor = 0;

      async function runSlot() {
        while (cursor < workItems.length) {
          if (cancelledRef.current || stale()) break;

          const item = workItems[cursor++];

          // Mark as running.
          if (stale()) break;
          setState((prev) => {
            const next = new Map(prev.results);
            next.set(item.fileId, { ...next.get(item.fileId)!, status: 'running' });
            return {
              ...prev,
              progress: {
                current: completedCount,
                total,
                percent: Math.round((completedCount / total) * 100),
                currentFile: item.path,
              },
              results: next,
            };
          });

          let status: FileVerifyStatus;
          let errorMessage: string | undefined;

          try {
            // Backend banks an identical pair's full-content hash into
            // files.strong_hash, so re-verifying a pair whose rows are still
            // current on disk is decided from the column without reading.
            const result = await api.invoke<VerifyPairResult>('verify_duplicate_pair', {
              fileA: item.anchorFileId,
              fileB: item.fileId,
            });
            status = result.identical ? 'verified' : 'mismatch';
          } catch (err) {
            status = 'error';
            errorMessage =
              typeof err === 'string' ? err : (err as Error)?.message ?? String(err);
            console.error(`Deep verify error for ${item.path}:`, err);
          }

          completedCount += 1;
          if (status === 'verified') verifiedCount += 1;
          else if (status === 'mismatch') mismatchCount += 1;
          else errorCount += 1;

          if (stale()) break;
          setState((prev) => {
            const next = new Map(prev.results);
            next.set(item.fileId, {
              fileId: item.fileId,
              path: item.path,
              status,
              errorMessage,
            });
            return {
              ...prev,
              progress: {
                current: completedCount,
                total,
                percent: Math.round((completedCount / total) * 100),
                currentFile: null,
              },
              results: next,
            };
          });
        }
      }

      // Launch CONCURRENCY slots and wait for all to drain.
      const slots = Array.from({ length: Math.min(CONCURRENCY, workItems.length) }, () =>
        runSlot(),
      );
      await Promise.all(slots);

      // A reset (rule change, refresh) or a newer run owns the state now -
      // this run's summary must not resurrect a UI the user discarded.
      if (stale()) return;

      const phase: VerifyPhase = cancelledRef.current ? 'cancelled' : 'done';

      setState((prev) => ({
        ...prev,
        phase,
        progress: {
          current: completedCount,
          total,
          percent: cancelledRef.current
            ? Math.round((completedCount / total) * 100)
            : 100,
          currentFile: null,
        },
        summary: {
          total: completedCount,
          verified: verifiedCount,
          mismatches: mismatchCount,
          errors: errorCount,
        },
      }));
    },
    [],
  );

  return { state, start, cancel, reset };
}

// ── Derived helpers ────────────────────────────────────────────────────────────

/**
 * Compute the aggregate verification status for a single group given the
 * current verification results.
 *
 * The anchor (files[0]) is never in the results map — only files[1..n] are
 * verified.  Mismatch files are filtered out of the group before this
 * component renders, so `mismatch` is not a possible return value here.
 */
export function groupVerifyStatus(
  group: DuplicateGroup,
  results: Map<number, FileVerifyResult>,
): GroupVerifyStatus {
  // files[1..n] are the verified files; files[0] is the anchor (not in results).
  const nonAnchorFiles = group.files.slice(1);
  if (nonAnchorFiles.length === 0) return 'unverified';

  // If none appear in results — verification hasn't run for this group yet.
  const anyInResults = nonAnchorFiles.some((f) => results.has(f.fileId));
  if (!anyInResults) return 'unverified';

  const statuses = nonAnchorFiles
    .map((f) => results.get(f.fileId)?.status ?? 'pending')
    .filter((s) => s !== 'pending'); // pending = not yet started

  if (statuses.some((s) => s === 'running')) return 'running';

  const hasError = statuses.some((s) => s === 'error');
  const allVerified = statuses.length > 0 && statuses.every((s) => s === 'verified');

  if (allVerified && !hasError) return 'verified';
  if (hasError) return 'partial'; // errors only (mismatches already filtered out)
  return 'unverified';
}
