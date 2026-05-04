// Far-Manager-style dual-pane file browser.
//
// Phase 1 features:
//   - Two independent panes, each with its own current working directory.
//   - Tab key switches active pane (visible focus indicator).
//   - Click / Cmd+click / Shift+click selection in the active pane.
//   - F6 enqueues a Move operation: source = active pane selection, dest =
//     inactive pane's current directory. Runs on the global queue.
//   - Catalog search bar at top: type, click result -> active pane navigates
//     to that file's parent directory and pre-selects it.
//   - Cwd per pane persists across reloads via localStorage.
//
// Out of scope for Phase 1 (will land in Phase 2):
//   - Metadata mode (Ctrl+I), bulk-edit pane.
//   - Permanent delete (F8), Mkdir (F7), Rename (F2).
//   - Drag-and-drop between panes.
//   - Virtualized list (current implementation handles a few thousand rows fine).

import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from 'react';
import {
  Folder,
  File as FileIcon,
  ChevronRight,
  Home,
  ArrowUp,
  ArrowDownAZ,
  ArrowDownZA,
  CornerLeftUp,
  Loader2,
  FileText,
  Filter as FilterIcon,
  Pencil,
  RefreshCw,
  Sliders,
  X as XIcon,
} from 'lucide-react';
import { api } from '../../api';
import type { FileWithFrame, ScanRootWithAvailability } from '../../types/models';
import BlinkViewer from '../BlinkViewer';
import CatalogSearch from './CatalogSearch';
import MetadataPane from './MetadataPane';
import {
  type CatalogSearchHit,
  type DirectoryListing,
  type FileOpFinishedPayload,
  type FileOpProgressPayload,
  type PaneId,
  type PaneState,
  PARENT_ROW,
  findEnclosingScanRoot,
  formatBytes,
  getBasename,
  getClampedParent,
  getParentPath,
} from './types';

const LS_LEFT_CWD = 'dualpane.cwd.left';
const LS_RIGHT_CWD = 'dualpane.cwd.right';
const LS_LEFT_SORT = 'dualpane.sortDir.left';
const LS_RIGHT_SORT = 'dualpane.sortDir.right';

interface Props {
  scanRoots: ScanRootWithAvailability[];
}

interface State {
  panes: Record<PaneId, PaneState>;
  activePane: PaneId;
}

type Action =
  | { type: 'set_cwd'; pane: PaneId; cwd: string }
  | { type: 'set_selection'; pane: PaneId; selection: Set<string>; anchor?: string | null }
  | { type: 'toggle_select'; pane: PaneId; path: string; multi: boolean }
  | { type: 'set_active'; pane: PaneId }
  | { type: 'toggle_mode'; pane: PaneId }
  | { type: 'set_filter'; pane: PaneId; filterText: string }
  | { type: 'set_sort'; pane: PaneId; sortDir: 'asc' | 'desc' };

function reducer(state: State, action: Action): State {
  switch (action.type) {
    case 'set_cwd': {
      // Reset per-folder ephemeral state but keep persistent settings (sortDir).
      const next = {
        ...state.panes[action.pane],
        cwd: action.cwd,
        selection: new Set<string>(),
        anchor: null,
        filterText: '',
      };
      return { ...state, panes: { ...state.panes, [action.pane]: next } };
    }
    case 'set_selection': {
      const cur = state.panes[action.pane];
      const next = {
        ...cur,
        selection: action.selection,
        anchor: action.anchor !== undefined ? action.anchor : cur.anchor,
      };
      return { ...state, panes: { ...state.panes, [action.pane]: next } };
    }
    case 'toggle_select': {
      const sel = new Set(state.panes[action.pane].selection);
      if (action.multi) {
        if (sel.has(action.path)) sel.delete(action.path);
        else sel.add(action.path);
      } else {
        sel.clear();
        sel.add(action.path);
      }
      return {
        ...state,
        panes: { ...state.panes, [action.pane]: { ...state.panes[action.pane], selection: sel, anchor: action.path } },
      };
    }
    case 'set_active':
      return { ...state, activePane: action.pane };
    case 'toggle_mode': {
      const current = state.panes[action.pane];
      const nextMode = current.mode === 'files' ? 'metadata' : 'files';
      return {
        ...state,
        panes: { ...state.panes, [action.pane]: { ...current, mode: nextMode } },
      };
    }
    case 'set_filter': {
      // Filter changes invalidate selection (selected items might be hidden).
      return {
        ...state,
        panes: {
          ...state.panes,
          [action.pane]: {
            ...state.panes[action.pane],
            filterText: action.filterText,
            selection: new Set(),
            anchor: null,
          },
        },
      };
    }
    case 'set_sort': {
      return {
        ...state,
        panes: {
          ...state.panes,
          [action.pane]: { ...state.panes[action.pane], sortDir: action.sortDir },
        },
      };
    }
  }
}

function initialState(scanRoots: ScanRootWithAvailability[]): State {
  const defaultRoot = scanRoots[0]?.path ?? '';
  const left = (typeof window !== 'undefined' && localStorage.getItem(LS_LEFT_CWD)) || defaultRoot;
  const right = (typeof window !== 'undefined' && localStorage.getItem(LS_RIGHT_CWD)) || defaultRoot;
  const leftSort: 'asc' | 'desc' =
    (typeof window !== 'undefined' && (localStorage.getItem(LS_LEFT_SORT) as 'asc' | 'desc' | null)) || 'asc';
  const rightSort: 'asc' | 'desc' =
    (typeof window !== 'undefined' && (localStorage.getItem(LS_RIGHT_SORT) as 'asc' | 'desc' | null)) || 'asc';
  return {
    activePane: 'left',
    panes: {
      left: { mode: 'files', cwd: left, selection: new Set(), anchor: null, scrollTop: 0, filterText: '', sortDir: leftSort },
      right: { mode: 'files', cwd: right, selection: new Set(), anchor: null, scrollTop: 0, filterText: '', sortDir: rightSort },
    },
  };
}

interface ActiveOp {
  operationId: number;
  kind: string;
  current: number;
  total: number;
  stage: string;
  message: string;
  finished?: FileOpFinishedPayload;
}

