import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';
import type { CollabFramesChange, ProjectCard } from '../types/models';

const REFRESH_INTERVAL_MS = 5 * 60 * 1000;

/** A hub call refused this build with the stable `collab_api_outdated`
 *  prefix (P17) — core emits this exact string for the conflict. */
function isOutdated(msg: string): boolean {
  return msg.startsWith('collab_api_outdated');
}

/** Cached-first project list: instant cache render, then a hub refresh on
 * mount and every 5 minutes while the page is open (spec §2 poll cadence).
 *
 * R29 (controller ruling, Task 11b fix round 1): notifications for
 * per-frame/publish outcomes moved to the app-root `useCollabNotifications`
 * hook (mounted in `Layout.tsx`), because a data-loss-risk outcome
 * (`collab-replication-paused`) and background auto-publish/version-poll
 * ticks must reach `notify()` regardless of which page is open — this hook
 * only runs while `Projects.tsx` is mounted. `refresh_collab_frames` is
 * still called here to force an immediate version-poll tick (rather than
 * waiting on the ~15 s background cadence), but its return value is no
 * longer turned into a notification — that would double-toast alongside the
 * live `collab-frames-changed` event the SAME tick already emits. */
export function useProjects() {
  const { notify } = useNotifications();
  const [projects, setProjects] = useState<ProjectCard[]>([]);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [signedOut, setSignedOut] = useState(false);
  const [updateRequired, setUpdateRequired] = useState(false);
  const mounted = useRef(true);
  // Д4d joined-project diff baseline: the set of project ids we last saw. `null`
  // until a baseline loads (the cached list on mount, then every successful
  // refresh) so the very first fetch never mis-fires a "Joined" toast.
  const knownIdsRef = useRef<Set<string> | null>(null);

  const refresh = useCallback(async () => {
    setRefreshing(true);
    let fresh: ProjectCard[] | null = null;
    try {
      fresh = await api.invoke<ProjectCard[]>('refresh_collab_projects');
      if (mounted.current) {
        setProjects(fresh);
        setSignedOut(false);
        setUpdateRequired(false);
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      if (isOutdated(msg)) {
        if (mounted.current) setUpdateRequired(true);
      } else if (msg.toLowerCase().includes('sign in')) {
        // Core emits two SignedOut messages ("Sign in to use collaboration
        // projects." and "Signed out or device revoked — sign in again.");
        // a case-insensitive "sign in" test catches both.
        if (mounted.current) setSignedOut(true);
      } else {
        console.error('[projects] refresh failed:', err);
      }
    } finally {
      if (mounted.current) setRefreshing(false);
    }

    // Д4d joined-project diff: a project we don't coordinate that appeared since
    // the last known baseline means our join request was approved. Skip the
    // no-baseline round (knownIds === null) and coordinators (they created the
    // project, they didn't join it). Always refresh the baseline from `fresh`,
    // even when the diff was skipped, so the next round diffs against this list.
    if (fresh) {
      const knownIds = knownIdsRef.current;
      if (knownIds !== null) {
        for (const card of fresh) {
          if (!knownIds.has(card.projectId) && !card.coordinator) {
            notify({
              title: `Joined "${card.title}"`,
              detail: 'Your join request was approved.',
              kind: 'project',
              tone: 'success',
              link: `/projects/${card.projectId}`,
              dedupeKey: `project-joined-${card.projectId}`,
            });
          }
        }
      }
      knownIdsRef.current = new Set(fresh.map((p) => p.projectId));
    }

    // Force an immediate version-poll tick only when the projects refresh
    // succeeded (i.e. signed in). The tick's own `collab-frames-changed`
    // events reach `notify()` through `useCollabNotifications`, not here.
    if (!fresh) return;
    try {
      await api.invoke<CollabFramesChange[]>('refresh_collab_frames');
    } catch (err) {
      // S6 — a failed frames poll is logged, never silently ignored. It is not
      // a sign-out signal, so it must not flip `signedOut`.
      const msg = err instanceof Error ? err.message : String(err);
      if (isOutdated(msg)) {
        if (mounted.current) setUpdateRequired(true);
      } else {
        console.error('[projects] frames refresh failed:', err);
      }
    }
  }, [notify]);

  useEffect(() => {
    mounted.current = true;
    (async () => {
      try {
        const cached = await api.invoke<ProjectCard[]>('list_collab_projects');
        if (mounted.current) setProjects(cached);
        // Seed the joined-diff baseline from the local cache so the first hub
        // refresh can surface any project we were approved into meanwhile.
        knownIdsRef.current = new Set(cached.map((p) => p.projectId));
      } catch (err) {
        console.error('[projects] cached list failed:', err);
      } finally {
        if (mounted.current) setLoading(false);
      }
      void refresh();
    })();
    const timer = setInterval(() => void refresh(), REFRESH_INTERVAL_MS);
    return () => {
      mounted.current = false;
      clearInterval(timer);
    };
  }, [refresh]);

  return { projects, loading, refreshing, signedOut, updateRequired, refresh };
}
