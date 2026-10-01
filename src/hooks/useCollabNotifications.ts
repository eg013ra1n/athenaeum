import { useEffect, useRef } from 'react';
import { api } from '../api';
import { useNotifications, type NotifyLike } from '../contexts/NotificationContext';
import { publishingDeviceRefusal } from '../components/collab/project/usePublishing';
import type {
  CollabDeletionChoice,
  CollabFrameChanged,
  CollabFrameLost,
  CollabFramesChange,
  CollabFramesLanded,
  CollabPublishFinished,
  ProjectCard,
} from '../types/models';

/* No content-shaped `dedupeKey` on any collab live notification (final
 * review I4): the dedupe set persists in localStorage, so a key such as
 * `frames-<kind>-<project>-<count>` would swallow a later, genuine outcome
 * that happens to carry the same numbers — days later, forever. Each event is
 * one discrete outcome, and this hook is the ONE place each of these events
 * reaches `notify()` (R29), so there is no second delivery to collapse.
 *
 * The one key used is core's own `CollabDeletionChoice.dedupeKey`, which is
 * per OCCURRENCE (`collab-deletion-choice:<ids>:<batch id>`): a replay of the
 * same batch (a reconnect) is shown once, every new batch notifies. */

function notifyFrameChange(notify: NotifyLike, change: CollabFramesChange, title: string) {
  if (change.count === 0) return;
  const link = `/projects/${change.projectId}`;
  const n = change.count;
  const plural = n === 1 ? 'frame' : 'frames';
  switch (change.kind) {
    case 'newFrames':
      notify({
        title: `${n} new ${plural} in ${title}`,
        detail: 'Open the project to see them.',
        kind: 'project',
        link,
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
      });
      break;
    case 'approved':
      notify({
        title: n === 1 ? 'Your contribution was approved' : `${n} of your contributions were approved`,
        detail: title,
        kind: 'project',
        tone: 'success',
        link,
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
      });
      break;
    case 'excluded':
      notify({
        title: `${n} ${plural} excluded from ${title}`,
        detail: title,
        kind: 'project',
        tone: 'warning',
        link,
      });
      break;
    case 'newVersions':
      notify({
        title: `${n} updated ${plural} in ${title}`,
        detail: 'A member republished a new version.',
        kind: 'project',
        link,
      });
      break;
    default:
      // Any future kind: the outcome is visible in the project itself, no
      // toast needed.
      break;
  }
}

/**
 * R29: the one app-root owner of every live collab notification. Mounted
 * once in `Layout.tsx` (next to `useProjectMatches`, same precedent) so a
 * data-loss-risk, user-actionable outcome — a mass deletion waiting for a
 * choice, a frame lost everywhere, an edited replica set aside (L4, L5) —
 * reaches `notify()` regardless of which page or tab is open, and a
 * background auto-publish or a manifest change arriving on the hub's event
 * feed surfaces a toast even when nobody is on the Projects page. Per-page
 * hooks/components (`useProjects`, the project page's `LibraryTab`, `CollabAttention`) keep
 * their own state/UI concerns only — no `notify()` calls of their own for
 * these events, so there is exactly one place that can toast for each.
 */