export default function DualPaneFileBrowser({ scanRoots }: Props) {
  const [state, dispatch] = useReducer(reducer, scanRoots, initialState);
  const [listings, setListings] = useState<Record<PaneId, DirectoryListing | null>>({
    left: null,
    right: null,
  });
  const [loading, setLoading] = useState<Record<PaneId, boolean>>({ left: false, right: false });
  const [errors, setErrors] = useState<Record<PaneId, string | null>>({ left: null, right: null });
  const [activeOps, setActiveOps] = useState<Record<number, ActiveOp>>({});
  const [confirmMove, setConfirmMove] = useState<{ count: number; dest: string; sources: string[] } | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<{ count: number; bytes: number; targets: string[] } | null>(null);
  const [mkdirState, setMkdirState] = useState<{ destDir: string } | null>(null);
  const [renameState, setRenameState] = useState<{ oldPath: string; oldName: string } | null>(null);
  const [blinkFrames, setBlinkFrames] = useState<FileWithFrame[] | null>(null);
  const refreshTokenRef = useRef(0);
  /** Per-pane generation counter for in-flight directory loads. Each call to
   *  loadListing increments its pane's counter and captures the value; only
   *  the latest call's result is applied. Without this, a slow load that
   *  resolves AFTER a newer one would overwrite the fresh listing with stale
   *  rows — desyncing the breadcrumb (cwd) from the displayed contents. */
  const loadGenRef = useRef<Record<PaneId, number>>({ left: 0, right: 0 });

  // Persist cwd + sortDir per pane.
  useEffect(() => {
    try {
      localStorage.setItem(LS_LEFT_CWD, state.panes.left.cwd);
    } catch {}
  }, [state.panes.left.cwd]);
  useEffect(() => {
    try {
      localStorage.setItem(LS_RIGHT_CWD, state.panes.right.cwd);
    } catch {}
  }, [state.panes.right.cwd]);
  useEffect(() => {
    try {
      localStorage.setItem(LS_LEFT_SORT, state.panes.left.sortDir);
    } catch {}
  }, [state.panes.left.sortDir]);
  useEffect(() => {
    try {
      localStorage.setItem(LS_RIGHT_SORT, state.panes.right.sortDir);
    } catch {}
  }, [state.panes.right.sortDir]);

  // Load directory listing for a pane whenever its cwd changes (or refresh
  // fires). On a "missing" error: retry once after a short delay to absorb
  // transient FS races (e.g. macOS sometimes reports the parent of a
  // just-rmdir'd sibling as briefly inaccessible), then walk up to the
  // nearest existing ancestor, then fall back to any other scan root.
  const loadListing = useCallback(async (pane: PaneId, cwd: string) => {
    if (!cwd) {
      setListings(prev => ({ ...prev, [pane]: null }));
      return;
    }
    // Bump the generation BEFORE any awaits — this invalidates any older
    // in-flight load for the same pane.
    const gen = ++loadGenRef.current[pane];
    const isStale = () => loadGenRef.current[pane] !== gen;

    setLoading(prev => ({ ...prev, [pane]: true }));
    setErrors(prev => ({ ...prev, [pane]: null }));

    const attempt = async (): Promise<DirectoryListing> => {
      return api.invoke<DirectoryListing>('get_directory_contents', { directoryPath: cwd });
    };
    const isMissingMsg = (m: string) =>
      /does not exist/i.test(m) || /not found/i.test(m) || /No such/i.test(m);

    try {
      let listing: DirectoryListing;
      try {
        listing = await attempt();
      } catch (e) {
        if (isStale()) return;
        const msg = String(e);
        if (!isMissingMsg(msg)) throw e;
        // Brief retry — covers transient FS visibility after a sibling rmdir.
        console.warn(`pane ${pane} cwd "${cwd}" reported missing — retrying once`);
        await new Promise((r) => setTimeout(r, 120));
        if (isStale()) return;
        listing = await attempt();
      }
      if (isStale()) return;
      setListings(prev => ({ ...prev, [pane]: listing }));
    } catch (e) {
      if (isStale()) return;
      const msg = String(e);
      if (isMissingMsg(msg)) {
        // Walk up to the nearest existing ancestor inside the same scan
        // root, then fall back to any other configured scan root.
        const rootPaths = scanRoots.map(r => r.path);
        const enclosing = findEnclosingScanRoot(cwd, rootPaths);
        let recover: string | null = getClampedParent(cwd, rootPaths);
        if (!recover && enclosing && enclosing !== cwd) {
          recover = enclosing;
        }
        if (!recover) {
          const fallback = scanRoots.find(
            (r) => r.path !== cwd && r.path !== enclosing,
          );
          if (fallback) recover = fallback.path;
        }
        if (recover) {
          console.warn(`pane ${pane} cwd "${cwd}" missing; recovering to "${recover}"`);
          setErrors(prev => ({ ...prev, [pane]: null }));
          setLoading(prev => ({ ...prev, [pane]: false }));
          dispatch({ type: 'set_cwd', pane, cwd: recover });
          return;
        }
      }
      console.error(`get_directory_contents(${cwd}) failed:`, e);
      setErrors(prev => ({ ...prev, [pane]: msg }));
      setListings(prev => ({ ...prev, [pane]: null }));
    } finally {
      // Only the freshest load owns the loading flag.
      if (!isStale()) {
        setLoading(prev => ({ ...prev, [pane]: false }));
      }
    }
  }, [scanRoots]);

  /** Manual reload trigger — bound to the per-pane Refresh button. */
  const reloadPane = useCallback((pane: PaneId) => {
    loadListing(pane, state.panes[pane].cwd);
  }, [loadListing, state.panes]);

  useEffect(() => { loadListing('left', state.panes.left.cwd); }, [state.panes.left.cwd, loadListing]);
  useEffect(() => { loadListing('right', state.panes.right.cwd); }, [state.panes.right.cwd, loadListing]);

  // Force-reload both panes when an operation completes.
  const refreshBoth = useCallback(() => {
    refreshTokenRef.current += 1;
    loadListing('left', state.panes.left.cwd);
    loadListing('right', state.panes.right.cwd);
  }, [loadListing, state.panes.left.cwd, state.panes.right.cwd]);

  // Subscribe to file-op progress + finished events.
  useEffect(() => {
    let unlistenProgress: (() => void) | null = null;
    let unlistenFinished: (() => void) | null = null;
    api.listen<FileOpProgressPayload>('file-op-progress', (p) => {
      setActiveOps(prev => ({
        ...prev,
        [p.operationId]: {
          operationId: p.operationId,
          kind: p.kind,
          current: p.current,
          total: p.total,
          stage: p.stage,
          message: p.message,
          finished: prev[p.operationId]?.finished,
        },
      }));
    }).then(fn => { unlistenProgress = fn; });

    api.listen<FileOpFinishedPayload>('file-op-finished', (p) => {
      setActiveOps(prev => {
        const existing = prev[p.operation_id];
        if (!existing) return prev;
        return { ...prev, [p.operation_id]: { ...existing, finished: p } };
      });
      // Refresh both panes — the listing may have changed.
      refreshBoth();
      // Auto-dismiss after a brief delay.
      window.setTimeout(() => {
        setActiveOps(prev => {
          const next = { ...prev };
          delete next[p.operation_id];
          return next;
        });
      }, 1500);
    }).then(fn => { unlistenFinished = fn; });

    return () => {
      unlistenProgress?.();
      unlistenFinished?.();
    };
  }, [refreshBoth]);

  // Move handler: F6 — collects sources from active-pane selection, dest from
  // inactive-pane cwd. Drag-drop uses the same confirm dialog with explicit
  // sources/dest from the drop event.
  const enqueueMove = useCallback(async () => {
    const active = state.panes[state.activePane];
    const inactive = state.panes[state.activePane === 'left' ? 'right' : 'left'];
    const sources = Array.from(active.selection);
    if (sources.length === 0) return;
    if (!inactive.cwd) return;
    setConfirmMove({ count: sources.length, dest: inactive.cwd, sources });
  }, [state.activePane, state.panes]);

  const performMove = useCallback(async () => {
    if (!confirmMove) return;
    const { sources, dest } = confirmMove;
    setConfirmMove(null);
    try {
      const opId = await api.invoke<number>('enqueue_move_operation', {
        sources,
        destDir: dest,
      });
      setActiveOps(prev => ({
        ...prev,
        [opId]: {
          operationId: opId,
          kind: 'move',
          current: 0,
          total: sources.length,
          stage: 'pending',
          message: `Queued: ${sources.length} items`,
        },
      }));
      dispatch({ type: 'set_selection', pane: state.activePane, selection: new Set() });
    } catch (e) {
      console.error('enqueue_move_operation failed:', e);
      alert(`Move failed to enqueue: ${e}`);
    }
  }, [confirmMove, state.activePane]);

  // Delete handler: F8.
  const enqueueDelete = useCallback(() => {
    const active = state.panes[state.activePane];
    const sel = Array.from(active.selection);
    if (sel.length === 0) return;
    const listing = listings[state.activePane];
    let bytes = 0;
    if (listing) {
      for (const f of listing.files) {
        if (active.selection.has(f.file.path)) bytes += f.file.size;
      }
    }
    setConfirmDelete({ count: sel.length, bytes, targets: sel });
  }, [listings, state.activePane, state.panes]);

  const performDelete = useCallback(async () => {
    if (!confirmDelete) return;
    const active = state.panes[state.activePane];
    const targets = Array.from(active.selection);
    setConfirmDelete(null);
    try {
      const opId = await api.invoke<number>('enqueue_delete_operation', { targets });
      setActiveOps(prev => ({
        ...prev,
        [opId]: {
          operationId: opId,
          kind: 'delete',
          current: 0,
          total: targets.length,
          stage: 'pending',
          message: `Queued: delete ${targets.length} items`,
        },
      }));
      dispatch({ type: 'set_selection', pane: state.activePane, selection: new Set() });
    } catch (e) {
      console.error('enqueue_delete_operation failed:', e);
      alert(`Delete failed to enqueue: ${e}`);
    }
  }, [confirmDelete, state.activePane, state.panes]);

  // Mkdir handler: F7.
  const openMkdir = useCallback(() => {
    const active = state.panes[state.activePane];
    if (!active.cwd) return;
    setMkdirState({ destDir: active.cwd });
  }, [state.activePane, state.panes]);

  const performMkdir = useCallback(async (name: string) => {
    if (!mkdirState) return;
    const trimmed = name.trim();
    if (!trimmed || trimmed.includes('/') || trimmed.includes('\\')) {
      alert('Folder name must be a single path component.');
      return;
    }
    setMkdirState(null);
    const newPath = mkdirState.destDir.endsWith('/') || mkdirState.destDir.endsWith('\\')
      ? mkdirState.destDir + trimmed
      : mkdirState.destDir + (mkdirState.destDir.includes('\\') ? '\\' : '/') + trimmed;
    try {
      await api.invoke('mkdir_in_scan_root', { path: newPath });
      // Refresh both panes (the dir might be visible in either).
      refreshBoth();
    } catch (e) {
      console.error('mkdir_in_scan_root failed:', e);
      alert(`Mkdir failed: ${e}`);
    }
  }, [mkdirState, refreshBoth]);

  // Rename handler: F2.
  const openRename = useCallback(() => {
    const active = state.panes[state.activePane];
    const sel = Array.from(active.selection);
    if (sel.length !== 1) return;
    const oldPath = sel[0];
    const sep = oldPath.includes('\\') ? '\\' : '/';
    const oldName = oldPath.split(sep).filter(Boolean).pop() ?? '';
    setRenameState({ oldPath, oldName });
  }, [state.activePane, state.panes]);

  const performRename = useCallback(async (newName: string) => {
    if (!renameState) return;
    const trimmed = newName.trim();
    if (!trimmed || trimmed === renameState.oldName) {
      setRenameState(null);
      return;
    }
    if (trimmed.includes('/') || trimmed.includes('\\')) {
      alert('Name must be a single path component.');
      return;
    }
    const oldPath = renameState.oldPath;
    setRenameState(null);
    try {
      await api.invoke('rename_path', { oldPath, newName: trimmed });
      refreshBoth();
      dispatch({ type: 'set_selection', pane: state.activePane, selection: new Set() });
    } catch (e) {
      console.error('rename_path failed:', e);
      alert(`Rename failed: ${e}`);
    }
  }, [renameState, refreshBoth, state.activePane]);

  // Catalog search reveal: navigate the active pane to the hit's parent dir,
  // and select the file.
  const onSearchReveal = useCallback((hit: CatalogSearchHit) => {
    const parent = getParentPath(hit.path);
    if (!parent) return;
    dispatch({ type: 'set_cwd', pane: state.activePane, cwd: parent });
    // Pre-select the file once the listing loads. We schedule it after the
    // cwd update so the loadListing effect fires first.
    window.setTimeout(() => {
      dispatch({
        type: 'set_selection',
        pane: state.activePane,
        selection: new Set([hit.path]),
      });
    }, 0);
  }, [state.activePane]);

  /** Apply the per-pane filter + name sort to a raw listing.
   *  Folders and files are sorted independently. The sort is on basename
   *  (locale-aware, case-insensitive). The filter is a case-insensitive
   *  substring match on the basename of each entry. */
  const visibleListing = useCallback((pane: PaneId): DirectoryListing | null => {
    const raw = listings[pane];
    if (!raw) return null;
    const { filterText, sortDir } = state.panes[pane];
    const needle = filterText.trim().toLowerCase();
    const sortMul = sortDir === 'asc' ? 1 : -1;

    const subBasename = (p: string) => getBasename(p).toLowerCase();
    let subdirectories = needle
      ? raw.subdirectories.filter((p) => subBasename(p).includes(needle))
      : raw.subdirectories.slice();
    subdirectories.sort((a, b) => subBasename(a).localeCompare(subBasename(b)) * sortMul);

    let files = needle
      ? raw.files.filter((f) => f.file.filename.toLowerCase().includes(needle))
      : raw.files.slice();
    files.sort((a, b) => a.file.filename.toLowerCase().localeCompare(b.file.filename.toLowerCase()) * sortMul);

    return { subdirectories, files };
  }, [listings, state.panes]);

  // Build the keyboard navigation order for a pane: synthetic parent row
  // (if not at scan root) + visible subdirectories + visible files.
  const keyboardOrder = useCallback((pane: PaneId): string[] => {
    const listing = visibleListing(pane);
    if (!listing) return [];
    const cwd = state.panes[pane].cwd;
    const rootPaths = scanRoots.map((r) => r.path);
    const parent = getClampedParent(cwd, rootPaths);
    const order: string[] = [];
    if (parent !== null) order.push(PARENT_ROW);
    for (const sub of listing.subdirectories) order.push(sub);
    for (const f of listing.files) order.push(f.file.path);
    return order;
  }, [visibleListing, scanRoots, state.panes]);

  /** Open the blink viewer with the FITS/XISF files currently selected in the
   *  active pane (skipping folders, sidecars, etc). No-op if nothing eligible. */
  const openBlink = useCallback(() => {
    const listing = visibleListing(state.activePane);
    if (!listing) return;
    const sel = state.panes[state.activePane].selection;
    if (sel.size === 0) return;
    const frames = listing.files.filter(
      (f) => sel.has(f.file.path) && (f.file.format === 'FITS' || f.file.format === 'XISF'),
    );
    if (frames.length === 0) return;
    setBlinkFrames(frames);
  }, [state.activePane, state.panes, visibleListing]);

  /** Activate the row at `path` — folders/parent navigate, files no-op (Phase 2). */
  const activatePath = useCallback((path: string) => {
    const active = state.panes[state.activePane];
    if (path === PARENT_ROW) {
      const rootPaths = scanRoots.map((r) => r.path);
      const parent = getClampedParent(active.cwd, rootPaths);
      if (parent) dispatch({ type: 'set_cwd', pane: state.activePane, cwd: parent });
      return;
    }
    // Always check the raw listing for membership — even filtered-out folders
    // shouldn't be activatable, but selection clears on filter so this is a
    // belt-and-braces guard. visibleListing captures both filter + sort.
    const listing = visibleListing(state.activePane);
    if (listing && listing.subdirectories.includes(path)) {
      dispatch({ type: 'set_cwd', pane: state.activePane, cwd: path });
    }
  }, [visibleListing, scanRoots, state.activePane, state.panes]);

  // True when ANY confirm/prompt dialog is open. Pane shortcuts (F-keys,
  // Tab, Cmd+I, Cmd+A, Space, Enter, arrows, Backspace) are suppressed in
  // that case so they can't accidentally fire underneath the dialog.
  const isModalOpen = !!(confirmMove || confirmDelete || mkdirState || renameState || blinkFrames);

  // Modal-only key handler: ESC cancels any dialog; Y/Enter confirms simple
  // Move/Delete dialogs. Mkdir/Rename prompts handle their own Enter/Escape
  // via the input element so they can keep accepting text input.
  useEffect(() => {
    if (!isModalOpen) return;
    function onModalKey(e: KeyboardEvent) {
      const target = e.target as HTMLElement | null;
      const isEditable = target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA');
      if (e.key === 'Escape') {
        e.preventDefault();
        if (confirmMove) setConfirmMove(null);
        else if (confirmDelete) setConfirmDelete(null);
        else if (mkdirState) setMkdirState(null);
        else if (renameState) setRenameState(null);
        else if (blinkFrames) setBlinkFrames(null);
        return;
      }
      // 'Y' / Enter confirm only for simple click-to-confirm dialogs (no
      // text input). Skip when the user is typing in an input.
      if (!isEditable && (e.key === 'y' || e.key === 'Y' || e.key === 'Enter')) {
        if (confirmMove) {
          e.preventDefault();
          void performMove();
        } else if (confirmDelete) {
          e.preventDefault();
          void performDelete();
        }
      }
    }
    window.addEventListener('keydown', onModalKey);
    return () => window.removeEventListener('keydown', onModalKey);
  }, [isModalOpen, confirmMove, confirmDelete, mkdirState, renameState, blinkFrames, performMove, performDelete]);

  // Keyboard handler: Tab switches active pane, F6 = move.
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      // Suppress all pane shortcuts while a dialog is open — only the
      // modal-only handler above runs in that mode.
      if (isModalOpen) return;
      // Ignore when focus is in an input/textarea (search field, dialogs).
      const target = e.target as HTMLElement | null;
      if (target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA')) return;
      if (e.key === 'Tab') {
        e.preventDefault();
        dispatch({ type: 'set_active', pane: state.activePane === 'left' ? 'right' : 'left' });
      } else if (e.key === 'F6') {
        e.preventDefault();
        void enqueueMove();
      } else if (e.key === 'F8' || (e.key === 'Delete' && (e.metaKey || e.ctrlKey))) {
        e.preventDefault();
        enqueueDelete();
      } else if (e.key === 'F7') {
        e.preventDefault();
        openMkdir();
      } else if (e.key === 'F2') {
        e.preventDefault();
        openRename();
      } else if (e.key === 'i' && (e.metaKey || e.ctrlKey)) {
        // Toggle the OTHER pane into metadata mode so the active pane keeps
        // its selection visible while the opposite side shows the metadata
        // for those selected files.
        e.preventDefault();
        const other: PaneId = state.activePane === 'left' ? 'right' : 'left';
        dispatch({ type: 'toggle_mode', pane: other });
      } else if (e.key === 'a' && (e.metaKey || e.ctrlKey)) {
        // Select all visible entries (filter applies — Cmd+A respects the
        // current filter).
        const listing = visibleListing(state.activePane);
        if (!listing) return;
        e.preventDefault();
        const all = new Set<string>([
          ...listing.subdirectories,
          ...listing.files.map((f) => f.file.path),
        ]);
        dispatch({ type: 'set_selection', pane: state.activePane, selection: all });
      } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        const order = keyboardOrder(state.activePane);
        if (order.length === 0) return;
        e.preventDefault();
        const cur = state.panes[state.activePane].anchor;
        const idx = cur ? order.indexOf(cur) : -1;
        let next: number;
        if (idx < 0) {
          next = e.key === 'ArrowDown' ? 0 : order.length - 1;
        } else {
          next = e.key === 'ArrowDown'
            ? Math.min(idx + 1, order.length - 1)
            : Math.max(idx - 1, 0);
        }
        const nextPath = order[next];
        if (nextPath === PARENT_ROW) {
          // Cursor on parent row — clear selection (parent is not selectable).
          dispatch({ type: 'set_selection', pane: state.activePane, selection: new Set(), anchor: PARENT_ROW });
        } else if (e.shiftKey && cur && cur !== PARENT_ROW) {
          // Shift+arrow: extend range from anchor to next position.
          const anchorIdx = order.indexOf(cur);
          if (anchorIdx >= 0) {
            const [lo, hi] = anchorIdx < next ? [anchorIdx, next] : [next, anchorIdx];
            const range = new Set(
              order.slice(lo, hi + 1).filter((p) => p !== PARENT_ROW),
            );
            dispatch({ type: 'set_selection', pane: state.activePane, selection: range, anchor: cur });
          }
        } else {
          dispatch({ type: 'toggle_select', pane: state.activePane, path: nextPath, multi: false });
        }
      } else if (e.key === 'Enter') {
        const cur = state.panes[state.activePane].anchor;
        if (!cur) return;
        e.preventDefault();
        activatePath(cur);
      } else if (e.key === ' ' || e.code === 'Space') {
        // Space → blink the active pane's selection (FITS/XISF only).
        // Always preventDefault so the page doesn't scroll.
        e.preventDefault();
        openBlink();
      } else if (e.key === 'Backspace') {
        // Backspace = navigate up one level (clamped to scan root).
        const active = state.panes[state.activePane];
        const rootPaths = scanRoots.map((r) => r.path);
        const parent = getClampedParent(active.cwd, rootPaths);
        if (parent) {
          e.preventDefault();
          dispatch({ type: 'set_cwd', pane: state.activePane, cwd: parent });
        }
      }
    }
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [isModalOpen, enqueueMove, enqueueDelete, openMkdir, openRename, openBlink, state.activePane, state.panes, visibleListing, keyboardOrder, activatePath, scanRoots]);

  // Status bar metrics for the active pane.
  const activePaneListing = listings[state.activePane];
  const activePaneSelection = state.panes[state.activePane].selection;
  const activeSelectionStats = useMemo(() => {
    if (!activePaneListing) return { count: 0, bytes: 0 };
    let bytes = 0;
    for (const f of activePaneListing.files) {
      if (activePaneSelection.has(f.file.path)) bytes += f.file.size;
    }
    return { count: activePaneSelection.size, bytes };
  }, [activePaneListing, activePaneSelection]);

  return (
    // Fill the parent flex cell. FileManager's wrapper is `h-full flex flex-col
    // min-h-0`, and the browse-tab branch wraps this in `flex-1 min-h-0`, so
    // h-full here stretches the dual-pane to the exact remaining viewport
    // height — no fragile vh calc, no leftover space below.
    <div className="flex flex-col gap-3 h-full min-h-0">
      {/* Search */}
      <CatalogSearch onReveal={onSearchReveal} />

      {/* Two panes — flex-fill the remaining height; each pane scrolls on its own */}
      <div className="grid grid-cols-2 gap-3 flex-1 min-h-0">
        <Pane
          id="left"
          state={state.panes.left}
          listing={visibleListing('left')}
          rawListing={listings.left}
          loading={loading.left}
          error={errors.left}
          isActive={state.activePane === 'left'}
          otherListing={visibleListing('right')}
          otherSelection={state.panes.right.selection}
          onActivate={() => dispatch({ type: 'set_active', pane: 'left' })}
          onCwd={(cwd) => dispatch({ type: 'set_cwd', pane: 'left', cwd })}
          onToggleSelect={(path, multi) =>
            dispatch({ type: 'toggle_select', pane: 'left', path, multi })
          }
          onSetSelection={(sel, anchor) =>
            dispatch({ type: 'set_selection', pane: 'left', selection: sel, anchor })
          }
          onToggleMode={() => dispatch({ type: 'toggle_mode', pane: 'left' })}
          onSetFilter={(filterText) => dispatch({ type: 'set_filter', pane: 'left', filterText })}
          onSetSort={(sortDir) => dispatch({ type: 'set_sort', pane: 'left', sortDir })}
          onReload={() => reloadPane('left')}
          onMetaSaved={refreshBoth}
          scanRoots={scanRoots}
        />
        <Pane
          id="right"
          state={state.panes.right}
          listing={visibleListing('right')}
          rawListing={listings.right}
          loading={loading.right}
          error={errors.right}
          isActive={state.activePane === 'right'}
          otherListing={visibleListing('left')}
          otherSelection={state.panes.left.selection}
          onActivate={() => dispatch({ type: 'set_active', pane: 'right' })}
          onCwd={(cwd) => dispatch({ type: 'set_cwd', pane: 'right', cwd })}
          onToggleSelect={(path, multi) =>
            dispatch({ type: 'toggle_select', pane: 'right', path, multi })
          }
          onSetSelection={(sel, anchor) =>
            dispatch({ type: 'set_selection', pane: 'right', selection: sel, anchor })
          }
          onToggleMode={() => dispatch({ type: 'toggle_mode', pane: 'right' })}
          onSetFilter={(filterText) => dispatch({ type: 'set_filter', pane: 'right', filterText })}
          onSetSort={(sortDir) => dispatch({ type: 'set_sort', pane: 'right', sortDir })}
          onReload={() => reloadPane('right')}
          onMetaSaved={refreshBoth}
          scanRoots={scanRoots}
        />
      </div>

      {/* Active operations */}
      {Object.values(activeOps).length > 0 && (
        <div className="space-y-2">
          {Object.values(activeOps).map((op) => (
            <OpProgressRow key={op.operationId} op={op} />
          ))}
        </div>
      )}

      {/* Status bar */}
      <div className="flex items-center justify-between border-t border-border pt-2 text-xs text-content-muted">
        <div>
          Active: <span className="text-content font-mono">{state.activePane}</span>
          {' · '}
          {activeSelectionStats.count > 0
            ? `${activeSelectionStats.count} selected (${formatBytes(activeSelectionStats.bytes)})`
            : 'No selection'}
          {activePaneListing && ` · ${activePaneListing.files.length} files`}
        </div>
        <div className="font-mono">
          Tab · ↑↓ · Enter · ⌫ Up · Space Blink · ⌘A All · Shift+click range · F2 Rename · F6 Move · F7 Mkdir · F8 Delete · ⌘I Meta
        </div>
      </div>

      {/* Move confirmation */}
      {confirmMove && (
        <Modal onClose={() => setConfirmMove(null)} title={`Move ${confirmMove.count} items?`}>
          <ItemNameList paths={confirmMove.sources} />
          <div className="text-sm text-content-muted mb-4 break-all">
            Destination: <span className="font-mono">{confirmMove.dest}</span>
          </div>
          <ModalButtons
            onCancel={() => setConfirmMove(null)}
            primaryLabel="Move"
            primaryClass="bg-accent text-white hover:bg-accent-hover"
            onPrimary={performMove}
          />
        </Modal>
      )}

      {/* Delete confirmation */}
      {confirmDelete && (
        <Modal
          onClose={() => setConfirmDelete(null)}
          title={`Permanently delete ${confirmDelete.count} items?`}
        >
          <ItemNameList paths={confirmDelete.targets} />
          <div className="text-sm text-content-muted mb-4">
            {formatBytes(confirmDelete.bytes)} will be removed from disk and the catalog. This
            cannot be undone.
          </div>
          <ModalButtons
            onCancel={() => setConfirmDelete(null)}
            primaryLabel="Delete"
            primaryClass="bg-red-600 text-white hover:bg-red-700"
            onPrimary={performDelete}
          />
        </Modal>
      )}

      {/* Mkdir prompt */}
      {mkdirState && (
        <PromptModal
          title="New folder"
          description={
            <span>
              Create inside: <span className="font-mono break-all">{mkdirState.destDir}</span>
            </span>
          }
          placeholder="folder name"
          initialValue=""
          onCancel={() => setMkdirState(null)}
          onConfirm={performMkdir}
          primaryLabel="Create"
        />
      )}

      {/* Blink viewer — mounts as a full-screen overlay over the dual-pane. */}
      {blinkFrames && (
        <BlinkViewer
          frames={blinkFrames}
          onClose={() => setBlinkFrames(null)}
        />
      )}

      {/* Rename prompt */}
      {renameState && (
        <PromptModal
          title="Rename"
          description={
            <span>
              Current name: <span className="font-mono break-all">{renameState.oldName}</span>
            </span>
          }
          placeholder="new name"
          initialValue={renameState.oldName}
          onCancel={() => setRenameState(null)}
          onConfirm={performRename}
          primaryLabel="Rename"
        />
      )}
    </div>
  );
}

