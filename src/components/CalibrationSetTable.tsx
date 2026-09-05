import { useState, useEffect, useRef, useCallback } from "react";
import { useNavigate } from "react-router-dom";
import { ChevronUp, ChevronDown, ChevronsUpDown, Eye, Settings, Star, Pencil, Hammer, Trash2 } from "lucide-react";
import type { CalibrationSetDetail, CalibrationSetConsumer, FileWithFrame, MasterProvenanceInfo, DeleteMasterResult } from "../types/models";
import { ImageTypeValues, isMasterType } from "../types/helpers";
import { format } from "date-fns";
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';
import { describeRecipeJson } from '../utils/recipeFormat';
import BlinkViewer from "./BlinkViewer";
import { ConfirmDialog } from "./ConfirmDialog";
import { ArchiveProgress } from "./archive/ArchiveProgress";

const CONSUMER_INITIAL_VISIBLE = 8;

interface CalibrationSetTableProps {
  sets: CalibrationSetDetail[];
  showFilterColumn?: boolean;
  /** Callback to edit sub-calibration for a set (for flat/dark sets only) */
  onEditSubCalibration?: (setId: number, setType: 'flat' | 'dark') => void;
  /** Set IDs that have custom metadata edits */
  customMetadataSetIds?: number[];
  /** When set, scroll the matching row into view, highlight it, and auto-expand it. */
  highlightSetId?: number | null;
  /** Show a "Create Master" action on raw (non-master, non-superseded) sets. */
  onCreateMaster?: (setId: number) => void;
  /** Set IDs with an in-flight master build (renders a spinner label). */
  buildingSetIds?: number[];
}

type SortField = "id" | "imagetyp" | "filter" | "exptime" | "ccd_temp" | "gain" | "offset" | "binning" | "date_start" | "frame_count";
type SortDirection = "asc" | "desc";

