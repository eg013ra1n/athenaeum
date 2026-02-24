import { useState, useEffect } from 'react';
import { FolderPlus, Play, Filter, Trash2, CheckCircle2, Loader2, Copy, FolderOpen, RefreshCw, AlertTriangle, Info, AlertCircle, ChevronDown, ChevronRight } from 'lucide-react';
import { pickDirectory, revealItemInDir } from '../api/desktop';
import { api } from '../api';
import { useScanRootsWithAvailability, useDuplicates, useDuplicateFolders, moveToBlackHole } from '../hooks/useTauri';
import { useScanProgressContext } from '../contexts/ScanProgressContext';
import { format } from 'date-fns';
import DirectoryTree from '../components/DirectoryTree';
import type { ScanResult, RelinkResult, FileWithFrame, MissingFileRecord } from '../types/models';
import { ConfirmDialog } from '../components/ConfirmDialog';
import { AlertDialog } from '../components/AlertDialog';
import { ScanSummaryModal } from '../components/ScanSummaryModal';
import { MissingFilesPanel } from '../components/MissingFilesPanel';

type TabMode = 'directories' | 'browse' | 'duplicates' | 'missing-metadata';
type DuplicatesViewMode = 'files' | 'folders';
type MissingCategory = 'all' | 'coordinates' | 'object' | 'datetime' | 'instrument' | 'frametype';