// Generic confirm modal helpers — used by all dialogs in this view.

/** Renders the first few item basenames inline ("foo.fit, bar.fit, baz.fit
 *  and 12 more"). Helps the user verify they selected the right things
 *  before confirming a move/delete. */
function ItemNameList({ paths, max = 3 }: { paths: string[]; max?: number }) {
  if (paths.length === 0) return null;
  const names = paths.slice(0, max).map(getBasename);
  const extra = paths.length - names.length;
  return (
    <div className="text-sm text-content mb-3 break-all">
      <div className="text-xs text-content-muted mb-1">Items:</div>
      <div className="font-mono">
        {names.join(', ')}
        {extra > 0 && (
          <span className="text-content-muted">
            {' '}and {extra} more
          </span>
        )}
      </div>
    </div>
  );
}

function Modal({ onClose, title, children }: { onClose: () => void; title: string; children: React.ReactNode }) {
  return (
    <div
      className="fixed inset-0 z-30 flex items-center justify-center bg-black/50"
      onClick={onClose}
    >
      <div
        className="bg-surface-elevated border border-border rounded-md p-5 max-w-md w-full shadow-xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="font-semibold text-content mb-2">{title}</div>
        {children}
      </div>
    </div>
  );
}

function ModalButtons({
  onCancel,
  primaryLabel,
  primaryClass,
  onPrimary,
}: {
  onCancel: () => void;
  primaryLabel: string;
  primaryClass: string;
  onPrimary: () => void;
}) {
  return (
    <div className="flex gap-2 justify-end">
      <button
        onClick={onCancel}
        className="px-3 py-1.5 rounded border border-border hover:bg-surface text-sm"
      >
        Cancel
      </button>
      <button onClick={onPrimary} className={`px-3 py-1.5 rounded text-sm ${primaryClass}`}>
        {primaryLabel}
      </button>
    </div>
  );
}

