import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../api';
import type { ProjectCard } from '../types/models';

const REFRESH_INTERVAL_MS = 5 * 60 * 1000;

/** Cached-first project list: instant cache render, then a hub refresh on
 * mount and every 5 minutes while the page is open (spec §2 poll cadence). */
export function useProjects() {
  const [projects, setProjects] = useState<ProjectCard[]>([]);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [signedOut, setSignedOut] = useState(false);
  const mounted = useRef(true);

  const refresh = useCallback(async () => {
    setRefreshing(true);
    try {
      const fresh = await api.invoke<ProjectCard[]>('refresh_collab_projects');
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
  }, []);

  useEffect(() => {
    mounted.current = true;
    (async () => {
      try {
        const cached = await api.invoke<ProjectCard[]>('list_collab_projects');
        if (mounted.current) setProjects(cached);
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
