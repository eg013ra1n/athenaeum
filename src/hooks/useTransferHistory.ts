// Transfers history hook (Transfers Status Model v2, §D8) — fetches
// `list_sync_history`, the hub device-name map, and the collab project-name map,
// then groups the rows by `(direction, packageId)` for the unified list's
// "Completed"/"Failed" rows. Owns its own 5s poll + a `refetch()` the page fires
// on `sync-finished`. Mirrors the fetch/poll shape the former
// `TransfersHistoryTab` had, minus its rendering (now the page's job).

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { api } from '../api';
import { groupHistory, type HistoryGroup } from '../components/transfers/historyGrouping';
import type { Direction, HistoryRow, ProjectCard, SyncHistoryQuery } from '../types/models';

const HISTORY_LIMIT = 500;
const POLL_MS = 5_000;

export interface UseTransferHistory {
  groups: HistoryGroup[];
  /** hub node-id (hex) → friendly device name; empty when signed out / hub down. */
  deviceNames: Record<string, string>;
  /** collab project id → title. */
  projectNames: Record<string, string>;
  loading: boolean;
  /** Re-read history now (the page calls this on `sync-finished`). */
  refetch: () => void;
  /** Optimistically drop a deleted batch's history rows from local state (UX
   *  wave 2 trash action) — the group vanishes instantly, before the reconciling
   *  `refetch()`. Keyed on the same `(direction, packageId)` a group is built by. */
  removeLocal: (direction: Direction, packageKey: string) => void;
}

export function useTransferHistory(): UseTransferHistory {
  const [history, setHistory] = useState<HistoryRow[]>([]);
  const [deviceNames, setDeviceNames] = useState<Record<string, string>>({});
  const [projectNames, setProjectNames] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(false);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const refetch = useCallback(() => {
    setLoading(true);
    const query: SyncHistoryQuery = {
      filename: null,
      object: null,
      // Direction filtering is done by the unified list's chips (client-side),
      // so fetch both halves here — one query feeds every filter.
      direction: null,
      peer: null,
      project: null,
      limit: HISTORY_LIMIT,
    };
    api
      .invoke<HistoryRow[]>('list_sync_history', { query })
      .then((rows) => {
        if (mounted.current) setHistory(rows);
      })
      .catch((err) => console.error('[useTransferHistory] list_sync_history failed:', err))
      .finally(() => {
        if (mounted.current) setLoading(false);
      });
  }, []);

  useEffect(() => {
    refetch();
    const id = setInterval(refetch, POLL_MS);
    return () => clearInterval(id);
  }, [refetch]);

  // Device-name map (hub node-id → friendly name). Degrades to hex on any
  // failure — the render falls back automatically.
  useEffect(() => {
    let cancelled = false;
    api
      .invoke<Record<string, string>>('get_sync_device_names')
      .then((names) => {
        if (!cancelled && mounted.current) setDeviceNames(names ?? {});
      })
      .catch((err) => console.error('[useTransferHistory] get_sync_device_names failed:', err));
    return () => {
      cancelled = true;
    };
  }, []);

  // Project id → title (collab rows only). Degrades to a short id.
  useEffect(() => {
    let cancelled = false;
    api
      .invoke<ProjectCard[]>('list_collab_projects')
      .then((cards) => {
        if (cancelled || !mounted.current) return;
        setProjectNames(Object.fromEntries(cards.map((c) => [c.projectId, c.title])));
      })
      .catch((err) => console.error('[useTransferHistory] list_collab_projects failed:', err));
    return () => {
      cancelled = true;
    };
  }, []);

  // Optimistic local removal: drop every history row of the deleted batch. Sent
  // rows carry the full package-dir basename as `packageId`, received rows the
  // wire uuid — both equal the `package_key` the delete command targets, so an
  // exact match cleans the group out of the next `groupHistory` pass.
  const removeLocal = useCallback((direction: Direction, packageKey: string) => {
    setHistory((prev) =>
      prev.filter((r) => !(r.direction === direction && r.packageId === packageKey)),
    );
  }, []);

  const groups = useMemo(() => groupHistory(history), [history]);

  return { groups, deviceNames, projectNames, loading, refetch, removeLocal };
}