function PromptModal({
  title,
  description,
  placeholder,
  initialValue,
  onCancel,
  onConfirm,
  primaryLabel,
}: {
  title: string;
  description: React.ReactNode;
  placeholder: string;
  initialValue: string;
  onCancel: () => void;
  onConfirm: (value: string) => void;
  primaryLabel: string;
}) {
  const [value, setValue] = useState(initialValue);
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);
  return (
    <Modal onClose={onCancel} title={title}>
      <div className="text-sm text-content-muted mb-3">{description}</div>
      <input
        ref={inputRef}
        value={value}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') onConfirm(value);
          else if (e.key === 'Escape') onCancel();
        }}
        placeholder={placeholder}
        className="w-full px-3 py-2 mb-4 rounded border border-border bg-surface text-content outline-none focus:border-accent text-sm font-mono"
      />
      <ModalButtons
        onCancel={onCancel}
        primaryLabel={primaryLabel}
        primaryClass="bg-accent text-white hover:bg-accent-hover"
        onPrimary={() => onConfirm(value)}
      />
    </Modal>
  );
}

interface PaneProps {
  id: PaneId;
  state: PaneState;
  /** Filter+sort applied — what the file list actually renders. */
  listing: DirectoryListing | null;
  /** Pre-filter listing — used to show "x of N" totals in the filter row. */
  rawListing: DirectoryListing | null;
  loading: boolean;
  error: string | null;
  isActive: boolean;
  /** Other pane's listing — supplies metadata when this pane is in metadata mode. */
  otherListing: DirectoryListing | null;
  otherSelection: Set<string>;
  onActivate: () => void;
  onCwd: (cwd: string) => void;
  onToggleSelect: (path: string, multi: boolean) => void;
  onSetSelection: (selection: Set<string>, anchor?: string | null) => void;
  onToggleMode: () => void;
  onSetFilter: (filterText: string) => void;
  onSetSort: (sortDir: 'asc' | 'desc') => void;
  onReload: () => void;
  onMetaSaved: () => void;
  scanRoots: ScanRootWithAvailability[];
}

