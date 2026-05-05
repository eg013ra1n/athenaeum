import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { api } from '../../api';
import { ImageType } from '../../types/models';
import type { FileWithFrame, MissingMetadataRow } from '../../types/models';

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

type ModalKind = 'camera' | 'date' | 'type' | null;

interface MissingMetadataViewProps {
  /** Called with the total number of rows currently loaded, so the parent
      tab button can show a count badge. Passed `null` while loading. */
  onCountChange?: (count: number | null) => void;
}

export const MissingMetadataView: React.FC<MissingMetadataViewProps> = ({ onCountChange }) => {
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

  // Toolbar height is measured live so the sticky table header can be offset
  // below it. The toolbar uses flex-wrap, so the height can change with the
  // window width — a fixed offset would overlap or leave a gap.
  const toolbarRef = useRef<HTMLDivElement>(null);
  const [toolbarHeight, setToolbarHeight] = useState(0);

  useEffect(() => {
    const el = toolbarRef.current;
    if (!el) return;
    const update = () => setToolbarHeight(el.getBoundingClientRect().height);
    update();
    const ro = new ResizeObserver(update);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // ── Fetch ────────────────────────────────────────────────────────────────

  const loadMissing = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const rows = await api.invoke<MissingMetadataRow[]>('get_frames_with_missing_metadata', {
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
    } catch (err) {
      console.error('Failed to load frames:', err);
      setError(typeof err === 'string' ? err : 'Failed to load frames');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadMissing();
  }, [loadMissing]);

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
        frame.imagetyp === ImageType.Light && flags.coordinates;
      if (plateSolveEligible) {
        plateSolveIds.push(frameId);
      }

      // Find Object: LIGHT AND has valid coordinates AND missing object
      const hasCoords =
        frame.ra != null &&
        frame.dec != null &&
        !(frame.ra === 0 && frame.dec === 0);
      if (frame.imagetyp === ImageType.Light && hasCoords && flags.object) {
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
      await bulkBlackHole.start(eligibleIds.blackHole, 'missing_metadata');
      // Drop selection and reload — moved files won't appear in the next
      // missing-metadata fetch.
      setSelectedIds(new Set());
      void loadMissing();
    } catch {
      // useBulkMoveToBlackHole already logs; surface as an inline error.
    }
  }, [bulkBlackHole, eligibleIds.blackHole, loadMissing]);

  // ── Reload callbacks ─────────────────────────────────────────────────────

  const handleSolveComplete = useCallback(() => { void loadMissing(); }, [loadMissing]);
  const handleFindObjectComplete = useCallback(() => { void loadMissing(); }, [loadMissing]);
  const handleFramesRemoved = useCallback(() => { void loadMissing(); }, [loadMissing]);
  const handleModalApplied = useCallback(() => {
    setOpenModal(null);
    void loadMissing();
  }, [loadMissing]);

  // ── Render ───────────────────────────────────────────────────────────────

  return (
    <div className="bg-surface-elevated rounded-lg p-2 flex flex-col">
      {/* Toolbar (refresh is in the toolbar itself so this view has no
          dedicated header row — the tab title already names the section).
          Wrapped in a sticky container so the controls remain on-screen while
          the table scrolls. Negative margins + padding extend the background
          over the parent's p-2 so scrolling content doesn't peek through. */}
      <div
        ref={toolbarRef}
        className="sticky top-0 z-20 bg-surface-elevated -mx-2 -mt-2 px-2 pt-2 rounded-t-lg"
      >
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
        />
      </div>

      {/* Black-hole batch progress + result banner. Mirrors the
          plate-solve / find-object panels in placement. */}
      {bulkBlackHole.isRunning && (
        <div className="mb-2 p-2 bg-surface rounded border border-border text-xs text-content-secondary">
          {bulkBlackHole.progress
            ? `Moving ${bulkBlackHole.progress.current.toLocaleString()} / ${bulkBlackHole.progress.total.toLocaleString()} to Black Hole…`
            : 'Moving to Black Hole…'}
        </div>
      )}
      {!bulkBlackHole.isRunning && bulkBlackHole.result && (bulkBlackHole.result.moved > 0 || bulkBlackHole.result.failed.length > 0) && (
        <div
          className={`mb-2 p-2 rounded border text-xs ${
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
        <div className="mb-2 p-2 rounded border border-error/50 bg-error-muted text-error text-xs">
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
      <PlateSolveBatchPanel
        ref={plateSolveRef}
        hideTriggerButtons
        onSolveComplete={handleSolveComplete}
      />

      {/* Find object progress/completion panel — same pattern */}
      <FillObjectsPanel
        ref={findObjectRef}
        hideTriggerButtons
        onComplete={handleFindObjectComplete}
      />

      {/* Table */}
      <MissingMetadataTable
        rows={filteredRows}
        loading={loading}
        error={error}
        selectedIds={selectedIds}
        onToggleRow={handleToggleRow}
        onToggleAll={handleToggleAll}
        onToggleGroup={handleToggleGroup}
        onRevealInBrowser={handleRevealInBrowser}
        stickyHeaderTop={toolbarHeight}
      />

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
  );
};
