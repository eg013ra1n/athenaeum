import { Fragment, useCallback, useEffect, useState, type ReactNode } from 'react';
import { FolderPlus } from 'lucide-react';
import { api } from '../../api';
import { pickDirectory } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { useScanRootsWithAvailability } from '../../hooks/useTauri';
import { useScanProgressContext } from '../../contexts/ScanProgressContext';
import { useNotifications } from '../../contexts/NotificationContext';
import { listArchiveRoots, listArchivedFrameSets, deleteArchiveRoot, setDefaultArchiveRoot } from '../../api/archive';
import { ConfirmDialog } from '../ConfirmDialog';
import { AlertDialog } from '../AlertDialog';
import { ScanSummaryModal } from '../ScanSummaryModal';
import { FolderBrowserModal } from '../FolderBrowserModal';
import { FolderRail } from './FolderRail';
import { AddFolderDialog } from './AddFolderDialog';
import { MonitoredInspector } from './MonitoredInspector';
import { RoleInspector, RolePlaceholderInspector } from './RoleInspector';
import { ArchiveInspector } from './ArchiveInspector';
import { ROLE_META, type RailSelection, type RoleKind, type AddableKind } from './roleMeta';
import type { ArchiveRoot, ArchivedFrameSetSummary, ScanResult } from '../../types/helpers';
import type { FolderOverview, RelinkResult } from '../../types/models';

interface FoldersTabProps {
  /** Increments when the Transfers deep-link asks to focus Sync Incoming. */
  selectSyncIncomingToken: number;
}

/** Strip trailing separators so prefix comparisons are exact. */
const stripTrailing = (p: string) => p.replace(/[\\/]+$/, '');

