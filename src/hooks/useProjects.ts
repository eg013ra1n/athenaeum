import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../api';
import { useNotifications, type NotifyLike } from '../contexts/NotificationContext';
import type { CollabFramesChange, ProjectCard } from '../types/models';

const REFRESH_INTERVAL_MS = 5 * 60 * 1000;

/** Payload of the `collab-published` event (Task 7,
 * `api::collab::COLLAB_PUBLISHED_EVENT`) — emitted as raw JSON on the Rust
 * side (no ts-rs type), so it is declared by hand here. `heldBack` is a
 * COUNT on this event (unlike `PublishResult.heldBack`, an array). */
interface CollabPublishedEvent {
  projectId: string;
  announced: number;
  updated: number;
  heldBack: number;
}

/** A hub call refused this build with the stable `collab_api_outdated`
 *  prefix (P17) — core emits this exact string for the conflict. */
function isOutdated(msg: string): boolean {
  return msg.startsWith('collab_api_outdated');
}

/** One stable dedupe key per (project, kind, count) — the same delta can
 *  reach the frontend twice (the manual poll's own return value AND the live
 *  `collab-frames-changed` event it emits), so both paths route through this
 *  key and the second delivery is swallowed by the notification history's
 *  dedupe set. */
function frameChangeDedupeKey(change: CollabFramesChange): string {
  return `frames-${change.kind}-${change.projectId}-${change.count}`;
}

function notifyFrameChange(notify: NotifyLike, change: CollabFramesChange, title: string) {
  if (change.count === 0) return;
  const link = `/projects/${change.projectId}`;
  const n = change.count;
  const plural = n === 1 ? 'frame' : 'frames';
  const dedupeKey = frameChangeDedupeKey(change);
  switch (change.kind) {
    case 'newFrames':
      notify({
        title: `${n} new ${plural} in ${title}`,
        detail: 'Open the project to see them.',
        kind: 'project',
        link,
        dedupeKey,
      });
      break;
    case 'pendingFrames':
      // Coordinator-only (hub visibility gates foreign pending rows).
      notify({
        title: `${n} ${plural} awaiting your approval`,
        detail: title,
        kind: 'project',
        tone: 'info',
        link,
        dedupeKey,
      });
      break;
    case 'approved':
      notify({
        title: n === 1 ? 'Your contribution was approved' : `${n} of your contributions were approved`,
        detail: title,
        kind: 'project',
        tone: 'success',
        link,
        dedupeKey,
      });
      break;
    case 'rejected':
      notify({
        title: n === 1 ? 'Your contribution was rejected' : `${n} of your contributions were rejected`,
        detail: title,
        kind: 'project',
        tone: 'warning',
        hasErrors: true,
        link,
        dedupeKey,
      });
      break;
    case 'excluded':
      notify({
        title: `${n} ${plural} excluded from ${title}`,
        detail: title,
        kind: 'project',
        tone: 'warning',
        link,
        dedupeKey,
      });
      break;
    case 'newVersions':
      notify({
        title: `${n} updated ${plural} in ${title}`,
        detail: 'A member republished a new version.',
        kind: 'project',
        link,
        dedupeKey,
      });
      break;
    default:
      // Any future kind: the outcome is visible in the project itself, no
      // toast needed.
      break;
  }
}

/** Cached-first project list: instant cache render, then a hub refresh on
 * mount and every 5 minutes while the page is open (spec §2 poll cadence).
 * Each refresh also polls per-frame changes and turns the returned deltas
 * into `notify()` calls; live `collab-published` (Task 7) and
 * `collab-frames-changed` (Task 8) events are also listened for, so an
 * auto-publish or a background version-poll tick surfaces a notification
 * even between refreshes. */
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
  // Live listeners need the current project titles without re-subscribing on
  // every projects update.
  const projectsRef = useRef<ProjectCard[]>([]);
  projectsRef.current = projects;

  const titleFor = useCallback((pid: string) => {
    return projectsRef.current.find((p) => p.projectId === pid)?.title ?? pid;
  }, []);

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

    // Frame-change poll only when the projects refresh succeeded (i.e. signed in).
    if (!fresh) return;
    try {
      const changes = await api.invoke<CollabFramesChange[]>('refresh_collab_frames');
      for (const change of changes) {
        notifyFrameChange(notify, change, titleFor(change.projectId));
      }
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
  }, [notify, titleFor]);

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

  // Live `collab-frames-changed` (Task 8) — background version-poll ticks
  // (every 15 s in core) reach the frontend here even between the 5-minute
  // refreshes above. StrictMode-safe listener pattern (CLAUDE.md).
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<CollabFramesChange>('collab-frames-changed', (change) => {
        if (cancelled) return;
        notifyFrameChange(notify, change, titleFor(change.projectId));
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[projects] frames-changed listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [notify, titleFor]);

  // Live `collab-published` (Task 7) — surfaces a background auto-publish
  // outcome (Task 10) even when nobody is on the project's Contribute tab. A
  // manual publish from `ProjectDetail` notifies inline instead (that page
  // and this hook are never mounted together, so there is no double toast).
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<CollabPublishedEvent>('collab-published', (res) => {
        if (cancelled) return;
        const sent = res.announced + res.updated;
        if (sent === 0 && res.heldBack === 0) return;
        const title = titleFor(res.projectId);
        const link = `/projects/${res.projectId}`;
        notify({
          title: sent === 0 ? `Nothing new to publish in ${title}` : `Published ${sent} frames in ${title}`,
          detail:
            res.heldBack > 0
              ? `${res.announced} new · ${res.updated} updated · ${res.heldBack} held back`
              : `${res.announced} new · ${res.updated} updated`,
          kind: 'project',
          tone: sent === 0 ? 'warning' : 'success',
          hasErrors: res.heldBack > 0,
          link,
          dedupeKey: `publish-live-${res.projectId}-${res.announced}-${res.updated}-${res.heldBack}`,
        });
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[projects] published listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [notify, titleFor]);

  return { projects, loading, refreshing, signedOut, updateRequired, refresh };
}
