import { useState } from 'react';
import { FolderPlus, Play, Filter, Trash2, CheckCircle2, XCircle, Loader2, Copy } from 'lucide-react';
import { open } from '@tauri-apps/plugin-dialog';
import { useScanRoots, useScan, useInitializeDatabase, useDuplicates } from '../hooks/useTauri';
import { format } from 'date-fns';
import DirectoryTree from '../components/DirectoryTree';
import type { ScanResult } from '../types/models';

type TabMode = 'directories' | 'browse' | 'duplicates';

export default function FileManager() {
  const { dbPath, loading: dbLoading, error: dbError } = useInitializeDatabase();
  const { scanRoots, loading: rootsLoading, error: rootsError, addScanRoot, deleteScanRoot } = useScanRoots();
  const { startScan } = useScan();
  const { duplicates, loading: dupsLoading, error: dupsError, refresh: refreshDuplicates } = useDuplicates();
  const [activeTab, setActiveTab] = useState<TabMode>('directories');
  const [refreshTrigger, setRefreshTrigger] = useState(0);
  const [scanningMap, setScanningMap] = useState<Record<number, boolean>>({});
  const [scanResultMap, setScanResultMap] = useState<Record<number, ScanResult>>({});
  const [scanError, setScanError] = useState<string | null>(null);

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
    } catch (error) {
      console.error('Scan failed:', error);
      setScanError(typeof error === 'string' ? error : 'Scan failed');
    } finally {
      setScanningMap(prev => ({ ...prev, [rootId]: false }));
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

                return (
                  <div key={root.id} className="bg-gray-800 rounded-lg p-4 border border-gray-700">
                    <div className="flex items-center justify-between mb-2">
                      <div className="flex-1">
                        <span className="block font-mono text-sm font-semibold">{root.path}</span>
                        {root.last_scan && (
                          <span className="text-xs text-gray-400">
                            Last scan: {format(new Date(root.last_scan), 'PPpp')}
                          </span>
                        )}
                      </div>
                      <div className="flex items-center gap-2">
                        <button
                          onClick={() => root.id && handleStartScan(root.id)}
                          disabled={isScanning}
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
        /* Duplicates View */
        <div className="bg-gray-800 rounded-lg p-4">
          <div className="flex items-center justify-between mb-3">
            <h3 className="text-lg font-semibold">Duplicate Groups ({duplicates.length})</h3>
            <button
              onClick={refreshDuplicates}
              disabled={dupsLoading}
              className="text-sm text-blue-400 hover:text-blue-300 disabled:opacity-50"
            >
              {dupsLoading ? 'Loading...' : 'Refresh'}
            </button>
          </div>

          {dupsError && (
            <div className="mb-4 p-3 bg-red-900/30 border border-red-700 rounded">
              <p className="text-red-400 text-sm">Error loading duplicates: {String(dupsError)}</p>
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
              {duplicates.map((group, idx) => (
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
                    {group.file_paths.map((path, pathIdx) => (
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
                        <button className="ml-4 px-3 py-1 text-sm text-red-400 hover:text-red-300 hover:bg-red-900/20 rounded transition">
                          Delete
                        </button>
                      </div>
                    ))}
                  </div>

                  <div className="mt-3 pt-3 border-t border-gray-700 flex items-center justify-between text-sm">
                    <span className="text-gray-400">
                      Total wasted space: {((group.size * (group.file_count - 1)) / 1024 / 1024).toFixed(2)} MB
                    </span>
                    <button className="px-3 py-1 text-yellow-400 hover:text-yellow-300 hover:bg-yellow-900/20 rounded transition">
                      Keep Best & Delete Others
                    </button>
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
    </div>
  );
}