export default function FileManager() {
  const { scanRoots, loading: rootsLoading, error: rootsError, addScanRoot, deleteScanRoot, toggleDuplicatesFlag, toggleUniqueCameraFlag, relinkScanRoot } = useScanRootsWithAvailability();
  const { startRescanWithProgress, isScanning } = useScanProgressContext();
  const { duplicates, loading: dupsLoading, error: dupsError, load: loadDuplicates, refresh: refreshDuplicates } = useDuplicates();
  const { folders: duplicateFolders, loading: foldersLoading, error: foldersError, load: loadFolders, refresh: refreshFolders } = useDuplicateFolders(70);
  const [activeTab, setActiveTab] = useState<TabMode>('directories');
  const [duplicatesView, setDuplicatesView] = useState<DuplicatesViewMode>('files');
  const [typeFilter, setTypeFilter] = useState<string>('All Types');
  const [refreshTrigger, setRefreshTrigger] = useState(0);
  const [scanResultMap, setScanResultMap] = useState<Record<number, ScanResult>>({});
  const [scanError, setScanError] = useState<string | null>(null);
  const [movingToBlackHole, setMovingToBlackHole] = useState<Record<string, boolean>>({});
  const [relinkingRootId, setRelinkingRootId] = useState<number | null>(null);
  const [relinkResult, setRelinkResult] = useState<RelinkResult | null>(null);
  // Missing metadata tab state
  const [missingCategory, setMissingCategory] = useState<MissingCategory>('all');
  const [missingFrames, setMissingFrames] = useState<FileWithFrame[]>([]);
  const [loadingMissing, setLoadingMissing] = useState(false);
  const [missingError, setMissingError] = useState<string | null>(null);
  const [scanSummaryModal, setScanSummaryModal] = useState<{
    isOpen: boolean;
    rootId: number | null;
    rootPath: string;
    missingFilesCount?: number;
  }>({
    isOpen: false,
    rootId: null,
    rootPath: '',
  });
  const [confirmDialog, setConfirmDialog] = useState<{
    isOpen: boolean;
    title: string;
    message: string;
    onConfirm: () => void;
    confirmDanger?: boolean;
  }>({
    isOpen: false,
    title: '',
    message: '',
    onConfirm: () => {},
    confirmDanger: false,
  });
  const [alertDialog, setAlertDialog] = useState<{
    isOpen: boolean;
    title: string;
    message: string;
    variant: 'error' | 'warning' | 'info';
  }>({
    isOpen: false,
    title: '',
    message: '',
    variant: 'info',
  });

  // Scan error log expand state (per scan root)
  const [expandedErrors, setExpandedErrors] = useState<Record<number, boolean>>({});

  // Missing files tracking state
  const [missingFilesCountMap, setMissingFilesCountMap] = useState<Record<number, number>>({});
  const [missingFilesMap, setMissingFilesMap] = useState<Record<number, MissingFileRecord[]>>({});
  const [expandedMissingPanels, setExpandedMissingPanels] = useState<Set<number>>(new Set());
  const [loadingMissingFiles, setLoadingMissingFiles] = useState<number | null>(null);

  // Data file locations state
  const [showDataPaths, setShowDataPaths] = useState(false);
  const [dbPath, setDbPath] = useState<string>('');
  const [logPath, setLogPath] = useState<string>('');

  const showConfirm = (title: string, message: string, onConfirm: () => void, confirmDanger = false) => {
    setConfirmDialog({ isOpen: true, title, message, onConfirm, confirmDanger });
  };

  const showAlert = (title: string, message: string, variant: 'error' | 'warning' | 'info' = 'info') => {
    setAlertDialog({ isOpen: true, title, message, variant });
  };

  // Fetch data file locations on mount
  useEffect(() => {
    api.invoke<string>('get_database_path').then(setDbPath).catch(console.error);
    api.invoke<string>('get_log_path').then(setLogPath).catch(console.error);
  }, []);

  // Lazy load duplicates when Duplicates tab is clicked
  useEffect(() => {
    if (activeTab === 'duplicates') {
      loadDuplicates();
    }
  }, [activeTab, loadDuplicates]);

  // Lazy load folder similarity when Folders sub-tab is clicked
  useEffect(() => {
    if (activeTab === 'duplicates' && duplicatesView === 'folders') {
      loadFolders();
    }
  }, [activeTab, duplicatesView, loadFolders]);

  // Load missing files counts on mount and when scan roots change
  useEffect(() => {
    const loadMissingCounts = async () => {
      try {
        const counts = await api.invoke<Record<number, number>>('get_missing_files_counts');
        setMissingFilesCountMap(counts);
      } catch (error) {
        console.error('Failed to load missing files counts:', error);
      }
    };
    loadMissingCounts();
  }, [scanRoots, refreshTrigger]);

  // Load missing files for a specific root (when panel is expanded)
  const loadMissingFilesForRoot = async (rootId: number) => {
    if (missingFilesMap[rootId]) return; // Already loaded
    setLoadingMissingFiles(rootId);
    try {
      const files = await api.invoke<MissingFileRecord[]>('get_missing_files', { rootId });
      setMissingFilesMap(prev => ({ ...prev, [rootId]: files }));
    } catch (error) {
      console.error('Failed to load missing files:', error);
    } finally {
      setLoadingMissingFiles(null);
    }
  };

  // Toggle missing files panel expansion
  const handleToggleMissingPanel = async (rootId: number) => {
    const newExpanded = new Set(expandedMissingPanels);
    if (newExpanded.has(rootId)) {
      newExpanded.delete(rootId);
    } else {
      newExpanded.add(rootId);
      await loadMissingFilesForRoot(rootId);
    }
    setExpandedMissingPanels(newExpanded);
  };

  // Refresh missing files for a root (called by panel after actions)
  const handleRefreshMissingFiles = async (rootId: number) => {
    try {
      const [files, counts] = await Promise.all([
        api.invoke<MissingFileRecord[]>('get_missing_files', { rootId }),
        api.invoke<Record<number, number>>('get_missing_files_counts'),
      ]);
      setMissingFilesMap(prev => ({ ...prev, [rootId]: files }));
      setMissingFilesCountMap(counts);
    } catch (error) {
      console.error('Failed to refresh missing files:', error);
    }
  };

  // Load frames with missing metadata
  const loadMissingMetadata = async (category: MissingCategory) => {
    try {
      setLoadingMissing(true);
      setMissingError(null);
      const frames = await api.invoke<FileWithFrame[]>('get_frames_with_missing_metadata', { category });
      setMissingFrames(frames);
    } catch (error) {
      console.error('Failed to load missing metadata:', error);
      setMissingError(typeof error === 'string' ? error : 'Failed to load frames with missing metadata');
    } finally {
      setLoadingMissing(false);
    }
  };

  // Load missing metadata when tab is selected or category changes
  useEffect(() => {
    if (activeTab === 'missing-metadata') {
      loadMissingMetadata(missingCategory);
    }
  }, [activeTab, missingCategory]);

  // Handle adding a new directory
  const handleAddDirectory = async () => {
    try {
      const selected = await pickDirectory();

      if (selected && typeof selected === 'string') {
        await addScanRoot(selected);
      }
    } catch (error) {
      console.error('Failed to add directory:', error);
      showAlert('Add Directory Failed', typeof error === 'string' ? error : 'Failed to add directory', 'error');
    }
  };

  // Handle removing a scan root
  const handleRemoveScanRoot = async (id: number) => {
    showConfirm(
      'Remove Directory',
      'Are you sure you want to remove this directory from monitoring?',
      async () => {
        try {
          await deleteScanRoot(id);
        } catch (error) {
          console.error('Failed to remove directory:', error);
        }
      },
      true
    );
  };

  // Handle starting a scan for a specific root
  const handleStartScan = async (rootId: number) => {
    const root = scanRoots.find(r => r.id === rootId);
    if (!root) return;
    const rootPath = root.path;

    try {
      setScanError(null);

      // Unified rescan: checks missing files then scans (all in one progress modal)
      const result = await startRescanWithProgress(rootId, rootPath);
      setScanResultMap(prev => ({ ...prev, [rootId]: result }));
      setRefreshTrigger(prev => prev + 1);

      // Open the scan summary modal with missing files count
      setScanSummaryModal({
        isOpen: true,
        rootId,
        rootPath,
        missingFilesCount: result.missingFilesCount,
      });
    } catch (error) {
      console.error('Scan failed:', error);
      setScanError(typeof error === 'string' ? error : 'Scan failed');
    }
  };

  // Handle relinking a scan root to a new location
  const handleRelinkScanRoot = async (rootId: number) => {
    try {
      setRelinkingRootId(rootId);
      setRelinkResult(null);

      const selectedPath = await pickDirectory();

      if (!selectedPath || typeof selectedPath !== 'string') {
        setRelinkingRootId(null);
        return;
      }

      const result = await relinkScanRoot(rootId, selectedPath);
      setRelinkResult(result);
    } catch (error) {
      console.error('Relink failed:', error);
      showAlert('Relink Failed', typeof error === 'string' ? error : 'Relink failed', 'error');
    } finally {
      setRelinkingRootId(null);
    }
  };

  return (
    <div className="p-6">
      <div className="mb-6">
        <h2 className="text-3xl font-bold mb-2">File Manager</h2>
        <p className="text-content-muted">
          Manage monitored directories and view FITS/XISF metadata
        </p>
      </div>

      {/* Data file locations */}
      <div className="mb-4">
        <button
          onClick={() => setShowDataPaths(!showDataPaths)}
          className="flex items-center gap-2 text-sm text-content-muted hover:text-content transition"
        >
          <Info size={14} />
          Data file locations
          {showDataPaths ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
        </button>
        {showDataPaths && (
          <div className="mt-2 bg-surface-secondary rounded p-3 text-sm font-mono space-y-2">
            <div className="flex items-center gap-2">
              <span className="text-content-muted min-w-[80px]">Database:</span>
              <span className="text-content truncate">{dbPath || '—'}</span>
              {dbPath && (
                <button
                  onClick={() => revealItemInDir(dbPath)}
                  className="text-content-muted hover:text-content transition flex-shrink-0"
                  title="Reveal in file manager"
                >
                  <FolderOpen size={14} />
                </button>
              )}
            </div>
            <div className="flex items-center gap-2">
              <span className="text-content-muted min-w-[80px]">Log file:</span>
              <span className="text-content truncate">{logPath || '—'}</span>
              {logPath && (
                <button
                  onClick={() => revealItemInDir(logPath)}
                  className="text-content-muted hover:text-content transition flex-shrink-0"
                  title="Reveal in file manager"
                >
                  <FolderOpen size={14} />
                </button>
              )}
            </div>
          </div>
        )}
      </div>

      {/* Tab Navigation */}
      <div className="flex gap-2 mb-3 border-b border-border">
        <button
          onClick={() => setActiveTab('directories')}
          className={`px-4 py-2 transition relative ${
            activeTab === 'directories'
              ? 'text-accent border-b-2 border-accent'
              : 'text-content-muted hover:text-content'
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
              ? 'text-accent border-b-2 border-accent'
              : 'text-content-muted hover:text-content'
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
              ? 'text-accent border-b-2 border-accent'
              : 'text-content-muted hover:text-content'
          }`}
        >
          <div className="flex items-center gap-2">
            <Copy size={16} />
            Duplicates
          </div>
        </button>
        <button
          onClick={() => setActiveTab('missing-metadata')}
          className={`px-4 py-2 transition relative ${
            activeTab === 'missing-metadata'
              ? 'text-orange border-b-2 border-orange'
              : 'text-content-muted hover:text-content'
          }`}
        >
          <div className="flex items-center gap-2">
            <AlertCircle size={16} />
            Missing Metadata
          </div>
        </button>
      </div>

      {/* Error Alerts */}
      {rootsError && (
        <div className="mb-6 p-4 bg-error-muted border border-error/50 rounded-lg">
          <p className="text-error">Error loading scan roots: {String(rootsError)}</p>
        </div>
      )}
      {scanError && (
        <div className="mb-6 p-4 bg-error-muted border border-error/50 rounded-lg">
          <p className="text-error">Scan error: {String(scanError)}</p>
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
              className="flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover rounded-lg transition disabled:opacity-50 disabled:cursor-not-allowed"
            >
              <FolderPlus size={20} />
              Add Directory
            </button>
          </div>

          {rootsLoading ? (
            <div className="flex items-center justify-center py-12">
              <Loader2 className="animate-spin mr-2" size={20} />
              <span className="text-content-muted">Loading directories...</span>
            </div>
          ) : scanRoots.length === 0 ? (
            <div className="bg-surface-elevated rounded-lg p-8 text-center">
              <p className="text-content-muted">
                No directories added yet. Click "Add Directory" to start.
              </p>
            </div>
          ) : (
            <div className="space-y-3">
              {scanRoots.map((root) => {
                const rootIsScanning = root.id ? isScanning(root.id) : false;
                const scanResult = root.id ? scanResultMap[root.id] : null;
                const isUnavailable = !root.is_available;

                return (
                  <div
                    key={root.id}
                    className={`bg-surface-elevated rounded-lg p-4 border ${
                      isUnavailable
                        ? 'border-warning bg-warning/10'
                        : 'border-border'
                    }`}
                  >
                    {/* Unavailability Warning Banner */}
                    {isUnavailable && (
                      <div className="mb-3 p-3 bg-warning-muted border border-warning/50 rounded flex items-start gap-3">
                        <AlertTriangle className="text-warning flex-shrink-0 mt-0.5" size={20} />
                        <div className="flex-1">
                          <p className="font-semibold text-warning mb-1">Directory Not Found</p>
                          <p className="text-sm text-warning/80 mb-2">
                            This directory is not accessible. It may have been moved, renamed, or is on an unmounted drive.
                          </p>
                          <button
                            onClick={() => root.id && handleRelinkScanRoot(root.id)}
                            disabled={relinkingRootId === root.id}
                            className="flex items-center gap-2 px-3 py-1.5 bg-warning hover:brightness-90 disabled:bg-surface-hover disabled:cursor-not-allowed text-white rounded text-sm transition"
                          >
                            <RefreshCw size={16} className={relinkingRootId === root.id ? 'animate-spin' : ''} />
                            {relinkingRootId === root.id ? 'Relinking...' : 'Relink Directory'}
                          </button>
                        </div>
                      </div>
                    )}


                    <div className="flex items-center justify-between mb-2">
                      <div className="flex-1">
                        <div className="flex items-center gap-2">
                          <span className="block font-mono text-sm font-semibold">{root.path}</span>
                          {isUnavailable && (
                            <span className="px-2 py-0.5 bg-warning-muted border border-warning/50 rounded text-xs text-warning">
                              Offline
                            </span>
                          )}
                        </div>
                        {root.last_scan && (
                          <span className="text-xs text-content-muted">
                            Last scan: {format(new Date(root.last_scan), 'MMM d, yyyy HH:mm')}
                          </span>
                        )}
                      </div>
                      <div className="flex items-center gap-2">
                        <label className="flex items-center gap-2 px-3 py-2 bg-surface-hover rounded cursor-pointer hover:brightness-110 transition">
                          <input
                            type="checkbox"
                            checked={root.find_duplicates}
                            onChange={(e) => root.id && toggleDuplicatesFlag(root.id, e.target.checked)}
                            className="w-4 h-4 rounded border-border text-accent focus:ring-2 focus:ring-accent focus:ring-offset-0 bg-surface-hover"
                          />
                          <span className="text-sm text-content-secondary">Include in duplicates</span>
                        </label>
                        <label className="flex items-center gap-2 px-3 py-2 bg-surface-hover rounded cursor-pointer hover:brightness-110 transition" title="Appends a unique suffix to INSTRUME for calibration separation when using identical cameras across scan roots. Re-scan required after toggling.">
                          <input
                            type="checkbox"
                            checked={root.unique_camera}
                            onChange={async (e) => {
                              if (!root.id) return;
                              try {
                                await toggleUniqueCameraFlag(root.id, e.target.checked);
                              } catch (err) {
                                console.error('Failed to toggle unique camera:', err);
                              }
                            }}
                            className="w-4 h-4 rounded border-border text-accent focus:ring-2 focus:ring-accent focus:ring-offset-0 bg-surface-hover"
                          />
                          <span className="text-sm text-content-secondary">Unique camera</span>
                        </label>
                        <button
                          onClick={() => root.id && handleStartScan(root.id)}
                          disabled={rootIsScanning || isUnavailable}
                          className="flex items-center gap-2 px-3 py-2 bg-success hover:brightness-90 rounded transition disabled:opacity-50 disabled:cursor-not-allowed"
                        >
                          {rootIsScanning ? (
                            <Loader2 className="animate-spin" size={16} />
                          ) : (
                            <Play size={16} />
                          )}
                          {rootIsScanning ? 'Scanning...' : root.last_scan ? 'Rescan' : 'Scan'}
                        </button>
                        <button
                          onClick={() => root.id && handleRemoveScanRoot(root.id)}
                          className="text-error hover:text-error/90 p-2 rounded hover:bg-error-muted transition"
                        >
                          <Trash2 size={18} />
                        </button>
                      </div>
                    </div>

                    {/* Scan Result */}
                    {scanResult && (
                      <div className="mt-3 p-3 bg-success-muted border border-success/50 rounded">
                        <div className="flex items-center justify-between">
                          <div className="flex items-center gap-2">
                            <CheckCircle2 className="text-success flex-shrink-0" size={16} />
                            <span className="text-success font-semibold text-sm">Scan Complete</span>
                          </div>
                          <div className="flex items-center gap-4 text-sm">
                            <span className="text-content-secondary">
                              <span className="font-semibold text-success">{scanResult.files_processed}</span> processed
                              {scanResult.lights_count > 0 && (
                                <span className="text-warning ml-2">
                                  ({scanResult.lights_count} lights)
                                </span>
                              )}
                              {(scanResult.darks_count + scanResult.flats_count + scanResult.bias_count + scanResult.darkflats_count) > 0 && (
                                <span className="text-accent ml-1">
                                  + {scanResult.darks_count + scanResult.flats_count + scanResult.bias_count + scanResult.darkflats_count} cal
                                </span>
                              )}
                            </span>
                            {scanResult.errors.length > 0 && (
                              <span className="text-error text-xs">
                                {scanResult.errors.length} errors
                              </span>
                            )}
                            <button
                              onClick={() => root.id && setScanSummaryModal({
                                isOpen: true,
                                rootId: root.id,
                                rootPath: root.path,
                              })}
                              className="p-1.5 hover:bg-surface-hover rounded transition"
                              title="View scan details"
                            >
                              <Info size={16} className="text-content-muted hover:text-accent" />
                            </button>
                          </div>
                        </div>
                      </div>
                    )}

                    {/* Missing Files Indicator and Panel */}
                    {root.id && (missingFilesCountMap[root.id] ?? 0) > 0 && (
                      <div className="mt-3">
                        <button
                          onClick={() => root.id && handleToggleMissingPanel(root.id)}
                          className="flex items-center gap-2 w-full p-2 text-left hover:bg-orange/25 rounded-lg transition"
                        >
                          {expandedMissingPanels.has(root.id) ? (
                            <ChevronDown className="text-orange" size={16} />
                          ) : (
                            <ChevronRight className="text-orange" size={16} />
                          )}
                          <AlertTriangle className="text-orange" size={16} />
                          <span className="text-sm text-orange font-medium">
                            {missingFilesCountMap[root.id]} missing file{missingFilesCountMap[root.id] !== 1 ? 's' : ''}
                          </span>
                          {loadingMissingFiles === root.id && (
                            <Loader2 className="animate-spin text-orange ml-2" size={14} />
                          )}
                        </button>
                        {expandedMissingPanels.has(root.id) && missingFilesMap[root.id] && (
                          <div className="mt-2">
                            <MissingFilesPanel
                              rootId={root.id}
                              missingFiles={missingFilesMap[root.id]}
                              onRefresh={() => root.id && handleRefreshMissingFiles(root.id)}
                            />
                          </div>
                        )}
                      </div>
                    )}

                    {/* Persistent scan error log */}
                    {(() => {
                      const scanResult = root.id ? scanResultMap[root.id] : undefined;
                      const displayErrors = scanResult?.errors ?? root.last_scan_errors ?? [];
                      const isExpanded = root.id ? (expandedErrors[root.id] ?? false) : false;
                      if (displayErrors.length === 0) return null;
                      return (
                        <div className="mt-2 border border-error/30 rounded overflow-hidden">
                          <button
                            onClick={() => root.id && setExpandedErrors(prev => ({
                              ...prev,
                              [root.id!]: !prev[root.id!]
                            }))}
                            className="w-full flex items-center justify-between px-3 py-2 bg-error-muted hover:bg-surface-hover transition text-sm"
                          >
                            <span className="flex items-center gap-2 text-error font-medium">
                              <AlertCircle size={14} />
                              {displayErrors.length} file{displayErrors.length !== 1 ? 's' : ''} failed in last scan
                            </span>
                            <ChevronDown
                              size={14}
                              className={`text-error transition-transform ${isExpanded ? 'rotate-180' : ''}`}
                            />
                          </button>
                          {isExpanded && (
                            <div className="px-3 py-2 max-h-40 overflow-y-auto space-y-1 bg-surface">
                              {displayErrors.map((err, i) => (
                                <p key={i} className="text-xs text-error/80 font-mono break-all">{err}</p>
                              ))}
                            </div>
                          )}
                        </div>
                      );
                    })()}
                  </div>
                );
              })}
            </div>
          )}

          {/* Relink Result Display */}
          {relinkResult && (
            <div className="mt-4 bg-surface-hover rounded-lg p-4 border border-border">
              <h4 className="font-semibold mb-3 flex items-center gap-2">
                <CheckCircle2 className="text-success" size={18} />
                Relinking Complete
              </h4>
              <div className="grid grid-cols-3 gap-4 text-sm">
                <div>
                  <p className="text-content-muted">Matched</p>
                  <p className="text-xl font-bold text-success">{relinkResult.files_matched}</p>
                </div>
                <div>
                  <p className="text-content-muted">New Files</p>
                  <p className="text-xl font-bold text-accent">{relinkResult.files_new}</p>
                </div>
                <div>
                  <p className="text-content-muted">Orphaned</p>
                  <p className="text-xl font-bold text-warning">{relinkResult.files_orphaned}</p>
                </div>
              </div>
              {relinkResult.files_orphaned > 0 && (
                <p className="mt-3 text-sm text-warning">
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
          <div className="bg-surface-elevated rounded-lg p-8 text-center">
            <p className="text-content-muted mb-4">
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
          <div className="flex gap-2 mb-4 border-b border-border">
            <button
              onClick={() => setDuplicatesView('files')}
              className={`px-4 py-2 transition relative ${
                duplicatesView === 'files'
                  ? 'text-warning border-b-2 border-warning'
                  : 'text-content-muted hover:text-content'
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
                  ? 'text-warning border-b-2 border-warning'
                  : 'text-content-muted hover:text-content'
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
            <div className="bg-surface-elevated rounded-lg p-4">
              <div className="flex items-center justify-between mb-3">
                <h3 className="text-lg font-semibold">Duplicate Groups ({duplicates.length})</h3>
                <div className="flex items-center gap-3">
                  <select
                    value={typeFilter}
                    onChange={(e) => setTypeFilter(e.target.value)}
                    className="bg-surface-hover border border-border rounded px-3 py-1.5 text-sm focus:outline-none focus:ring-2 focus:ring-accent"
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
                    className="text-sm text-accent hover:text-accent-hover disabled:opacity-50"
                  >
                    {dupsLoading ? 'Loading...' : 'Refresh'}
                  </button>
                </div>
              </div>

              {dupsError && (
                <div className="mb-4 p-3 bg-error-muted border border-error/50 rounded">
                  <p className="text-error text-sm">Error: {String(dupsError)}</p>
                </div>
              )}

              {dupsLoading ? (
                <div className="flex items-center justify-center py-12">
                  <Loader2 className="animate-spin mr-2" size={24} />
                  <span className="text-content-muted">Loading duplicates...</span>
                </div>
              ) : duplicates.length === 0 ? (
                <div className="text-content-muted text-center py-12">
                  <CheckCircle2 className="mx-auto mb-3 text-success" size={48} />
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
                    <div key={idx} className="bg-surface rounded-lg p-4 border border-border">
                      <div className="flex items-center justify-between mb-3">
                        <div className="flex items-center gap-3">
                          <Copy className="text-warning" size={20} />
                          <div>
                            <span className="font-semibold text-warning">
                              {group.file_count} identical files
                            </span>
                            <span className="text-content-muted text-sm ml-3">
                              Size: {(group.size / 1024 / 1024).toFixed(2)} MB each
                            </span>
                          </div>
                        </div>
                        <span className="text-xs font-mono text-content-muted">
                          Hash: {group.content_hash.substring(0, 12)}...
                        </span>
                      </div>

                      <div className="space-y-2">
                        {group.file_paths.map((path, pathIdx) => {
                          const fileId = group.file_ids[pathIdx];
                          return (
                            <div
                              key={pathIdx}
                              className="flex items-center justify-between p-3 bg-surface-elevated rounded hover:bg-surface-hover transition"
                            >
                              <div className="flex-1 min-w-0">
                                <p className="font-mono text-sm truncate" title={path}>
                                  {path}
                                </p>
                                <p className="text-xs text-content-muted mt-1">
                                  Copy {pathIdx + 1} of {group.file_count}
                                </p>
                              </div>
                              <button
                                onClick={() => {
                                  showConfirm(
                                    'Move to Black Hole',
                                    `Move "${path}" to Black Hole?`,
                                    async () => {
                                      try {
                                        setMovingToBlackHole(prev => ({ ...prev, [path]: true }));
                                        await moveToBlackHole(fileId, 'duplicates');
                                        await refreshDuplicates();
                                      } catch (err) {
                                        showAlert('Move Failed', `Failed: ${String(err)}`, 'error');
                                      } finally {
                                        setMovingToBlackHole(prev => ({ ...prev, [path]: false }));
                                      }
                                    },
                                    true
                                  );
                                }}
                                disabled={movingToBlackHole[path]}
                                title="Move to Black Hole"
                                className="ml-4 p-2 text-error hover:text-error/90 hover:bg-error-muted rounded transition disabled:opacity-50"
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

                      <div className="mt-3 pt-3 border-t border-border text-sm">
                        <span className="text-content-muted">
                          Total wasted space: {((group.size * (group.file_count - 1)) / 1024 / 1024).toFixed(2)} MB
                        </span>
                      </div>
                    </div>
                  ))}

                  {/* Summary */}
                  <div className="bg-info-muted border border-info/50 rounded-lg p-4">
                    <div className="flex items-center justify-between">
                      <div>
                        <p className="font-semibold text-info/80">Total Duplicates Summary</p>
                        <p className="text-sm text-content-muted mt-1">
                          {duplicates.reduce((acc, g) => acc + g.file_count, 0)} duplicate files in {duplicates.length} groups
                        </p>
                      </div>
                      <div className="text-right">
                        <p className="text-2xl font-bold text-info/80">
                          {(duplicates.reduce((acc, g) => acc + (g.size * (g.file_count - 1)), 0) / 1024 / 1024 / 1024).toFixed(2)} GB
                        </p>
                        <p className="text-sm text-content-muted">wasted space</p>
                      </div>
                    </div>
                  </div>
                </div>
              )}
            </div>
          )}

          {/* Folder View */}
          {duplicatesView === 'folders' && (
            <div className="bg-surface-elevated rounded-lg p-4">
              <div className="flex items-center justify-between mb-3">
                <h3 className="text-lg font-semibold">Folder Similarity ({duplicateFolders.length})</h3>
                <button
                  onClick={refreshFolders}
                  disabled={foldersLoading}
                  className="text-sm text-accent hover:text-accent-hover disabled:opacity-50"
                >
                  {foldersLoading ? 'Loading...' : 'Refresh'}
                </button>
              </div>

              {foldersError && (
                <div className="mb-4 p-3 bg-error-muted border border-error/50 rounded">
                  <p className="text-error text-sm">Error: {String(foldersError)}</p>
                </div>
              )}

              {foldersLoading ? (
                <div className="flex items-center justify-center py-12">
                  <Loader2 className="animate-spin mr-2" size={24} />
                  <span className="text-content-muted">Analyzing folders...</span>
                </div>
              ) : duplicateFolders.length === 0 ? (
                <div className="text-content-muted text-center py-12">
                  <CheckCircle2 className="mx-auto mb-3 text-success" size={48} />
                  <p className="font-semibold mb-1">No similar folders found!</p>
                  <p className="text-sm">No folders have &gt;70% duplicate files.</p>
                </div>
              ) : (
                <div className="space-y-4">
                  {duplicateFolders.map((folder, idx) => (
                    <div key={idx} className="bg-surface rounded-lg p-4 border border-border">
                      <div className="mb-3">
                        <div className="flex items-center gap-2 mb-2">
                          <FolderOpen className="text-orange" size={20} />
                          <span className="text-lg font-semibold text-orange">
                            {folder.similarity_percent.toFixed(1)}% Similar
                          </span>
                        </div>
                      </div>

                      <div className="space-y-3">
                        <div className="bg-surface-elevated rounded p-3">
                          <div className="flex items-start justify-between">
                            <div className="flex-1">
                              <p className="text-xs text-content-muted mb-1">Folder A:</p>
                              <p className="font-mono text-sm">{folder.folder_a}</p>
                              <p className="text-xs text-content-muted mt-1">{folder.unique_a} unique files</p>
                            </div>
                            <button
                              onClick={() => {
                                showConfirm(
                                  'Move Folder to Black Hole',
                                  `Move all files in "${folder.folder_a}" to Black Hole?`,
                                  async () => {
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
                                      showAlert('Move Failed', `Failed: ${String(err)}`, 'error');
                                    }
                                  },
                                  true
                                );
                              }}
                              title="Delete this folder (move all files to Black Hole)"
                              className="p-2 text-error hover:text-error/90 hover:bg-error-muted rounded transition"
                            >
                              <Trash2 size={16} />
                            </button>
                          </div>
                        </div>

                        <div className="bg-surface-elevated rounded p-3">
                          <div className="flex items-start justify-between">
                            <div className="flex-1">
                              <p className="text-xs text-content-muted mb-1">Folder B:</p>
                              <p className="font-mono text-sm">{folder.folder_b}</p>
                              <p className="text-xs text-content-muted mt-1">{folder.unique_b} unique files</p>
                            </div>
                            <button
                              onClick={() => {
                                showConfirm(
                                  'Move Folder to Black Hole',
                                  `Move all files in "${folder.folder_b}" to Black Hole?`,
                                  async () => {
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
                                      showAlert('Move Failed', `Failed: ${String(err)}`, 'error');
                                    }
                                  },
                                  true
                                );
                              }}
                              title="Delete this folder (move all files to Black Hole)"
                              className="p-2 text-error hover:text-error/90 hover:bg-error-muted rounded transition"
                            >
                              <Trash2 size={16} />
                            </button>
                          </div>
                        </div>
                      </div>

                      <div className="mt-3 pt-3 border-t border-border grid grid-cols-2 gap-4 text-sm">
                        <div>
                          <p className="text-content-muted">Shared Files:</p>
                          <p className="font-semibold text-warning">{folder.shared_files}</p>
                        </div>
                        <div>
                          <p className="text-content-muted">Shared Size:</p>
                          <p className="font-semibold text-warning">
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

      {/* Missing Metadata Tab */}
      {activeTab === 'missing-metadata' && (
        <div className="bg-surface-elevated rounded-lg p-4">
          <div className="flex items-center justify-between mb-4">
            <h3 className="text-lg font-semibold">Frames with Missing Metadata</h3>
            <div className="flex items-center gap-3">
              <select
                value={missingCategory}
                onChange={(e) => setMissingCategory(e.target.value as MissingCategory)}
                className="bg-surface-hover border border-border rounded px-3 py-1.5 text-sm focus:outline-none focus:ring-2 focus:ring-accent"
              >
                <option value="all">All Missing</option>
                <option value="coordinates">Missing Coordinates</option>
                <option value="object">Missing Object Name</option>
                <option value="datetime">Missing Date/Time</option>
                <option value="instrument">Missing Instrument</option>
                <option value="frametype">Missing Frame Type</option>
              </select>
              <button
                onClick={() => loadMissingMetadata(missingCategory)}
                disabled={loadingMissing}
                className="text-sm text-accent hover:text-accent-hover disabled:opacity-50"
              >
                {loadingMissing ? 'Loading...' : 'Refresh'}
              </button>
            </div>
          </div>

          {/* Results count */}
          <div className="mb-4 text-sm text-content-muted">
            Showing {missingFrames.length} frames with missing metadata
          </div>

          {missingError && (
            <div className="mb-4 p-3 bg-error-muted border border-error/50 rounded">
              <p className="text-error text-sm">Error: {missingError}</p>
            </div>
          )}

          {loadingMissing ? (
            <div className="flex items-center justify-center py-12">
              <Loader2 className="animate-spin mr-2" size={24} />
              <span className="text-content-muted">Loading frames...</span>
            </div>
          ) : missingFrames.length === 0 ? (
            <div className="text-content-muted text-center py-12">
              <CheckCircle2 className="mx-auto mb-3 text-success" size={48} />
              <p className="font-semibold mb-1">All metadata complete!</p>
              <p className="text-sm">No frames are missing the selected metadata.</p>
            </div>
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full">
                <thead className="bg-surface sticky top-0">
                  <tr>
                    <th className="px-4 py-3 text-left text-xs font-medium text-content-muted uppercase">File</th>
                    <th className="px-4 py-3 text-left text-xs font-medium text-content-muted uppercase">Missing</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-border">
                  {missingFrames.map((item, idx) => {
                    const frame = item.frame;
                    const missing: string[] = [];

                    // Check what's missing
                    if (frame) {
                      const hasCoords = (frame.ra !== null && frame.dec !== null) ||
                                        (frame.objctra !== null && frame.objctdec !== null);
                      if (!hasCoords) missing.push('Coordinates');
                      if (!frame.object) missing.push('Object');
                      if (!frame.date_obs) missing.push('Date');
                      if (!frame.instrume) missing.push('Instrument');
                    }

                    return (
                      <tr key={item.file.id || idx} className="hover:bg-surface-hover transition">
                        <td className="px-4 py-3">
                          <div className="flex flex-col">
                            <span className="font-medium text-sm truncate max-w-md" title={item.file.path}>
                              {item.file.filename}
                            </span>
                            <span className="text-xs text-content-muted truncate max-w-md" title={item.file.path}>
                              {item.file.path}
                            </span>
                          </div>
                        </td>
                        <td className="px-4 py-3">
                          <div className="flex flex-wrap gap-1">
                            {missing.map((m) => (
                              <span
                                key={m}
                                className={`px-2 py-0.5 rounded text-xs font-medium ${
                                  m === 'Coordinates'
                                    ? 'bg-error-muted text-error border border-error/50'
                                    : m === 'Object'
                                    ? 'bg-orange/25 text-orange border border-orange/50'
                                    : m === 'Date'
                                    ? 'bg-warning-muted text-warning border border-warning/50'
                                    : 'bg-info-muted text-accent border border-info/50'
                                }`}
                              >
                                {m}
                              </span>
                            ))}
                          </div>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}
        </div>
      )}

      {/* Confirm Dialog */}
      <ConfirmDialog
        isOpen={confirmDialog.isOpen}
        title={confirmDialog.title}
        message={confirmDialog.message}
        onConfirm={() => {
          setConfirmDialog({ ...confirmDialog, isOpen: false });
          confirmDialog.onConfirm();
        }}
        onCancel={() => setConfirmDialog({ ...confirmDialog, isOpen: false })}
        confirmDanger={confirmDialog.confirmDanger}
      />

      {/* Alert Dialog */}
      <AlertDialog
        isOpen={alertDialog.isOpen}
        title={alertDialog.title}
        message={alertDialog.message}
        variant={alertDialog.variant}
        onClose={() => setAlertDialog({ ...alertDialog, isOpen: false })}
      />

      {/* Scan Summary Modal */}
      {scanSummaryModal.rootId && scanResultMap[scanSummaryModal.rootId] && (
        <ScanSummaryModal
          isOpen={scanSummaryModal.isOpen}
          onClose={() => setScanSummaryModal({ ...scanSummaryModal, isOpen: false })}
          scanResult={scanResultMap[scanSummaryModal.rootId]}
          rootPath={scanSummaryModal.rootPath}
          missingFilesCount={scanSummaryModal.missingFilesCount}
        />
      )}
    </div>
  );
}
