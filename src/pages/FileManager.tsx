import { useState, useEffect, useCallback } from 'react';
import { useLocation, useNavigate } from 'react-router-dom';
import { Folder, Filter, Trash2, CheckCircle2, Loader2, Copy, FolderOpen, AlertCircle } from 'lucide-react';
import { useScanRootsWithAvailability, useDuplicates, useDuplicateFolders, moveToBlackHole } from '../hooks/useTauri';
import DualPaneFileBrowser, { type DualPaneRevealRequest } from '../components/dualpane/DualPaneFileBrowser';
import { ConfirmDialog } from '../components/ConfirmDialog';
import { AlertDialog } from '../components/AlertDialog';
import { MissingMetadataView } from '../components/missing-metadata/MissingMetadataView';
import { DuplicatesView } from '../components/duplicates/DuplicatesView';
import FoldersTab from '../components/folders/FoldersTab';

type TabMode = 'directories' | 'browse' | 'duplicates' | 'missing-metadata';
type DuplicatesViewMode = 'files' | 'folders';

// Boundary-safe folder membership: `/data/Set1` must not capture
// `/data/Set10` (this feeds a move-to-Black-Hole, so a false match
// relocates real files). Separator detected from the folder path itself.
const isInFolder = (path: string, folder: string) =>
  path.startsWith(folder + (folder.includes('\\') ? '\\' : '/'));

export default function FileManager() {
  // The Folders tab owns its own scan-root state; this instance exists for the
  // Browse Files tab, which hands the roots to the dual-pane browser. The two
  // instances are kept in step by the `onRootsChanged` callback below — without
  // it, a folder added on the Folders tab stays invisible to Browse Files until
  // this page remounts.
  const { scanRoots, error: rootsError, refresh: refreshScanRoots } = useScanRootsWithAvailability();
  const { duplicates, loading: dupsLoading, error: dupsError, load: loadDuplicates, refresh: refreshDuplicates } = useDuplicates();
  const { folders: duplicateFolders, loading: foldersLoading, error: foldersError, load: loadFolders, refresh: refreshFolders } = useDuplicateFolders(70);
  const [activeTab, setActiveTab] = useState<TabMode>('directories');
  const [browserReveal, setBrowserReveal] = useState<DualPaneRevealRequest | undefined>(undefined);
  const location = useLocation();
  const navigate = useNavigate();

  // External pages (e.g. Missing Metadata) can hand off a file path via
  // `navigate('/files', { state: { reveal: { path, token } } })`. Switch to
  // the Browse Files tab, forward the reveal request to the dual-pane
  // browser, and clear the location state so back-navigation doesn't
  // re-trigger the reveal.
  useEffect(() => {
    const state = location.state as { reveal?: DualPaneRevealRequest } | null;
    const incoming = state?.reveal;
    if (!incoming) return;
    setActiveTab('browse');
    setBrowserReveal(incoming);
    navigate(location.pathname, { replace: true, state: null });
  }, [location.state, location.pathname, navigate]);

  // Deep-link from the `/transfers` app-data warning strip (UX-1): land on the
  // Folders tab and select the Sync Incoming role. The token is a monotonic
  // counter (0 = never requested) so FoldersTab can latch each request exactly
  // once — clearing the navigation state below must not cancel a pending
  // selection, and a repeat deep-link must still re-select the role.
  const [syncIncomingToken, setSyncIncomingToken] = useState(0);
  useEffect(() => {
    const state = location.state as { focusSyncIncoming?: boolean } | null;
    if (!state?.focusSyncIncoming) return;
    setActiveTab('directories');
    setSyncIncomingToken((t) => t + 1);
    navigate(location.pathname, { replace: true, state: null });
  }, [location.state, location.pathname, navigate]);

  // Both callbacks are stable so the child's dependency arrays don't churn.
  // Lowering the token once FoldersTab has consumed it is what stops the
  // selection from being re-forced every time the user returns to the tab
  // (FoldersTab unmounts on tab switch, taking its own latch with it).
  const handleSyncIncomingHandled = useCallback(() => setSyncIncomingToken(0), []);
  const handleFolderRootsChanged = useCallback(() => { void refreshScanRoots(); }, [refreshScanRoots]);

  const [duplicatesView, setDuplicatesView] = useState<DuplicatesViewMode>('files');
  const [missingMetadataCount, setMissingMetadataCount] = useState<number | null>(null);
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

  const showConfirm = (title: string, message: string, onConfirm: () => void, confirmDanger = false) => {
    setConfirmDialog({ isOpen: true, title, message, onConfirm, confirmDanger });
  };

  const showAlert = (title: string, message: string, variant: 'error' | 'warning' | 'info' = 'info') => {
    setAlertDialog({ isOpen: true, title, message, variant });
  };


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

  return (
    <div className="p-4 pt-3 h-full flex flex-col min-h-0">
      <div className="mb-4">
        <h2 className="text-2xl font-bold">
          File Manager
          <span className="text-sm font-normal text-content-muted ml-3">
            Manage monitored directories and view FITS/XISF metadata
          </span>
        </h2>
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
            <Folder size={16} />
            Folders
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
            Missing Metadata{missingMetadataCount != null ? ` (${missingMetadataCount})` : ''}
          </div>
        </button>
      </div>

      {/* Error Alerts. The Folders tab renders its own scan-root error, so the
          page-level banner is suppressed there to avoid a duplicate. */}
      {rootsError && activeTab !== 'directories' && (
        <div className="mb-6 p-4 bg-error-muted border border-error/50 rounded-lg">
          <p className="text-error">Error loading scan roots: {String(rootsError)}</p>
        </div>
      )}

      {/* Tab Content */}
      {activeTab === 'directories' && (
        <FoldersTab
          selectSyncIncomingToken={syncIncomingToken}
          onRootsChanged={handleFolderRootsChanged}
          onSyncIncomingHandled={handleSyncIncomingHandled}
        />
      )}

      {activeTab === 'browse' && (
        /* Directory View Tab */
        scanRoots.length === 0 ? (
          <div className="bg-surface-elevated rounded-lg p-8 text-center">
            <p className="text-content-muted mb-4">
              No directories added yet. Go to the "Folders" tab to add directories.
            </p>
          </div>
        ) : (
          <div className="flex-1 min-h-0">
            <DualPaneFileBrowser scanRoots={scanRoots} reveal={browserReveal} />
          </div>
        )
      )}

      {activeTab === 'duplicates' && (
        /* Duplicates View with Multi-View Tabs */
        <div className="flex-1 overflow-auto min-h-0">
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
            <DuplicatesView
              duplicates={duplicates}
              loading={dupsLoading}
              error={dupsError ? String(dupsError) : null}
              refresh={refreshDuplicates}
            />
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
                                          if (isInFolder(path, folder.folder_a)) {
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
                                          if (isInFolder(path, folder.folder_b)) {
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
        <div className="flex-1 overflow-auto min-h-0">
          <MissingMetadataView onCountChange={setMissingMetadataCount} />
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
    </div>
  );
}
