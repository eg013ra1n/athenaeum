import { useState, useEffect } from 'react';
import { FolderPlus, Play, Filter, Trash2, CheckCircle2, XCircle, Loader2, Copy, FolderOpen, RefreshCw, AlertTriangle, FileWarning } from 'lucide-react';
import { open } from '@tauri-apps/plugin-dialog';
import { invoke } from '@tauri-apps/api/core';
import { useScanRootsWithAvailability, useScan, useInitializeDatabase, useDuplicates, useDuplicateFolders, moveToBlackHole } from '../hooks/useTauri';
import { format } from 'date-fns';
import DirectoryTree from '../components/DirectoryTree';
import type { ScanResult, RelinkResult, OrphanedFile } from '../types/models';

type TabMode = 'directories' | 'browse' | 'duplicates';
type DuplicatesViewMode = 'files' | 'folders';

export default function FileManager() {
  const { dbPath, loading: dbLoading, error: dbError } = useInitializeDatabase();
  const { scanRoots, loading: rootsLoading, error: rootsError, addScanRoot, deleteScanRoot, toggleDuplicatesFlag, relinkScanRoot } = useScanRootsWithAvailability();
  const { startScan } = useScan();
  const { duplicates, loading: dupsLoading, error: dupsError, refresh: refreshDuplicates } = useDuplicates();
  const { folders: duplicateFolders, loading: foldersLoading, error: foldersError, refresh: refreshFolders } = useDuplicateFolders(70);
  const [activeTab, setActiveTab] = useState<TabMode>('directories');
  const [duplicatesView, setDuplicatesView] = useState<DuplicatesViewMode>('files');
  const [typeFilter, setTypeFilter] = useState<string>('All Types');
  const [refreshTrigger, setRefreshTrigger] = useState(0);
  const [scanningMap, setScanningMap] = useState<Record<number, boolean>>({});
  const [scanResultMap, setScanResultMap] = useState<Record<number, ScanResult>>({});
  const [scanError, setScanError] = useState<string | null>(null);
  const [movingToBlackHole, setMovingToBlackHole] = useState<Record<string, boolean>>({});
  const [relinkingRootId, setRelinkingRootId] = useState<number | null>(null);
  const [relinkResult, setRelinkResult] = useState<RelinkResult | null>(null);
  const [missingFilesMap, setMissingFilesMap] = useState<Record<number, OrphanedFile[]>>({});
  const [checkingMissingFiles, setCheckingMissingFiles] = useState<Record<number, boolean>>({});

  // Check for missing files in available scan roots
  const checkMissingFiles = async (rootId: number) => {
    try {
      setCheckingMissingFiles(prev => ({ ...prev, [rootId]: true }));
      const missingFiles = await invoke<OrphanedFile[]>('check_missing_files_in_scan_root', { rootId });
      setMissingFilesMap(prev => ({ ...prev, [rootId]: missingFiles }));
    } catch (error) {
      console.error('Failed to check missing files:', error);
    } finally {
      setCheckingMissingFiles(prev => ({ ...prev, [rootId]: false }));
    }
  };

  // Check missing files for all available scan roots when they load
  useEffect(() => {
    scanRoots.forEach(root => {
      if (root.id && root.is_available && !missingFilesMap[root.id]) {
        checkMissingFiles(root.id);
      }
    });
  }, [scanRoots]);

  // Handle deleting missing files
  const handleDeleteMissingFiles = async (rootId: number) => {
    const missingFiles = missingFilesMap[rootId];
    if (!missingFiles || missingFiles.length === 0) return;

    if (confirm(`Are you sure you want to delete ${missingFiles.length} missing file records from the database?`)) {
      try {
        const fileIds = missingFiles.map(f => f.id);
        await invoke('delete_orphaned_files', { fileIds });

        // Update the missing files map
        setMissingFilesMap(prev => ({ ...prev, [rootId]: [] }));

        alert(`Deleted ${missingFiles.length} missing file records`);
      } catch (error) {
        console.error('Failed to delete missing files:', error);
        alert(typeof error === 'string' ? error : 'Failed to delete missing files');
      }
    }
  };

  // Handle adding a new directory
  const handleAddDirectory = async () => {
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        title: 'Select directory to monitor',
      });

      if (selected && typeof selected === 'string') {
        await addScanRoot(selected);
      }
    } catch (error) {
      console.error('Failed to add directory:', error);
      alert(typeof error === 'string' ? error : 'Failed to add directory');
    }
  };

  // Handle removing a scan root
  const handleRemoveScanRoot = async (id: number) => {
    if (confirm('Are you sure you want to remove this directory from monitoring?')) {
      try {
        await deleteScanRoot(id);
      } catch (error) {
        console.error('Failed to remove directory:', error);
      }
    }
  };

  // Handle starting a scan for a specific root
  const handleStartScan = async (rootId: number) => {
    try {
      setScanningMap(prev => ({ ...prev, [rootId]: true }));
      setScanError(null);
      const result = await startScan(rootId);
      setScanResultMap(prev => ({ ...prev, [rootId]: result }));
      setRefreshTrigger(prev => prev + 1); // Trigger refresh after scanning

      // Re-check for missing files after scan completes
      await checkMissingFiles(rootId);
    } catch (error) {
      console.error('Scan failed:', error);
      setScanError(typeof error === 'string' ? error : 'Scan failed');
    } finally {
      setScanningMap(prev => ({ ...prev, [rootId]: false }));
    }
  };

  // Handle relinking a scan root to a new location
  const handleRelinkScanRoot = async (rootId: number) => {
    try {
      setRelinkingRootId(rootId);
      setRelinkResult(null);

      const selectedPath = await open({
        directory: true,
        multiple: false,
        title: 'Select new location for scan root',
      });

      if (!selectedPath || typeof selectedPath !== 'string') {
        setRelinkingRootId(null);
        return;
      }

      const result = await relinkScanRoot(rootId, selectedPath);
      setRelinkResult(result);
    } catch (error) {
      console.error('Relink failed:', error);
      alert(typeof error === 'string' ? error : 'Relink failed');
    } finally {
      setRelinkingRootId(null);
    }
  };

  // Show loading state while database initializes
  if (dbLoading) {
    return (
      <div className="flex items-center justify-center h-screen">
        <div className="text-center">
          <Loader2 className="animate-spin mx-auto mb-4" size={48} />
          <p className="text-gray-400">Initializing database...</p>
        </div>
      </div>
    );
  }

  // Show error if database initialization failed
  if (dbError) {
    return (
      <div className="flex items-center justify-center h-screen">
        <div className="text-center text-red-400">
          <XCircle className="mx-auto mb-4" size={48} />
          <p className="font-semibold mb-2">Database initialization failed</p>
          <p className="text-sm">{String(dbError)}</p>
        </div>
      </div>
    );
  }

  return (
    <div className="p-6">
      <div className="mb-6">
        <h2 className="text-3xl font-bold mb-2">File Manager</h2>
        <p className="text-gray-400">
          Manage monitored directories and view FITS/XISF metadata
        </p>
        {dbPath && (
          <p className="text-xs text-gray-500 mt-1">Database: {dbPath}</p>
        )}
      </div>

      {/* Tab Navigation */}
      <div className="flex gap-2 mb-6 border-b border-gray-700">
        <button
          onClick={() => setActiveTab('directories')}
          className={`px-4 py-2 transition relative ${
            activeTab === 'directories'
              ? 'text-blue-400 border-b-2 border-blue-400'
              : 'text-gray-400 hover:text-gray-200'
          }`}
        >
          <div className="flex items-center gap-2">
            <FolderPlus size={16} />
            Monitored Directories
          </div>
        </button>
        <button
          onClick={() => setActiveTab('browse')}
          className={`px-4 py-2 transition relative ${
            activeTab === 'browse'
              ? 'text-blue-400 border-b-2 border-blue-400'
              : 'text-gray-400 hover:text-gray-200'
          }`}
        >
          <div className="flex items-center gap-2">
            <Filter size={16} />
            Browse Files
          </div>
        </button>
        <button
          onClick={() => setActiveTab('duplicates')}
          className={`px-4 py-2 transition relative ${
            activeTab === 'duplicates'
              ? 'text-blue-400 border-b-2 border-blue-400'
              : 'text-gray-400 hover:text-gray-200'
          }`}
        >
          <div className="flex items-center gap-2">
            <Copy size={16} />
            Duplicates ({duplicates.length})
          </div>
        </button>
      </div>

      {/* Error Alerts */}
      {rootsError && (
        <div className="mb-6 p-4 bg-red-900/30 border border-red-700 rounded-lg">
          <p className="text-red-400">Error loading scan roots: {String(rootsError)}</p>
        </div>
      )}
      {scanError && (
        <div className="mb-6 p-4 bg-red-900/30 border border-red-700 rounded-lg">
          <p className="text-red-400">Scan error: {String(scanError)}</p>
        </div>
      )}

      {/* Tab Content */}
      {activeTab === 'directories' && (
        /* Monitored Directories Tab */
        <div>
          <div className="flex items-center justify-between mb-4">
            <h3 className="text-xl font-semibold">Monitored Directories</h3>
            <button
              onClick={handleAddDirectory}
              disabled={rootsLoading}
              className="flex items-center gap-2 px-4 py-2 bg-blue-600 hover:bg-blue-700 rounded-lg transition disabled:opacity-50 disabled:cursor-not-allowed"
            >
              <FolderPlus size={20} />
              Add Directory
            </button>
          </div>

          {rootsLoading ? (
            <div className="flex items-center justify-center py-12">
              <Loader2 className="animate-spin mr-2" size={20} />
              <span className="text-gray-400">Loading directories...</span>
            </div>
          ) : scanRoots.length === 0 ? (
            <div className="bg-gray-800 rounded-lg p-8 text-center">
              <p className="text-gray-500">
                No directories added yet. Click "Add Directory" to start.
              </p>
            </div>
          ) : (
            <div className="space-y-3">
              {scanRoots.map((root) => {
                const isScanning = root.id ? scanningMap[root.id] : false;
                const scanResult = root.id ? scanResultMap[root.id] : null;
                const isUnavailable = !root.is_available;

                return (
                  <div
                    key={root.id}
                    className={`bg-gray-800 rounded-lg p-4 border ${
                      isUnavailable
                        ? 'border-yellow-600 bg-yellow-900/10'
                        : 'border-gray-700'
                    }`}
                  >
                    {/* Unavailability Warning Banner */}
                    {isUnavailable && (
                      <div className="mb-3 p-3 bg-yellow-900/30 border border-yellow-700 rounded flex items-start gap-3">
                        <AlertTriangle className="text-yellow-400 flex-shrink-0 mt-0.5" size={20} />
                        <div className="flex-1">
                          <p className="font-semibold text-yellow-400 mb-1">Directory Not Found</p>
                          <p className="text-sm text-yellow-300 mb-2">
                            This directory is not accessible. It may have been moved, renamed, or is on an unmounted drive.
                          </p>
                          <button
                            onClick={() => root.id && handleRelinkScanRoot(root.id)}
                            disabled={relinkingRootId === root.id}
                            className="flex items-center gap-2 px-3 py-1.5 bg-yellow-600 hover:bg-yellow-700 disabled:bg-gray-600 disabled:cursor-not-allowed text-white rounded text-sm transition"
                          >
                            <RefreshCw size={16} className={relinkingRootId === root.id ? 'animate-spin' : ''} />
                            {relinkingRootId === root.id ? 'Relinking...' : 'Relink Directory'}
                          </button>
                        </div>
                      </div>
                    )}

                    {/* Missing Files Warning Banner */}
                    {!isUnavailable && root.id && missingFilesMap[root.id] && missingFilesMap[root.id].length > 0 && (
                      <div className="mb-3 p-3 bg-orange-900/30 border border-orange-700 rounded flex items-start gap-3">
                        <FileWarning className="text-orange-400 flex-shrink-0 mt-0.5" size={20} />
                        <div className="flex-1">
                          <p className="font-semibold text-orange-400 mb-1">Missing Files Detected</p>
                          <p className="text-sm text-orange-300 mb-2">
                            {missingFilesMap[root.id].length} file{missingFilesMap[root.id].length !== 1 ? 's' : ''} in the database no longer exist on disk.
                            These files may have been deleted or moved to another location.
                          </p>
                          <div className="flex gap-2">
                            <button
                              onClick={() => root.id && checkMissingFiles(root.id)}
                              disabled={checkingMissingFiles[root.id]}
                              className="flex items-center gap-2 px-3 py-1.5 bg-orange-600 hover:bg-orange-700 disabled:bg-gray-600 disabled:cursor-not-allowed text-white rounded text-sm transition"
                            >
                              <RefreshCw size={16} className={checkingMissingFiles[root.id] ? 'animate-spin' : ''} />
                              {checkingMissingFiles[root.id] ? 'Checking...' : 'Recheck'}
                            </button>
                            <button
                              onClick={() => root.id && handleDeleteMissingFiles(root.id)}
                              className="flex items-center gap-2 px-3 py-1.5 bg-red-600 hover:bg-red-700 text-white rounded text-sm transition"
                            >
                              <Trash2 size={16} />
                              Delete Records
                            </button>
                          </div>
                        </div>
                      </div>
                    )}

                    <div className="flex items-center justify-between mb-2">
                      <div className="flex-1">
                        <div className="flex items-center gap-2">
                          <span className="block font-mono text-sm font-semibold">{root.path}</span>
                          {isUnavailable && (
                            <span className="px-2 py-0.5 bg-yellow-900/50 border border-yellow-700 rounded text-xs text-yellow-400">
                              Offline
                            </span>
                          )}
                        </div>
                        {root.last_scan && (
                          <span className="text-xs text-gray-400">
                            Last scan: {format(new Date(root.last_scan), 'PPpp')}
                          </span>
                        )}
                      </div>
                      <div className="flex items-center gap-2">
                        <label className="flex items-center gap-2 px-3 py-2 bg-gray-700 rounded cursor-pointer hover:bg-gray-650 transition">
                          <input
                            type="checkbox"
                            checked={root.find_duplicates}
                            onChange={(e) => root.id && toggleDuplicatesFlag(root.id, e.target.checked)}
                            className="w-4 h-4 rounded border-gray-600 text-blue-600 focus:ring-2 focus:ring-blue-500 focus:ring-offset-0 bg-gray-600"
                          />
                          <span className="text-sm text-gray-300">Include in duplicates</span>
                        </label>
                        <button
                          onClick={() => root.id && handleStartScan(root.id)}
                          disabled={isScanning || isUnavailable}
                          className="flex items-center gap-2 px-3 py-2 bg-green-600 hover:bg-green-700 rounded transition disabled:opacity-50 disabled:cursor-not-allowed"
                        >
                          {isScanning ? (
                            <Loader2 className="animate-spin" size={16} />
                          ) : (
                            <Play size={16} />
                          )}
                          {isScanning ? 'Scanning...' : 'Rescan'}
                        </button>
                        <button
                          onClick={() => root.id && handleRemoveScanRoot(root.id)}
                          className="text-red-400 hover:text-red-300 p-2 rounded hover:bg-red-900/20 transition"
                        >
                          <Trash2 size={18} />
                        </button>
                      </div>
                    </div>

                    {/* Scan Result */}
                    {scanResult && (
                      <div className="mt-3 p-3 bg-green-900/30 border border-green-700 rounded">
                        <div className="flex items-start gap-2">
                          <CheckCircle2 className="text-green-400 flex-shrink-0 mt-0.5" size={16} />
                          <div className="flex-1 text-sm">
                            <p className="text-green-400 font-semibold mb-1">Scan Complete</p>
                            <div className="text-gray-300 space-y-0.5">
                              <p>Found: {scanResult.files_found} files</p>
                              <p>Processed: {scanResult.files_processed} files</p>
                              <p>Skipped: {scanResult.files_skipped} files</p>
                            </div>
                            {scanResult.errors.length > 0 && (
                              <details className="mt-2">
                                <summary className="cursor-pointer text-red-400 text-xs">
                                  {scanResult.errors.length} errors
                                </summary>
                                <ul className="mt-1 space-y-0.5 text-xs">
                                  {scanResult.errors.map((error, idx) => (
                                    <li key={idx} className="text-red-300">{String(error)}</li>
                                  ))}
                                </ul>
                              </details>
                            )}
                          </div>
                        </div>
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          )}

          {/* Relink Result Display */}
          {relinkResult && (
            <div className="mt-4 bg-gray-700 rounded-lg p-4 border border-gray-600">
              <h4 className="font-semibold mb-3 flex items-center gap-2">
                <CheckCircle2 className="text-green-500" size={18} />
                Relinking Complete
              </h4>
              <div className="grid grid-cols-3 gap-4 text-sm">
                <div>
                  <p className="text-gray-400">Matched</p>
                  <p className="text-xl font-bold text-green-400">{relinkResult.files_matched}</p>
                </div>
                <div>
                  <p className="text-gray-400">New Files</p>
                  <p className="text-xl font-bold text-blue-400">{relinkResult.files_new}</p>
                </div>
                <div>
                  <p className="text-gray-400">Orphaned</p>
                  <p className="text-xl font-bold text-yellow-400">{relinkResult.files_orphaned}</p>
                </div>
              </div>
              {relinkResult.files_orphaned > 0 && (
                <p className="mt-3 text-sm text-yellow-400">
                  {relinkResult.files_orphaned} files could not be found at the new location.
                  You can manage orphaned files in Settings.
                </p>
              )}
            </div>
          )}
        </div>
      )}

      {activeTab === 'browse' && (
        /* Directory View Tab */
        scanRoots.length === 0 ? (
          <div className="bg-gray-800 rounded-lg p-8 text-center">
            <p className="text-gray-500 mb-4">
              No directories added yet. Go to "Monitored Directories" tab to add directories.
            </p>
          </div>
        ) : (
          <DirectoryTree
            scanRoots={scanRoots}
            duplicates={duplicates}
            refreshTrigger={refreshTrigger}
          />
        )
      )}

      {activeTab === 'duplicates' && (
        /* Duplicates View with Multi-View Tabs */
        <div>
          {/* Sub-tabs for different views */}
          <div className="flex gap-2 mb-4 border-b border-gray-700">
            <button
              onClick={() => setDuplicatesView('files')}
              className={`px-4 py-2 transition relative ${
                duplicatesView === 'files'
                  ? 'text-yellow-400 border-b-2 border-yellow-400'
                  : 'text-gray-400 hover:text-gray-200'
              }`}
            >
              <div className="flex items-center gap-2">
                <Copy size={16} />
                File View ({duplicates.length})
              </div>
            </button>
            <button
              onClick={() => setDuplicatesView('folders')}
              className={`px-4 py-2 transition relative ${
                duplicatesView === 'folders'
                  ? 'text-yellow-400 border-b-2 border-yellow-400'
                  : 'text-gray-400 hover:text-gray-200'
              }`}
            >
              <div className="flex items-center gap-2">
                <FolderOpen size={16} />
                Folder View ({duplicateFolders.length})
              </div>
            </button>
          </div>

          {/* File View */}
          {duplicatesView === 'files' && (
            <div className="bg-gray-800 rounded-lg p-4">
              <div className="flex items-center justify-between mb-3">
                <h3 className="text-lg font-semibold">Duplicate Groups ({duplicates.length})</h3>
                <div className="flex items-center gap-3">
                  <select
                    value={typeFilter}
                    onChange={(e) => setTypeFilter(e.target.value)}
                    className="bg-gray-700 border border-gray-600 rounded px-3 py-1.5 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
                  >
                    <option value="All Types">All Types</option>
                    <option value="Lights">Lights</option>
                    <option value="Darks">Darks</option>
                    <option value="Flats">Flats</option>
                    <option value="Bias">Bias</option>
                    <option value="Other">Other</option>
                  </select>
                  <button
                    onClick={refreshDuplicates}
                    disabled={dupsLoading}
                    className="text-sm text-blue-400 hover:text-blue-300 disabled:opacity-50"
                  >
                    {dupsLoading ? 'Loading...' : 'Refresh'}
                  </button>
                </div>
              </div>

              {dupsError && (
                <div className="mb-4 p-3 bg-red-900/30 border border-red-700 rounded">
                  <p className="text-red-400 text-sm">Error: {String(dupsError)}</p>
                </div>
              )}

              {dupsLoading ? (
                <div className="flex items-center justify-center py-12">
                  <Loader2 className="animate-spin mr-2" size={24} />
                  <span className="text-gray-400">Loading duplicates...</span>
                </div>
              ) : duplicates.length === 0 ? (
                <div className="text-gray-500 text-center py-12">
                  <CheckCircle2 className="mx-auto mb-3 text-green-400" size={48} />
                  <p className="font-semibold mb-1">No duplicates found!</p>
                  <p className="text-sm">All your files are unique.</p>
                </div>
              ) : (
                <div className="space-y-4">
                  {duplicates.filter(group => {
                    if (typeFilter === 'All Types') return true;

                    // Determine type from file paths
                    const samplePath = group.file_paths[0]?.toLowerCase() || '';
                    if (typeFilter === 'Lights' && (samplePath.includes('/lights/') || samplePath.includes('/light/'))) return true;
                    if (typeFilter === 'Darks' && (samplePath.includes('/darks/') || samplePath.includes('/dark/') || samplePath.includes('/calibration/darks'))) return true;
                    if (typeFilter === 'Flats' && (samplePath.includes('/flats/') || samplePath.includes('/flat/') || samplePath.includes('/calibration/flats'))) return true;
                    if (typeFilter === 'Bias' && samplePath.includes('/bias/') || samplePath.includes('/calibration/bias')) return true;
                    if (typeFilter === 'Other' &&
                        !samplePath.includes('/lights/') && !samplePath.includes('/light/') &&
                        !samplePath.includes('/darks/') && !samplePath.includes('/dark/') &&
                        !samplePath.includes('/flats/') && !samplePath.includes('/flat/') &&
                        !samplePath.includes('/bias/')) return true;
                    return false;
                  }).map((group, idx) => (
                    <div key={idx} className="bg-gray-900 rounded-lg p-4 border border-gray-700">
                      <div className="flex items-center justify-between mb-3">
                        <div className="flex items-center gap-3">
                          <Copy className="text-yellow-400" size={20} />
                          <div>
                            <span className="font-semibold text-yellow-400">
                              {group.file_count} identical files
                            </span>
                            <span className="text-gray-400 text-sm ml-3">
                              Size: {(group.size / 1024 / 1024).toFixed(2)} MB each
                            </span>
                          </div>
                        </div>
                        <span className="text-xs font-mono text-gray-500">
                          Hash: {group.content_hash.substring(0, 12)}...
                        </span>
                      </div>

                      <div className="space-y-2">
                        {group.file_paths.map((path, pathIdx) => {
                          const fileId = group.file_ids[pathIdx];
                          return (
                            <div
                              key={pathIdx}
                              className="flex items-center justify-between p-3 bg-gray-800 rounded hover:bg-gray-750 transition"
                            >
                              <div className="flex-1 min-w-0">
                                <p className="font-mono text-sm truncate" title={path}>
                                  {path}
                                </p>
                                <p className="text-xs text-gray-500 mt-1">
                                  Copy {pathIdx + 1} of {group.file_count}
                                </p>
                              </div>
                              <button
                                onClick={async () => {
                                  if (!confirm(`Move "${path}" to Black Hole?`)) return;
                                  try {
                                    setMovingToBlackHole(prev => ({ ...prev, [path]: true }));
                                    await moveToBlackHole(fileId, 'duplicates');
                                    await refreshDuplicates();
                                  } catch (err) {
                                    alert(`Failed: ${String(err)}`);
                                  } finally {
                                    setMovingToBlackHole(prev => ({ ...prev, [path]: false }));
                                  }
                                }}
                                disabled={movingToBlackHole[path]}
                                title="Move to Black Hole"
                                className="ml-4 p-2 text-red-400 hover:text-red-300 hover:bg-red-900/20 rounded transition disabled:opacity-50"
                              >
                                {movingToBlackHole[path] ? (
                                  <Loader2 className="animate-spin" size={16} />
                                ) : (
                                  <Trash2 size={16} />
                                )}
                              </button>
                            </div>
                          );
                        })}
                      </div>

                      <div className="mt-3 pt-3 border-t border-gray-700 text-sm">
                        <span className="text-gray-400">
                          Total wasted space: {((group.size * (group.file_count - 1)) / 1024 / 1024).toFixed(2)} MB
                        </span>
                      </div>
                    </div>
                  ))}

                  {/* Summary */}
                  <div className="bg-blue-900/20 border border-blue-700 rounded-lg p-4">
                    <div className="flex items-center justify-between">
                      <div>
                        <p className="font-semibold text-blue-300">Total Duplicates Summary</p>
                        <p className="text-sm text-gray-400 mt-1">
                          {duplicates.reduce((acc, g) => acc + g.file_count, 0)} duplicate files in {duplicates.length} groups
                        </p>
                      </div>
                      <div className="text-right">
                        <p className="text-2xl font-bold text-blue-300">
                          {(duplicates.reduce((acc, g) => acc + (g.size * (g.file_count - 1)), 0) / 1024 / 1024 / 1024).toFixed(2)} GB
                        </p>
                        <p className="text-sm text-gray-400">wasted space</p>
                      </div>
                    </div>
                  </div>
                </div>
              )}
            </div>
          )}

          {/* Folder View */}
          {duplicatesView === 'folders' && (
            <div className="bg-gray-800 rounded-lg p-4">
              <div className="flex items-center justify-between mb-3">
                <h3 className="text-lg font-semibold">Folder Similarity ({duplicateFolders.length})</h3>
                <button
                  onClick={refreshFolders}
                  disabled={foldersLoading}
                  className="text-sm text-blue-400 hover:text-blue-300 disabled:opacity-50"
                >
                  {foldersLoading ? 'Loading...' : 'Refresh'}
                </button>
              </div>

              {foldersError && (
                <div className="mb-4 p-3 bg-red-900/30 border border-red-700 rounded">
                  <p className="text-red-400 text-sm">Error: {String(foldersError)}</p>
                </div>
              )}

              {foldersLoading ? (
                <div className="flex items-center justify-center py-12">
                  <Loader2 className="animate-spin mr-2" size={24} />
                  <span className="text-gray-400">Analyzing folders...</span>
                </div>
              ) : duplicateFolders.length === 0 ? (
                <div className="text-gray-500 text-center py-12">
                  <CheckCircle2 className="mx-auto mb-3 text-green-400" size={48} />
                  <p className="font-semibold mb-1">No similar folders found!</p>
                  <p className="text-sm">No folders have &gt;70% duplicate files.</p>
                </div>
              ) : (
                <div className="space-y-4">
                  {duplicateFolders.map((folder, idx) => (
                    <div key={idx} className="bg-gray-900 rounded-lg p-4 border border-gray-700">
                      <div className="mb-3">
                        <div className="flex items-center gap-2 mb-2">
                          <FolderOpen className="text-orange-400" size={20} />
                          <span className="text-lg font-semibold text-orange-400">
                            {folder.similarity_percent.toFixed(1)}% Similar
                          </span>
                        </div>
                      </div>

                      <div className="space-y-3">
                        <div className="bg-gray-800 rounded p-3">
                          <div className="flex items-start justify-between">
                            <div className="flex-1">
                              <p className="text-xs text-gray-500 mb-1">Folder A:</p>
                              <p className="font-mono text-sm">{folder.folder_a}</p>
                              <p className="text-xs text-gray-400 mt-1">{folder.unique_a} unique files</p>
                            </div>
                            <button
                              onClick={async () => {
                                if (!confirm(`Move all files in "${folder.folder_a}" to Black Hole?`)) return;
                                try {
                                  // Get all file IDs that belong to this folder
                                  const folderFileIds: number[] = [];
                                  duplicates.forEach(group => {
                                    group.file_paths.forEach((path, idx) => {
                                      if (path.startsWith(folder.folder_a)) {
                                        folderFileIds.push(group.file_ids[idx]);
                                      }
                                    });
                                  });

                                  // Move all files to black hole
                                  for (const fileId of folderFileIds) {
                                    await moveToBlackHole(fileId, 'duplicates');
                                  }
                                  await refreshDuplicates();
                                  await refreshFolders();
                                } catch (err) {
                                  alert(`Failed: ${String(err)}`);
                                }
                              }}
                              title="Delete this folder (move all files to Black Hole)"
                              className="p-2 text-red-400 hover:text-red-300 hover:bg-red-900/20 rounded transition"
                            >
                              <Trash2 size={16} />
                            </button>
                          </div>
                        </div>

                        <div className="bg-gray-800 rounded p-3">
                          <div className="flex items-start justify-between">
                            <div className="flex-1">
                              <p className="text-xs text-gray-500 mb-1">Folder B:</p>
                              <p className="font-mono text-sm">{folder.folder_b}</p>
                              <p className="text-xs text-gray-400 mt-1">{folder.unique_b} unique files</p>
                            </div>
                            <button
                              onClick={async () => {
                                if (!confirm(`Move all files in "${folder.folder_b}" to Black Hole?`)) return;
                                try {
                                  // Get all file IDs that belong to this folder
                                  const folderFileIds: number[] = [];
                                  duplicates.forEach(group => {
                                    group.file_paths.forEach((path, idx) => {
                                      if (path.startsWith(folder.folder_b)) {
                                        folderFileIds.push(group.file_ids[idx]);
                                      }
                                    });
                                  });

                                  // Move all files to black hole
                                  for (const fileId of folderFileIds) {
                                    await moveToBlackHole(fileId, 'duplicates');
                                  }
                                  await refreshDuplicates();
                                  await refreshFolders();
                                } catch (err) {
                                  alert(`Failed: ${String(err)}`);
                                }
                              }}
                              title="Delete this folder (move all files to Black Hole)"
                              className="p-2 text-red-400 hover:text-red-300 hover:bg-red-900/20 rounded transition"
                            >
                              <Trash2 size={16} />
                            </button>
                          </div>
                        </div>
                      </div>

                      <div className="mt-3 pt-3 border-t border-gray-700 grid grid-cols-2 gap-4 text-sm">
                        <div>
                          <p className="text-gray-500">Shared Files:</p>
                          <p className="font-semibold text-yellow-400">{folder.shared_files}</p>
                        </div>
                        <div>
                          <p className="text-gray-500">Shared Size:</p>
                          <p className="font-semibold text-yellow-400">
                            {(folder.shared_size / 1024 / 1024 / 1024).toFixed(2)} GB
                          </p>
                        </div>
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
