// src/components/calibration/CalibrationTableView.tsx
import { useState, useMemo, useRef, useCallback, useEffect } from 'react';
import { useNavigate } from 'react-router-dom';
import { ExternalLink } from 'lucide-react';
import type {
  CalibrationHierarchyView as CalibrationHierarchyViewData,
  CalibrationFilterGroup,
  CalibrationSetWithFrameCount,
  SubCalibrationDetail,
  FrameAnalysis,
} from '../../types/models';
import type { EnrichedLightFrame } from './LightsAnalysisTable';
import { ReassignFramePanel } from './ReassignFramePanel';

type CalSetKind = 'Flat' | 'Dark' | 'Bias';

function tabForCalSet(kind: CalSetKind, isMaster: boolean): 'flats' | 'darks' | 'master-flats' | 'master-darks' {
  if (kind === 'Flat') return isMaster ? 'master-flats' : 'flats';
  return isMaster ? 'master-darks' : 'darks';
}

interface CalibrationTableViewProps {
  data: CalibrationHierarchyViewData;
  allFrames: EnrichedLightFrame[];
  visibleFrameIds?: Set<number>;
  analysisData?: Map<number, FrameAnalysis>;
  onManualCalibration?: (frameIds: number[]) => void;
  onBlink?: (frameIds: number[]) => void;
  reassignMode?: boolean;
  /** When set, scroll to + highlight the matching set in the Flat/Dark/Bias table on mount. */
  highlightCalSet?: { setId: number; kind: 'flat' | 'dark' | 'bias' } | null;
  /** Called once the highlight has been forwarded to the in-page handlers. */
  onHighlightConsumed?: () => void;
}

// ── Section colors (per spec) ──────────────────────────────────────────────
const SECTION_COLORS = {
  lights: '#5e81ac',
  flats: '#88c0d0',
  darks: '#b48ead',
  bias: '#d08770',
} as const;

// ── Sort helpers ───────────────────────────────────────────────────────────

type SortDir = 'asc' | 'desc';

function sortRows<T>(rows: T[], field: keyof T | null, dir: SortDir): T[] {
  if (!field) return rows;
  return [...rows].sort((a, b) => {
    const av = a[field];
    const bv = b[field];
    if (av == null && bv == null) return 0;
    if (av == null) return dir === 'asc' ? 1 : -1;
    if (bv == null) return dir === 'asc' ? -1 : 1;
    if (typeof av === 'number' && typeof bv === 'number') {
      return dir === 'asc' ? av - bv : bv - av;
    }
    const as = String(av);
    const bs = String(bv);
    return dir === 'asc' ? as.localeCompare(bs) : bs.localeCompare(as);
  });
}

// ── Date/temp helpers (ported from CalibrationGraphView) ───────────────────