function Pane({
  id,
  state,
  listing,
  rawListing,
  loading,
  error,
  isActive,
  otherListing,
  otherSelection,
  onActivate,
  onCwd,
  onToggleSelect,
  onSetSelection,
  onToggleMode,
  onSetFilter,
  onSetSort,
  onReload,
  onMetaSaved,
  scanRoots,
}: PaneProps) {
  // The pane is clamped to the scan root that contains its current cwd.
  // Going DEEPER is fine; going ABOVE the enclosing scan root is not.
  const enclosingRoot = useMemo(
    () => findEnclosingScanRoot(state.cwd, scanRoots.map(r => r.path)),
    [state.cwd, scanRoots],
  );
  const atRoot = !!enclosingRoot && enclosingRoot === state.cwd;
  const onCwdUp = useCallback(() => {
    if (atRoot || !enclosingRoot) return;
    const parent = getParentPath(state.cwd);
    if (!parent || parent === state.cwd) return;
    // Don't let parent traversal escape the enclosing scan root.
    const parentRoot = findEnclosingScanRoot(parent, scanRoots.map(r => r.path));
    if (parentRoot !== enclosingRoot) return;
    onCwd(parent);
  }, [atRoot, enclosingRoot, state.cwd, scanRoots, onCwd]);

  const isMetadataMode = state.mode === 'metadata';
  const parentPath = useMemo(
    () => getClampedParent(state.cwd, scanRoots.map((r) => r.path)),
    [state.cwd, scanRoots],
  );
  return (
    <div
      onClick={onActivate}
      // `select-none` blocks accidental text selection from clicks/double-clicks
      // on row labels and breadcrumb chips. Form controls inside (filter input,
      // metadata-pane fields, dialogs) override this via user-agent styling so
      // typing/text-selection inside inputs still works normally.
      className={`select-none flex flex-col bg-surface-elevated border rounded-md overflow-hidden ${
        isActive
          ? 'border-amber-400 shadow-[0_0_0_2px_rgba(251,191,36,0.45)]'
          : 'border-border'
      }`}
    >
      {/* Toolbar — collapses navigation widgets when in metadata mode */}
      <div className="flex items-center gap-2 px-2 py-1.5 border-b border-border bg-surface text-xs">
        {!isMetadataMode ? (
          <>
            <button
              onClick={onCwdUp}
              disabled={!state.cwd || atRoot}
              className="p-1 hover:bg-surface-hover rounded disabled:opacity-50"
              title={atRoot ? 'At scan root' : 'Up'}
            >
              <ArrowUp size={14} />
            </button>
            <button
              onClick={(e) => { e.stopPropagation(); onReload(); }}
              className="p-1 hover:bg-surface-hover rounded text-content-muted"
              title="Refresh this pane"
            >
              <RefreshCw size={14} />
            </button>
            <select
              value={enclosingRoot ?? ''}
              onChange={(e) => onCwd(e.target.value)}
              className="bg-transparent text-content text-xs outline-none border-0 cursor-pointer"
              title="Jump to scan root"
            >
              <option value="" disabled>
                Jump to root…
              </option>
              {scanRoots.map((r) => (
                <option key={r.path} value={r.path}>
                  {r.path}
                </option>
              ))}
            </select>
            <Breadcrumb path={state.cwd} root={enclosingRoot} onNavigate={onCwd} />
          </>
        ) : (
          <div className="flex items-center gap-2 text-content font-mono">
            <Sliders size={14} className="text-accent" />
            Metadata editor
            <span className="text-content-muted">— editing selection from {id === 'left' ? 'right' : 'left'} pane</span>
          </div>
        )}
        {loading && <Loader2 size={14} className="animate-spin text-content-muted ml-auto" />}
        <button
          onClick={(e) => { e.stopPropagation(); onToggleMode(); }}
          className={`ml-auto p-1 rounded ${isMetadataMode ? 'bg-accent text-white' : 'hover:bg-surface-hover text-content-muted'}`}
          title={isMetadataMode ? 'Show files (Ctrl+I)' : 'Show metadata editor (Ctrl+I)'}
        >
          {isMetadataMode ? <FileText size={14} /> : <Sliders size={14} />}
        </button>
      </div>

      {/* Filter + sort row — only visible in files mode. */}
      {!isMetadataMode && (
        <div className="flex items-center gap-2 px-2 py-1 border-b border-border bg-surface/60 text-xs">
          <FilterIcon size={12} className="text-content-muted flex-shrink-0" />
          <input
            value={state.filterText}
            onChange={(e) => onSetFilter(e.target.value)}
            placeholder="Filter this folder…"
            className="flex-1 bg-transparent outline-none text-content text-xs placeholder:text-content-muted min-w-0"
          />
          {state.filterText && (
            <button
              onClick={(e) => { e.stopPropagation(); onSetFilter(''); }}
              className="p-0.5 rounded hover:bg-surface-hover text-content-muted"
              title="Clear filter"
            >
              <XIcon size={12} />
            </button>
          )}
          {state.filterText && rawListing && listing && (
            <span className="text-content-muted whitespace-nowrap">
              {listing.subdirectories.length + listing.files.length}
              /{rawListing.subdirectories.length + rawListing.files.length}
            </span>
          )}
          <button
            onClick={(e) => { e.stopPropagation(); onSetSort(state.sortDir === 'asc' ? 'desc' : 'asc'); }}
            className="flex items-center gap-1 px-1.5 py-0.5 rounded hover:bg-surface-hover text-content-muted"
            title={`Sort ${state.sortDir === 'asc' ? 'A→Z (click for Z→A)' : 'Z→A (click for A→Z)'}`}
          >
            {state.sortDir === 'asc' ? <ArrowDownAZ size={12} /> : <ArrowDownZA size={12} />}
          </button>
        </div>
      )}

      {/* Body — files or metadata. */}
      <div className="flex-1 overflow-hidden text-sm">
        {error && (
          <div className="p-3 text-red-400 text-xs">{error}</div>
        )}
        {!error && isMetadataMode && (
          <MetadataPane
            otherListing={otherListing}
            otherSelection={otherSelection}
            onSaved={onMetaSaved}
          />
        )}
        {!error && !isMetadataMode && listing && (
          <div className="h-full overflow-y-auto">
            <FileList
              paneId={id}
              listing={listing}
              selection={state.selection}
              anchor={state.anchor}
              parentPath={parentPath}
              onCwd={onCwd}
              onToggleSelect={onToggleSelect}
              onSetSelection={onSetSelection}
            />
          </div>
        )}
        {!error && !isMetadataMode && !listing && !loading && (
          <div className="p-3 text-content-muted text-xs">No directory loaded.</div>
        )}
      </div>
    </div>
  );
}