export default function FoldersTab({ selectSyncIncomingToken }: FoldersTabProps) {
  const {
    scanRoots, loading: rootsLoading, error: rootsError, clearError: clearRootsError,
    deleteScanRoot, toggleDuplicatesFlag, toggleUniqueCameraFlag, toggleMonitorEnabled,
    relinkScanRoot, refresh: refreshScanRoots,
  } = useScanRootsWithAvailability();
  const { startRescanWithProgress, isScanning, activeScans } = useScanProgressContext();
  const { notify } = useNotifications();

  const [archiveRoots, setArchiveRoots] = useState<ArchiveRoot[]>([]);
  const [archivedSets, setArchivedSets] = useState<ArchivedFrameSetSummary[]>([]);
  const [overview, setOverview] = useState<FolderOverview | null>(null);
  const [missingCounts, setMissingCounts] = useState<Record<number, number>>({});
  /**
   * Effective calibration-library dir from the backend resolver (settings key
   * first, dedicated root only as fallback). `undefined` = not fetched yet, so
   * downstream consumers fall back to scan-root evidence instead of reading a
   * pre-load `null` as "role free".
   */
  const [calibrationDir, setCalibrationDir] = useState<string | null | undefined>(undefined);
  const [selection, setSelection] = useState<RailSelection | null>(null);
  const [addDialog, setAddDialog] = useState<{ open: boolean; preselect?: AddableKind }>({ open: false });
  const [scanResultMap, setScanResultMap] = useState<Record<number, ScanResult>>({});
  const [relinkingRootId, setRelinkingRootId] = useState<number | null>(null);
  /** Relink outcome tagged with the root that produced it — never shown on another row. */
  const [relinkResult, setRelinkResult] = useState<{ rootId: number; result: RelinkResult } | null>(null);
  const [relinkBrowserRootId, setRelinkBrowserRootId] = useState<number | null>(null);
  const [roleChangeBrowser, setRoleChangeBrowser] = useState<RoleKind | null>(null);
  const [scanSummary, setScanSummary] = useState<{ rootId: number; rootPath: string; missingFilesCount?: number } | null>(null);
  const [confirm, setConfirm] = useState<{ title: string; message: string; onConfirm: () => void; danger?: boolean } | null>(null);
  const [alert, setAlert] = useState<{ title: string; message: string; variant: 'error' | 'warning' | 'info' } | null>(null);

  const showAlert = (title: string, message: string) => setAlert({ title, message, variant: 'error' });

  const refreshAux = useCallback(async () => {
    try {
      const [roots, sets, ov, counts, calDir] = await Promise.all([
        listArchiveRoots(),
        listArchivedFrameSets(),
        api.invoke<FolderOverview>('get_folder_overview'),
        api.invoke<Record<number, number>>('get_missing_files_counts'),
        api.invoke<string | null>('get_calibration_library_dir'),
      ]);
      setArchiveRoots(roots);
      setArchivedSets(sets);
      setOverview(ov);
      setMissingCounts(counts);
      setCalibrationDir(calDir ?? null);
    } catch (e) {
      console.error('[FoldersTab] aux refresh failed:', e);
    }
  }, []);

  useEffect(() => { void refreshAux(); }, [refreshAux]);

  const refreshAll = useCallback(() => { void refreshScanRoots(); void refreshAux(); }, [refreshScanRoots, refreshAux]);

  /**
   * A "covered" calibration library: the effective dir is stored as a setting
   * because the folder sits inside a monitored root, so it has NO scan-root row
   * of its own. It is still an assigned role and must be visible + inspectable.
   */
  const coveredCalibrationDir: string | null =
    calibrationDir && calibrationDir.trim() !== '' && !scanRoots.some((r) => r.kind === 'calibration_library')
      ? calibrationDir
      : null;

  /** Longest monitored path that contains `dir` — the folder that scans it. */
  const coveringRootPath = useCallback((dir: string): string | null => {
    const target = stripTrailing(dir);
    let best: string | null = null;
    for (const r of scanRoots) {
      const p = stripTrailing(r.path);
      if (!p) continue;
      if (target === p || target.startsWith(`${p}/`) || target.startsWith(`${p}\\`)) {
        if (!best || best.length < p.length) best = p;
      }
    }
    return best;
  }, [scanRoots]);

  // Default selection: first monitored folder once loaded.
  useEffect(() => {
    if (selection || rootsLoading) return;
    const first = scanRoots.filter((r) => r.kind === 'normal')[0] ?? scanRoots[0];
    if (first?.id) setSelection({ type: 'scan', id: first.id });
  }, [scanRoots, rootsLoading, selection]);

  // Transfers deep-link → select Sync Incoming (root or placeholder).
  useEffect(() => {
    if (selectSyncIncomingToken === 0) return;
    const root = scanRoots.find((r) => r.kind === 'sync_incoming');
    setSelection(root?.id ? { type: 'scan', id: root.id } : { type: 'placeholder', kind: 'sync_incoming' });
  }, [selectSyncIncomingToken, scanRoots]);

  const scanPercent = useCallback((rootId: number) => {
    const p = activeScans.get(rootId)?.progress;
    return p ? p.percent : null;
  }, [activeScans]);

  const handleScan = async (rootId: number) => {
    const root = scanRoots.find((r) => r.id === rootId);
    if (!root) return;
    try {
      const result = await startRescanWithProgress(rootId, root.path);
      setScanResultMap((prev) => ({ ...prev, [rootId]: result }));
      setScanSummary({ rootId, rootPath: root.path, missingFilesCount: result.missingFilesCount });
      refreshAll();
    } catch (e) {
      console.error('[FoldersTab] scan failed:', e);
      showAlert('Scan failed', typeof e === 'string' ? e : String(e));
    }
  };

  const finishRelink = async (rootId: number, path: string) => {
    try {
      setRelinkingRootId(rootId);
      setRelinkResult(null);
      const result = await relinkScanRoot(rootId, path);
      setRelinkResult({ rootId, result });
      refreshAll();
    } catch (e) {
      console.error('[FoldersTab] relink failed:', e);
      const msg = typeof e === 'string' ? e : String(e);
      showAlert('Relink failed', msg);
      notify({ title: 'Relink failed', detail: msg, kind: 'files', tone: 'warning' });
    } finally {
      setRelinkingRootId(null);
    }
  };

  const handleRelink = async (rootId: number) => {
    if (!isTauri) { setRelinkResult(null); setRelinkBrowserRootId(rootId); return; }
    const picked = await pickDirectory();
    if (picked && typeof picked === 'string') await finishRelink(rootId, picked);
  };

  const handleRemoveScanRoot = (id: number) => setConfirm({
    title: 'Remove folder',
    message: 'Remove this folder from the catalog? Its catalog entries are forgotten; files on disk are never touched.',
    danger: true,
    onConfirm: async () => {
      try {
        await deleteScanRoot(id);
        setSelection(null);
        refreshAll();
      } catch (e) {
        console.error('[FoldersTab] remove failed:', e);
        clearRootsError();
        showAlert('Remove failed', typeof e === 'string' ? e : String(e));
      }
    },
  });

  const handleReleaseRole = (kind: RoleKind) => setConfirm({
    title: `Release ${ROLE_META[kind].label} role`,
    message: 'The folder stays monitored and files on disk are untouched. You can assign the role again at any time.',
    onConfirm: async () => {
      try {
        await api.invoke(ROLE_META[kind].clearCommand);
        setSelection(null);
        refreshAll();
      } catch (e) {
        console.error('[FoldersTab] release role failed:', e);
        showAlert('Release failed', typeof e === 'string' ? e : String(e));
      }
    },
  });

  const applyRoleChange = async (kind: RoleKind, path: string) => {
    try {
      if (kind === 'calibration_library') {
        await api.invoke<string>('switch_calibration_library_dir', { path });
      } else {
        await api.invoke(ROLE_META[kind].clearCommand);
        await api.invoke<string>(ROLE_META[kind].setCommand, { path });
      }
      refreshAll();
    } catch (e) {
      console.error('[FoldersTab] change role folder failed:', e);
      const msg = typeof e === 'string' ? e : String(e);
      showAlert('Change folder failed', msg);
      notify({ title: `Could not move ${ROLE_META[kind].label}`, detail: msg, kind: 'files', tone: 'warning', hasErrors: true });
    }
  };

  const handleChangeRoleFolder = (kind: RoleKind) => {
    const proceed = async () => {
      if (!isTauri) { setRoleChangeBrowser(kind); return; }
      const picked = await pickDirectory();
      if (picked && typeof picked === 'string') await applyRoleChange(kind, picked);
    };
    if (kind === 'calibration_library') {
      setConfirm({
        title: 'Move Calibration Library',
        message: 'The old library folder is removed from the catalog (its masters’ catalog entries are deleted; files on disk are kept). The new folder becomes the master destination in one step.',
        danger: true,
        onConfirm: proceed,
      });
    } else {
      void proceed();
    }
  };

  const handleDeleteArchiveRoot = (root: ArchiveRoot) => setConfirm({
    title: 'Remove archive folder',
    message: `Remove "${root.path}" from the configured list? Files in that folder are not deleted.`,
    danger: true,
    onConfirm: async () => {
      try {
        await deleteArchiveRoot(root.id);
        setSelection(null);
        refreshAll();
      } catch (e) {
        console.error('[FoldersTab] delete archive root failed:', e);
        showAlert('Remove failed', typeof e === 'string' ? e : String(e));
      }
    },
  });

  const empty = !rootsLoading && scanRoots.length === 0 && archiveRoots.length === 0;

  // ── Inspector resolution ──────────────────────────────────────────────────
  let inspector: ReactNode = null;
  if (selection?.type === 'scan') {
    const root = scanRoots.find((r) => r.id === selection.id);
    if (root) {
      const ov = overview?.scan_roots.find((s) => s.root_id === root.id);
      if (root.kind === 'normal') {
        inspector = (
          <MonitoredInspector
            root={root}
            overview={ov}
            missingCount={root.id ? (missingCounts[root.id] ?? 0) : 0}
            scanResult={root.id ? (scanResultMap[root.id] ?? null) : null}
            isScanning={root.id ? isScanning(root.id) : false}
            relinking={relinkingRootId === root.id}
            relinkResult={relinkResult?.rootId === root.id ? relinkResult.result : null}
            onScan={() => root.id && handleScan(root.id)}
            onRelink={() => root.id && handleRelink(root.id)}
            onShowScanDetails={() => root.id && setScanSummary({ rootId: root.id, rootPath: root.path })}
            onToggleDuplicates={(v) => root.id && toggleDuplicatesFlag(root.id, v)}
            onToggleUniqueCamera={(v) => { if (root.id) void toggleUniqueCameraFlag(root.id, v).catch((e) => console.error('[FoldersTab] unique-camera toggle failed:', e)); }}
            onToggleMonitor={(v) => { if (root.id) void toggleMonitorEnabled(root.id, v).catch((e) => console.error('[FoldersTab] monitor toggle failed:', e)); }}
            onRemove={() => root.id && handleRemoveScanRoot(root.id)}
            onMissingChanged={() => void refreshAux()}
          />
        );
      } else {
        const kind = root.kind as RoleKind;
        inspector = (
          <RoleInspector
            kind={kind}
            root={root}
            dir={root.path}
            coveredBy={null}
            overview={ov}
            isScanning={root.id ? isScanning(root.id) : false}
            onScan={() => root.id && handleScan(root.id)}
            onChangeFolder={() => handleChangeRoleFolder(kind)}
            onReleaseRole={() => handleReleaseRole(kind)}
            onToggleDuplicates={(v) => root.id && toggleDuplicatesFlag(root.id, v)}
            onToggleMonitor={(v) => { if (root.id) void toggleMonitorEnabled(root.id, v).catch((e) => console.error('[FoldersTab] monitor toggle failed:', e)); }}
          />
        );
      }
    }
  } else if (selection?.type === 'placeholder') {
    if (selection.kind === 'calibration_library' && coveredCalibrationDir) {
      // Covered library: no scan root of its own, so no scan controls and no
      // behavior switches — the covering monitored folder owns both. The role
      // itself is still fully manageable (change folder / release).
      inspector = (
        <RoleInspector
          kind="calibration_library"
          root={null}
          dir={coveredCalibrationDir}
          coveredBy={coveringRootPath(coveredCalibrationDir)}
          overview={undefined}
          isScanning={false}
          onScan={() => { /* no dedicated root — the covering folder scans it */ }}
          onChangeFolder={() => handleChangeRoleFolder('calibration_library')}
          onReleaseRole={() => handleReleaseRole('calibration_library')}
          onToggleDuplicates={() => { /* not rendered without a root */ }}
          onToggleMonitor={() => { /* not rendered without a root */ }}
        />
      );
    } else {
      inspector = <RolePlaceholderInspector kind={selection.kind} onSetUp={() => setAddDialog({ open: true, preselect: selection.kind })} />;
    }
  } else if (selection?.type === 'archive') {
    const root = archiveRoots.find((r) => r.id === selection.id);
    if (root) {
      inspector = (
        <ArchiveInspector
          root={root}
          archivedSets={archivedSets}
          totalZipBytes={overview?.archive_roots.find((a) => a.archive_root_id === root.id)?.total_zip_bytes ?? 0}
          onSetDefault={async () => {
            try {
              await setDefaultArchiveRoot(root.id);
              refreshAll();
            } catch (e) {
              console.error('[FoldersTab] set default archive root failed:', e);
              showAlert('Failed', typeof e === 'string' ? e : String(e));
            }
          }}
          onRemove={() => handleDeleteArchiveRoot(root)}
        />
      );
    }
  }

  /**
   * Remount key: per-selection local state inside an inspector (missing-file
   * panel, zip lists) must never survive a selection switch.
   */
  const selectionKey = selection
    ? selection.type === 'placeholder' ? `placeholder-${selection.kind}` : `${selection.type}-${selection.id}`
    : 'none';

  return (
    <div className="flex-1 min-h-0 flex flex-col">
      {rootsError && (
        <div className="mb-3 p-3 bg-error-muted border border-error/50 rounded-lg">
          <p className="text-error text-sm">Error loading folders: {String(rootsError)}</p>
        </div>
      )}

      {empty ? (
        <div className="flex-1 flex flex-col items-center justify-center text-center bg-surface-elevated rounded-lg">
          <FolderPlus size={40} className="text-info opacity-70" />
          <div className="mt-3 text-lg font-bold text-content">No folders yet</div>
          <p className="mt-1 max-w-sm text-sm text-content-muted">
            Add a folder with your FITS/XISF files to start cataloging. Roles and archive destinations can come later.
          </p>
          <button onClick={() => setAddDialog({ open: true })}
            className="mt-4 flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover text-surface font-semibold rounded-lg transition">
            <FolderPlus size={18} /> Add Folder
          </button>
        </div>
      ) : (
        <div className="flex-1 min-h-0 flex gap-3">
          <FolderRail
            scanRoots={scanRoots}
            archiveRoots={archiveRoots}
            archivedSets={archivedSets}
            overview={overview}
            missingCounts={missingCounts}
            coveredCalibrationDir={coveredCalibrationDir}
            selection={selection}
            onSelect={(sel) => { setSelection(sel); setRelinkResult(null); }}
            onAdd={(preselect) => setAddDialog({ open: true, preselect })}
            onRescan={handleScan}
            isScanning={isScanning}
            scanPercent={scanPercent}
          />
          <Fragment key={selectionKey}>
            {inspector ?? (
              <div className="flex-1 bg-surface-elevated rounded-lg flex items-center justify-center text-sm text-content-muted">
                Select a folder on the left.
              </div>
            )}
          </Fragment>
        </div>
      )}

      <AddFolderDialog
        isOpen={addDialog.open}
        preselect={addDialog.preselect}
        scanRoots={scanRoots}
        coveredCalibrationDir={calibrationDir === undefined ? undefined : (calibrationDir || null)}
        onClose={() => setAddDialog({ open: false })}
        onAdded={refreshAll}
      />

      <ConfirmDialog
        isOpen={confirm !== null}
        title={confirm?.title ?? ''}
        message={confirm?.message ?? ''}
        onConfirm={() => { const c = confirm; setConfirm(null); c?.onConfirm(); }}
        onCancel={() => setConfirm(null)}
        confirmDanger={confirm?.danger}
      />
      <AlertDialog
        isOpen={alert !== null}
        title={alert?.title ?? ''}
        message={alert?.message ?? ''}
        variant={alert?.variant ?? 'info'}
        onClose={() => setAlert(null)}
      />
      {scanSummary && scanResultMap[scanSummary.rootId] && (
        <ScanSummaryModal
          isOpen={true}
          onClose={() => setScanSummary(null)}
          scanResult={scanResultMap[scanSummary.rootId]}
          rootPath={scanSummary.rootPath}
          missingFilesCount={scanSummary.missingFilesCount}
        />
      )}
      {/* Web mode: relink + role-change directory browsers */}
      <FolderBrowserModal
        isOpen={relinkBrowserRootId !== null}
        scope="scan"
        onSelect={(path) => { const id = relinkBrowserRootId; setRelinkBrowserRootId(null); if (id != null) void finishRelink(id, path); }}
        onClose={() => setRelinkBrowserRootId(null)}
      />
      <FolderBrowserModal
        isOpen={roleChangeBrowser !== null}
        scope="scan"
        onSelect={(path) => { const k = roleChangeBrowser; setRoleChangeBrowser(null); if (k) void applyRoleChange(k, path); }}
        onClose={() => setRoleChangeBrowser(null)}
      />
    </div>
  );
}