export function useCollabNotifications() {
  const { notify } = useNotifications();
  const titlesRef = useRef<Map<string, string>>(new Map());

  // Project titles for toasts: one cached `list_collab_projects` call — the
  // event payloads carry only a `projectId`. A title that arrives after this
  // resolves (a brand new project) falls back to the raw id — acceptable,
  // the toast still links to the right place.
  useEffect(() => {
    let cancelled = false;
    api
      .invoke<ProjectCard[]>('list_collab_projects')
      .then((cards) => {
        if (cancelled) return;
        titlesRef.current = new Map(cards.map((c) => [c.projectId, c.title]));
      })
      .catch((err) => console.error('[collab] cached project list failed:', err));
    return () => {
      cancelled = true;
    };
  }, []);

  const titleFor = (projectId: string) => titlesRef.current.get(projectId) ?? projectId;

  // `collab-deletion-choice` (L4) — a mass deletion (or a second deletion
  // within 24 h) is waiting for "Re-fetch" or "Stop keeping". One
  // notification per batch: core's key is per occurrence.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<CollabDeletionChoice>('collab-deletion-choice', (p) => {
        if (cancelled) return;
        const n = p.count;
        notify({
          title: `${n} ${n === 1 ? 'replica was' : 'replicas were'} deleted — choose what to do`,
          detail: `${p.projectIds.map(titleFor).join(', ')}: re-fetch or stop keeping on the Library tab. Nothing else is paused.`,
          kind: 'project',
          tone: 'warning',
          hasErrors: true,
          link: p.projectIds.length === 1 ? `/projects/${p.projectIds[0]}?tab=library` : '/projects',
          dedupeKey: p.dedupeKey,
        });
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[collab] deletion-choice listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [notify]);

  // `collab-frame-lost` (L4, I7) — no holder of the current version is left
  // anywhere, so an automatic re-fetch has nowhere to fetch from. When the
  // file still sits in the PREVIOUS Collaboration folder (a re-designation,
  // owner rule A) the notice points there — never at the Trash.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<CollabFrameLost>('collab-frame-lost', (p) => {
        if (cancelled) return;
        const project = titleFor(p.projectId);
        notify(
          p.inPreviousFolder
            ? {
                title: `${p.fileName}: no member holds it — it is still in the previous Collaboration folder`,
                detail: `${project}: the file is still at ${p.previousPath ?? 'its previous path'}; it is fetched again if another member serves it.`,
                kind: 'project',
                tone: 'warning',
                hasErrors: true,
                link: `/projects/${p.projectId}?tab=library`,
              }
            : {
                title: `${p.fileName} is lost everywhere — restore it from the Trash`,
                detail: `${project}: no member holds the current version any more, so it cannot be fetched again.`,
                kind: 'project',
                tone: 'warning',
                hasErrors: true,
                link: `/projects/${p.projectId}?tab=library`,
              },
        );
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[collab] frame-lost listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [notify]);

  // `collab-frame-changed` (L5) — a replica edited in place stops serving at
  // once and waits under "Changed files".
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<CollabFrameChanged>('collab-frame-changed', (p) => {
        if (cancelled) return;
        notify({
          title: `${p.fileName} changed on disk and was set aside`,
          detail: `${titleFor(p.projectId)}: it is no longer served — re-fetch the original or delete it under Changed files.`,
          kind: 'project',
          tone: 'warning',
          link: `/projects/${p.projectId}?tab=library`,
        });
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[collab] frame-changed listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [notify]);

  // `collab-frames-changed` (Task 8) — manifest changes the live exchange
  // applies from the hub's event feed (wave 3: no poll) reach the frontend
  // here.
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
      .catch((err) => console.error('[collab] frames-changed listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [notify]);

  // `collab-publish-finished` (spec §5.4) — the ONE notification of a started
  // run's outcome (calibrate, publish, republish or auto; manual or
  // background). Commands refused before a run starts notify nowhere here.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<CollabPublishFinished>('collab-publish-finished', (f) => {
        if (cancelled) return;
        const title = titleFor(f.projectId);
        const sent = f.announced + f.updated;
        const base = `/projects/${f.projectId}?tab=mine`;
        if (f.outcome === 'done' && f.calibrated + sent + f.heldBack + f.stale === 0) return;
        switch (f.outcome) {
          case 'done':
            if (sent === 0 && f.calibrated > 0) {
              notify({
                title: `Calibrated ${f.calibrated} frames in ${title} — review them`,
                detail: f.heldBack > 0 ? `${f.heldBack} held back` : '',
                kind: 'project',
                tone: 'success',
                hasErrors: f.heldBack > 0,
                link: `${base}&segment=review`,
              });
            } else if (sent > 0) {
              notify({
                title: `Published ${sent} frames in ${title}`,
                detail: `${f.announced} new · ${f.updated} updated${f.heldBack > 0 ? ` · ${f.heldBack} held back` : ''}`,
                kind: 'project',
                tone: 'success',
                hasErrors: f.heldBack > 0,
                link: `${base}&segment=published`,
              });
            } else if (f.heldBack > 0) {
              notify({
                title: `Nothing new to publish in ${title}`,
                detail: `${f.heldBack} held back`,
                kind: 'project',
                tone: 'warning',
                link: `${base}&segment=held`,
              });
            }
            break;
          case 'cancelled':
            notify({ title: `Stopped in ${title}`, detail: '', kind: 'project', tone: 'info', toast: false, link: base });
            break;
          case 'refused': {
            const device = f.error ? publishingDeviceRefusal(f.error) : null;
            notify({
              title: device ? `${device} publishes ${title}` : `Not published in ${title}`,
              detail: device
                ? 'Use Publish from this device in Project settings to take over.'
                : (f.error ?? ''),
              kind: 'project',
              tone: 'warning',
              toast: f.trigger === 'manual',
              link: base,
            });
            break;
          }
          case 'failed':
            console.error('[collab] publish run failed:', f.error);
            notify({
              title: `Publishing failed in ${title}`,
              detail: f.error ?? '',
              kind: 'project',
              tone: 'warning',
              hasErrors: true,
              link: base,
            });
            break;
        }
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[collab] publish-finished listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [notify]);

  // `collab-frames-landed` (P9 replication outcome) — a discrete outcome of
  // a replication fetch (manual "Sync now" or the auto-replicate worker);
  // core only emits it when something landed or failed, never on an empty
  // pass.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<CollabFramesLanded>('collab-frames-landed', (l) => {
        if (cancelled) return;
        if (l.landed === 0 && l.failed === 0) return;
        const title = titleFor(l.projectId);
        const link = `/projects/${l.projectId}?tab=library`;
        const landedPlural = l.landed === 1 ? 'frame' : 'frames';
        notify({
          title:
            l.failed > 0
              ? `${l.landed} ${landedPlural} landed, ${l.failed} failed in ${title}`
              : `${l.landed} ${landedPlural} landed in ${title}`,
          detail: l.awaitingGc > 0 ? `${l.awaitingGc} awaiting cleanup` : title,
          kind: 'project',
          tone: l.failed > 0 ? 'warning' : 'success',
          hasErrors: l.failed > 0,
          link,
        });
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[collab] frames-landed listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [notify]);
}