interface FileListProps {
  paneId: PaneId;
  listing: DirectoryListing;
  selection: Set<string>;
  anchor: string | null;
  /** When set, renders a synthetic parent-folder row at the top of the list. */
  parentPath: string | null;
  onCwd: (cwd: string) => void;
  onToggleSelect: (path: string, multi: boolean) => void;
  onSetSelection: (selection: Set<string>, anchor?: string | null) => void;
}

function FileList({ paneId, listing, selection, anchor, parentPath, onCwd, onToggleSelect, onSetSelection }: FileListProps) {
  const listRef = useRef<HTMLDivElement>(null);
  // Keep the focused (anchor) row visible — fires when cursor crosses the
  // viewport edge from arrow-key navigation.
  useEffect(() => {
    if (!anchor || !listRef.current) return;
    const safe = anchor.replace(/"/g, '\\"');
    const el = listRef.current.querySelector(`[data-row-key="${safe}"]`);
    if (el && 'scrollIntoView' in el) {
      (el as HTMLElement).scrollIntoView({ block: 'nearest' });
    }
  }, [anchor]);
  // Visual order of all rows — folders first, then files. Used by shift+click
  // to compute the inclusive range from the anchor to the clicked path.
  const orderedPaths: string[] = [
    ...listing.subdirectories,
    ...listing.files.map((f) => f.file.path),
  ];

  const handleClick = (e: React.MouseEvent, path: string) => {
    if (e.shiftKey && anchor) {
      const a = orderedPaths.indexOf(anchor);
      const b = orderedPaths.indexOf(path);
      if (a >= 0 && b >= 0) {
        const [lo, hi] = a < b ? [a, b] : [b, a];
        const range = new Set(orderedPaths.slice(lo, hi + 1));
        // Preserve anchor so subsequent shift-clicks extend from the same point.
        onSetSelection(range, anchor);
        return;
      }
    }
    const multi = e.metaKey || e.ctrlKey;
    onToggleSelect(path, multi);
  };

  /** Manual double-click detector for the parent row.
   *
   *  React's onDoubleClick is unreliable here: the row's onClick mutates
   *  state (anchor → PARENT_ROW) which causes a re-render between the two
   *  clicks. The browser's native double-click recognition can race with
   *  React's reconciliation and occasionally miss the second click. A
   *  ref-based timing check is bulletproof. */
  const parentClickRef = useRef<number>(0);
  const onParentClick = () => {
    if (!parentPath) return;
    const now = Date.now();
    if (now - parentClickRef.current < 400) {
      // Treat as double-click → navigate.
      parentClickRef.current = 0;
      onCwd(parentPath);
    } else {
      // First click within window → just focus the row.
      parentClickRef.current = now;
      onSetSelection(new Set(), PARENT_ROW);
    }
  };

  return (
    <div ref={listRef} role="listbox" aria-label={`${paneId} pane file list`}>
      {parentPath && (
        <div data-row-key={PARENT_ROW}>
          <button
            // Manual double-click detection — see onParentClick. React's
            // onDoubleClick races with state-driven re-renders here.
            onClick={onParentClick}
            title={`Double-click to go up to ${parentPath}`}
            className={`w-full flex items-center gap-2 px-2 py-1 text-left ${
              anchor === PARENT_ROW ? 'bg-accent/30 text-accent' : 'hover:bg-surface-hover text-content-muted'
            }`}
          >
            <CornerLeftUp size={14} className={anchor === PARENT_ROW ? 'text-accent' : 'text-content-muted'} />
            <span className="font-mono">..</span>
            <span className="text-xs text-content-muted truncate">{parentPath}</span>
          </button>
        </div>
      )}
      {listing.subdirectories.map((subPath) => {
        const isSel = selection.has(subPath);
        return (
          <div key={subPath} data-row-key={subPath}>
            <button
              onClick={(e) => handleClick(e, subPath)}
              onDoubleClick={() => onCwd(subPath)}
              className={`w-full flex items-center gap-2 px-2 py-1 text-left ${
                isSel ? 'bg-accent/30 text-accent' : 'hover:bg-surface-hover text-content'
              }`}
            >
              <Folder size={14} className={isSel ? 'text-accent' : 'text-yellow-500'} />
              <span className="truncate font-mono">{getBasename(subPath)}</span>
            </button>
          </div>
        );
      })}
      {listing.files.map((f) => {
        const isSel = selection.has(f.file.path);
        const isOverridden = f.frame?.override_ === true;
        return (
          <button
            key={f.file.path}
            data-row-key={f.file.path}
            onClick={(e) => handleClick(e, f.file.path)}
            className={`w-full flex items-center gap-2 px-2 py-1 text-left ${
              isSel ? 'bg-accent/30 text-accent' : 'hover:bg-surface-hover text-content'
            }`}
          >
            <FileIcon size={14} className={isSel ? 'text-accent' : 'text-content-muted'} />
            <span className="truncate font-mono">{f.file.filename}</span>
            {isOverridden && (
              <Pencil
                size={11}
                className="flex-shrink-0 text-amber-400"
                aria-label="Custom metadata applied"
              >
                <title>Custom metadata applied — open metadata pane (⌘I) to compare with original</title>
              </Pencil>
            )}
            <span className="ml-auto text-xs text-content-muted">{formatBytes(f.file.size)}</span>
          </button>
        );
      })}
      {listing.subdirectories.length === 0 && listing.files.length === 0 && (
        <div className="p-3 text-content-muted text-xs italic">Empty directory.</div>
      )}
    </div>
  );
}

interface BreadcrumbProps {
  path: string;
  /** Scan root that bounds the breadcrumb. Only segments BELOW this root are
   *  shown — the root itself is already displayed in the "Jump to root"
   *  selector beside the breadcrumb. When cwd equals the root, the breadcrumb
   *  is empty. */
  root: string | null;
  onNavigate: (cwd: string) => void;
}

function Breadcrumb({ path, root, onNavigate }: BreadcrumbProps) {
  if (!path || !root) return null;
  const sep = path.includes('\\') ? '\\' : '/';
  const allParts = path.split(/[/\\]/).filter(Boolean);
  const rootParts = root.split(/[/\\]/).filter(Boolean);
  // Only show segments that come AFTER the scan root.
  const descendantParts = allParts.slice(rootParts.length);
  const atRoot = descendantParts.length === 0;
  const segments = descendantParts.map((part, i) => {
    // Reconstruct the absolute path for this segment.
    const fullParts = [...rootParts, ...descendantParts.slice(0, i + 1)];
    const target = (sep === '/' ? '/' : '') + fullParts.join(sep);
    return { name: part, path: target };
  });
  return (
    <nav className="flex items-center gap-1 overflow-x-auto text-content-muted text-xs flex-1 min-w-0" aria-label="path">
      <button
        onClick={() => onNavigate(root)}
        disabled={atRoot}
        className="flex items-center hover:text-content disabled:opacity-50 disabled:cursor-default flex-shrink-0"
        title={atRoot ? root : `Back to ${root}`}
      >
        <Home size={12} />
      </button>
      {segments.map((seg, i) => (
        <span key={i} className="flex items-center gap-1 flex-shrink-0">
          <ChevronRight size={10} />
          <button
            onClick={() => onNavigate(seg.path)}
            className="hover:text-content truncate max-w-[140px]"
            title={seg.path}
          >
            {seg.name}
          </button>
        </span>
      ))}
    </nav>
  );
}

interface OpProgressRowProps {
  op: ActiveOp;
}

function OpProgressRow({ op }: OpProgressRowProps) {
  const percent = op.total > 0 ? Math.round((op.current / op.total) * 100) : 0;
  const finished = op.finished;
  const tone = finished?.outcome === 'completed'
    ? 'border-green-600 bg-green-600/10'
    : finished?.outcome === 'cancelled'
      ? 'border-yellow-600 bg-yellow-600/10'
      : finished?.outcome === 'failed'
        ? 'border-red-600 bg-red-600/10'
        : 'border-border bg-surface-elevated';
  return (
    <div className={`border rounded-md p-2 ${tone}`}>
      <div className="flex items-center gap-2 text-xs">
        <span className="font-mono">{op.kind}#{op.operationId}</span>
        <span className="text-content-muted">{op.message}</span>
        {!finished && (
          <span className="ml-auto text-content-muted">
            {op.current}/{op.total} ({percent}%)
          </span>
        )}
        {finished && (
          <span className="ml-auto font-semibold">
            {finished.outcome.toUpperCase()}
          </span>
        )}
      </div>
      {!finished && (
        <div className="h-1.5 mt-1 bg-surface rounded-full overflow-hidden">
          <div
            className="h-full bg-accent transition-all"
            style={{ width: `${percent}%` }}
          />
        </div>
      )}
    </div>
  );
}
