import { useEffect } from 'react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';
import type { ProjectSetMatchEvent } from '../types/models';

/** Join-first-shoot-later (spec §7): a freshly clustered set whose center falls
 * inside one of my projects' targets raises a discrete suggestion. Never
 * auto-links. */
export function useProjectMatches() {
  const { notify } = useNotifications();

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<ProjectSetMatchEvent>('project-set-match', (p) => {
        if (cancelled) return;
        const first = p.matches[0];
        if (!first) return;
        notify({
          title: `Frame set matches project ${first.projectTitle}`,
          detail: `${p.setName ?? `Set #${p.framesSetId}`} lies within the project target — link it from the Projects page.`,
          kind: 'project',
          tone: 'info',
          link: '/projects',
          dedupeKey: `project-match-${first.projectId}-${p.framesSetId}`,
        });
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[projects] match listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [notify]);
}