// Standard date-time format: YYYY-MM-DD HH:MM:SS
// This format MUST be consistent across the entire app (analysis tab, blink viewer, calibration tab)
const pad = (n: number) => String(n).padStart(2, '0');
function fmtDateOnly(d: Date): string {
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}
function fmtTimeOnly(d: Date): string {
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function formatSetDateRange(dateStart: string, dateEnd: string): string {
  const s = new Date(dateStart);
  const e = new Date(dateEnd);
  if (s.toDateString() === e.toDateString()) {
    return `${fmtDateOnly(s)} ${fmtTimeOnly(s)} – ${fmtTimeOnly(e)}`;
  }
  return `${fmtDateOnly(s)} – ${fmtDateOnly(e)}`;
}

function formatFrameDateRange(frames: { date_obs: string | null }[]): string {
  const dates = frames.map(f => f.date_obs).filter((d): d is string => d != null).sort();
  if (dates.length === 0) return '—';
  const s = new Date(dates[0]);
  const e = new Date(dates[dates.length - 1]);
  if (s.toDateString() === e.toDateString()) {
    return `${fmtDateOnly(s)} ${fmtTimeOnly(s)} – ${fmtTimeOnly(e)}`;
  }
  return `${fmtDateOnly(s)} – ${fmtDateOnly(e)}`;
}

function formatTempRange(temps: (number | null)[]): string {
  const valid = temps.filter((t): t is number => t != null);
  if (valid.length === 0) return '—';
  const min = Math.min(...valid);
  const max = Math.max(...valid);
  if (Math.abs(max - min) < 0.1) return `${min.toFixed(1)}°`;
  return `${min.toFixed(1)}–${max.toFixed(1)}°`;
}

function formatIntegration(seconds: number): string {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = Math.round(seconds % 60);
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

// ── Derived data types ─────────────────────────────────────────────────────

export interface LightRow {
  /** Unique key for this row */
  rowKey: string;
  frameCount: number;
  filterDisplay: string;
  filter: string | null;
  exptime: number | null;
  tempRange: string;
  dateRange: string;
  camera: string | null;
  focallen: number | null;
  flatSetId: number | null;
  flatSet: CalibrationSetWithFrameCount | null;
  darkSetId: number | null;
  darkSet: CalibrationSetWithFrameCount | null;
  status: 'complete' | 'partial' | 'none';
  frames: EnrichedLightFrame[];
  allFrameIds: number[];
  avgFwhm: number | null;
  avgEcc: number | null;
  avgSnr: number | null;
  totalIntegration: number | null;
  // for sorting
  sortTemp: number | null;
  sortDate: string | null;
}

interface FlatRow {
  setId: number;
  filter: string | null;
  exptime: number | null;
  tempRange: string;
  binning: string | null;
  frameCount: number;
  dateRange: string;
  camera: string | null;
  focallen: number | null;
  subCalSetId: number | null;
  subCalType: 'Dark' | 'DarkFlat' | 'Bias' | null;
  gain: number | null;
  offset: number | null;
  isMaster: boolean;
  /** Light frame count using this flat */
  usedByLightCount: number;
  usedByLightFrameIds: number[];
  // camera reference for match badges
  cameraGain: number | null;
  cameraBinning: string | null;
  cameraOffset: number | null;
  // for sorting
  sortTemp: number | null;
  sortDate: string | null;
}

interface DarkRow {
  setId: number;
  exptime: number | null;
  tempRange: string;
  binning: string | null;
  frameCount: number;
  dateRange: string;
  camera: string | null;
  biasSetId: number | null;
  gain: number | null;
  offset: number | null;
  isMaster: boolean;
  usedByLightCount: number;
  usedByLightFrameIds: number[];
  cameraGain: number | null;
  cameraBinning: string | null;
  cameraOffset: number | null;
  sortTemp: number | null;
  sortDate: string | null;
}

interface BiasRow {
  setId: number;
  binning: string | null;
  frameCount: number;
  dateRange: string;
  camera: string | null;
  gain: number | null;
  offset: number | null;
  isMaster: boolean;
  /** Light frame count (indirect via darks) */
  usedByLightCount: number;
  usedByLightFrameIds: number[];
  cameraGain: number | null;
  cameraBinning: string | null;
  cameraOffset: number | null;
  sortDate: string | null;
}

interface DerivedTableData {
  lightRows: LightRow[];
  flatRows: FlatRow[];
  darkRows: DarkRow[];
  biasRows: BiasRow[];
}

// ── deriveGraph (adapted from CalibrationGraphView) ────────────────────────

function deriveTableData(
  data: CalibrationHierarchyViewData,
  allFrames: EnrichedLightFrame[],
  visibleFrameIds: Set<number> | undefined,
  analysisData: Map<number, FrameAnalysis> | undefined,
): DerivedTableData {
  // Build a camera → filterGroup map, merging across dates
  const cameraMap = new Map<string, {
    gain: number | null; offset: number | null; binning: string | null;
    filterGroups: Map<string, { fg: CalibrationFilterGroup; frames: EnrichedLightFrame[] }>;
  }>();

  for (const dg of data.date_groups) {
    for (const cg of dg.camera_groups) {
      if (!cameraMap.has(cg.instrume)) {
        const first = cg.filter_groups[0]?.light_frames[0];
        cameraMap.set(cg.instrume, {
          gain: first?.gain ?? null,
          offset: first?.offset ?? null,
          binning: first?.binning ?? null,
          filterGroups: new Map(),
        });
      }
      const cam = cameraMap.get(cg.instrume)!;
      for (const fg of cg.filter_groups) {
        const key = `${fg.filter ?? ''}::${fg.exptime ?? ''}`;
        if (!cam.filterGroups.has(key)) cam.filterGroups.set(key, { fg, frames: [] });
        const entry = cam.filterGroups.get(key)!;
        // Merge calibration sets: accumulate unique sets by ID
        const existingFlatIds = new Set(entry.fg.flat_sets.map(fs => fs.set.id));
        const newFlats = fg.flat_sets.filter(fs => !existingFlatIds.has(fs.set.id));
        if (newFlats.length > 0) entry.fg = { ...entry.fg, flat_sets: [...entry.fg.flat_sets, ...newFlats] };
        const existingDarkIds = new Set(entry.fg.dark_sets.map(ds => ds.set.id));
        const newDarks = fg.dark_sets.filter(ds => !existingDarkIds.has(ds.set.id));
        if (newDarks.length > 0) entry.fg = { ...entry.fg, dark_sets: [...entry.fg.dark_sets, ...newDarks] };
        for (const lf of fg.light_frames) {
          const enriched = allFrames.find(f => f.frame_id === lf.frame_id);
          if (enriched) entry.frames.push(enriched);
        }
      }
    }
  }

  // Build frame lookup for resolving per-set camera reference params
  const frameById = new Map<number, EnrichedLightFrame>();
  for (const f of allFrames) frameById.set(f.frame_id, f);

  // Derive camera reference (gain/binning/offset) from linked light frames
  function linkedCameraParams(lightFrameIds: number[]): { cameraGain: number | null; cameraBinning: string | null; cameraOffset: number | null } {
    for (const id of lightFrameIds) {
      const f = frameById.get(id);
      if (f) return { cameraGain: f.gain, cameraBinning: f.binning, cameraOffset: f.offset };
    }
    return { cameraGain: null, cameraBinning: null, cameraOffset: null };
  }

  // Deduplicated flat/dark/bias sets
  const flatSetMap = new Map<number, { set: CalibrationSetWithFrameCount; lightFrameIds: number[] }>();
  const darkSetMap = new Map<number, { set: CalibrationSetWithFrameCount; lightFrameIds: number[]; biasSubCal: SubCalibrationDetail | null }>();
  const biasSetMap = new Map<number, { set: CalibrationSetWithFrameCount; lightFrameIds: number[] }>();

  // Build light rows
  const lightRows: LightRow[] = [];

  for (const [instrume, camData] of cameraMap) {
    for (const [, { fg, frames }] of camData.filterGroups) {
      // Filter by visibleFrameIds if set
      const visibleFrames = visibleFrameIds
        ? frames.filter(f => visibleFrameIds.has(f.frame_id))
        : frames;
      if (visibleFrames.length === 0) continue;

      // Group by (flat_set_id, dark_set_id)
      const comboMap = new Map<string, {
        darkId: number | null;
        flatId: number | null;
        frames: EnrichedLightFrame[];
      }>();
      for (const frame of visibleFrames) {
        const darkId = frame.calibration_status.dark_set_id;
        const flatId = frame.calibration_status.flat_set_id;
        const key = `${flatId ?? 'null'}::${darkId ?? 'null'}`;
        if (!comboMap.has(key)) comboMap.set(key, { darkId, flatId, frames: [] });
        comboMap.get(key)!.frames.push(frame);
      }

      for (const [, combo] of comboMap) {
        const groupFrames = combo.frames;
        const hasFlat = combo.flatId != null;
        const hasDark = combo.darkId != null;
        const status: LightRow['status'] =
          hasFlat && hasDark ? 'complete' :
          hasFlat || hasDark ? 'partial' :
          'none';

        const flatSet = combo.flatId != null
          ? fg.flat_sets.find(fs => fs.set.id === combo.flatId) ?? null
          : null;
        const darkSet = combo.darkId != null
          ? fg.dark_sets.find(ds => ds.set.id === combo.darkId) ?? null
          : null;

        // Collect flat/dark/bias sets globally
        if (flatSet && combo.flatId != null) {
          if (!flatSetMap.has(combo.flatId)) {
            flatSetMap.set(combo.flatId, { set: flatSet, lightFrameIds: [] });
          }
          flatSetMap.get(combo.flatId)!.lightFrameIds.push(...groupFrames.map(f => f.frame_id));
        }
        if (darkSet && combo.darkId != null) {
          const biasSubCal = darkSet.sub_calibration.find(sc => sc.calibration_type === 'Bias') ?? null;
          if (!darkSetMap.has(combo.darkId)) {
            darkSetMap.set(combo.darkId, { set: darkSet, lightFrameIds: [], biasSubCal });
          }
          darkSetMap.get(combo.darkId)!.lightFrameIds.push(...groupFrames.map(f => f.frame_id));

          // Collect bias
          if (biasSubCal) {
            const biasId = biasSubCal.set.id;
            if (biasId != null) {
              if (!biasSetMap.has(biasId)) {
                // Create a minimal CalibrationSetWithFrameCount for the bias set
                const biasSetWithCount: CalibrationSetWithFrameCount = {
                  set: biasSubCal.set,
                  frame_count: biasSubCal.set.frame_count,
                  frame_ids: [],
                  warnings: [],
                  sub_calibration: [],
                };
                biasSetMap.set(biasId, { set: biasSetWithCount, lightFrameIds: [] });
              }
              biasSetMap.get(biasId)!.lightFrameIds.push(...groupFrames.map(f => f.frame_id));
            }
          }
        }

        const temps = groupFrames.map(f => f.ccd_temp);
        const dates = groupFrames.map(f => f.date_obs).filter((d): d is string => d != null).sort();

        // Compute metrics from analysis data
        let avgFwhm: number | null = null;
        let avgEcc: number | null = null;
        let avgSnr: number | null = null;
        if (analysisData) {
          let fwhmSum = 0, fwhmCount = 0;
          let eccSum = 0, eccCount = 0;
          let snrSumSq = 0, snrCount = 0;
          for (const f of groupFrames) {
            const a = analysisData.get(f.frame_id);
            if (a) {
              fwhmSum += a.median_fwhm; fwhmCount++;
              eccSum += a.median_eccentricity; eccCount++;
              const linear = Math.pow(10, a.frame_snr / 20);
              snrSumSq += linear * linear; snrCount++;
            }
          }
          if (fwhmCount > 0) avgFwhm = fwhmSum / fwhmCount;
          if (eccCount > 0) avgEcc = eccSum / eccCount;
          if (snrCount > 0) avgSnr = 20 * Math.log10(Math.sqrt(snrSumSq));
        }

        lightRows.push({
          rowKey: `${fg.filter ?? ''}::${fg.exptime ?? ''}::${combo.flatId ?? 'null'}::${combo.darkId ?? 'null'}`,
          frameCount: groupFrames.length,
          filterDisplay: fg.filter_display,
          filter: fg.filter,
          exptime: fg.exptime,
          tempRange: formatTempRange(temps),
          dateRange: formatFrameDateRange(groupFrames),
          camera: instrume,
          focallen: groupFrames[0]?.focallen ?? null,
          flatSetId: combo.flatId,
          flatSet: flatSet ?? null,
          darkSetId: combo.darkId,
          darkSet: darkSet ?? null,
          status,
          frames: groupFrames,
          allFrameIds: groupFrames.map(f => f.frame_id),
          avgFwhm,
          avgEcc,
          avgSnr,
          totalIntegration: fg.exptime != null ? groupFrames.length * fg.exptime : null,
          sortTemp: temps.filter((t): t is number => t != null).reduce((a, b) => a + b, 0) / (temps.filter(t => t != null).length || 1) || null,
          sortDate: dates[0] ?? null,
        });
      }
    }
  }

  // Build flat rows
  const flatRows: FlatRow[] = [];
  for (const [setId, { set, lightFrameIds }] of flatSetMap) {
    const s = set.set;
    const subCal = set.sub_calibration.find(sc => sc.calibration_type === 'DarkFlat')
      ?? set.sub_calibration.find(sc => sc.calibration_type === 'Dark')
      ?? set.sub_calibration.find(sc => sc.calibration_type === 'Bias')
      ?? null;
    const camRef = linkedCameraParams(lightFrameIds);
    flatRows.push({
      setId,
      filter: s.filter,
      exptime: s.exptime,
      tempRange: formatTempRange([s.ccd_temp]),
      binning: s.binning,
      frameCount: s.frame_count,
      dateRange: formatSetDateRange(s.date_start, s.date_end),
      camera: s.instrume,
      focallen: s.focallen,
      subCalSetId: subCal?.set.id ?? null,
      // CalibrationLink.calibration_type is a plain `string` on the wire
      // (Rust field has no enum, just a doc-commented convention); narrowed
      // here since a sub-calibration link is always Dark/DarkFlat/Bias.
      subCalType: (subCal?.calibration_type ?? null) as 'Dark' | 'DarkFlat' | 'Bias' | null,
      gain: s.gain,
      offset: s.offset,
      isMaster: s.is_master,
      usedByLightCount: lightFrameIds.length,
      usedByLightFrameIds: lightFrameIds,
      ...camRef,
      sortTemp: s.ccd_temp,
      sortDate: s.date_start,
    });
  }

  // Build dark rows
  const darkRows: DarkRow[] = [];
  for (const [setId, { set, lightFrameIds, biasSubCal }] of darkSetMap) {
    const s = set.set;
    const camRef = linkedCameraParams(lightFrameIds);
    darkRows.push({
      setId,
      exptime: s.exptime,
      tempRange: formatTempRange([s.ccd_temp]),
      binning: s.binning,
      frameCount: s.frame_count,
      dateRange: formatSetDateRange(s.date_start, s.date_end),
      camera: s.instrume,
      biasSetId: biasSubCal?.set.id ?? null,
      gain: s.gain,
      offset: s.offset,
      isMaster: s.is_master,
      usedByLightCount: lightFrameIds.length,
      usedByLightFrameIds: lightFrameIds,
      ...camRef,
      sortTemp: s.ccd_temp,
      sortDate: s.date_start,
    });
  }

  // Build bias rows
  const biasRows: BiasRow[] = [];
  for (const [setId, { set, lightFrameIds }] of biasSetMap) {
    const s = set.set;
    const camRef = linkedCameraParams(lightFrameIds);
    biasRows.push({
      setId,
      binning: s.binning,
      frameCount: s.frame_count,
      dateRange: formatSetDateRange(s.date_start, s.date_end),
      camera: s.instrume,
      gain: s.gain,
      offset: s.offset,
      isMaster: s.is_master,
      usedByLightCount: lightFrameIds.length,
      usedByLightFrameIds: lightFrameIds,
      ...camRef,
      sortDate: s.date_start,
    });
  }

  return { lightRows, flatRows, darkRows, biasRows };
}

// ── Shared UI helpers ──────────────────────────────────────────────────────

function MasterBadge() {
  return (
    <span className="ml-1 text-[9px] font-bold bg-accent/15 text-accent px-1 py-px rounded align-middle">M</span>
  );
}

function MatchBadge({
  label,
  value,
  matches,
}: {
  label: string;
  value: number | string | null;
  matches: boolean | null; // null = can't determine
}) {
  if (value == null) return <span className="text-content-muted text-sm">—</span>;
  const cls = matches === null ? 'text-content-muted' : matches ? 'text-success' : 'text-warning';
  return (
    <span className={`text-sm ${cls}`} title={label}>
      {typeof value === 'number' ? value : value}
    </span>
  );
}

function StatusDot({ status }: { status: LightRow['status'] }) {
  const color =
    status === 'complete' ? '#a3be8c' :
    status === 'partial' ? '#ebcb8b' :
    '#bf616a';
  return (
    <span
      className="inline-block w-2 h-2 rounded-full flex-shrink-0"
      style={{ backgroundColor: color }}
      title={status === 'complete' ? 'Flat + Dark' : status === 'partial' ? 'Partial calibration' : 'No calibration'}
    />
  );
}

function MissingMark() {
  return <span className="text-error font-bold text-sm">✗</span>;
}

// ── Section header ─────────────────────────────────────────────────────────

function SectionHeader({
  title,
  count,
  color,
}: {
  title: string;
  count: number;
  color: string;
}) {
  return (
    <div
      className="w-full flex items-center gap-2 px-3 py-1.5"
      style={{ backgroundColor: color + '22', borderLeft: `3px solid ${color}` }}
    >
      <span className="text-xs font-bold uppercase tracking-wider" style={{ color }}>
        {title}
      </span>
      <span
        className="ml-1 text-[10px] font-semibold rounded px-1.5 py-px"
        style={{ backgroundColor: color + '33', color }}
      >
        {count}
      </span>
    </div>
  );
}

// ── Sortable column header cell ────────────────────────────────────────────

function SortTh<F extends string>({
  field,
  label,
  sortField,
  sortDir,
  onSort,
  className = '',
}: {
  field: F;
  label: string;
  sortField: F | null;
  sortDir: SortDir;
  onSort: (f: F) => void;
  className?: string;
}) {
  const active = sortField === field;
  return (
    <th
      onClick={() => onSort(field)}
      className={`px-1.5 py-1.5 text-left text-xs font-semibold text-content-secondary cursor-pointer select-none whitespace-nowrap hover:text-content transition-colors sticky top-0 z-10 bg-surface-elevated ${className}`}
    >
      {label}
      {active && (
        <span className="ml-0.5 text-accent">
          {sortDir === 'asc' ? '↑' : '↓'}
        </span>
      )}
    </th>
  );
}

// ── Clickable set ID badge ─────────────────────────────────────────────────

function SetIdBadge({
  id,
  color,
  onClick,
  isMaster,
  linkLike,
  title,
}: {
  id: number;
  color: string;
  onClick?: (e: React.MouseEvent<HTMLButtonElement>) => void;
  isMaster?: boolean;
  linkLike?: boolean;
  title?: string;
}) {
  return (
    <button
      onClick={onClick}
      className={`font-mono text-xs font-bold rounded px-1 py-px transition-colors hover:brightness-110 inline-flex items-center gap-0.5 ${linkLike ? 'cursor-pointer hover:underline' : ''}`}
      style={{ color, backgroundColor: color + '1a' }}
      title={title ?? `Set #${id}`}
    >
      <span>#{id}</span>
      {isMaster && <MasterBadge />}
      {linkLike && <ExternalLink size={10} className="opacity-70" />}
    </button>
  );
}

// ── Lights Table ───────────────────────────────────────────────────────────

type LightSortField = 'frameCount' | 'filter' | 'exptime' | 'sortTemp' | 'sortDate' | 'camera' | 'focallen' | 'flatSetId' | 'darkSetId' | 'avgFwhm' | 'avgEcc' | 'avgSnr' | 'totalIntegration';

function LightsTable({
  rows,
  highlightedRowKeys,
  selectedRowKey,
  onRowClick,
  onFlatClick,
  onDarkClick,
  compact,
}: {
  rows: LightRow[];
  highlightedRowKeys: Set<string>;
  selectedRowKey?: string | null;
  onRowClick: (row: LightRow) => void;
  onFlatClick: (flatSetId: number) => void;
  onDarkClick: (darkSetId: number) => void;
  compact?: boolean;
}) {
  const [sortField, setSortField] = useState<LightSortField | null>('sortDate');
  const [sortDir, setSortDir] = useState<SortDir>('desc');

  const handleSort = useCallback((field: LightSortField) => {
    if (field === sortField) {
      setSortDir(d => d === 'asc' ? 'desc' : 'asc');
    } else {
      setSortField(field);
      setSortDir('asc');
    }
  }, [sortField]);

  const sorted = useMemo(
    () => sortRows(rows, sortField as keyof LightRow | null, sortDir),
    [rows, sortField, sortDir]
  );

  if (sorted.length === 0) {
    return <div className="px-4 py-3 text-xs text-content-muted italic">No light frames visible.</div>;
  }

  const thProps = { sortField, sortDir, onSort: handleSort };

  return (
    <div>
      <table className="w-full" style={{ borderCollapse: 'separate', borderSpacing: 0 }}>
        <thead>
          <tr className="border-b border-border/40">
            <th className="px-1.5 py-1.5 w-5 sticky top-0 z-10 bg-surface-elevated" />
            <SortTh field="frameCount" label="Frames" {...thProps} />
            <SortTh field="filter" label="Filter" {...thProps} />
            <SortTh field="exptime" label="Exp" {...thProps} />
            <SortTh field="sortTemp" label="Temp" {...thProps} />
            <SortTh field="sortDate" label="Date Range" {...thProps} />
            <SortTh field="camera" label="Camera" {...thProps} />
            <SortTh field="focallen" label="FL" {...thProps} />
            <SortTh field="flatSetId" label="Flat" {...thProps} />
            <SortTh field="darkSetId" label="Dark" {...thProps} />
            {!compact && <SortTh field="avgFwhm" label="FWHM" {...thProps} />}
            {!compact && <SortTh field="avgEcc" label="Ecc" {...thProps} />}
            {!compact && <SortTh field="avgSnr" label="SNR" {...thProps} />}
            {!compact && <SortTh field="totalIntegration" label="Total" {...thProps} />}
          </tr>
        </thead>
        <tbody>
          {sorted.map(row => {
            const highlighted = highlightedRowKeys.has(row.rowKey);
            const isSelected = selectedRowKey === row.rowKey;
            const rowBg =
              isSelected ? 'bg-warning/20 ring-1 ring-inset ring-warning/40' :
              highlighted ? 'bg-accent/20' :
              row.status === 'complete' ? 'bg-success/[0.04]' :
              row.status === 'partial' ? 'bg-warning/[0.04]' :
              'bg-error/[0.04]';
            return (
              <tr
                key={row.rowKey}
                onClick={() => onRowClick(row)}
                className={`border-b border-border/20 cursor-pointer hover:bg-surface-hover/50 transition-colors ${rowBg}`}
                style={{ height: 28 }}
              >
                <td className="px-1.5 py-1">
                  <StatusDot status={row.status} />
                </td>
                <td className="px-1.5 py-1 text-sm text-content-secondary tabular-nums">{row.frameCount}</td>
                <td className="px-1.5 py-1 text-sm text-content-secondary">
                  {row.filter ?? <span className="text-content-muted italic">—</span>}
                </td>
                <td className="px-1.5 py-1 text-sm text-content-secondary">
                  {row.exptime != null ? `${row.exptime}s` : '—'}
                </td>
                <td className="px-1.5 py-1 text-sm text-content-secondary">{row.tempRange}</td>
                <td className="px-1.5 py-1 text-sm font-mono text-content-secondary whitespace-nowrap">{row.dateRange}</td>
                <td className="px-1.5 py-1 text-sm text-content-secondary whitespace-nowrap">{row.camera ?? '—'}</td>
                <td className="px-1.5 py-1 text-sm text-content-secondary">{row.focallen != null ? `${row.focallen}mm` : '—'}</td>
                <td className="px-1.5 py-1 text-sm">
                  {row.flatSetId != null ? (
                    <SetIdBadge
                      id={row.flatSetId}
                      color={SECTION_COLORS.flats}
                      onClick={e => { e.stopPropagation(); onFlatClick(row.flatSetId!); }}
                    />
                  ) : <MissingMark />}
                </td>
                <td className="px-1.5 py-1 text-sm">
                  {row.darkSetId != null ? (
                    <SetIdBadge
                      id={row.darkSetId}
                      color={SECTION_COLORS.darks}
                      onClick={e => { e.stopPropagation(); onDarkClick(row.darkSetId!); }}
                    />
                  ) : <MissingMark />}
                </td>
                {!compact && (
                  <td className="px-1.5 py-1 text-sm text-content-secondary">
                    {row.avgFwhm != null ? row.avgFwhm.toFixed(2) : '—'}
                  </td>
                )}
                {!compact && (
                  <td className="px-1.5 py-1 text-sm text-content-secondary">
                    {row.avgEcc != null ? row.avgEcc.toFixed(2) : '—'}
                  </td>
                )}
                {!compact && (
                  <td className="px-1.5 py-1 text-sm text-content-secondary">
                    {row.avgSnr != null ? row.avgSnr.toFixed(1) : '—'}
                  </td>
                )}
                {!compact && (
                  <td className="px-1.5 py-1 text-sm text-content-secondary whitespace-nowrap">
                    {row.totalIntegration != null ? formatIntegration(row.totalIntegration) : '—'}
                  </td>
                )}
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

// ── Flats Table ────────────────────────────────────────────────────────────

type FlatSortField = 'setId' | 'filter' | 'exptime' | 'sortTemp' | 'binning' | 'frameCount' | 'sortDate' | 'camera' | 'focallen' | 'subCalSetId' | 'gain' | 'offset' | 'usedByLightCount';

function FlatsTable({
  rows,
  highlightedSetIds,
  onRowClick,
  onSubCalClick,
  onLightsClick,
  onJumpToEquipment,
  compact,
}: {
  rows: FlatRow[];
  highlightedSetIds: Set<number>;
  onRowClick: (row: FlatRow) => void;
  onSubCalClick: (id: number, type: 'Dark' | 'DarkFlat' | 'Bias') => void;
  onLightsClick: (ids: number[]) => void;
  onJumpToEquipment: (row: { camera: string | null; setId: number; kind: CalSetKind; isMaster: boolean }) => void;
  compact?: boolean;
}) {
  const [sortField, setSortField] = useState<FlatSortField | null>('sortDate');
  const [sortDir, setSortDir] = useState<SortDir>('desc');

  const handleSort = useCallback((field: FlatSortField) => {
    if (field === sortField) {
      setSortDir(d => d === 'asc' ? 'desc' : 'asc');
    } else {
      setSortField(field);
      setSortDir('asc');
    }
  }, [sortField]);

  const sorted = useMemo(
    () => sortRows(rows, sortField as keyof FlatRow | null, sortDir),
    [rows, sortField, sortDir]
  );

  if (sorted.length === 0) {
    return <div className="px-4 py-3 text-xs text-content-muted italic">No flat sets found.</div>;
  }

  const thProps = { sortField, sortDir, onSort: handleSort };

  return (
    <div>
      <table className="w-full" style={{ borderCollapse: 'separate', borderSpacing: 0 }}>
        <thead>
          <tr className="border-b border-border/40">
            <SortTh field="setId" label="Set" {...thProps} />
            <SortTh field="filter" label="Filter" {...thProps} />
            <SortTh field="exptime" label="Exp" {...thProps} />
            <SortTh field="sortTemp" label="Temp" {...thProps} />
            <SortTh field="frameCount" label="Frames" {...thProps} />
            <SortTh field="sortDate" label="Date Range" {...thProps} />
            <SortTh field="camera" label="Camera" {...thProps} />
            <SortTh field="focallen" label="FL" {...thProps} />
            <SortTh field="subCalSetId" label="SubCal" {...thProps} />
            {!compact && <SortTh field="gain" label="G" {...thProps} />}
            {!compact && <th className="px-1.5 py-1.5 text-left text-xs font-semibold text-content-secondary whitespace-nowrap sticky top-0 z-10 bg-surface-elevated">B</th>}
            {!compact && <SortTh field="offset" label="O" {...thProps} />}
            {!compact && <SortTh field="usedByLightCount" label="Lights" {...thProps} />}
          </tr>
        </thead>
        <tbody>
          {sorted.map(row => {
            const highlighted = highlightedSetIds.has(row.setId);
            const gMatch = row.cameraGain != null && row.gain != null && Math.abs(row.gain - row.cameraGain) < 0.01;
            const bMatch = row.cameraBinning != null && row.binning === row.cameraBinning;
            const oMatch = row.cameraOffset != null && row.offset != null && Math.abs(row.offset - row.cameraOffset) < 0.01;
            return (
              <tr
                key={row.setId}
                onClick={() => onRowClick(row)}
                className={`border-b border-border/20 cursor-pointer hover:bg-surface-hover/50 transition-colors ${highlighted ? 'bg-accent/20' : ''}`}
                style={{ height: 28 }}
              >
                <td className="px-1.5 py-1 text-sm">
                  <SetIdBadge
                    id={row.setId}
                    color={SECTION_COLORS.flats}
                    isMaster={row.isMaster}
                    linkLike={row.camera != null}
                    title={row.camera != null ? `Open #${row.setId} in Equipment` : `Set #${row.setId}`}
                    onClick={e => {
                      e.stopPropagation();
                      if (row.camera == null) return;
                      onJumpToEquipment({ camera: row.camera, setId: row.setId, kind: 'Flat', isMaster: row.isMaster });
                    }}
                  />
                </td>
                <td className="px-1.5 py-1 text-sm text-content-secondary">
                  {row.filter ?? <span className="text-content-muted italic">—</span>}
                </td>
                <td className="px-1.5 py-1 text-sm text-content-secondary">
                  {row.exptime != null ? `${row.exptime}s` : '—'}
                </td>
                <td className="px-1.5 py-1 text-sm text-content-secondary">{row.tempRange}</td>
                <td className="px-1.5 py-1 text-sm text-content-secondary tabular-nums">{row.frameCount}</td>
                <td className="px-1.5 py-1 text-sm font-mono text-content-secondary whitespace-nowrap">{row.dateRange}</td>
                <td className="px-1.5 py-1 text-sm text-content-secondary whitespace-nowrap">{row.camera ?? '—'}</td>
                <td className="px-1.5 py-1 text-sm text-content-secondary">{row.focallen != null ? `${row.focallen}mm` : '—'}</td>
                <td className="px-1.5 py-1 text-sm">
                  {row.subCalSetId != null ? (
                    <SetIdBadge
                      id={row.subCalSetId}
                      color={row.subCalType === 'Bias' ? SECTION_COLORS.bias : SECTION_COLORS.darks}
                      onClick={e => { e.stopPropagation(); onSubCalClick(row.subCalSetId!, row.subCalType!); }}
                    />
                  ) : <span className="text-content-muted">—</span>}
                </td>
                {!compact && <td className="px-1.5 py-1 text-sm"><MatchBadge label="Gain" value={row.gain} matches={row.cameraGain != null ? gMatch : null} /></td>}
                {!compact && <td className="px-1.5 py-1 text-sm"><MatchBadge label="Binning" value={row.binning} matches={row.cameraBinning != null ? bMatch : null} /></td>}
                {!compact && <td className="px-1.5 py-1 text-sm"><MatchBadge label="Offset" value={row.offset} matches={row.cameraOffset != null ? oMatch : null} /></td>}
                {!compact && (
                  <td className="px-1.5 py-1 text-sm">
                    {row.usedByLightCount > 0 ? (
                      <button
                        onClick={e => { e.stopPropagation(); onLightsClick(row.usedByLightFrameIds); }}
                        className="text-success hover:underline tabular-nums"
                      >
                        {row.usedByLightCount}
                      </button>
                    ) : (
                      <span className="text-content-muted">0</span>
                    )}
                  </td>
                )}
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

// ── Darks Table ────────────────────────────────────────────────────────────

type DarkSortField = 'setId' | 'exptime' | 'sortTemp' | 'binning' | 'frameCount' | 'sortDate' | 'camera' | 'biasSetId' | 'gain' | 'offset' | 'usedByLightCount';

function DarksTable({
  rows,
  highlightedSetIds,
  onRowClick,
  onBiasClick,
  onLightsClick,
  onJumpToEquipment,
  compact,
}: {
  rows: DarkRow[];
  highlightedSetIds: Set<number>;
  onRowClick: (row: DarkRow) => void;
  onBiasClick: (id: number) => void;
  onLightsClick: (ids: number[]) => void;
  onJumpToEquipment: (row: { camera: string | null; setId: number; kind: CalSetKind; isMaster: boolean }) => void;
  compact?: boolean;
}) {
  const [sortField, setSortField] = useState<DarkSortField | null>('sortDate');
  const [sortDir, setSortDir] = useState<SortDir>('desc');

  const handleSort = useCallback((field: DarkSortField) => {
    if (field === sortField) {
      setSortDir(d => d === 'asc' ? 'desc' : 'asc');
    } else {
      setSortField(field);
      setSortDir('asc');
    }
  }, [sortField]);

  const sorted = useMemo(
    () => sortRows(rows, sortField as keyof DarkRow | null, sortDir),
    [rows, sortField, sortDir]
  );

  if (sorted.length === 0) {
    return <div className="px-4 py-3 text-xs text-content-muted italic">No dark sets found.</div>;
  }

  const thProps = { sortField, sortDir, onSort: handleSort };

  return (
    <div>
      <table className="w-full" style={{ borderCollapse: 'separate', borderSpacing: 0 }}>
        <thead>
          <tr className="border-b border-border/40">
            <SortTh field="setId" label="Set" {...thProps} />
            <SortTh field="exptime" label="Exp" {...thProps} />
            <SortTh field="sortTemp" label="Temp" {...thProps} />
            <SortTh field="frameCount" label="Frames" {...thProps} />
            <SortTh field="sortDate" label="Date Range" {...thProps} />
            <SortTh field="camera" label="Camera" {...thProps} />
            <SortTh field="biasSetId" label="Bias" {...thProps} />
            {!compact && <SortTh field="gain" label="G" {...thProps} />}
            {!compact && <th className="px-1.5 py-1.5 text-left text-xs font-semibold text-content-secondary whitespace-nowrap sticky top-0 z-10 bg-surface-elevated">B</th>}
            {!compact && <SortTh field="offset" label="O" {...thProps} />}
            {!compact && <SortTh field="usedByLightCount" label="Lights" {...thProps} />}
          </tr>
        </thead>
        <tbody>
          {sorted.map(row => {
            const highlighted = highlightedSetIds.has(row.setId);
            const gMatch = row.cameraGain != null && row.gain != null && Math.abs(row.gain - row.cameraGain) < 0.01;
            const bMatch = row.cameraBinning != null && row.binning === row.cameraBinning;
            const oMatch = row.cameraOffset != null && row.offset != null && Math.abs(row.offset - row.cameraOffset) < 0.01;
            return (
              <tr
                key={row.setId}
                onClick={() => onRowClick(row)}
                className={`border-b border-border/20 cursor-pointer hover:bg-surface-hover/50 transition-colors ${highlighted ? 'bg-accent/20' : ''}`}
                style={{ height: 28 }}
              >
                <td className="px-1.5 py-1 text-sm">
                  <SetIdBadge
                    id={row.setId}
                    color={SECTION_COLORS.darks}
                    isMaster={row.isMaster}
                    linkLike={row.camera != null}
                    title={row.camera != null ? `Open #${row.setId} in Equipment` : `Set #${row.setId}`}
                    onClick={e => {
                      e.stopPropagation();
                      if (row.camera == null) return;
                      onJumpToEquipment({ camera: row.camera, setId: row.setId, kind: 'Dark', isMaster: row.isMaster });
                    }}
                  />
                </td>
                <td className="px-1.5 py-1 text-sm text-content-secondary">
                  {row.exptime != null ? `${row.exptime}s` : '—'}
                </td>
                <td className="px-1.5 py-1 text-sm text-content-secondary">{row.tempRange}</td>
                <td className="px-1.5 py-1 text-sm text-content-secondary tabular-nums">{row.frameCount}</td>
                <td className="px-1.5 py-1 text-sm font-mono text-content-secondary whitespace-nowrap">{row.dateRange}</td>
                <td className="px-1.5 py-1 text-sm text-content-secondary whitespace-nowrap">{row.camera ?? '—'}</td>
                <td className="px-1.5 py-1 text-sm">
                  {row.biasSetId != null ? (
                    <SetIdBadge
                      id={row.biasSetId}
                      color={SECTION_COLORS.bias}
                      onClick={e => { e.stopPropagation(); onBiasClick(row.biasSetId!); }}
                    />
                  ) : <span className="text-content-muted">—</span>}
                </td>
                {!compact && <td className="px-1.5 py-1 text-sm"><MatchBadge label="Gain" value={row.gain} matches={row.cameraGain != null ? gMatch : null} /></td>}
                {!compact && <td className="px-1.5 py-1 text-sm"><MatchBadge label="Binning" value={row.binning} matches={row.cameraBinning != null ? bMatch : null} /></td>}
                {!compact && <td className="px-1.5 py-1 text-sm"><MatchBadge label="Offset" value={row.offset} matches={row.cameraOffset != null ? oMatch : null} /></td>}
                {!compact && (
                  <td className="px-1.5 py-1 text-sm">
                    {row.usedByLightCount > 0 ? (
                      <button
                        onClick={e => { e.stopPropagation(); onLightsClick(row.usedByLightFrameIds); }}
                        className="text-success hover:underline tabular-nums"
                      >
                        {row.usedByLightCount}
                      </button>
                    ) : (
                      <span className="text-content-muted">0</span>
                    )}
                  </td>
                )}
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

// ── Bias Table ─────────────────────────────────────────────────────────────

type BiasSortField = 'setId' | 'binning' | 'frameCount' | 'sortDate' | 'camera' | 'gain' | 'offset' | 'usedByLightCount';

function BiasTable({
  rows,
  highlightedSetIds,
  onRowClick,
  onLightsClick,
  onJumpToEquipment,
  compact,
}: {
  rows: BiasRow[];
  highlightedSetIds: Set<number>;
  onRowClick: (row: BiasRow) => void;
  onLightsClick: (ids: number[]) => void;
  onJumpToEquipment: (row: { camera: string | null; setId: number; kind: CalSetKind; isMaster: boolean }) => void;
  compact?: boolean;
}) {
  const [sortField, setSortField] = useState<BiasSortField | null>('sortDate');
  const [sortDir, setSortDir] = useState<SortDir>('desc');

  const handleSort = useCallback((field: BiasSortField) => {
    if (field === sortField) {
      setSortDir(d => d === 'asc' ? 'desc' : 'asc');
    } else {
      setSortField(field);
      setSortDir('asc');
    }
  }, [sortField]);

  const sorted = useMemo(
    () => sortRows(rows, sortField as keyof BiasRow | null, sortDir),
    [rows, sortField, sortDir]
  );

  if (sorted.length === 0) {
    return <div className="px-4 py-3 text-xs text-content-muted italic">No bias sets found.</div>;
  }

  const thProps = { sortField, sortDir, onSort: handleSort };

  return (
    <div>
      <table className="w-full" style={{ borderCollapse: 'separate', borderSpacing: 0 }}>
        <thead>
          <tr className="border-b border-border/40">
            <SortTh field="setId" label="Set" {...thProps} />
            <SortTh field="frameCount" label="Frames" {...thProps} />
            <SortTh field="sortDate" label="Date Range" {...thProps} />
            <SortTh field="camera" label="Camera" {...thProps} />
            {!compact && <th className="px-1.5 py-1.5 text-left text-xs font-semibold text-content-secondary whitespace-nowrap sticky top-0 z-10 bg-surface-elevated">B</th>}
            {!compact && <SortTh field="gain" label="G" {...thProps} />}
            {!compact && <SortTh field="offset" label="O" {...thProps} />}
            {!compact && <SortTh field="usedByLightCount" label="Lights" {...thProps} />}
          </tr>
        </thead>
        <tbody>
          {sorted.map(row => {
            const highlighted = highlightedSetIds.has(row.setId);
            const gMatch = row.cameraGain != null && row.gain != null && Math.abs(row.gain - row.cameraGain) < 0.01;
            const bMatch = row.cameraBinning != null && row.binning === row.cameraBinning;
            const oMatch = row.cameraOffset != null && row.offset != null && Math.abs(row.offset - row.cameraOffset) < 0.01;
            return (
              <tr
                key={row.setId}
                onClick={() => onRowClick(row)}
                className={`border-b border-border/20 cursor-pointer hover:bg-surface-hover/50 transition-colors ${highlighted ? 'bg-accent/20' : ''}`}
                style={{ height: 28 }}
              >
                <td className="px-1.5 py-1 text-sm">
                  <SetIdBadge
                    id={row.setId}
                    color={SECTION_COLORS.bias}
                    isMaster={row.isMaster}
                    linkLike={row.camera != null}
                    title={row.camera != null ? `Open #${row.setId} in Equipment` : `Set #${row.setId}`}
                    onClick={e => {
                      e.stopPropagation();
                      if (row.camera == null) return;
                      onJumpToEquipment({ camera: row.camera, setId: row.setId, kind: 'Bias', isMaster: row.isMaster });
                    }}
                  />
                </td>
                <td className="px-1.5 py-1 text-sm text-content-secondary tabular-nums">{row.frameCount}</td>
                <td className="px-1.5 py-1 text-sm font-mono text-content-secondary whitespace-nowrap">{row.dateRange}</td>
                <td className="px-1.5 py-1 text-sm text-content-secondary whitespace-nowrap">{row.camera ?? '—'}</td>
                {!compact && <td className="px-1.5 py-1 text-sm"><MatchBadge label="Binning" value={row.binning} matches={row.cameraBinning != null ? bMatch : null} /></td>}
                {!compact && <td className="px-1.5 py-1 text-sm"><MatchBadge label="Gain" value={row.gain} matches={row.cameraGain != null ? gMatch : null} /></td>}
                {!compact && <td className="px-1.5 py-1 text-sm"><MatchBadge label="Offset" value={row.offset} matches={row.cameraOffset != null ? oMatch : null} /></td>}
                {!compact && (
                  <td className="px-1.5 py-1 text-sm">
                    {row.usedByLightCount > 0 ? (
                      <button
                        onClick={e => { e.stopPropagation(); onLightsClick(row.usedByLightFrameIds); }}
                        className="text-success hover:underline tabular-nums"
                      >
                        {row.usedByLightCount}
                      </button>
                    ) : (
                      <span className="text-content-muted">0</span>
                    )}
                  </td>
                )}
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

// ── Main component ─────────────────────────────────────────────────────────

export function CalibrationTableView({
  data,
  allFrames,
  visibleFrameIds,
  analysisData,
  onManualCalibration,
  onBlink,
  reassignMode = false,
  highlightCalSet,
  onHighlightConsumed,
}: CalibrationTableViewProps) {
  const navigate = useNavigate();

  // Cross-page jump: open the matching camera + tab on the Equipment page,
  // with the row scrolled into view, highlighted, and auto-expanded.
  const handleJumpToEquipment = useCallback(
    (row: { camera: string | null; setId: number; kind: CalSetKind; isMaster: boolean }) => {
      if (row.camera == null) return;
      const tab = tabForCalSet(row.kind, row.isMaster);
      const params = new URLSearchParams({
        camera: row.camera,
        tab,
        highlightSet: String(row.setId),
      });
      navigate(`/equipment?${params.toString()}`);
    },
    [navigate]
  );

  // Selected light row for reassign mode
  const [selectedLightRow, setSelectedLightRow] = useState<LightRow | null>(null);

  // Horizontal split (left tables / right panel) for reassign mode
  const [hSplitPct, setHSplitPct] = useState(60);
  const isHDragging = useRef(false);
  const hContainerRef = useRef<HTMLDivElement>(null);

  const handleHDividerDown = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    isHDragging.current = true;
    const onMove = (ev: MouseEvent) => {
      if (!isHDragging.current || !hContainerRef.current) return;
      const rect = hContainerRef.current.getBoundingClientRect();
      const pct = ((ev.clientX - rect.left) / rect.width) * 100;
      setHSplitPct(Math.max(30, Math.min(75, pct)));
    };
    const onUp = () => {
      isHDragging.current = false;
      document.removeEventListener('mousemove', onMove);
      document.removeEventListener('mouseup', onUp);
    };
    document.addEventListener('mousemove', onMove);
    document.addEventListener('mouseup', onUp);
  }, []);

  // Highlighted state: light row keys and cal set IDs
  const [highlightedLightKeys, setHighlightedLightKeys] = useState<Set<string>>(new Set());
  const [highlightedFlatIds, setHighlightedFlatIds] = useState<Set<number>>(new Set());
  const [highlightedDarkIds, setHighlightedDarkIds] = useState<Set<number>>(new Set());
  const [highlightedBiasIds, setHighlightedBiasIds] = useState<Set<number>>(new Set());

  // Section container refs for scrolling
  const flatsSectionRef = useRef<HTMLDivElement>(null);
  const darksSectionRef = useRef<HTMLDivElement>(null);
  const biasSectionRef = useRef<HTMLDivElement>(null);
  const lightsSectionRef = useRef<HTMLDivElement>(null);

  const derived = useMemo(
    () => deriveTableData(data, allFrames, visibleFrameIds, analysisData),
    [data, allFrames, visibleFrameIds, analysisData]
  );

  const { lightRows, flatRows, darkRows, biasRows } = derived;

  // Clear all highlights
  const clearHighlights = useCallback(() => {
    setHighlightedLightKeys(new Set());
    setHighlightedFlatIds(new Set());
    setHighlightedDarkIds(new Set());
    setHighlightedBiasIds(new Set());
  }, []);

  // Click a light row: highlight it + flat + dark; in reassign mode also select it
  const handleLightRowClick = useCallback((row: LightRow) => {
    if (reassignMode) {
      // In reassign mode: toggle selected row for the right panel
      setSelectedLightRow(prev => prev?.rowKey === row.rowKey ? null : row);
      setHighlightedLightKeys(new Set([row.rowKey]));
      setHighlightedFlatIds(row.flatSetId != null ? new Set([row.flatSetId]) : new Set());
      setHighlightedDarkIds(row.darkSetId != null ? new Set([row.darkSetId]) : new Set());
      setHighlightedBiasIds(new Set());
      return;
    }
    // Toggle: if this row is already the only highlighted one, clear
    if (highlightedLightKeys.size === 1 && highlightedLightKeys.has(row.rowKey)) {
      clearHighlights();
      return;
    }
    setHighlightedLightKeys(new Set([row.rowKey]));
    setHighlightedFlatIds(row.flatSetId != null ? new Set([row.flatSetId]) : new Set());
    setHighlightedDarkIds(row.darkSetId != null ? new Set([row.darkSetId]) : new Set());
    setHighlightedBiasIds(new Set());
  }, [reassignMode, highlightedLightKeys, clearHighlights]);

  // Click flat set ID (from light row or cross-ref): scroll to and highlight flat row
  const handleFlatClick = useCallback((flatSetId: number) => {
    setHighlightedFlatIds(new Set([flatSetId]));
    setTimeout(() => {
      flatsSectionRef.current?.scrollIntoView({ behavior: 'smooth', block: 'start' });
    }, 60);
  }, []);

  // Click dark set ID: scroll to and highlight dark row
  const handleDarkClick = useCallback((darkSetId: number) => {
    setHighlightedDarkIds(new Set([darkSetId]));
    setTimeout(() => {
      darksSectionRef.current?.scrollIntoView({ behavior: 'smooth', block: 'start' });
    }, 60);
  }, []);

  // Click bias set ID: scroll to and highlight bias row
  const handleBiasClick = useCallback((biasSetId: number) => {
    setHighlightedBiasIds(new Set([biasSetId]));
    setTimeout(() => {
      biasSectionRef.current?.scrollIntoView({ behavior: 'smooth', block: 'start' });
    }, 60);
  }, []);

  // Click sub-calibration badge in flats table: route to dark or bias section
  const handleSubCalClick = useCallback((id: number, type: 'Dark' | 'DarkFlat' | 'Bias') => {
    if (type === 'Bias') {
      handleBiasClick(id);
    } else {
      handleDarkClick(id);
    }
  }, [handleBiasClick, handleDarkClick]);

  // Click flat row: highlight it + all lights that use it
  const handleFlatRowClick = useCallback((row: FlatRow) => {
    if (highlightedFlatIds.size === 1 && highlightedFlatIds.has(row.setId)) {
      clearHighlights();
      return;
    }
    setHighlightedFlatIds(new Set([row.setId]));
    // Find light rows that use this flat
    const lightKeys = lightRows
      .filter(lr => lr.flatSetId === row.setId)
      .map(lr => lr.rowKey);
    setHighlightedLightKeys(new Set(lightKeys));
    setHighlightedDarkIds(new Set());
    setHighlightedBiasIds(new Set());
  }, [highlightedFlatIds, lightRows, clearHighlights]);

  // Click dark row: highlight it + all lights that use it
  const handleDarkRowClick = useCallback((row: DarkRow) => {
    if (highlightedDarkIds.size === 1 && highlightedDarkIds.has(row.setId)) {
      clearHighlights();
      return;
    }
    setHighlightedDarkIds(new Set([row.setId]));
    const lightKeys = lightRows
      .filter(lr => lr.darkSetId === row.setId)
      .map(lr => lr.rowKey);
    setHighlightedLightKeys(new Set(lightKeys));
    setHighlightedFlatIds(new Set());
    setHighlightedBiasIds(new Set());
    if (row.biasSetId != null) setHighlightedBiasIds(new Set([row.biasSetId]));
  }, [highlightedDarkIds, lightRows, clearHighlights]);

  // Click bias row: highlight it + all darks that reference it
  const handleBiasRowClick = useCallback((row: BiasRow) => {
    if (highlightedBiasIds.size === 1 && highlightedBiasIds.has(row.setId)) {
      clearHighlights();
      return;
    }
    setHighlightedBiasIds(new Set([row.setId]));
    const darkIds = darkRows
      .filter(dr => dr.biasSetId === row.setId)
      .map(dr => dr.setId);
    setHighlightedDarkIds(new Set(darkIds));
    setHighlightedLightKeys(new Set());
    setHighlightedFlatIds(new Set());
  }, [highlightedBiasIds, darkRows, clearHighlights]);

  // Cross-page jump from Equipment chip → highlight + scroll to the originating
  // calibration set in the matching table. Fires once per highlightCalSet
  // change, after derived rows are ready, then signals the parent to clear.
  useEffect(() => {
    if (!highlightCalSet) return;
    const { setId, kind } = highlightCalSet;
    const exists =
      kind === 'flat' ? flatRows.some(r => r.setId === setId) :
      kind === 'dark' ? darkRows.some(r => r.setId === setId) :
      biasRows.some(r => r.setId === setId);
    if (!exists) return;
    if (kind === 'flat') {
      handleFlatClick(setId);
    } else if (kind === 'dark') {
      handleDarkClick(setId);
    } else {
      handleBiasClick(setId);
    }
    onHighlightConsumed?.();
    // intentionally only re-runs when highlightCalSet (or the derived rows) change
  }, [highlightCalSet, flatRows, darkRows, biasRows, handleFlatClick, handleDarkClick, handleBiasClick, onHighlightConsumed]);

  // Highlight light rows by frame IDs (from "Lights" count click in cal tables)
  const handleLightsByFrameIds = useCallback((frameIds: number[]) => {
    const idSet = new Set(frameIds);
    const keys = lightRows
      .filter(lr => lr.allFrameIds.some(id => idSet.has(id)))
      .map(lr => lr.rowKey);
    setHighlightedLightKeys(new Set(keys));
    setHighlightedFlatIds(new Set());
    setHighlightedDarkIds(new Set());
    setHighlightedBiasIds(new Set());
    setTimeout(() => {
      lightsSectionRef.current?.scrollIntoView({ behavior: 'smooth', block: 'start' });
    }, 60);
  }, [lightRows]);

  // Resizable split
  const [splitPct, setSplitPct] = useState(60);
  const isDragging = useRef(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const [bottomTab, setBottomTab] = useState<'flats' | 'darks' | 'bias'>('flats');

  const handleDividerDown = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    isDragging.current = true;
    const onMove = (ev: MouseEvent) => {
      if (!isDragging.current || !containerRef.current) return;
      const rect = containerRef.current.getBoundingClientRect();
      const pct = ((ev.clientY - rect.top) / rect.height) * 100;
      setSplitPct(Math.max(20, Math.min(80, pct)));
    };
    const onUp = () => {
      isDragging.current = false;
      document.removeEventListener('mousemove', onMove);
      document.removeEventListener('mouseup', onUp);
    };
    document.addEventListener('mousemove', onMove);
    document.addEventListener('mouseup', onUp);
  }, []);

  // Determine if all cal tables fit without scrolling
  // Row height ~28px, section header ~28px, table header ~28px
  const ROW_H = 28;
  const calSections = [
    { key: 'flats' as const, rows: flatRows.length },
    { key: 'darks' as const, rows: darkRows.length },
    { key: 'bias' as const, rows: biasRows.length },
  ].filter(s => s.rows > 0);

  const totalCalHeight = calSections.reduce((sum, s) => sum + ROW_H + ROW_H + s.rows * ROW_H, 0); // header + th + rows per section
  const bottomPanelRef = useRef<HTMLDivElement>(null);
  const [bottomHeight, setBottomHeight] = useState(0);

  // Measure bottom panel height on mount and resize
  const measuredRef = useCallback((node: HTMLDivElement | null) => {
    if (node) {
      const observer = new ResizeObserver(entries => {
        for (const entry of entries) setBottomHeight(entry.contentRect.height);
      });
      observer.observe(node);
      bottomPanelRef.current = node;
      return () => observer.disconnect();
    }
  }, []);

  const allCalFit = bottomHeight > 0 ? totalCalHeight <= bottomHeight : totalCalHeight < 400;

  // The vertical-split tables block (shared between normal and reassign mode)
  const tablesBlock = (
    <div ref={containerRef} className="flex flex-col h-full overflow-hidden border border-border/40 bg-surface">
      {/* Top: Lights */}
      <div style={{ height: `${splitPct}%` }} className="flex flex-col min-h-0">
        <div ref={lightsSectionRef}>
          <SectionHeader title="Lights" count={lightRows.length} color={SECTION_COLORS.lights} />
        </div>
        <div className="flex-1 overflow-y-auto">
          <LightsTable
            rows={lightRows}
            highlightedRowKeys={highlightedLightKeys}
            selectedRowKey={reassignMode ? selectedLightRow?.rowKey : null}
            onRowClick={handleLightRowClick}
            onFlatClick={handleFlatClick}
            onDarkClick={handleDarkClick}
            compact={reassignMode}
          />
        </div>
      </div>

      {/* Divider */}
      <div className="h-1.5 cursor-row-resize bg-border/50 hover:bg-accent/50 active:bg-accent transition-colors flex-shrink-0" onMouseDown={handleDividerDown} />

      {/* Bottom: Calibration tables */}
      <div ref={measuredRef} style={{ height: `${100 - splitPct}%` }} className="flex flex-col min-h-0">
        {allCalFit ? (
          /* All fit — show stacked */
          <div className="flex-1 overflow-y-auto">
            {flatRows.length > 0 && (
              <div ref={flatsSectionRef}>
                <SectionHeader title="Flats" count={flatRows.length} color={SECTION_COLORS.flats} />
                <FlatsTable rows={flatRows} highlightedSetIds={highlightedFlatIds} onRowClick={handleFlatRowClick} onSubCalClick={handleSubCalClick} onLightsClick={handleLightsByFrameIds} onJumpToEquipment={handleJumpToEquipment} compact={reassignMode} />
              </div>
            )}
            {darkRows.length > 0 && (
              <div ref={darksSectionRef} className="border-t border-border/30">
                <SectionHeader title="Darks" count={darkRows.length} color={SECTION_COLORS.darks} />
                <DarksTable rows={darkRows} highlightedSetIds={highlightedDarkIds} onRowClick={handleDarkRowClick} onBiasClick={handleBiasClick} onLightsClick={handleLightsByFrameIds} onJumpToEquipment={handleJumpToEquipment} compact={reassignMode} />
              </div>
            )}
            {biasRows.length > 0 && (
              <div ref={biasSectionRef} className="border-t border-border/30">
                <SectionHeader title="Bias" count={biasRows.length} color={SECTION_COLORS.bias} />
                <BiasTable rows={biasRows} highlightedSetIds={highlightedBiasIds} onRowClick={handleBiasRowClick} onLightsClick={handleLightsByFrameIds} onJumpToEquipment={handleJumpToEquipment} compact={reassignMode} />
              </div>
            )}
          </div>
        ) : (
          /* Don't fit — show tabs */
          <>
            <div className="flex border-b border-border/30 bg-surface flex-shrink-0">
              {calSections.map(tab => (
                <button
                  key={tab.key}
                  onClick={() => setBottomTab(tab.key)}
                  className={`flex items-center gap-2 px-4 py-1.5 text-xs font-semibold transition-colors ${
                    bottomTab === tab.key ? 'border-b-2 -mb-px' : 'text-content-muted hover:text-content-secondary'
                  }`}
                  style={bottomTab === tab.key ? { color: SECTION_COLORS[tab.key], borderBottomColor: SECTION_COLORS[tab.key] } : undefined}
                >
                  {tab.key.charAt(0).toUpperCase() + tab.key.slice(1)}
                  <span className="text-[10px] font-medium opacity-60">{tab.rows}</span>
                </button>
              ))}
            </div>
            <div className="flex-1 overflow-y-auto" ref={bottomTab === 'flats' ? flatsSectionRef : bottomTab === 'darks' ? darksSectionRef : biasSectionRef}>
              {bottomTab === 'flats' && <FlatsTable rows={flatRows} highlightedSetIds={highlightedFlatIds} onRowClick={handleFlatRowClick} onSubCalClick={handleSubCalClick} onLightsClick={handleLightsByFrameIds} onJumpToEquipment={handleJumpToEquipment} compact={reassignMode} />}
              {bottomTab === 'darks' && <DarksTable rows={darkRows} highlightedSetIds={highlightedDarkIds} onRowClick={handleDarkRowClick} onBiasClick={handleBiasClick} onLightsClick={handleLightsByFrameIds} onJumpToEquipment={handleJumpToEquipment} compact={reassignMode} />}
              {bottomTab === 'bias' && <BiasTable rows={biasRows} highlightedSetIds={highlightedBiasIds} onRowClick={handleBiasRowClick} onLightsClick={handleLightsByFrameIds} onJumpToEquipment={handleJumpToEquipment} compact={reassignMode} />}
            </div>
          </>
        )}
      </div>
    </div>
  );

  if (reassignMode) {
    return (
      <div ref={hContainerRef} className="flex h-full min-h-0" style={{ userSelect: isHDragging.current ? 'none' : undefined }}>
        {/* Left: tables at ~hSplitPct% width */}
        <div style={{ width: `${hSplitPct}%` }} className="flex flex-col min-w-0 min-h-0 h-full">
          {tablesBlock}
        </div>
        {/* Horizontal resizable divider */}
        <div
          className="w-1.5 cursor-col-resize bg-border/50 hover:bg-accent/50 active:bg-accent transition-colors flex-shrink-0"
          onMouseDown={handleHDividerDown}
        />
        {/* Right: frame selection panel */}
        <div style={{ width: `${100 - hSplitPct}%` }} className="flex flex-col min-w-0 min-h-0 h-full">
          <ReassignFramePanel
            lightRow={selectedLightRow}
            analysisData={analysisData}
            onManualCalibration={onManualCalibration ?? (() => {})}
            onBlink={onBlink}
          />
        </div>
      </div>
    );
  }

  return tablesBlock;
}
