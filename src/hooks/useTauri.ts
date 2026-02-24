// Custom hooks for backend command invocation
import { api } from '../api';
import { useState, useEffect, useCallback } from 'react';
import type {
  ScanRoot,
  ScanRootWithAvailability,
  FileWithFrame,
  DuplicateGroup,
  FolderSimilarity,
  ScanResult,
  RelinkResult,
} from '../types/models';

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
      const roots = await api.invoke<ScanRoot[]>('get_scan_roots');
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
      const newRoot = await api.invoke<ScanRoot>('add_scan_root', { path });
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
      await api.invoke('delete_scan_root', { id });
      setScanRoots((prev) => prev.filter((root) => root.id !== id));
      setError(null);
    } catch (e) {
      setError(e as string);
      throw e;
    } finally {
      setLoading(false);
    }
  }, []);

  const toggleDuplicatesFlag = useCallback(async (id: number, enabled: boolean) => {
    try {
      await api.invoke('set_scan_root_duplicates_flag', { id, enabled });
      setScanRoots((prev) =>
        prev.map((root) =>
          root.id === id ? { ...root, find_duplicates: enabled } : root
        )
      );
      setError(null);
    } catch (e) {
      setError(e as string);
      throw e;
    }
  }, []);

  const toggleUniqueCameraFlag = useCallback(async (id: number, enabled: boolean): Promise<void> => {
    try {
      await api.invoke('set_scan_root_unique_camera_flag', { id, enabled });
      setScanRoots((prev) =>
        prev.map((root) =>
          root.id === id ? { ...root, unique_camera: enabled } : root
        )
      );
      setError(null);
    } catch (e) {
      setError(e as string);
      throw e;
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
    toggleDuplicatesFlag,
    toggleUniqueCameraFlag,
    refresh: fetchScanRoots,
  };
}

/**
 * Manage scan roots with availability checking
 */
export function useScanRootsWithAvailability() {
  const [scanRoots, setScanRoots] = useState<ScanRootWithAvailability[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchScanRootsWithAvailability = useCallback(async () => {
    try {
      setLoading(true);
      const roots = await api.invoke<ScanRoot[]>('get_scan_roots');
      const availability = await api.invoke<[number, boolean][]>('check_all_scan_roots_availability');

      const availabilityMap = new Map(availability);
      const rootsWithAvailability: ScanRootWithAvailability[] = roots.map(root => ({
        ...root,
        is_available: availabilityMap.get(root.id!) ?? false,
      }));

      setScanRoots(rootsWithAvailability);
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
      const newRoot = await api.invoke<ScanRoot>('add_scan_root', { path });
      const rootWithAvailability: ScanRootWithAvailability = {
        ...newRoot,
        is_available: true, // Newly added roots should be available
      };
      setScanRoots((prev) => [...prev, rootWithAvailability]);
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
      await api.invoke('delete_scan_root', { id });
      setScanRoots((prev) => prev.filter((root) => root.id !== id));
      setError(null);
    } catch (e) {
      setError(e as string);
      throw e;
    } finally {
      setLoading(false);
    }
  }, []);

  const toggleDuplicatesFlag = useCallback(async (id: number, enabled: boolean) => {
    try {
      await api.invoke('set_scan_root_duplicates_flag', { id, enabled });
      setScanRoots((prev) =>
        prev.map((root) =>
          root.id === id ? { ...root, find_duplicates: enabled } : root
        )
      );
      setError(null);
    } catch (e) {
      setError(e as string);
      throw e;
    }
  }, []);

  const toggleUniqueCameraFlag = useCallback(async (id: number, enabled: boolean): Promise<void> => {
    try {
      await api.invoke('set_scan_root_unique_camera_flag', { id, enabled });
      setScanRoots((prev) =>
        prev.map((root) =>
          root.id === id ? { ...root, unique_camera: enabled } : root
        )
      );
      setError(null);
    } catch (e) {
      setError(e as string);
      throw e;
    }
  }, []);

  const relinkScanRoot = useCallback(async (rootId: number, newPath: string) => {
    try {
      setLoading(true);
      const result = await api.invoke<RelinkResult>('relink_scan_root', { rootId, newPath });

      // Refresh scan roots directly instead of calling fetchScanRootsWithAvailability
      // This avoids dependency issues that could cause duplicate executions
      const roots = await api.invoke<ScanRoot[]>('get_scan_roots');
      const availability = await api.invoke<[number, boolean][]>('check_all_scan_roots_availability');
      const availabilityMap = new Map(availability);
      const rootsWithAvailability: ScanRootWithAvailability[] = roots.map(root => ({
        ...root,
        is_available: availabilityMap.get(root.id!) ?? false,
      }));
      setScanRoots(rootsWithAvailability);

      return result;
    } catch (e) {
      setError(e as string);
      throw e;
    } finally {
      setLoading(false);
    }
  }, []); // Empty dependency array - function is stable

  useEffect(() => {
    fetchScanRootsWithAvailability();
  }, [fetchScanRootsWithAvailability]);

  const clearError = useCallback(() => setError(null), []);

  return {
    scanRoots,
    loading,
    error,
    clearError,
    addScanRoot,
    deleteScanRoot,
    toggleDuplicatesFlag,
    toggleUniqueCameraFlag,
    relinkScanRoot,
    refresh: fetchScanRootsWithAvailability,
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
      const result = await api.invoke<ScanResult>('start_scan', { rootId });
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
      const result = await api.invoke<FileWithFrame[]>('get_files', { limit: limit || null });
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
      const result = await api.invoke<FileWithFrame[]>('get_files_by_directory', {
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
 * Fetch duplicate groups - lazy loaded (call load() to fetch)
 */
export function useDuplicates() {
  const [duplicates, setDuplicates] = useState<DuplicateGroup[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  const fetchDuplicates = useCallback(async () => {
    try {
      setLoading(true);
      const result = await api.invoke<DuplicateGroup[]>('get_duplicates');
      setDuplicates(result);
      setError(null);
      setLoaded(true);
    } catch (e) {
      setError(e as string);
    } finally {
      setLoading(false);
    }
  }, []);

  // Load only if not already loaded
  const load = useCallback(() => {
    if (!loaded && !loading) {
      fetchDuplicates();
    }
  }, [loaded, loading, fetchDuplicates]);

  return {
    duplicates,
    loading,
    error,
    loaded,
    load,        // Call to trigger initial load
    refresh: fetchDuplicates,  // Force refresh
  };
}

/**
 * Fetch duplicate folders (folder similarity) - lazy loaded (call load() to fetch)
 */
export function useDuplicateFolders(threshold: number = 70.0) {
  const [folders, setFolders] = useState<FolderSimilarity[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  const fetchFolders = useCallback(async () => {
    try {
      setLoading(true);
      const result = await api.invoke<FolderSimilarity[]>('get_duplicate_folders', { threshold });
      setFolders(result);
      setError(null);
      setLoaded(true);
    } catch (e) {
      setError(e as string);
    } finally {
      setLoading(false);
    }
  }, [threshold]);

  // Load only if not already loaded
  const load = useCallback(() => {
    if (!loaded && !loading) {
      fetchFolders();
    }
  }, [loaded, loading, fetchFolders]);

  return {
    folders,
    loading,
    error,
    loaded,
    load,        // Call to trigger initial load
    refresh: fetchFolders,  // Force refresh
  };
}

/**
 * Move a file to black hole
 */
export async function moveToBlackHole(fileId: number, fromWhere: string): Promise<number> {
  return await api.invoke<number>('move_to_black_hole', { fileId, fromWhere });
}
