import { useEffect, useRef } from 'react';
import { api } from '../api';
import { useNotifications, type NotifyLike } from '../contexts/NotificationContext';
import { formatGb } from '../components/collab/format';
import type {
  CollabFramesChange,
  CollabFramesLanded,
  CollabReplicationPaused,
  ProjectCard,
} from '../types/models';

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

/** One stable dedupe key per (project, kind, count) — the same delta can
 *  reach the frontend twice (a manual poll's own return value AND the live
 *  event it emits internally), so both paths would route through this key
 *  and a second delivery is swallowed by the notification history's dedupe
 *  set. `useProjects`'s manual refresh no longer notifies at all (R29) —
 *  this hook is the one place `collab-frames-changed` reaches `notify()` —
 *  but the key stays collision-proof against any future second caller. */
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

/**
 * R29: the one app-root owner of every live collab notification. Mounted
 * once in `Layout.tsx` (next to `useProjectMatches`, same precedent) so a
 * data-loss-risk, user-actionable outcome — the loss guard pausing
 * replication — reaches `notify()` regardless of which page or tab is open,
 * and a background auto-publish or version-poll tick surfaces a toast even
 * when nobody is on the Projects page. Per-page hooks/components
 * (`useProjects`, `ReceiveTab`) keep their own state/UI concerns only — no
 * `notify()` calls of their own for these four events, so there is exactly
 * one place that can toast for each.
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

  // `collab-replication-paused` (P14) — a data-loss-risk, user-actionable
  // outcome that must reach `notify()` regardless of which tab is open
  // (ReceiveTab keeps its own inline banner + Restore/Stop keeping buttons,
  // but raises no notification of its own).
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<CollabReplicationPaused>('collab-replication-paused', (p) => {
        if (cancelled) return;
        notify({
          title: `Replication paused: ${p.missing} frames missing (${formatGb(p.missingBytes)})`,
          detail: titleFor(p.projectId),
          kind: 'project',
          tone: 'warning',
          hasErrors: true,
          link: `/projects/${p.projectId}?tab=receive`,
          dedupeKey: `paused-${p.projectId}-${p.missing}`,
        });
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[collab] replication-paused listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [notify]);

  // `collab-frames-changed` (Task 8) — background version-poll ticks (every
  // 15 s in core) reach the frontend here.
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

  // `collab-published` (Task 7) — surfaces a manual OR a background
  // auto-publish (Task 10) outcome.
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
          title:
            sent === 0 ? `Nothing new to publish in ${title}` : `Published ${sent} frames in ${title}`,
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
      .catch((err) => console.error('[collab] published listen failed:', err));
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
        const link = `/projects/${l.projectId}?tab=receive`;
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
          dedupeKey: `landed-${l.projectId}-${l.landed}-${l.failed}-${l.awaitingGc}`,
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
