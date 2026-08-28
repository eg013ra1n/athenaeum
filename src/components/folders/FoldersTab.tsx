import { Fragment, useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { FolderPlus, X } from 'lucide-react';
import { api } from '../../api';
import { pickDirectory } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { useScanRootsWithAvailability } from '../../hooks/useTauri';
import { useScanProgressContext } from '../../contexts/ScanProgressContext';
import { useContentIndex } from '../../hooks/useContentIndex';
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
import { ROLE_META, isRoleKind, type RailSelection, type RoleKind, type AddableKind } from './roleMeta';
import type { ArchiveRoot, ArchivedFrameSetSummary, ScanResult } from '../../types/helpers';
import type { FolderOverview, RelinkResult } from '../../types/models';

interface FoldersTabProps {
  /** Increments when the Transfers deep-link asks to focus Sync Incoming. */
  selectSyncIncomingToken: number;
  /**
   * Fired after any mutation kicks off a model refresh, so a parent holding its
   * own scan-root copy (FileManager, which feeds the Browse Files tab) can
   * resync. Optional — this component is self-sufficient without it.
   */
  onRootsChanged?: () => void;
  /**
   * Fired once the deep-link token has actually been consumed, so the parent can
   * lower it back to 0. Required for correctness across tab switches: this
   * component unmounts when the user leaves the Folders tab and the latch ref
   * below resets with it, so a token left raised would re-select Sync Incoming
   * on every return. Optional — standalone use just never lowers the token.
   */
  onSyncIncomingHandled?: () => void;
}

/** Strip trailing separators so prefix comparisons are exact. */
const stripTrailing = (p: string) => p.replace(/[\\/]+$/, '');

export default function FoldersTab({ selectSyncIncomingToken, onRootsChanged, onSyncIncomingHandled }: FoldersTabProps) {
  const {
    scanRoots, loading: rootsLoading, error: rootsError, clearError: clearRootsError,
    deleteScanRoot, toggleDuplicatesFlag, toggleUniqueCameraFlag, toggleMonitorEnabled,
    relinkScanRoot, refresh: refreshScanRoots,
  } = useScanRootsWithAvailability();
  const { startRescanWithProgress, isScanning, activeScans } = useScanProgressContext();
  const { notify } = useNotifications();
  // Content index — status + manual start for the rail's build button.
  const contentIndex = useContentIndex();

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
  /**
   * First `refreshAux` has settled. Gates the "No folders yet" screen: archive
   * roots and the calibration dir arrive on a separate async pass from the scan
   * roots, so their pre-load empties would otherwise read as "nothing
   * configured" for one paint — the empty state offers Add Folder to someone
   * who already has folders.
   */
  const [auxLoaded, setAuxLoaded] = useState(false);
  const [selection, setSelection] = useState<RailSelection | null>(null);
  const [addDialog, setAddDialog] = useState<{ open: boolean; preselect?: AddableKind }>({ open: false });
  const [scanResultMap, setScanResultMap] = useState<Record<number, ScanResult>>({});
  const [relinkingRootId, setRelinkingRootId] = useState<number | null>(null);
  /** Root whose removal is in flight. Removing a big root walks every file,
   *  frame and calibration link it owns, so the click needs to look like it
   *  did something — without this the UI just blinks and a failure (the whole
   *  delete is one transaction, so it either lands or doesn't) is the only
   *  visible outcome. */
  const [removingRootId, setRemovingRootId] = useState<number | null>(null);
  /** Relink outcome tagged with the root that produced it — never shown on another row. */
  const [relinkResult, setRelinkResult] = useState<{ rootId: number; result: RelinkResult } | null>(null);
  const [relinkBrowserRootId, setRelinkBrowserRootId] = useState<number | null>(null);
  const [roleChangeBrowser, setRoleChangeBrowser] = useState<RoleKind | null>(null);
  const [scanSummary, setScanSummary] = useState<{ rootId: number; rootPath: string; missingFilesCount?: number } | null>(null);
  const [confirm, setConfirm] = useState<{ title: string; message: string; onConfirm: () => void; danger?: boolean } | null>(null);
  const [alert, setAlert] = useState<{ title: string; message: string; variant: 'error' | 'warning' | 'info' } | null>(null);

  const showAlert = (title: string, message: string) => setAlert({ title, message, variant: 'error' });

  /**
   * A failed behavior toggle rejects AND leaves the hook's `error` set, which
   * paints the global "Error loading folders" banner — wrong noun, wrong cause,
   * and the switch silently snapped back. Clear that banner and report the real
   * failure through the same alert the remove-root path uses.
   */
  const reportToggleFailure = (what: string, e: unknown) => {
    console.error(`[FoldersTab] ${what} toggle failed:`, e);
    clearRootsError();
    showAlert('Could not change setting', typeof e === 'string' ? e : String(e));
  };

  /**
   * Five independent reads, applied independently: one failing call must not
   * blank the others' state — an emptied archive-root list would fake the
   * "No folders yet" screen, and a dropped calibration dir would hide an
   * assigned role. Every rejection is logged; none is shown as a banner.
   */
  const refreshAux = useCallback(async () => {
    const [roots, sets, ov, counts, calDir] = await Promise.allSettled([
      listArchiveRoots(),
      listArchivedFrameSets(),
      api.invoke<FolderOverview>('get_folder_overview'),
      api.invoke<Record<number, number>>('get_missing_files_counts'),
      api.invoke<string | null>(ROLE_META.calibration_library.getCommand),
    ]);
    if (roots.status === 'fulfilled') setArchiveRoots(roots.value);
    else console.error('[FoldersTab] list_archive_roots failed:', roots.reason);
    if (sets.status === 'fulfilled') setArchivedSets(sets.value);
    else console.error('[FoldersTab] list_archived_frame_sets failed:', sets.reason);
    if (ov.status === 'fulfilled') setOverview(ov.value);
    else console.error('[FoldersTab] get_folder_overview failed:', ov.reason);
    if (counts.status === 'fulfilled') setMissingCounts(counts.value);
    else console.error('[FoldersTab] get_missing_files_counts failed:', counts.reason);
    if (calDir.status === 'fulfilled') setCalibrationDir(calDir.value ?? null);
    else console.error('[FoldersTab] get_calibration_library_dir failed:', calDir.reason);
    // `allSettled` always resolves, so this marks "the pass ran", not "the pass
    // succeeded" — a backend that keeps failing must still reach a usable
    // screen rather than hang on a spinner-shaped nothing.
    setAuxLoaded(true);
  }, []);

  useEffect(() => { void refreshAux(); }, [refreshAux]);

  const refreshAll = useCallback(() => {
    void refreshScanRoots();
    void refreshAux();
    // Fire-and-forget: the parent's copy is a separate hook instance with its
    // own fetch, so nothing here depends on when it settles.
    onRootsChanged?.();
  }, [refreshScanRoots, refreshAux, onRootsChanged]);

  /**
   * The resolver's answer, normalized once: `undefined` = not fetched yet (fall
   * back to scan-root evidence), `null` = unset or explicitly cleared. Both the
   * rail derivation below and the Add-dialog prop read this, so "empty" means
   * the same thing in both places.
   */
  const effectiveCalibrationDir: string | null | undefined =
    calibrationDir === undefined ? undefined : (calibrationDir?.trim() ? calibrationDir : null);

  /**
   * A "covered" calibration library: the effective dir is stored as a setting
   * because the folder sits inside a monitored root, so it has NO scan-root row
   * of its own. It is still an assigned role and must be visible + inspectable.
   */
  const coveredCalibrationDir: string | null =
    effectiveCalibrationDir && !scanRoots.some((r) => r.kind === 'calibration_library')
      ? effectiveCalibrationDir
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

  /**
   * Transfers deep-link → select Sync Incoming (root or placeholder). Latched:
   * `scanRoots` is a fresh array on every refresh/toggle, so without the latch
   * this effect would yank the user's selection back on each of them. The latch
   * is set only once a row (or, with roots loaded, the placeholder) was actually
   * selected — a token arriving before the roots do still resolves on arrival.
   * `onSyncIncomingHandled` fires at exactly those two latch points so the
   * parent can lower the token; the ref keeps guarding repeat runs (StrictMode
   * double-fire, refresh churn) in the window before that lands.
   */
  const handledSyncToken = useRef(0);
  useEffect(() => {
    if (selectSyncIncomingToken === 0) {
      // Token consumed (or never raised) — re-arm, so a later deep-link that
      // reuses the value 1 still reads as new within this same mount.
      handledSyncToken.current = 0;
      return;
    }
    if (handledSyncToken.current === selectSyncIncomingToken) return;
    const root = scanRoots.find((r) => r.kind === 'sync_incoming');
    if (root?.id) {
      setSelection({ type: 'scan', id: root.id });
      handledSyncToken.current = selectSyncIncomingToken;
      onSyncIncomingHandled?.();
      return;
    }
    // Still loading: the row may exist but isn't known yet — don't latch, and
    // leave the token raised so it resolves once the roots arrive.
    if (rootsLoading) return;
    setSelection({ type: 'placeholder', kind: 'sync_incoming' });
    handledSyncToken.current = selectSyncIncomingToken;
    onSyncIncomingHandled?.();
  }, [selectSyncIncomingToken, scanRoots, rootsLoading, onSyncIncomingHandled]);

  /**
   * Reconcile the held selection against the refreshed model:
   * - a placeholder whose role now HAS a root → promote to that row (role setup
   *   completed while its placeholder was selected);
   * - a placeholder for the calibration library is otherwise left alone — this
   *   branch only ever promotes or leaves as-is, so the covered case (no
   *   scan-root row for that kind) is preserved by the absence of a matching
   *   root, not by consulting `coveredCalibrationDir` here;
   * - a scan/archive id that no longer exists → drop to `null` so the
   *   default-selection effect re-picks (removals, dedicated→covered moves).
   */
  useEffect(() => {
    if (!selection) return;
    if (selection.type === 'placeholder') {
      const promoted = scanRoots.find((r) => r.kind === selection.kind);
      if (promoted?.id) setSelection({ type: 'scan', id: promoted.id });
      return;
    }
    if (rootsLoading) return;
    const stillThere = selection.type === 'scan'
      ? scanRoots.some((r) => r.id === selection.id)
      : archiveRoots.some((r) => r.id === selection.id);
    if (!stillThere) setSelection(null);
  }, [scanRoots, archiveRoots, rootsLoading, selection]);

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
      // The scan just added rows the index has not hashed. The job only re-arms
      // itself when sync is set up or content grouping is on, so without this
      // the rail's pending count would stay stale on exactly the setup where
      // the button is the only way to build it.
      contentIndex.refresh();
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
    try {
      const picked = await pickDirectory();
      if (picked && typeof picked === 'string') await finishRelink(rootId, picked);
    } catch (e) {
      console.error('[FoldersTab] relink folder pick failed:', e);
      showAlert('Could not open the folder picker', typeof e === 'string' ? e : String(e));
    }
  };

  const handleRemoveScanRoot = (id: number) => setConfirm({
    title: 'Remove folder',
    message: 'Remove this folder from the catalog? Its catalog entries are forgotten; files on disk are never touched.',
    danger: true,
    onConfirm: async () => {
      setRemovingRootId(id);
      try {
        await deleteScanRoot(id);
        setSelection(null);
        refreshAll();
      } catch (e) {
        console.error('[FoldersTab] remove failed:', e);
        clearRootsError();
        showAlert('Remove failed', typeof e === 'string' ? e : String(e));
      } finally {
        setRemovingRootId(null);
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
    const meta = ROLE_META[kind];
    const errText = (e: unknown) => (typeof e === 'string' ? e : String(e));
    try {
      if (kind === 'calibration_library') {
        // One backend call — it either lands whole or changes nothing.
        try {
          await api.invoke<string>('switch_calibration_library_dir', { path });
        } catch (e) {
          console.error('[FoldersTab] change role folder failed:', e);
          const msg = errText(e);
          showAlert('Change folder failed', msg);
          notify({ title: `Could not move ${meta.label}`, detail: msg, kind: 'files', tone: 'warning', hasErrors: true });
        }
        return;
      }
      // Sync/collab move is release-then-assign, and the two failures mean
      // different things (spec §7). A failed release changed nothing; a failed
      // assign left the role UNASSIGNED — say so instead of implying the old
      // folder still holds it.
      try {
        await api.invoke(meta.clearCommand);
      } catch (e) {
        console.error('[FoldersTab] release before role move failed:', e);
        const msg = errText(e);
        showAlert('Change folder failed', `${meta.label} was not moved — ${msg}`);
        notify({ title: `Could not move ${meta.label}`, detail: msg, kind: 'files', tone: 'warning', hasErrors: true });
        return;
      }
      try {
        await api.invoke<string>(meta.setCommand, { path });
      } catch (e) {
        console.error('[FoldersTab] assign after release failed:', e);
        const detail = `The role was released, but assigning the new folder failed: ${errText(e)}. The old folder remains monitored; assign a folder again from its row.`;
        showAlert(`${meta.label} role released`, detail);
        notify({ title: `${meta.label} is now unassigned`, detail, kind: 'files', tone: 'warning', hasErrors: true });
      }
    } finally {
      // Every outcome — moved, half-applied, refused — must reach the rail.
      refreshAll();
    }
  };

  const handleChangeRoleFolder = (kind: RoleKind) => {
    const proceed = async () => {
      if (!isTauri) { setRoleChangeBrowser(kind); return; }
      try {
        const picked = await pickDirectory();
        if (picked && typeof picked === 'string') await applyRoleChange(kind, picked);
      } catch (e) {
        console.error('[FoldersTab] role-change folder pick failed:', e);
        showAlert('Could not open the folder picker', typeof e === 'string' ? e : String(e));
      }
    };
    if (kind === 'calibration_library') {
      // The purge only happens when there IS a dedicated library root to
      // remove. A covered (settings-only) library has no row of its own, so the
      // move is a pure setting rewrite — warning about deleted catalog entries
      // there would describe something the backend will not do.
      const dedicated = scanRoots.some((r) => r.kind === 'calibration_library');
      setConfirm({
        title: 'Move Calibration Library',
        message: dedicated
          ? 'The old library folder is removed from the catalog (its masters’ catalog entries are deleted; files on disk are kept). The new folder becomes the master destination in one step.'
          : 'The Calibration Library destination moves to the new folder. Files on disk are not touched.',
        danger: dedicated,
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

  // A covered calibration library is a configured folder with no scan-root row
  // of its own — counting only rows would show "No folders yet" over it.
  const empty = auxLoaded && !rootsLoading && scanRoots.length === 0
    && archiveRoots.length === 0 && !coveredCalibrationDir;

  // ── Inspector resolution ──────────────────────────────────────────────────
  let inspector: ReactNode = null;
  if (selection?.type === 'scan') {
    const root = scanRoots.find((r) => r.id === selection.id);
    if (root) {
      const ov = overview?.scan_roots.find((s) => s.root_id === root.id);
      // `kind` is an open DB string: an unknown one (version downgrade) gets the
      // generic monitored inspector rather than a `ROLE_META[kind]` deref.
      if (root.kind === 'normal' || !isRoleKind(root.kind)) {
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
            onToggleDuplicates={(v) => { if (root.id) void toggleDuplicatesFlag(root.id, v).catch((e) => reportToggleFailure('duplicates', e)); }}
            onToggleUniqueCamera={(v) => { if (root.id) void toggleUniqueCameraFlag(root.id, v).catch((e) => reportToggleFailure('unique-camera', e)); }}
            onToggleMonitor={(v) => { if (root.id) void toggleMonitorEnabled(root.id, v).catch((e) => reportToggleFailure('monitor', e)); }}
            onRemove={() => root.id && handleRemoveScanRoot(root.id)}
            removing={removingRootId === root.id}
            onMissingChanged={() => void refreshAux()}
          />
        );
      } else {
        // Narrowed by `isRoleKind` above — safe to index `ROLE_META` downstream.
        const kind = root.kind;
        inspector = (
          <RoleInspector
            kind={kind}
            root={root}
            dir={root.path}
            coveredBy={null}
            overview={ov}
            isScanning={root.id ? isScanning(root.id) : false}
            relinking={relinkingRootId === root.id}
            relinkResult={relinkResult?.rootId === root.id ? relinkResult.result : null}
            onScan={() => root.id && handleScan(root.id)}
            onRelink={() => root.id && handleRelink(root.id)}
            onChangeFolder={() => handleChangeRoleFolder(kind)}
            onReleaseRole={() => handleReleaseRole(kind)}
            onToggleDuplicates={(v) => { if (root.id) void toggleDuplicatesFlag(root.id, v).catch((e) => reportToggleFailure('duplicates', e)); }}
            onToggleMonitor={(v) => { if (root.id) void toggleMonitorEnabled(root.id, v).catch((e) => reportToggleFailure('monitor', e)); }}
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
        <div className="mb-3 p-3 bg-error-muted border border-error/50 rounded-lg flex items-start gap-2">
          <p className="flex-1 min-w-0 text-error text-sm">Error loading folders: {String(rootsError)}</p>
          <button
            onClick={clearRootsError}
            aria-label="Dismiss"
            title="Dismiss"
            className="shrink-0 p-1 rounded hover:bg-surface-hover text-content-muted transition"
          >
            <X size={14} />
          </button>
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
            contentIndex={{
              pending: contentIndex.status?.pending ?? null,
              running: contentIndex.running,
              starting: contentIndex.starting,
              onBuild: contentIndex.start,
            }}
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
        coveredCalibrationDir={effectiveCalibrationDir}
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
