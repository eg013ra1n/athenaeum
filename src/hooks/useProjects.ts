import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';
import type { PackageStateChange, ProjectCard } from '../types/models';

const REFRESH_INTERVAL_MS = 5 * 60 * 1000;

/** Cached-first project list: instant cache render, then a hub refresh on
 * mount and every 5 minutes while the page is open (spec §2 poll cadence).
 * Each refresh also polls project packages and turns the returned state
 * changes into `notify()` calls (kind `project`). */
export function useProjects() {
  const { notify } = useNotifications();
  const [projects, setProjects] = useState<ProjectCard[]>([]);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [signedOut, setSignedOut] = useState(false);
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
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      // Core emits two SignedOut messages ("Sign in to use collaboration
      // projects." and "Signed out or device revoked — sign in again.");
      // a case-insensitive "sign in" test catches both.
      if (msg.toLowerCase().includes('sign in')) {
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

    // Package poll only when the projects refresh succeeded (i.e. signed in).
    if (!fresh) return;
    const known = fresh;
    try {
      const changes = await api.invoke<PackageStateChange[]>('refresh_collab_packages');
      const titleFor = (pid: string) => known.find((p) => p.projectId === pid)?.title ?? pid;
      for (const change of changes) {
        const title = titleFor(change.projectId);
        const link = `/projects/${change.projectId}`;
        switch (change.kind) {
          case 'newPackage':
            notify({
              title: `New package available in ${title}`,
              detail: change.detail ?? 'Open the project to download it.',
              kind: 'project',
              link,
              dedupeKey: `pkg-new-${change.packageId}`,
            });
            break;
          case 'approved':
            notify({
              title: 'Your contribution was approved',
              detail: title,
              kind: 'project',
              tone: 'success',
              link,
              dedupeKey: `pkg-approved-${change.packageId}`,
            });
            break;
          case 'rejected':
            notify({
              title: `Your contribution was rejected: ${change.detail ?? 'no reason given'}`,
              detail: title,
              kind: 'project',
              tone: 'warning',
              link,
              dedupeKey: `pkg-rejected-${change.packageId}`,
            });
            break;
          case 'downloadFailed':
            notify({
              title: 'Download failed',
              detail: change.detail ?? title,
              kind: 'project',
              tone: 'warning',
              hasErrors: true,
              link,
              dedupeKey: `pkg-dlfail-${change.packageId}`,
            });
            break;
          case 'awaitingApproval':
            // Coordinator-only (hub visibility gates foreign pending rows): a
            // contributor's package is waiting on our decision. The change
            // carries no detail, so we tag it with the project title.
            notify({
              title: 'Contribution awaiting your approval',
              detail: title,
              kind: 'project',
              tone: 'info',
              link,
              dedupeKey: `pkg-approval-${change.packageId}`,
            });
            break;
          default:
            // Any future kinds: the outcome is visible in the project's Receive
            // tab, no toast needed.
            break;
        }
      }
    } catch (err) {
      // S6 — a failed package poll is logged, never silently ignored. It is not
      // a sign-out signal, so it must not flip `signedOut`.
      console.error('[projects] package refresh failed:', err);
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

  return { projects, loading, refreshing, signedOut, refresh };
}