export default function CalibrationSetTable({ sets, showFilterColumn = false, onEditSubCalibration, customMetadataSetIds = [], highlightSetId, onCreateMaster, buildingSetIds }: CalibrationSetTableProps) {
  const [sortField, setSortField] = useState<SortField>("exptime");
  const [sortDirection, setSortDirection] = useState<SortDirection>("asc");
  const [expandedRows, setExpandedRows] = useState<Set<number>>(new Set());
  const [blinkFrames, setBlinkFrames] = useState<FileWithFrame[] | null>(null);
  const [loadingFrames, setLoadingFrames] = useState<number | null>(null);
  // Row currently flashing the cross-page-jump highlight; cleared after fade.
  const [flashSetId, setFlashSetId] = useState<number | null>(null);
  const highlightRowRef = useRef<HTMLTableRowElement | null>(null);
  // Lazy-loaded consumers (frame sets that use a calibration set), keyed by
  // setId. 'loading'/Error variants let us show a spinner / error inline.
  type ConsumerState =
    | { status: 'idle' }
    | { status: 'loading' }
    | { status: 'loaded'; consumers: CalibrationSetConsumer[] }
    | { status: 'error'; message: string };
  const [consumersBySet, setConsumersBySet] = useState<Map<number, ConsumerState>>(new Map());
  // Per-set "show all" toggle for the chip strip.
  const [expandedConsumers, setExpandedConsumers] = useState<Set<number>>(new Set());
  // Delete-master flow: the row awaiting confirmation, the file names we
  // resolved for it (null while still loading), and an in-flight guard so a
  // double-click on Delete can't fire the command twice.
  const [deleteTarget, setDeleteTarget] = useState<CalibrationSetDetail | null>(null);
  const [deleteFileNames, setDeleteFileNames] = useState<string[] | null>(null);
  // true = imported master (no provenance) → links are DELETED, nothing is
  // restored; false = built here → lineage comes back. null = not resolved
  // yet or the lookup failed, which falls back to the generic wording.
  const [deleteImported, setDeleteImported] = useState<boolean | null>(null);
  const [deleting, setDeleting] = useState(false);
  const navigate = useNavigate();
  const { notify } = useNotifications();

  const fetchConsumers = useCallback(async (setId: number) => {
    setConsumersBySet(prev => {
      const next = new Map(prev);
      next.set(setId, { status: 'loading' });
      return next;
    });
    try {
      const consumers = await api.invoke<CalibrationSetConsumer[]>('get_calibration_set_consumers', { setId });
      setConsumersBySet(prev => {
        const next = new Map(prev);
        next.set(setId, { status: 'loaded', consumers });
        return next;
      });
    } catch (err) {
      console.error('Failed to load calibration set consumers:', err);
      setConsumersBySet(prev => {
        const next = new Map(prev);
        next.set(setId, { status: 'error', message: String(err) });
        return next;
      });
    }
  }, []);

  // When a highlightSetId is supplied (e.g. from a Calibration Coverage jump),
  // auto-expand the row, scroll it into view, and flash the accent highlight.
  useEffect(() => {
    if (highlightSetId == null) return;
    if (!sets.some(s => s.id === highlightSetId)) return;
    setExpandedRows(prev => {
      if (prev.has(highlightSetId)) return prev;
      const next = new Set(prev);
      next.add(highlightSetId);
      return next;
    });
    if (!consumersBySet.has(highlightSetId)) {
      void fetchConsumers(highlightSetId);
    }
    setFlashSetId(highlightSetId);
    const scrollT = setTimeout(() => {
      highlightRowRef.current?.scrollIntoView({ behavior: 'smooth', block: 'center' });
    }, 60);
    const fadeT = setTimeout(() => setFlashSetId(null), 2500);
    return () => {
      clearTimeout(scrollT);
      clearTimeout(fadeT);
    };
    // consumersBySet intentionally omitted: re-fetching when the cache mutates would loop.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [highlightSetId, sets, fetchConsumers]);

  const handleSort = (field: SortField) => {
    if (sortField === field) {
      // Toggle direction
      setSortDirection(sortDirection === "asc" ? "desc" : "asc");
    } else {
      // New field, default to ascending
      setSortField(field);
      setSortDirection("asc");
    }
  };

  const toggleRowExpansion = (id: number) => {
    const newExpanded = new Set(expandedRows);
    if (newExpanded.has(id)) {
      newExpanded.delete(id);
    } else {
      newExpanded.add(id);
      // Lazy-fetch consumers the first time this row is opened.
      if (id > 0 && !consumersBySet.has(id)) {
        void fetchConsumers(id);
      }
    }
    setExpandedRows(newExpanded);
  };

  const handleViewFrames = async (setId: number, event: React.MouseEvent) => {
    event.stopPropagation(); // Prevent row expansion
    try {
      setLoadingFrames(setId);
      const frames = await api.invoke<FileWithFrame[]>('get_calibration_set_frames', { setId });
      setBlinkFrames(frames);
    } catch (err) {
      console.error('Failed to load calibration frames:', err);
      notify({ title: 'Failed to load frames', detail: String(err), kind: 'files', tone: 'warning', hasErrors: true });
    } finally {
      setLoadingFrames(null);
    }
  };

  // ── Delete master ─────────────────────────────────────────────────────────
  //
  // Destructive and irreversible (the file leaves the disk), so it goes
  // through ConfirmDialog rather than a bare click. The dialog names the
  // actual file: CalibrationSetDetail carries no path, so we resolve it with
  // the same `get_calibration_set_frames` call the View button uses while the
  // dialog is already open.

  const openDeleteDialog = (set: CalibrationSetDetail, event: React.MouseEvent) => {
    event.stopPropagation();
    setDeleteTarget(set);
    setDeleteFileNames(null);
    setDeleteImported(null);
    api.invoke<FileWithFrame[]>('get_calibration_set_frames', { setId: set.id })
      .then(frames => setDeleteFileNames(frames.map(f => f.file.filename)))
      .catch(err => {
        // Non-fatal: the confirmation just falls back to naming the set.
        console.error('Failed to resolve master file name:', err);
        setDeleteFileNames([]);
      });
    // What the user is consenting to differs by lineage: a null provenance
    // means an imported master, whose consumer links are DELETED rather than
    // moved back onto a raw set (there is none). Promising a restore there
    // would be a lie.
    api.invoke<MasterProvenanceInfo | null>('get_master_provenance', { masterSetId: set.id })
      .then(p => setDeleteImported(p === null))
      .catch(err => {
        // Non-fatal: unknown lineage keeps the generic wording.
        console.error('Failed to resolve master provenance:', err);
        setDeleteImported(null);
      });
  };

  const handleConfirmDelete = async () => {
    const set = deleteTarget;
    if (!set || set.id == null) return;
    setDeleting(true);
    try {
      const res = await api.invoke<DeleteMasterResult>('delete_master', { masterSetId: set.id });
      const linkDetail = res.restoredRawSetId != null
        ? `Raw set #${res.restoredRawSetId} restored; ${res.linksRepointed} link(s) repointed`
        : `${res.linksRepointed} link(s) removed (imported master)`;
      if (res.filesDeleted === 0) {
        // The catalog rows are gone but nothing left the disk. Reporting a
        // plain success here would hide the consequence: a file still sitting
        // in the library root gets re-ingested as an imported master.
        notify({
          title: 'Master deleted from catalog',
          detail: `${linkDetail}. No file was removed from disk (missing or locked); if it still exists it will re-appear as an imported master on the next scan.`,
          kind: 'masterbuild',
          tone: 'warning',
          hasErrors: true,
        });
      } else {
        notify({
          title: 'Master deleted',
          detail: linkDetail,
          kind: 'masterbuild',
          tone: 'success',
        });
      }
      // Both lists changed shape (master row gone, source set un-superseded) —
      // same refresh signal `useMasterBuilds` fires after a build completes.
      window.dispatchEvent(new Event('library-updated'));
    } catch (err) {
      console.error('Failed to delete master:', err);
      notify({
        title: 'Delete master failed',
        detail: String(err),
        kind: 'masterbuild',
        tone: 'warning',
        hasErrors: true,
      });
    } finally {
      setDeleting(false);
      setDeleteTarget(null);
      setDeleteFileNames(null);
      setDeleteImported(null);
    }
  };

  const deleteMessage = deleteTarget
    ? [
        deleteFileNames === null
          ? 'Resolving file…'
          : deleteFileNames.length > 0
            ? `${deleteFileNames.join(', ')} will be permanently deleted from disk.`
            : `The file of master set #${deleteTarget.id} (${deleteTarget.imagetyp}) will be permanently deleted from disk.`,
        '',
        deleteImported === true
          ? 'This master was imported; its calibration link(s) will be removed and nothing is restored.'
          : 'The raw source set becomes matchable again and every calibration link that pointed at this master moves back to it.',
        'Calibrated lights that used this master will show as stale.',
      ].join('\n')
    : '';

  // Sort sets
  const sortedSets = [...sets].sort((a, b) => {
    let aVal: any = a[sortField];
    let bVal: any = b[sortField];

    // Handle null values
    if (aVal === null || aVal === undefined) return 1;
    if (bVal === null || bVal === undefined) return -1;

    // String comparison
    if (typeof aVal === "string") {
      return sortDirection === "asc"
        ? aVal.localeCompare(bVal)
        : bVal.localeCompare(aVal);
    }

    // Numeric comparison
    return sortDirection === "asc" ? aVal - bVal : bVal - aVal;
  });

  const formatDate = (dateStr: string) => {
    try {
      return format(new Date(dateStr), "MMM d, yyyy");
    } catch {
      return dateStr;
    }
  };

  const formatTemp = (temp: number) => {
    return `${temp.toFixed(1)}°C`;
  };

  const SortIcon = ({ field }: { field: SortField }) => {
    if (sortField !== field) {
      return <ChevronsUpDown size={14} className="text-content-muted" />;
    }
    return sortDirection === "asc" ? (
      <ChevronUp size={14} className="text-accent" />
    ) : (
      <ChevronDown size={14} className="text-accent" />
    );
  };

  if (sets.length === 0) {
    return (
      <div className="text-center py-12 text-content-muted bg-surface-elevated rounded-lg">
        No calibration sets to display
      </div>
    );
  }

  return (
    <div className="overflow-x-auto bg-surface-elevated rounded-lg border border-border">
      <table className="w-full">
        <thead className="bg-surface sticky top-0">
          <tr>
            <th className="px-4 py-1.5 text-left text-xs font-medium text-content-muted uppercase tracking-wider">
              <button
                onClick={() => handleSort("id")}
                className="flex items-center gap-1 hover:text-content transition-colors"
              >
                ID
                <SortIcon field="id" />
              </button>
            </th>
            <th className="px-4 py-1.5 text-left text-xs font-medium text-content-muted uppercase tracking-wider">
              <button
                onClick={() => handleSort("imagetyp")}
                className="flex items-center gap-1 hover:text-content transition-colors"
              >
                Type
                <SortIcon field="imagetyp" />
              </button>
            </th>
            {showFilterColumn && (
              <th className="px-4 py-1.5 text-left text-xs font-medium text-content-muted uppercase tracking-wider">
                <button
                  onClick={() => handleSort("filter")}
                  className="flex items-center gap-1 hover:text-content transition-colors"
                >
                  Filter
                  <SortIcon field="filter" />
                </button>
              </th>
            )}
            <th className="px-4 py-1.5 text-left text-xs font-medium text-content-muted uppercase tracking-wider">
              <button
                onClick={() => handleSort("exptime")}
                className="flex items-center gap-1 hover:text-content transition-colors"
              >
                Exposure
                <SortIcon field="exptime" />
              </button>
            </th>
            <th className="px-4 py-1.5 text-left text-xs font-medium text-content-muted uppercase tracking-wider">
              <button
                onClick={() => handleSort("ccd_temp")}
                className="flex items-center gap-1 hover:text-content transition-colors"
              >
                Temp (°C)
                <SortIcon field="ccd_temp" />
              </button>
            </th>
            <th className="px-4 py-1.5 text-left text-xs font-medium text-content-muted uppercase tracking-wider">
              <button
                onClick={() => handleSort("gain")}
                className="flex items-center gap-1 hover:text-content transition-colors"
              >
                Gain
                <SortIcon field="gain" />
              </button>
            </th>
            <th className="px-4 py-1.5 text-left text-xs font-medium text-content-muted uppercase tracking-wider hidden md:table-cell">
              <button
                onClick={() => handleSort("offset")}
                className="flex items-center gap-1 hover:text-content transition-colors"
              >
                Offset
                <SortIcon field="offset" />
              </button>
            </th>
            <th className="px-4 py-1.5 text-left text-xs font-medium text-content-muted uppercase tracking-wider hidden lg:table-cell">
              <button
                onClick={() => handleSort("binning")}
                className="flex items-center gap-1 hover:text-content transition-colors"
              >
                Binning
                <SortIcon field="binning" />
              </button>
            </th>
            <th className="px-4 py-1.5 text-left text-xs font-medium text-content-muted uppercase tracking-wider">
              <button
                onClick={() => handleSort("date_start")}
                className="flex items-center gap-1 hover:text-content transition-colors"
              >
                Date Range
                <SortIcon field="date_start" />
              </button>
            </th>
            <th className="px-4 py-1.5 text-left text-xs font-medium text-content-muted uppercase tracking-wider">
              <button
                onClick={() => handleSort("frame_count")}
                className="flex items-center gap-1 hover:text-content transition-colors"
              >
                Frames
                <SortIcon field="frame_count" />
              </button>
            </th>
            <th className="px-4 py-1.5 text-center text-xs font-medium text-content-muted uppercase tracking-wider">
              Actions
            </th>
          </tr>
        </thead>
        <tbody className="divide-y divide-border">
          {sortedSets.map((set, index) => {
            const isExpanded = expandedRows.has(set.id || index);
            const rowId = set.id || index;
            const isHighlighted = set.id != null && set.id === flashSetId;

            return (
              <>
                <tr
                  key={rowId}
                  ref={set.id != null && set.id === highlightSetId ? highlightRowRef : undefined}
                  data-set-id={set.id ?? undefined}
                  onClick={() => toggleRowExpansion(rowId)}
                  className={`${
                    isHighlighted
                      ? "bg-accent/20 ring-2 ring-inset ring-accent"
                      : index % 2 === 0
                      ? "bg-surface-elevated"
                      : "bg-surface"
                  } ${set.superseded_by_set_id != null ? "opacity-50" : ""} hover:bg-surface-hover cursor-pointer transition-colors`}
                >
                  {/* ID */}
                  <td className="px-4 py-1 text-sm text-content-muted">
                    {set.id ?? "—"}
                    {set.superseded_by_set_id != null && (
                      <span
                        className="text-[10px] text-content-muted ml-1"
                        title={`Superseded by master set #${set.superseded_by_set_id}`}
                      >
                        → M#{set.superseded_by_set_id}
                      </span>
                    )}
                  </td>

                  {/* Type */}
                  <td className="px-4 py-1">
                    <div className="flex items-center gap-1">
                      {isMasterType(set.imagetyp) && (
                        <Star size={12} className="text-warning fill-warning" />
                      )}
                      {customMetadataSetIds.includes(set.id!) && (
                        <span title="Custom metadata">
                          <Pencil size={12} className="text-info" />
                        </span>
                      )}
                      <span
                        className={`px-2 py-1 rounded text-xs font-medium ${
                          set.imagetyp === "Dark" || set.imagetyp === "MasterDark"
                            ? "bg-purple/10 text-purple border border-purple/50"
                            : set.imagetyp === "Flat" || set.imagetyp === "MasterFlat"
                            ? "bg-warning-muted text-warning border border-warning/50"
                            : set.imagetyp === "Bias" || set.imagetyp === "MasterBias"
                            ? "bg-info-muted text-accent border border-accent/50"
                            : set.imagetyp === "DarkFlat" || set.imagetyp === "MasterDarkFlat"
                            ? "bg-purple/5 text-purple/80 border border-purple/30"
                            : "bg-surface/30 text-content-muted border border-border"
                        }`}
                      >
                        {set.imagetyp}
                      </span>
                    </div>
                  </td>

                  {/* Filter (for flats) */}
                  {showFilterColumn && (
                    <td className="px-4 py-1 text-sm text-content">
                      {set.filter || "—"}
                    </td>
                  )}

                  {/* Exposure */}
                  <td className="px-4 py-1 text-sm text-content">
                    {set.exptime !== null ? `${set.exptime}s` : "N/A"}
                  </td>

                  {/* Temperature */}
                  <td className="px-4 py-1 text-sm">
                    <div className="flex flex-col">
                      <span className="text-content font-medium">
                        {formatTemp(set.ccd_temp)}
                      </span>
                      <span className="text-content-muted text-xs">
                        ({formatTemp(set.temp_min)} - {formatTemp(set.temp_max)})
                      </span>
                    </div>
                  </td>

                  {/* Gain — a set that declares none cannot be matched
                      automatically: gain and offset are Exact + required for
                      lights, and "nothing to compare" is a refusal, so such a
                      set is silently invisible to auto-link until someone
                      fills it in (Bulk Edit, above). Say so rather than
                      printing a bland N/A. */}
                  <td className="px-4 py-1 text-sm text-content">
                    {set.gain !== null ? (
                      set.gain
                    ) : (
                      <span
                        className="text-warning"
                        title="No GAIN in this set — it will not be matched automatically. Select it and use Edit Metadata to fill it in."
                      >
                        none
                      </span>
                    )}
                  </td>

                  {/* Offset - Hidden on mobile */}
                  <td className="px-4 py-1 text-sm text-content hidden md:table-cell">
                    {set.offset !== null ? (
                      set.offset
                    ) : (
                      <span
                        className="text-warning"
                        title="No OFFSET in this set — it will not be matched automatically. Select it and use Edit Metadata to fill it in."
                      >
                        none
                      </span>
                    )}
                  </td>

                  {/* Binning - Hidden on tablet */}
                  <td className="px-4 py-1 text-sm text-content hidden lg:table-cell">
                    {set.binning || "N/A"}
                  </td>

                  {/* Date Range */}
                  <td className="px-4 py-1 text-sm">
                    <div className="flex flex-col">
                      <span className="text-content font-medium">
                        {set.date_display}
                      </span>
                      <span className="text-content-muted text-xs">
                        {formatDate(set.date_start)} - {formatDate(set.date_end)}
                      </span>
                    </div>
                  </td>

                  {/* Frame Count */}
                  <td className="px-4 py-1 text-sm text-content font-medium">
                    {set.frame_count}
                  </td>

                  {/* Actions */}
                  <td className="px-4 py-1 text-center">
                    <div className="flex items-center justify-center gap-2">
                      <button
                        onClick={(e) => handleViewFrames(set.id!, e)}
                        disabled={loadingFrames === set.id}
                        className="inline-flex items-center gap-1 px-3 py-1 bg-accent hover:bg-accent-hover text-white text-xs rounded transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                        title="View frames in blink viewer"
                      >
                        <Eye size={14} />
                        {loadingFrames === set.id ? 'Loading...' : 'View'}
                      </button>
                      {/* Don't show Sub-Cal button for Bias or Master types (masters don't need sub-calibration) */}
                      {onEditSubCalibration && set.id !== null && set.imagetyp !== ImageTypeValues.Bias && !isMasterType(set.imagetyp) && (
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            const setType = set.imagetyp === ImageTypeValues.Flat ? 'flat' : 'dark';
                            onEditSubCalibration(set.id!, setType);
                          }}
                          className="inline-flex items-center gap-1 px-2 py-1 bg-surface-hover hover:brightness-110 text-content text-xs rounded transition-colors"
                          title="Edit sub-calibration"
                        >
                          <Settings size={14} />
                          Sub-Cal
                        </button>
                      )}
                      {onCreateMaster && set.id != null && !isMasterType(set.imagetyp) && set.superseded_by_set_id == null && (
                        <button
                          onClick={(e) => { e.stopPropagation(); onCreateMaster(set.id!); }}
                          disabled={buildingSetIds?.includes(set.id)}
                          className="inline-flex items-center gap-1 px-2 py-1 bg-surface-hover hover:brightness-110 text-content text-xs rounded transition-colors disabled:opacity-50"
                          title="Integrate this set into a master frame"
                        >
                          <Hammer size={14} />
                          {buildingSetIds?.includes(set.id) ? 'Building…' : 'Create Master'}
                        </button>
                      )}
                      {/* `buildingSetIds` tracks SOURCE set ids, never a master's own id, so
                          it can't gate this row — the in-flight-build guard is the backend's
                          (409), and it checks the whole lineage. */}
                      {set.id != null && isMasterType(set.imagetyp) && (
                        <button
                          onClick={(e) => openDeleteDialog(set, e)}
                          disabled={deleting}
                          className="inline-flex items-center gap-1 px-2 py-1 bg-surface-hover hover:brightness-110 text-error text-xs rounded transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                          title="Delete this master and restore its raw source set"
                        >
                          <Trash2 size={14} />
                          Delete
                        </button>
                      )}
                    </div>
                  </td>
                </tr>

                {/* Expanded Row */}
                {isExpanded && (
                  <tr
                    key={`${rowId}-expanded`}
                    className="bg-surface border-t border-border"
                  >
                    <td colSpan={showFilterColumn ? 11 : 10} className="px-4 py-1">
                      <div className="flex flex-wrap gap-x-6 gap-y-1 text-sm">
                        <div>
                          <span className="text-content-muted">Resolution:</span>
                          <span className="ml-2 text-content">
                            {set.naxis1 && set.naxis2 ? `${set.naxis1} × ${set.naxis2}` : "—"}
                          </span>
                        </div>
                        <div>
                          <span className="text-content-muted">Bayer:</span>
                          <span className="ml-2 text-content">
                            {set.bayerpat || "Mono"}
                          </span>
                        </div>
                        <div>
                          <span className="text-content-muted">Pixel Size:</span>
                          <span className="ml-2 text-content">
                            {set.xpixsz ? `${set.xpixsz}µm` : "—"}
                          </span>
                        </div>
                        <div>
                          <span className="text-content-muted">Format:</span>
                          <span className="ml-2 text-content">
                            {set.format || "—"}
                          </span>
                        </div>
                        <div>
                          <span className="text-content-muted">Software:</span>
                          <span className="ml-2 text-content">
                            {set.swcreate || "—"}
                          </span>
                        </div>
                        <div className="md:hidden">
                          <span className="text-content-muted">Offset:</span>
                          <span className="ml-2 text-content">
                            {set.offset !== null ? set.offset : "N/A"}
                          </span>
                        </div>
                        <div className="lg:hidden">
                          <span className="text-content-muted">Binning:</span>
                          <span className="ml-2 text-content">
                            {set.binning || "N/A"}
                          </span>
                        </div>
                      </div>
                      {set.id != null && isMasterType(set.imagetyp) && (
                        <MasterProvenanceBlock setId={set.id} />
                      )}
                      {set.id != null && (
                        <ConsumerChipStrip
                          state={consumersBySet.get(set.id) ?? { status: 'idle' }}
                          showAll={expandedConsumers.has(set.id)}
                          onToggleShowAll={() => {
                            setExpandedConsumers(prev => {
                              const next = new Set(prev);
                              if (next.has(set.id!)) next.delete(set.id!);
                              else next.add(set.id!);
                              return next;
                            });
                          }}
                          onNavigate={frameSetId => {
                            // Map the parent set's imagetyp to the coverage-table
                            // bucket. DarkFlat lives in the Darks table; masters
                            // collapse to their non-master kind.
                            const t = set.imagetyp;
                            const kind: 'flat' | 'dark' | 'bias' =
                              t === ImageTypeValues.Flat || t === ImageTypeValues.MasterFlat
                                ? 'flat'
                                : t === ImageTypeValues.Bias || t === ImageTypeValues.MasterBias
                                ? 'bias'
                                : 'dark';
                            const params = new URLSearchParams({
                              tab: 'calibration',
                              highlightSet: String(set.id),
                              kind,
                            });
                            navigate(`/objects/${frameSetId}?${params.toString()}`);
                          }}
                          onRetry={() => fetchConsumers(set.id!)}
                        />
                      )}
                    </td>
                  </tr>
                )}
              </>
            );
          })}
        </tbody>
      </table>

      {/* Delete-master confirmation */}
      <ConfirmDialog
        isOpen={deleteTarget != null}
        title="Delete master?"
        message={deleteMessage}
        confirmText={deleting ? 'Deleting…' : 'Delete master'}
        confirmDanger
        onConfirm={() => { if (!deleting) void handleConfirmDelete(); }}
        onCancel={() => { if (!deleting) { setDeleteTarget(null); setDeleteFileNames(null); setDeleteImported(null); } }}
      />

      {/* BlinkViewer Modal */}
      {blinkFrames && (
        <BlinkViewer
          frames={blinkFrames}
          onClose={() => setBlinkFrames(null)}
          sourceType="calibration"
        />
      )}
    </div>
  );
}

