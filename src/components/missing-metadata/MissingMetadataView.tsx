import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { Pencil, X } from 'lucide-react';
import { api } from '../../api';
import { ImageTypeValues } from '../../types/helpers';
import type { ExcludedFrameRow, FileWithFrame, MissingMetadataRow } from '../../types/models';

import { MissingMetadataToolbar, type FilterChip, type EligibleCounts } from './MissingMetadataToolbar';
import { MissingMetadataTable, computeMissingFlags } from './MissingMetadataTable';
import { SetCameraModal } from './modals/SetCameraModal';
import { SetDateModal } from './modals/SetDateModal';
import { SetFrameTypeModal } from './modals/SetFrameTypeModal';
import { PlateSolveBatchPanel, type PlateSolveBatchPanelHandle } from '../plate-solve/PlateSolveBatchPanel';
import { FillObjectsPanel, type FillObjectsPanelHandle } from '../plate-solve/FillObjectsPanel';
import BlinkViewer from '../BlinkViewer';
import { ConfirmDialog } from '../ConfirmDialog';
import { useBulkMoveToBlackHole } from '../../hooks/useBulkMoveToBlackHole';
import MetadataPane from '../dualpane/MetadataPane';
import type { DirectoryListing } from '../dualpane/types';

type ModalKind = 'camera' | 'date' | 'type' | null;

export type MissingMetadataMode = 'missing' | 'excluded';

interface MissingMetadataViewProps {
  /** Called with the total number of rows currently loaded, so the parent
      tab button can show a count badge. Passed `null` while loading. */
  onCountChange?: (count: number | null) => void;
  /** Data source. `'missing'` (default) = File Manager → Missing Metadata
      tab. `'excluded'` = Objects → Excluded Frames page; fetches from the
      `excluded_frames` table joined to file/frame metadata so the same
      Plate-Solve / Set Camera / Set Date / Set Type / Black Hole toolset
      applies to the excluded list. */
  mode?: MissingMetadataMode;
  /** Optional caller-supplied extra column appended after the Frame Type
      column (e.g. an exclusion-reason badge in `excluded` mode). */
  extraColumn?: { header: string; render: (row: MissingMetadataRow) => React.ReactNode };
  /** Optional caller-supplied buttons rendered between the standard action
      group and the selection counter in the toolbar. Either a static node or
      a render function that receives the current selection + a refresh
      callback so buttons can act on what's selected. */
  extraActions?:
    | React.ReactNode
    | ((ctx: ExtraActionsContext) => React.ReactNode);
  /** Called after every successful refresh of the row list (e.g. so the
      caller can react to selection changes that came from internal edits). */
  onAfterRefresh?: (rows: MissingMetadataRow[]) => void;
  /** Called whenever the row selection changes, with the currently-selected
      MissingMetadataRow objects (file + frame). Lets the parent host a
      side panel that mirrors the selection (e.g. the MetadataPane on the
      Excluded Frames page). */
  onSelectionChange?: (rows: MissingMetadataRow[]) => void;
  /** When this prop changes value the view triggers a fresh load. Used by
      callers that need to reload after an external write. */
  refreshNonce?: number;
  /** When true, hide the row of filter chips in the toolbar (Coordinates /
      Object / Date / Camera / Type). Useful for callers whose data source
      doesn't benefit from per-flag filtering (e.g. the Excluded Frames
      page where every row is already an exclusion). */
  hideFilterChips?: boolean;
  /** Caller-specific hook fired after a successful save in the side-panel
      MetadataPane, with the file IDs that were just saved. Lets the caller
      run page-specific cleanup (e.g. the Excluded Frames page removes
      those file IDs from the excluded_frames table). The view then reloads
      its data automatically. */
  onMetadataSaved?: (savedFileIds: number[]) => void | Promise<void>;
}

export interface ExtraActionsContext {
  selectedCount: number;
  selectedFrameIds: number[];
  selectedFileIds: number[];
  refresh: () => Promise<void>;
}

