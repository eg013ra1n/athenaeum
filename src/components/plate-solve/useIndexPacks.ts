// Quad-index packs of the plate-solve settings: the merged manifest/installed
// status (`get_index_status`), the download (`download_index_packs`) and its
// live progress (`index-download-progress`). Kept out of the panel so the
// Star Catalog section stays readable; the catalog-tier download beside it
// keeps its own state.
import { useState, useEffect, useCallback } from 'react';
import { api } from '../../api';
import type { IndexStatusInfo, IndexDownloadProgress } from '../../types/helpers';

/** Spinner line for the progress block, e.g.
 *  `Pack 1/2 (base) — Downloading · 40 / 147 MB`. */
export function getIndexStatusText(progress: IndexDownloadProgress | null): string {
  if (!progress) return 'Starting — connecting to the index server…';
  const label =
    progress.nPacks > 1
      ? `Pack ${progress.packIndex + 1}/${progress.nPacks} (${progress.packName}) — `
      : `${progress.packName} — `;
  switch (progress.phase) {
    case 'pack':
      return `${label}Preparing pack…`;
    case 'downloading':
      return `${label}Downloading · ${(progress.current / 1e6).toFixed(0)} / ${(progress.total / 1e6).toFixed(0)} MB`;
    case 'verifying':
      return `${label}Verifying integrity…`;
    case 'extracting':
      return `${label}Extracting…`;
    case 'complete':
      return 'Finishing…';
    case 'error':
      return `${label}Download failed.`;
    default:
      return `${label}Working…`;
  }
}

export function useIndexPacks() {
  const [packs, setPacks] = useState<IndexStatusInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [downloading, setDownloading] = useState(false);
  const [progress, setProgress] = useState<IndexDownloadProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [startedAt, setStartedAt] = useState<number | null>(null);
  const [nowTs, setNowTs] = useState<number>(() => Date.now());

  const reload = useCallback(async () => {
    try {
      const result = await api.invoke<IndexStatusInfo[]>('get_index_status');
      setPacks(Array.isArray(result) ? result : []);
    } catch (err) {
      console.error('[PlateSolveSettingsPanel] Failed to load index status:', err);
      setPacks([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // Tick once a second while a download is active so the elapsed timer keeps
  // moving during the long first wait.
  useEffect(() => {
    if (!downloading) return;
    const id = setInterval(() => setNowTs(Date.now()), 1000);
    return () => clearInterval(id);
  }, [downloading]);

  // Reflect ANY index download for the panel's whole lifetime — including one
  // kicked off elsewhere. StrictMode-safe cancelled-flag form (see CLAUDE.md).
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<IndexDownloadProgress>('index-download-progress', (payload) => {
        if (cancelled) return;
        if (payload.phase === 'complete') {
          setDownloading(false);
          setProgress(null);
          setStartedAt(null);
          void reload();
        } else if (payload.phase === 'error') {
          console.error('[PlateSolveSettingsPanel] index download failed:', payload.message);
          setDownloading(false);
          setProgress(null);
          setStartedAt(null);
          setError(payload.message || 'Download failed. Please check your connection and try again.');
          void reload();
        } else {
          setDownloading(true);
          setError(null);
          setProgress(payload);
          setStartedAt((prev) => prev ?? Date.now());
        }
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[PlateSolveSettingsPanel] index-download listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [reload]);

  const download = useCallback(
    async (names: string[]) => {
      if (names.length === 0) return;
      setDownloading(true);
      setError(null);
      setProgress(null);
      setStartedAt(Date.now());
      setNowTs(Date.now());
      try {
        await api.invoke<string>('download_index_packs', { names });
        setDownloading(false);
        setProgress(null);
        setStartedAt(null);
      } catch (err) {
        console.error('[PlateSolveSettingsPanel] Failed to download index packs:', err);
        setError(String(err));
        setDownloading(false);
        setProgress(null);
        setStartedAt(null);
      }
      void reload();
    },
    [reload],
  );

  return { packs, loading, downloading, progress, error, startedAt, nowTs, download };
}
