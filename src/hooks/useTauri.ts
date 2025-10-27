// Custom hooks for Tauri command invocation
import { invoke } from '@tauri-apps/api/core';
import { useState, useEffect, useCallback } from 'react';
import type {
  ScanRoot,
  FileWithFrame,
  DuplicateGroup,
  ScanResult,
} from '../types/models';

/**
 * Initialize the database
 */
export function useInitializeDatabase() {
  const [dbPath, setDbPath] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const init = async () => {
      try {
        setLoading(true);
        const path = await invoke<string>('initialize_database');
        setDbPath(path);
        setError(null);
      } catch (e) {
        setError(e as string);
      } finally {
        setLoading(false);
      }
    };

    init();
  }, []);

  return { dbPath, loading, error };
}

/**
 * Manage scan roots (monitored directories)
 */
export function useScanRoots() {
  const [scanRoots, setScanRoots] = useState<ScanRoot[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchScanRoots = useCallback(async () => {
    try {
      setLoading(true);
      const roots = await invoke<ScanRoot[]>('get_scan_roots');
      setScanRoots(roots);
      setError(null);
    } catch (e) {
      setError(e as string);
    } finally {
      setLoading(false);
    }
  }, []);

  const addScanRoot = useCallback(async (path: string) => {
    try {
      setLoading(true);
      const newRoot = await invoke<ScanRoot>('add_scan_root', { path });
      setScanRoots((prev) => [...prev, newRoot]);
      setError(null);
      return newRoot;
    } catch (e) {
      setError(e as string);
      throw e;
    } finally {
      setLoading(false);
    }
  }, []);

  const deleteScanRoot = useCallback(async (id: number) => {
    try {
      setLoading(true);
      await invoke('delete_scan_root', { id });
      setScanRoots((prev) => prev.filter((root) => root.id !== id));
      setError(null);
    } catch (e) {
      setError(e as string);
      throw e;
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchScanRoots();
  }, [fetchScanRoots]);

  return {
    scanRoots,
    loading,
    error,
    addScanRoot,
    deleteScanRoot,
    refresh: fetchScanRoots,
  };
}

/**
 * Start a scan operation
 */
export function useScan() {
  const [scanning, setScanning] = useState(false);
  const [scanResult, setScanResult] = useState<ScanResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  const startScan = useCallback(async (rootId: number) => {
    try {
      setScanning(true);
      setError(null);
      const result = await invoke<ScanResult>('start_scan', { rootId });
      setScanResult(result);
      return result;
    } catch (e) {
      setError(e as string);
      throw e;
    } finally {
      setScanning(false);
    }
  }, []);

  return {
    scanning,
    scanResult,
    error,
    startScan,
  };
}

/**
 * Fetch files from the catalog
 */
export function useFiles(limit?: number) {
  const [files, setFiles] = useState<FileWithFrame[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchFiles = useCallback(async () => {
    try {
      setLoading(true);
      const result = await invoke<FileWithFrame[]>('get_files', { limit: limit || null });
      setFiles(result);
      setError(null);
    } catch (e) {
      setError(e as string);
    } finally {
      setLoading(false);
    }
  }, [limit]);

  useEffect(() => {
    fetchFiles();
  }, [fetchFiles]);

  return {
    files,
    loading,
    error,
    refresh: fetchFiles,
  };
}

/**
 * Fetch files from a specific directory
 */
export function useFilesByDirectory(directoryPath: string, limit?: number) {
  const [files, setFiles] = useState<FileWithFrame[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchFiles = useCallback(async () => {
    try {
      setLoading(true);
      const result = await invoke<FileWithFrame[]>('get_files_by_directory', {
        directory_path: directoryPath,
        limit: limit || null
      });
      setFiles(result);
      setError(null);
    } catch (e) {
      setError(e as string);
    } finally {
      setLoading(false);
    }
  }, [directoryPath, limit]);

  useEffect(() => {
    if (directoryPath) {
      fetchFiles();
    }
  }, [fetchFiles]);

  return {
    files,
    loading,
    error,
    refresh: fetchFiles,
  };
}

/**
 * Fetch duplicate groups
 */
export function useDuplicates() {
  const [duplicates, setDuplicates] = useState<DuplicateGroup[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchDuplicates = useCallback(async () => {
    try {
      setLoading(true);
      const result = await invoke<DuplicateGroup[]>('get_duplicates');
      setDuplicates(result);
      setError(null);
    } catch (e) {
      setError(e as string);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchDuplicates();
  }, [fetchDuplicates]);

  return {
    duplicates,
    loading,
    error,
    refresh: fetchDuplicates,
  };
}