export const MissingMetadataView: React.FC<MissingMetadataViewProps> = ({
  onCountChange,
  mode = 'missing',
  extraColumn,
  extraActions,
  onAfterRefresh,
  onSelectionChange,
  refreshNonce,
  hideFilterChips,
  onMetadataSaved,
}) => {
  const [allRows, setAllRows] = useState<MissingMetadataRow[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [activeChips, setActiveChips] = useState<Set<FilterChip>>(new Set());
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set());

  const [showBlink, setShowBlink] = useState(false);
  const [openModal, setOpenModal] = useState<ModalKind>(null);
  const [blackHoleConfirmOpen, setBlackHoleConfirmOpen] = useState(false);
  const bulkBlackHole = useBulkMoveToBlackHole();
  const navigate = useNavigate();

  // Side-panel MetadataPane toggle. When on, the table loses any caller-
  // supplied extra column and the bulk Set X buttons (the editor covers
  // both), and a MetadataPane appears on the right scoped to the current
  // selection. Same component the dual-pane file browser hosts — single
  // source of truth for bulk metadata editing.
  const [editorOpen, setEditorOpen] = useState(false);

  // Hand off a clicked filename to the FileManager / dual-pane browser via
  // react-router location state. The browser keys its reveal effect on
  // `token`, so a fresh value is required even for re-clicks of the same path.
  const handleRevealInBrowser = useCallback((filePath: string) => {
    navigate('/files', {
      state: { reveal: { path: filePath, token: Date.now() } },
    });
  }, [navigate]);

  // Imperative handles into the plate-solve and find-object panels so the
  // toolbar buttons can trigger a batch directly without the panels showing
  // their own trigger buttons. The panels still render progress and completion
  // banners inline below the toolbar.
  const plateSolveRef = useRef<PlateSolveBatchPanelHandle>(null);
  const findObjectRef = useRef<FillObjectsPanelHandle>(null);

  // ── Fetch ────────────────────────────────────────────────────────────────

  const loadMissing = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const rows = mode === 'excluded'
        // ExcludedFrameRow extends MissingMetadataRow with `reason` /
        // `excludedAt`; the extra fields ride along for `extraColumn` to
        // surface but are otherwise ignored by the shared filter / selection
        // / eligibility code below.
        ? (await api.invoke<ExcludedFrameRow[]>('get_excluded_frames_with_metadata')) as MissingMetadataRow[]
        : await api.invoke<MissingMetadataRow[]>('get_frames_with_missing_metadata', {
            category: 'all',
          });
      setAllRows(rows);
      // Preserve selection: drop IDs no longer present
      const presentIds = new Set(rows.map(r => r.frame.id).filter((id): id is number => id != null));
      setSelectedIds(prev => {
        const next = new Set<number>();
        for (const id of prev) {
          if (presentIds.has(id)) next.add(id);
        }
        return next;
      });
      onAfterRefresh?.(rows);
    } catch (err) {
      console.error('Failed to load frames:', err);
      setError(typeof err === 'string' ? err : 'Failed to load frames');
    } finally {
      setLoading(false);
    }
  }, [mode, onAfterRefresh]);

  useEffect(() => {
    void loadMissing();
  }, [loadMissing]);

  // External refresh trigger — bumping refreshNonce reloads the table even
  // when nothing in the local effect deps changed.
  useEffect(() => {
    if (refreshNonce == null) return;
    void loadMissing();
    // intentionally only re-runs when the parent bumps the nonce
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refreshNonce]);

  // Mirror the current selection to the parent so it can host selection-
  // aware side panels (e.g. the MetadataPane).
  useEffect(() => {
    if (!onSelectionChange) return;
    const selectedRows = allRows.filter(
      r => r.frame.id != null && selectedIds.has(r.frame.id),
    );
    onSelectionChange(selectedRows);
  }, [allRows, selectedIds, onSelectionChange]);

  // Report the current row count up to the parent (for the tab badge).
  // While loading we clear the count so the badge doesn't show a stale number.
  useEffect(() => {
    if (!onCountChange) return;
    if (loading) {
      onCountChange(null);
    } else {
      onCountChange(allRows.length);
    }
  }, [allRows.length, loading, onCountChange]);

  // ── Chip filtering ───────────────────────────────────────────────────────

  const toggleChip = useCallback((chip: FilterChip) => {
    setActiveChips(prev => {
      const next = new Set(prev);
      if (next.has(chip)) next.delete(chip);
      else next.add(chip);
      return next;
    });
  }, []);

  const filteredRows = useMemo(() => {
    if (activeChips.size === 0) return allRows;
    return allRows.filter(item => {
      const flags = computeMissingFlags(item.frame);
      for (const chip of activeChips) {
        if (!flags[chip]) return false;
      }
      return true;
    });
  }, [allRows, activeChips]);

  // ── Selection ────────────────────────────────────────────────────────────

  const handleToggleRow = useCallback((frameId: number) => {
    setSelectedIds(prev => {
      const next = new Set(prev);
      if (next.has(frameId)) next.delete(frameId);
      else next.add(frameId);
      return next;
    });
  }, []);

  const handleToggleAll = useCallback(() => {
    const allIds = filteredRows
      .map(r => r.frame.id)
      .filter((id): id is number => id != null);
    const allSelected = allIds.length > 0 && allIds.every(id => selectedIds.has(id));
    if (allSelected) {
      setSelectedIds(prev => {
        const next = new Set(prev);
        for (const id of allIds) next.delete(id);
        return next;
      });
    } else {
      setSelectedIds(prev => {
        const next = new Set(prev);
        for (const id of allIds) next.add(id);
        return next;
      });
    }
  }, [filteredRows, selectedIds]);

  const handleToggleGroup = useCallback((frameIds: number[], select: boolean) => {
    setSelectedIds(prev => {
      const next = new Set(prev);
      if (select) {
        for (const id of frameIds) next.add(id);
      } else {
        for (const id of frameIds) next.delete(id);
      }
      return next;
    });
  }, []);

  // ── Eligibility computation ──────────────────────────────────────────────
  // Computed from the full allRows set (not filtered) for selected IDs

  const { eligible, eligibleIds } = useMemo(() => {
    const selectedRows = allRows.filter(
      r => r.frame.id != null && selectedIds.has(r.frame.id),
    );

    const plateSolveIds: number[] = [];
    const findObjectIds: number[] = [];
    const blinkIds: number[] = [];
    const setCameraIds: number[] = [];
    const setDateIds: number[] = [];
    const setTypeIds: number[] = [];
    // Black Hole works on file IDs, not frame IDs — bulk_move_to_black_hole
    // identifies rows in the `files` table.
    const blackHoleFileIds: number[] = [];

    for (const item of selectedRows) {
      const frameId = item.frame.id!;
      const frame = item.frame;
      const flags = computeMissingFlags(item.frame);

      // Plate Solve: LIGHT AND missing coordinates. The existing flow is only
      // meant to seed coords on no-coord frames.
      const plateSolveEligible =
        frame.imagetyp === ImageTypeValues.Light && flags.coordinates;
      if (plateSolveEligible) {
        plateSolveIds.push(frameId);
      }

      // Find Object: LIGHT AND has valid coordinates AND missing object
      const hasCoords =
        frame.ra != null &&
        frame.dec != null &&
        !(frame.ra === 0 && frame.dec === 0);
      if (frame.imagetyp === ImageTypeValues.Light && hasCoords && flags.object) {
        findObjectIds.push(frameId);
      }

      // Blink: any selected frame
      blinkIds.push(frameId);

      // Set Camera: missing instrume (any type)
      if (flags.camera) setCameraIds.push(frameId);

      // Set Date: missing date_obs (any type)
      if (flags.date) setDateIds.push(frameId);

      // Set Type: missing imagetyp (any type)
      if (flags.type) setTypeIds.push(frameId);

      // Black Hole: any selected frame whose file row has an id
      if (item.file.id != null) blackHoleFileIds.push(item.file.id);
    }

    const eligible: EligibleCounts = {
      plateSolve: plateSolveIds.length,
      findObject: findObjectIds.length,
      blink: blinkIds.length,
      setCamera: setCameraIds.length,
      setDate: setDateIds.length,
      setType: setTypeIds.length,
      blackHole: blackHoleFileIds.length,
    };

    return {
      eligible,
      eligibleIds: {
        plateSolve: plateSolveIds,
        findObject: findObjectIds,
        blink: blinkIds,
        setCamera: setCameraIds,
        setDate: setDateIds,
        setType: setTypeIds,
        blackHole: blackHoleFileIds,
      },
    };
  }, [allRows, selectedIds]);

  // ── Rows for blink viewer ────────────────────────────────────────────────
  // BlinkViewer takes FileWithFrame[], not MissingMetadataRow[], so wrap
  // each row's frame in an Option-shaped object.

  const blinkRows: FileWithFrame[] = useMemo(() => {
    const ids = new Set(eligibleIds.blink);
    return allRows
      .filter(r => r.frame.id != null && ids.has(r.frame.id))
      .map(r => ({ file: r.file, frame: r.frame }));
  }, [allRows, eligibleIds.blink]);

  // ── Modified-at strings for SetDateModal ────────────────────────────────

  const eligibleSetDateModifiedAts = useMemo(() => {
    const ids = new Set(eligibleIds.setDate);
    return allRows
      .filter(r => r.frame.id != null && ids.has(r.frame.id))
      .map(r => r.file.modified_at);
  }, [allRows, eligibleIds.setDate]);

  // ── extraActions context: collect file IDs for currently-selected frames
  // so the caller's buttons (e.g. "Treat as Flat" on the Excluded page) can
  // act on the user's actual selection. ────────────────────────────────────

  const selectedFileIds = useMemo<number[]>(() => {
    const out: number[] = [];
    for (const row of allRows) {
      const fid = row.frame.id;
      if (fid != null && selectedIds.has(fid) && row.file.id != null) {
        out.push(row.file.id);
      }
    }
    return out;
  }, [allRows, selectedIds]);

  const selectedFrameIds = useMemo<number[]>(() => Array.from(selectedIds), [selectedIds]);

  const resolvedExtraActions = useMemo<React.ReactNode>(() => {
    if (extraActions == null) return null;
    if (typeof extraActions === 'function') {
      return extraActions({
        selectedCount: selectedIds.size,
        selectedFrameIds,
        selectedFileIds,
        refresh: loadMissing,
      });
    }
    return extraActions;
  }, [extraActions, selectedIds.size, selectedFrameIds, selectedFileIds, loadMissing]);

  // ── Toolbar action handlers ──────────────────────────────────────────────

  const handlePlateSolve = () => {
    if (eligibleIds.plateSolve.length === 0) return;
    plateSolveRef.current?.start(eligibleIds.plateSolve);
  };

  const handleFindObject = () => {
    if (eligibleIds.findObject.length === 0) return;
    findObjectRef.current?.start(eligibleIds.findObject);
  };

  const handleBlink = () => setShowBlink(true);

  const handleBlackHole = () => {
    if (eligibleIds.blackHole.length === 0) return;
    setBlackHoleConfirmOpen(true);
  };

  const handleConfirmBlackHole = useCallback(async () => {
    setBlackHoleConfirmOpen(false);
    if (eligibleIds.blackHole.length === 0) return;
    try {
      await bulkBlackHole.start(
        eligibleIds.blackHole,
        mode === 'excluded' ? 'excluded_frames' : 'missing_metadata',
      );
      // Drop selection and reload — moved files won't appear in the next
      // missing-metadata fetch.
      setSelectedIds(new Set());
      void loadMissing();
    } catch {
      // useBulkMoveToBlackHole already logs; surface as an inline error.
    }
  }, [bulkBlackHole, eligibleIds.blackHole, loadMissing, mode]);

  // ── Reload callbacks ─────────────────────────────────────────────────────

  const handleSolveComplete = useCallback(() => { void loadMissing(); }, [loadMissing]);
  const handleFindObjectComplete = useCallback(() => { void loadMissing(); }, [loadMissing]);
  const handleFramesRemoved = useCallback(() => { void loadMissing(); }, [loadMissing]);
  const handleModalApplied = useCallback(() => {
    setOpenModal(null);
    void loadMissing();
  }, [loadMissing]);

  // ── Side-panel MetadataPane ──────────────────────────────────────────────
  // Synthesize the DirectoryListing + path-based selection MetadataPane
  // expects (it filters listing.files by selection internally, so as long
  // as paths match it doesn't care that this isn't a real directory).

  const editorListing = useMemo<DirectoryListing>(() => {
    const selected = allRows.filter(
      r => r.frame.id != null && selectedIds.has(r.frame.id),
    );
    return {
      subdirectories: [],
      files: selected.map(r => ({ file: r.file, frame: r.frame })),
    };
  }, [allRows, selectedIds]);

  const editorSelection = useMemo<Set<string>>(() => {
    return new Set(editorListing.files.map(f => f.file.path));
  }, [editorListing]);

  // After a successful save in the side-panel editor: notify the caller (so
  // they can run page-specific cleanup, e.g. removing rows from the
  // excluded_frames table) then reload the table.
  const handleEditorSaved = useCallback(async () => {
    const savedFileIds = editorListing.files
      .map(f => f.file.id)
      .filter((id): id is number => id != null);
    try {
      await onMetadataSaved?.(savedFileIds);
    } catch (err) {
      console.error('onMetadataSaved hook failed:', err);
    } finally {
      void loadMissing();
    }
  }, [editorListing, onMetadataSaved, loadMissing]);

  // ── Render ───────────────────────────────────────────────────────────────

  return (
    <div className="h-full flex gap-2 min-h-0">
      {/* Table column. When the editor is open it shrinks to ~60% so the
          MetadataPane on the right can host the bulk-edit form. The card
          itself does NOT scroll — only the inner table area does. This
          way the toolbar / banners sit naturally at the top with no
          chance of a visual gap (the previous sticky + negative-margin
          layout could leave a slim "hole" through which table rows
          peeked between the tab header and the toolbar). */}
      <div className={`bg-surface-elevated rounded-lg flex flex-col min-w-0 min-h-0 overflow-hidden ${editorOpen ? 'w-3/5' : 'flex-1'}`}>
      {/* Header section: toolbar + transient banners. Lives outside the
          scroll container so it always hugs the top of the card. */}
      <div className="px-2 pt-2 flex flex-col gap-2 flex-shrink-0">
        <MissingMetadataToolbar
          activeChips={activeChips}
          onToggleChip={toggleChip}
          selectedCount={selectedIds.size}
          eligible={eligible}
          loading={loading}
          onRefresh={() => void loadMissing()}
          onPlateSolve={handlePlateSolve}
          onFindObject={handleFindObject}
          onBlink={handleBlink}
          onSetCamera={() => setOpenModal('camera')}
          onSetDate={() => setOpenModal('date')}
          onSetType={() => setOpenModal('type')}
          onBlackHole={handleBlackHole}
          onClearSelection={() => setSelectedIds(new Set())}
          extraActions={resolvedExtraActions}
          hideEditActions={editorOpen}
          hideFilterChips={hideFilterChips}
          onToggleEditor={() => setEditorOpen(o => !o)}
          editorOpen={editorOpen}
        />
      </div>

      {/* Black-hole batch progress + result banner. Mirrors the
          plate-solve / find-object panels in placement. */}
      {bulkBlackHole.isRunning && (
        <div className="px-2 p-2 bg-surface rounded border border-border text-xs text-content-secondary mx-2">
          {bulkBlackHole.progress
            ? `Moving ${bulkBlackHole.progress.current.toLocaleString()} / ${bulkBlackHole.progress.total.toLocaleString()} to Black Hole…`
            : 'Moving to Black Hole…'}
        </div>
      )}
      {!bulkBlackHole.isRunning && bulkBlackHole.result && (bulkBlackHole.result.moved > 0 || bulkBlackHole.result.failed.length > 0) && (
        <div
          className={`mx-2 p-2 rounded border text-xs ${
            bulkBlackHole.result.failed.length > 0
              ? 'bg-warning-muted border-warning/50 text-warning'
              : 'bg-success-muted border-success/50 text-success'
          }`}
        >
          Moved {bulkBlackHole.result.moved.toLocaleString()} file{bulkBlackHole.result.moved === 1 ? '' : 's'} to Black Hole.
          {bulkBlackHole.result.failed.length > 0
            && ` ${bulkBlackHole.result.failed.length} failed.`}
          <button
            type="button"
            onClick={() => bulkBlackHole.reset()}
            className="ml-2 underline hover:no-underline"
          >
            Dismiss
          </button>
        </div>
      )}
      {bulkBlackHole.error && !bulkBlackHole.isRunning && (
        <div className="mx-2 p-2 rounded border border-error/50 bg-error-muted text-error text-xs">
          Black Hole move failed: {bulkBlackHole.error}
          <button
            type="button"
            onClick={() => bulkBlackHole.reset()}
            className="ml-2 underline hover:no-underline"
          >
            Dismiss
          </button>
        </div>
      )}

      {/* Plate solve progress/completion panel — always mounted, driven
          imperatively by the toolbar button. Renders as an empty div when
          idle (no active batch, no completion banner). */}
      <div className="px-2">
        <PlateSolveBatchPanel
          ref={plateSolveRef}
          hideTriggerButtons
          onSolveComplete={handleSolveComplete}
        />
      </div>

      {/* Find object progress/completion panel — same pattern */}
      <div className="px-2">
        <FillObjectsPanel
          ref={findObjectRef}
          hideTriggerButtons
          onComplete={handleFindObjectComplete}
        />
      </div>

      {/* Scroll area: only the table scrolls. The table's own sticky <th>
          headers stick to top:0 of THIS container — no toolbar-height
          measurement needed. NB: no top padding here. A `pt-*` would let
          rows scroll up into the gap above the sticky headers (same class
          of bug we just fixed for the toolbar). The table's <thead>
          provides its own visual top edge. */}
      <div className="flex-1 min-h-0 overflow-y-auto px-2 pb-2">
        <MissingMetadataTable
          rows={filteredRows}
          loading={loading}
          error={error}
          selectedIds={selectedIds}
          onToggleRow={handleToggleRow}
          onToggleAll={handleToggleAll}
          onToggleGroup={handleToggleGroup}
          onRevealInBrowser={handleRevealInBrowser}
          stickyHeaderTop={0}
          extraColumn={editorOpen ? undefined : extraColumn}
        />
      </div>

      {/* Blink Viewer */}
      {showBlink && blinkRows.length > 0 && (
        <BlinkViewer
          frames={blinkRows}
          onClose={() => setShowBlink(false)}
          sourceType="light"
          onFramesRemoved={handleFramesRemoved}
        />
      )}

      {/* Modals */}
      <SetCameraModal
        isOpen={openModal === 'camera'}
        eligibleCount={eligible.setCamera}
        totalSelected={selectedIds.size}
        eligibleFrameIds={eligibleIds.setCamera}
        onApplied={handleModalApplied}
        onCancel={() => setOpenModal(null)}
      />
      <SetDateModal
        isOpen={openModal === 'date'}
        eligibleCount={eligible.setDate}
        totalSelected={selectedIds.size}
        eligibleFrameIds={eligibleIds.setDate}
        eligibleFileModifiedAts={eligibleSetDateModifiedAts}
        onApplied={handleModalApplied}
        onCancel={() => setOpenModal(null)}
      />
      <SetFrameTypeModal
        isOpen={openModal === 'type'}
        eligibleCount={eligible.setType}
        totalSelected={selectedIds.size}
        eligibleFrameIds={eligibleIds.setType}
        onApplied={handleModalApplied}
        onCancel={() => setOpenModal(null)}
      />

      <ConfirmDialog
        isOpen={blackHoleConfirmOpen}
        title="Move to Black Hole"
        message={`Move ${eligibleIds.blackHole.length.toLocaleString()} file${eligibleIds.blackHole.length === 1 ? '' : 's'} to the Black Hole? This is reversible — you can restore from the Black Hole tab.`}
        confirmText="Move"
        confirmDanger
        onConfirm={() => { void handleConfirmBlackHole(); }}
        onCancel={() => setBlackHoleConfirmOpen(false)}
      />
      </div>

      {/* Right-side metadata editor. Hosts the same MetadataPane the
          dual-pane file browser uses, so there's a single source of truth
          for bulk metadata editing. Visible only while editorOpen. */}
      {editorOpen && (
        <aside className="w-2/5 min-w-0 border border-border rounded-lg bg-surface-elevated flex flex-col overflow-hidden">
          <header className="flex items-center justify-between px-3 py-2 border-b border-border flex-shrink-0">
            <div className="flex items-center gap-2">
              <Pencil size={14} className="text-accent" />
              <span className="text-sm font-medium">Metadata Editor</span>
              <span className="text-xs text-content-muted">
                {editorListing.files.length === 0
                  ? '(no rows selected)'
                  : `${editorListing.files.length} row${editorListing.files.length === 1 ? '' : 's'}`}
              </span>
            </div>
            <button
              onClick={() => setEditorOpen(false)}
              className="text-content-muted hover:text-content transition-colors"
              title="Close editor"
            >
              <X size={16} />
            </button>
          </header>
          <div className="flex-1 min-h-0 overflow-auto">
            {editorListing.files.length === 0 ? (
              <div className="p-4 text-sm text-content-muted text-center">
                Select one or more rows in the table to edit their metadata.
              </div>
            ) : (
              <MetadataPane
                otherListing={editorListing}
                otherSelection={editorSelection}
                onSaved={() => { void handleEditorSaved(); }}
              />
            )}
          </div>
        </aside>
      )}
    </div>
  );
};