// ── Master provenance block ─────────────────────────────────────────────────
//
// Rendered in the expanded row for master-type sets. Lazily fetches
// `get_master_provenance`; `null` means the master was imported (found on
// disk / scanned in) rather than built by Athenaeum, so there's no lineage
// to show. Archiving originals is a queued background op (Task 14) — we
// reuse the existing `ArchiveProgress` widget (same `archive-progress` /
// `archive-finished` events as the frame-set archive flow) so the button
// reflects real in-flight state instead of just the synchronous "queue it"
// call, and re-fetch provenance once the op finishes so
// `originalsArchived`/`sourceFramesOnDisk` reflect the new state.

/** Last path segment (basename); same small helper as `CreateMasterDialog`'s
 *  local `basename()` — kept duplicated rather than shared for one line. */
function provenanceBasename(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

function MasterProvenanceBlock({ setId }: { setId: number }) {
  const [prov, setProv] = useState<MasterProvenanceInfo | null | 'loading'>('loading');
  const [archiveOpId, setArchiveOpId] = useState<number | null>(null);
  const [archiveStartError, setArchiveStartError] = useState<string | null>(null);
  const [restoreOpId, setRestoreOpId] = useState<number | null>(null);
  const [restoreStartError, setRestoreStartError] = useState<string | null>(null);
  const [forgetBusy, setForgetBusy] = useState(false);
  const [forgetError, setForgetError] = useState<string | null>(null);
  const [rebuildBusy, setRebuildBusy] = useState(false);
  const [rebuildError, setRebuildError] = useState<string | null>(null);

  const fetchProvenance = useCallback(() => {
    let gone = false;
    api.invoke<MasterProvenanceInfo | null>('get_master_provenance', { masterSetId: setId })
      .then(p => { if (!gone) setProv(p); })
      .catch(() => { if (!gone) setProv(null); });
    return () => { gone = true; };
  }, [setId]);

  useEffect(() => {
    setProv('loading');
    return fetchProvenance();
  }, [fetchProvenance]);

  // A finished (re)build fires 'library-updated' (useMasterBuilds) — re-fetch
  // without blanking the block so recipe/created reflect the rewritten master,
  // and drop the rebuild busy flag.
  useEffect(() => {
    const h = () => { setRebuildBusy(false); fetchProvenance(); };
    window.addEventListener('library-updated', h);
    return () => window.removeEventListener('library-updated', h);
  }, [fetchProvenance]);

  if (prov === 'loading') return null;
  if (prov === null) {
    return (
      <div className="mt-2 text-xs">
        <span className="px-1.5 py-0.5 rounded bg-surface-hover text-content-muted">imported master</span>
      </div>
    );
  }

  // Re-integrate the master in place from its original source frames. The
  // invoke returns as soon as the build thread is spawned; progress and the
  // completion notification ride the same master-build events as a fresh
  // build, and completion fires 'library-updated' (handled above).
  const startRebuild = async () => {
    setRebuildError(null);
    setRebuildBusy(true);
    try {
      await api.invoke('rebuild_master', { masterSetId: setId });
    } catch (e) {
      console.error('Failed to start master rebuild:', e);
      setRebuildError(String(e));
      setRebuildBusy(false);
    }
  };

  const startArchive = async () => {
    if (prov.sourceSetId == null) return;
    setArchiveStartError(null);
    try {
      const opId = await api.invoke<number>('archive_calibration_originals', { calibrationSetId: prov.sourceSetId });
      setArchiveOpId(opId);
    } catch (e) {
      setArchiveStartError(String(e));
    }
  };

  // Restore targets the SOURCE set (the raw calibration set whose originals
  // got zipped), same as `startArchive` above — `setId` here is the MASTER
  // set id, `prov.sourceSetId` is what `restore_calibration_originals`
  // (and `archive_calibration_originals`) actually operate on.
  const startRestore = async () => {
    if (prov.sourceSetId == null) return;
    setRestoreStartError(null);
    try {
      const opId = await api.invoke<number>('restore_calibration_originals', { calibrationSetId: prov.sourceSetId });
      setRestoreOpId(opId);
    } catch (e) {
      setRestoreStartError(String(e));
    }
  };

  // Escape hatch from the missing-zip dead end: clears the stale archive
  // markers (backend re-checks the zip really is gone — 409 otherwise).
  // window.confirm matches this codebase's destructive-action pattern.
  const forgetArchive = async () => {
    if (prov.sourceSetId == null) return;
    if (!window.confirm(
      'Forget this archive?\n\n' +
      'The catalog will mark the original files as NOT archived. Since the zip is gone, ' +
      'the files themselves cannot be recovered — they will show as missing on disk. ' +
      'This only clears the stale bookkeeping; nothing is deleted.',
    )) return;
    setForgetError(null);
    setForgetBusy(true);
    try {
      await api.invoke<number>('clear_stale_archive_markers', { calibrationSetId: prov.sourceSetId });
      fetchProvenance();
    } catch (e) {
      setForgetError(String(e));
    } finally {
      setForgetBusy(false);
    }
  };

  return (
    <div className="mt-2 text-xs space-y-1">
      <span className="px-1.5 py-0.5 rounded bg-accent/20 text-accent">built in Athenaeum</span>
      <div><span className="text-content-muted">Source set:</span> <span className="text-content">#{prov.sourceSetId ?? '—'} ({prov.memberCount} frames)</span></div>
      <div><span className="text-content-muted">Recipe:</span> <span className="text-content" title={prov.recipeJson}>{describeRecipeJson(prov.recipeJson)}</span></div>
      <div><span className="text-content-muted">Created:</span> <span className="text-content">{prov.createdAt}</span></div>
      <div className="mt-1">
        <button
          onClick={(e) => { e.stopPropagation(); void startRebuild(); }}
          disabled={rebuildBusy || !prov.sourceFramesOnDisk}
          title={prov.sourceFramesOnDisk
            ? 'Re-integrate this master in place from its original source frames'
            : 'Source frames are not on disk — restore originals first'}
          className="px-2 py-0.5 bg-surface-hover hover:brightness-110 rounded text-content disabled:opacity-50 disabled:cursor-not-allowed"
        >
          {rebuildBusy ? 'Rebuilding…' : 'Rebuild master'}
        </button>
        {rebuildError && <div className="text-error mt-1">{rebuildError}</div>}
      </div>
      <div>
        <span className="text-content-muted">Originals:</span>{' '}
        {prov.originalsArchived ? (
          <>
            {prov.archiveZipPath ? (
              <span className="text-content font-mono" title={prov.archiveZipPath}>
                {provenanceBasename(prov.archiveZipPath)}
              </span>
            ) : (
              <span className="text-content">archived to zip</span>
            )}
            {prov.archiveZipMissing ? (
              <>
                <div className="mt-1 text-error">archive file is missing on disk (deleted?)</div>
                <div className="mt-1">
                  <button
                    onClick={(e) => { e.stopPropagation(); void forgetArchive(); }}
                    disabled={forgetBusy}
                    className="px-2 py-0.5 bg-surface-hover hover:brightness-110 rounded text-content disabled:opacity-50"
                  >
                    {forgetBusy ? 'Clearing…' : 'Forget archive (clear markers)'}
                  </button>
                </div>
                {forgetError && <div className="text-error mt-1">{forgetError}</div>}
              </>
            ) : restoreOpId != null ? (
              <div className="mt-1 max-w-xs" onClick={e => e.stopPropagation()}>
                <ArchiveProgress
                  operationId={restoreOpId}
                  onFinished={() => fetchProvenance()}
                  onClose={() => setRestoreOpId(null)}
                />
              </div>
            ) : (
              <div className="mt-1">
                <button
                  onClick={(e) => { e.stopPropagation(); void startRestore(); }}
                  className="px-2 py-0.5 bg-surface-hover hover:brightness-110 rounded text-content disabled:opacity-50"
                >
                  Restore originals
                </button>
              </div>
            )}
            {restoreStartError && <div className="text-error mt-1">{restoreStartError}</div>}
          </>
        ) : prov.sourceFramesOnDisk && prov.sourceSetId != null ? (
          archiveOpId != null ? (
            <div className="mt-1 max-w-xs" onClick={e => e.stopPropagation()}>
              <ArchiveProgress
                operationId={archiveOpId}
                onFinished={() => fetchProvenance()}
                onClose={() => setArchiveOpId(null)}
              />
            </div>
          ) : (
            <button
              onClick={(e) => { e.stopPropagation(); void startArchive(); }}
              className="px-2 py-0.5 bg-surface-hover hover:brightness-110 rounded text-content disabled:opacity-50"
            >
              Archive originals to zip
            </button>
          )
        ) : (
          <span className="text-warning">missing on disk</span>
        )}
      </div>
      {archiveStartError && <div className="text-error">{archiveStartError}</div>}
    </div>
  );
}

// ── Consumer chip strip ─────────────────────────────────────────────────────
//
// Lazy-loaded list of frame sets (objects) that ultimately consume a
// calibration set, rendered as clickable chips. First CONSUMER_INITIAL_VISIBLE
// chips are shown by default; the rest fold behind a "Show all (N)" toggle.

type ConsumerChipState =
  | { status: 'idle' }
  | { status: 'loading' }
  | { status: 'loaded'; consumers: CalibrationSetConsumer[] }
  | { status: 'error'; message: string };

function ConsumerChipStrip({
  state,
  showAll,
  onToggleShowAll,
  onNavigate,
  onRetry,
}: {
  state: ConsumerChipState;
  showAll: boolean;
  onToggleShowAll: () => void;
  onNavigate: (frameSetId: number) => void;
  onRetry: () => void;
}) {
  if (state.status === 'idle' || state.status === 'loading') {
    return (
      <div className="mt-2 pt-2 border-t border-border/40 text-xs text-content-muted">
        Loading objects using this set…
      </div>
    );
  }
  if (state.status === 'error') {
    return (
      <div className="mt-2 pt-2 border-t border-border/40 text-xs text-error flex items-center gap-2">
        <span>Failed to load consumers: {state.message}</span>
        <button
          type="button"
          onClick={e => { e.stopPropagation(); onRetry(); }}
          className="px-2 py-0.5 rounded bg-surface-hover hover:brightness-110 text-content"
        >
          Retry
        </button>
      </div>
    );
  }
  const { consumers } = state;
  if (consumers.length === 0) {
    return (
      <div className="mt-2 pt-2 border-t border-border/40 text-xs text-content-muted italic">
        Not used by any object yet.
      </div>
    );
  }
  const visible = showAll ? consumers : consumers.slice(0, CONSUMER_INITIAL_VISIBLE);
  const hiddenCount = consumers.length - visible.length;

  return (
    <div className="mt-2 pt-2 border-t border-border/40">
      <div className="text-xs text-content-muted mb-1">
        Used in {consumers.length} {consumers.length === 1 ? 'object' : 'objects'}:
      </div>
      <div className="flex flex-wrap gap-1.5 items-center">
        {visible.map(c => (
          <button
            key={c.frameSetId}
            type="button"
            onClick={e => { e.stopPropagation(); onNavigate(c.frameSetId); }}
            title={`Open frame set #${c.frameSetId}`}
            className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-xs bg-accent/10 hover:bg-accent/20 text-accent border border-accent/30 transition-colors cursor-pointer"
          >
            <span className="font-medium">{c.name ?? `#${c.frameSetId}`}</span>
            {c.dateObsStart && (
              <span className="text-content-muted">· {formatConsumerDate(c.dateObsStart)}</span>
            )}
          </button>
        ))}
        {hiddenCount > 0 && !showAll && (
          <button
            type="button"
            onClick={e => { e.stopPropagation(); onToggleShowAll(); }}
            className="inline-flex items-center px-1.5 py-0.5 rounded text-xs text-content-muted hover:text-content hover:underline"
          >
            Show all ({consumers.length}) ▾
          </button>
        )}
        {showAll && consumers.length > CONSUMER_INITIAL_VISIBLE && (
          <button
            type="button"
            onClick={e => { e.stopPropagation(); onToggleShowAll(); }}
            className="inline-flex items-center px-1.5 py-0.5 rounded text-xs text-content-muted hover:text-content hover:underline"
          >
            Show less ▴
          </button>
        )}
      </div>
    </div>
  );
}

function formatConsumerDate(iso: string): string {
  // date_obs_start is ISO 8601 (sometimes just the date); render YYYY-MM-DD.
  // Fall back to the raw value if parsing fails.
  try {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso.slice(0, 10);
    return format(d, 'yyyy-MM-dd');
  } catch {
    return iso.slice(0, 10);
  }
}
