import { useState, useMemo, useCallback } from 'react';
import { FolderOpen, Trash2, AlertTriangle } from 'lucide-react';
import { revealItemInDir } from '../../api/desktop';
import { api } from '../../api';
import type { LightFrameWithCalibration, FrameAnalysis } from '../../types/models';

type SortField = 'date' | 'filter' | 'camera' | 'exptime' | 'stars' | 'fwhm' | 'eccentricity' | 'median_snr' | 'snr_db' | 'psf_signal' | 'snr_weight' | 'trail' | 'score';
type SortDirection = 'asc' | 'desc';

/** Frame enriched with camera/filter context from the hierarchy */
export interface EnrichedLightFrame extends LightFrameWithCalibration {
  /** Camera (instrume) from hierarchy context */
  camera: string;
  /** Filter name from hierarchy context */
  filter: string | null;
}

interface LightsAnalysisTableProps {
  frames: EnrichedLightFrame[];
  blackholedFileIds: Set<number>;
  selectedFrameIds: Set<number>;
  onSelectionChange: (selectedIds: Set<number>) => void;
  onBlackhole?: (fileId: number) => void;
  /** Analysis data keyed by frame_id */
  analysisData?: Map<number, FrameAnalysis>;
  /** Frame IDs that are below rejection thresholds */
  rejectedFrameIds?: Set<number>;
}

function formatDateTime(dateStr: string | null): string {
  if (!dateStr) return '-';
  try {
    const d = new Date(dateStr);
    const year = d.getFullYear();
    const month = String(d.getMonth() + 1).padStart(2, '0');
    const day = String(d.getDate()).padStart(2, '0');
    const hours = String(d.getHours()).padStart(2, '0');
    const minutes = String(d.getMinutes()).padStart(2, '0');
    const seconds = String(d.getSeconds()).padStart(2, '0');
    return `${year}-${month}-${day} ${hours}:${minutes}:${seconds}`;
  } catch {
    return '-';
  }
}

function formatTotalExposure(seconds: number): string {
  if (seconds < 60) return `${seconds.toFixed(0)}s`;
  if (seconds < 3600) return `${(seconds / 60).toFixed(1)}m`;
  const h = Math.floor(seconds / 3600);
  const m = Math.round((seconds % 3600) / 60);
  return m > 0 ? `${h}h ${m}m` : `${h}h`;
}

function SortableHeader({
  field,
  label,
  currentSort,
  currentDirection,
  onSort,
  avg,
  align = 'center',
}: {
  field: SortField;
  label: string;
  currentSort: SortField | null;
  currentDirection: SortDirection;
  onSort: (field: SortField) => void;
  avg?: string;
  align?: 'left' | 'center';
}) {
  const isActive = currentSort === field;

  return (
    <button
      onClick={() => onSort(field)}
      className={`flex flex-col gap-0.5 w-full text-xs font-semibold text-content-secondary hover:text-content transition-colors ${align === 'left' ? 'items-start' : 'items-center'}`}
    >
      <span className="flex items-center gap-1">
        {label}
        {isActive && (
          <span className="text-accent">
            {currentDirection === 'asc' ? '↑' : '↓'}
          </span>
        )}
      </span>
      {avg != null && (
        <span className="text-xs font-normal text-accent">{avg}</span>
      )}
    </button>
  );
}

export function LightsAnalysisTable({
  frames,
  blackholedFileIds,
  selectedFrameIds,
  onSelectionChange,
  onBlackhole,
  analysisData,
  rejectedFrameIds,
}: LightsAnalysisTableProps) {
  const [sortField, setSortField] = useState<SortField | null>('date');
  const [sortDirection, setSortDirection] = useState<SortDirection>('asc');

  const handleSort = (field: SortField) => {
    if (sortField === field) {
      setSortDirection(prev => prev === 'asc' ? 'desc' : 'asc');
    } else {
      setSortField(field);
      setSortDirection('asc');
    }
  };

  const handleReveal = useCallback(async (e: React.MouseEvent, path: string) => {
    e.stopPropagation();
    try {
      await revealItemInDir(path);
    } catch (err) {
      console.error('Failed to reveal:', err);
    }
  }, []);

  const handleBlackhole = useCallback(async (e: React.MouseEvent, fileId: number) => {
    e.stopPropagation();
    try {
      await api.invoke('move_to_black_hole', { fileId, fromWhere: 'frame_set_detail' });
      onBlackhole?.(fileId);
    } catch (err) {
      console.error('Failed to move to black hole:', err);
    }
  }, [onBlackhole]);

  const toggleFrame = useCallback((frameId: number) => {
    const next = new Set(selectedFrameIds);
    if (next.has(frameId)) {
      next.delete(frameId);
    } else {
      next.add(frameId);
    }
    onSelectionChange(next);
  }, [selectedFrameIds, onSelectionChange]);

  const toggleAll = useCallback(() => {
    if (selectedFrameIds.size === frames.length && frames.length > 0) {
      onSelectionChange(new Set());
    } else {
      onSelectionChange(new Set(frames.map(f => f.frame_id)));
    }
  }, [selectedFrameIds, frames, onSelectionChange]);

  const allSelected = frames.length > 0 && selectedFrameIds.size === frames.length;
  const someSelected = selectedFrameIds.size > 0 && selectedFrameIds.size < frames.length;

  // Helper to get analysis data for a frame
  const getAnalysis = useCallback((frameId: number): FrameAnalysis | undefined => {
    return analysisData?.get(frameId);
  }, [analysisData]);

  const averages = useMemo(() => {
    if (!analysisData || analysisData.size === 0) return null;

    const analyzed = frames.filter(f => analysisData.has(f.frame_id));
    if (analyzed.length === 0) return null;

    const n = analyzed.length;
    const sumA = (fn: (a: FrameAnalysis) => number) =>
      analyzed.reduce((acc, f) => acc + fn(analysisData.get(f.frame_id)!), 0);

    const framesWithExp = analyzed.filter(f => f.exptime != null);
    const exptime = framesWithExp.reduce((acc, f) => acc + f.exptime!, 0);

    const withScore = analyzed.filter(f => analysisData.get(f.frame_id)!.quality_score != null);

    return {
      count: n,
      exptime,
      stars: sumA(a => a.stars_detected) / n,
      fwhm: sumA(a => a.median_fwhm) / n,
      eccentricity: sumA(a => a.median_eccentricity) / n,
      median_snr: sumA(a => a.median_snr) / n,
      snr_db: sumA(a => a.snr_db) / n,
      psf_signal: sumA(a => a.psf_signal) / n,
      snr_weight: sumA(a => a.snr_weight) / n,
      trail: sumA(a => a.trail_r_squared) / n,
      score: withScore.length > 0
        ? withScore.reduce((acc, f) => acc + (analysisData.get(f.frame_id)!.quality_score ?? 0), 0) / withScore.length
        : null,
    };
  }, [frames, analysisData]);

  const sortedFrames = useMemo(() => {
    if (!sortField) return frames;

    return [...frames].sort((a, b) => {
      let comparison = 0;
      const aAnalysis = getAnalysis(a.frame_id);
      const bAnalysis = getAnalysis(b.frame_id);

      switch (sortField) {
        case 'date': {
          const timeA = a.date_obs ? new Date(a.date_obs).getTime() : 0;
          const timeB = b.date_obs ? new Date(b.date_obs).getTime() : 0;
          comparison = timeA - timeB;
          break;
        }
        case 'filter':
          comparison = (a.filter ?? '').localeCompare(b.filter ?? '');
          break;
        case 'camera':
          comparison = a.camera.localeCompare(b.camera);
          break;
        case 'exptime':
          comparison = (a.exptime ?? 0) - (b.exptime ?? 0);
          break;
        case 'stars':
          comparison = (aAnalysis?.stars_detected ?? -1) - (bAnalysis?.stars_detected ?? -1);
          break;
        case 'fwhm':
          comparison = (aAnalysis?.median_fwhm ?? -1) - (bAnalysis?.median_fwhm ?? -1);
          break;
        case 'eccentricity':
          comparison = (aAnalysis?.median_eccentricity ?? -1) - (bAnalysis?.median_eccentricity ?? -1);
          break;
        case 'median_snr':
          comparison = (aAnalysis?.median_snr ?? -1) - (bAnalysis?.median_snr ?? -1);
          break;
        case 'snr_db':
          comparison = (aAnalysis?.snr_db ?? -1) - (bAnalysis?.snr_db ?? -1);
          break;
        case 'psf_signal':
          comparison = (aAnalysis?.psf_signal ?? -1) - (bAnalysis?.psf_signal ?? -1);
          break;
        case 'snr_weight':
          comparison = (aAnalysis?.snr_weight ?? -1) - (bAnalysis?.snr_weight ?? -1);
          break;
        case 'trail':
          comparison = (aAnalysis?.trail_r_squared ?? -1) - (bAnalysis?.trail_r_squared ?? -1);
          break;
        case 'score':
          comparison = (aAnalysis?.quality_score ?? -1) - (bAnalysis?.quality_score ?? -1);
          break;
      }

      return sortDirection === 'asc' ? comparison : -comparison;
    });
  }, [frames, sortField, sortDirection, getAnalysis]);

  return (
    <div>
      <table className="w-full" role="table">
        <thead className="bg-surface sticky top-0 z-10">
          <tr>
            <th scope="col" className="w-10 px-2 py-2.5 text-center">
              <input
                type="checkbox"
                checked={allSelected}
                ref={el => { if (el) el.indeterminate = someSelected; }}
                onChange={toggleAll}
                className="rounded border-border text-accent focus:ring-accent cursor-pointer"
              />
            </th>
            <th scope="col" className="px-3 py-2.5 text-left">
              <SortableHeader field="date" label="Date/Time" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} align="left" />
            </th>
            <th scope="col" className="px-3 py-2.5 text-center">
              <SortableHeader field="camera" label="Camera" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} />
            </th>
            <th scope="col" className="px-3 py-2.5 text-center">
              <SortableHeader field="filter" label="Filter" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} />
            </th>
            <th scope="col" className="px-3 py-2.5 text-center">
              <SortableHeader field="exptime" label="Exposure" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} avg={averages ? formatTotalExposure(averages.exptime) : undefined} />
            </th>
            <th scope="col" className="px-3 py-2.5 text-center">
              <SortableHeader field="stars" label="Stars" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} avg={averages ? averages.stars.toFixed(0) : undefined} />
            </th>
            <th scope="col" className="px-3 py-2.5 text-center">
              <SortableHeader field="fwhm" label="FWHM" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} avg={averages ? averages.fwhm.toFixed(2) : undefined} />
            </th>
            <th scope="col" className="px-3 py-2.5 text-center">
              <SortableHeader field="eccentricity" label="Eccentricity" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} avg={averages ? averages.eccentricity.toFixed(3) : undefined} />
            </th>
            <th scope="col" className="px-3 py-2.5 text-center">
              <SortableHeader field="median_snr" label="SNR" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} avg={averages ? averages.median_snr.toFixed(1) : undefined} />
            </th>
            <th scope="col" className="px-3 py-2.5 text-center">
              <SortableHeader field="snr_db" label="SNR (dB)" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} avg={averages ? averages.snr_db.toFixed(1) : undefined} />
            </th>
            <th scope="col" className="px-3 py-2.5 text-center">
              <SortableHeader field="psf_signal" label="PSF Signal" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} avg={averages ? averages.psf_signal.toFixed(1) : undefined} />
            </th>
            <th scope="col" className="px-3 py-2.5 text-center">
              <SortableHeader field="snr_weight" label="SNR Weight" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} avg={averages ? averages.snr_weight.toFixed(1) : undefined} />
            </th>
            <th scope="col" className="px-3 py-2.5 text-center">
              <SortableHeader field="trail" label="Trail R²" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} avg={averages ? averages.trail.toFixed(3) : undefined} />
            </th>
            <th scope="col" className="px-3 py-2.5 text-center">
              <SortableHeader field="score" label="Score" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} avg={averages?.score != null ? `${(averages.score * 100).toFixed(0)}%` : undefined} />
            </th>
            <th scope="col" className="w-20 px-2 py-2.5 text-center text-xs font-semibold text-content-secondary">
              Actions
            </th>
          </tr>
        </thead>
        <tbody className="divide-y divide-border">
          {sortedFrames.map((frame, idx) => {
            const isBlackholed = blackholedFileIds.has(frame.file_id);
            const isSelected = selectedFrameIds.has(frame.frame_id);
            const isRejected = rejectedFrameIds?.has(frame.frame_id) ?? false;
            const analysis = getAnalysis(frame.frame_id);

            return (
              <tr
                key={frame.frame_id}
                className={`
                  ${isRejected ? 'bg-error/10' : idx % 2 === 0 ? 'bg-surface-elevated' : 'bg-surface'}
                  hover:bg-surface-hover transition-colors
                  ${isBlackholed ? 'opacity-50' : ''}
                `}
              >
                <td className="w-10 px-2 py-2 text-center">
                  <input
                    type="checkbox"
                    checked={isSelected}
                    onChange={() => toggleFrame(frame.frame_id)}
                    className="rounded border-border text-accent focus:ring-accent cursor-pointer"
                  />
                </td>
                <td className={`px-3 py-2 text-sm font-mono text-content-secondary ${isBlackholed ? 'line-through' : ''}`}>
                  {formatDateTime(frame.date_obs)}
                </td>
                <td className={`px-3 py-2 text-sm text-content-secondary text-center ${isBlackholed ? 'line-through' : ''}`}>
                  {frame.camera}
                </td>
                <td className={`px-3 py-2 text-sm text-content-secondary text-center ${isBlackholed ? 'line-through' : ''}`}>
                  {frame.filter ?? '-'}
                </td>
                <td className={`px-3 py-2 text-sm text-content-secondary text-center ${isBlackholed ? 'line-through' : ''}`}>
                  {frame.exptime !== null ? `${frame.exptime}s` : '-'}
                </td>
                {/* Analysis columns */}
                <td className="px-3 py-2 text-sm text-content-secondary text-center">
                  {analysis ? analysis.stars_detected : '-'}
                </td>
                <td className="px-3 py-2 text-sm text-content-secondary text-center">
                  {analysis ? analysis.median_fwhm.toFixed(2) : '-'}
                </td>
                <td className="px-3 py-2 text-sm text-content-secondary text-center">
                  {analysis ? analysis.median_eccentricity.toFixed(3) : '-'}
                </td>
                <td className="px-3 py-2 text-sm text-content-secondary text-center">
                  {analysis ? analysis.median_snr.toFixed(1) : '-'}
                </td>
                <td className="px-3 py-2 text-sm text-content-secondary text-center">
                  {analysis ? analysis.snr_db.toFixed(1) : '-'}
                </td>
                <td className="px-3 py-2 text-sm text-content-secondary text-center">
                  {analysis ? analysis.psf_signal.toFixed(1) : '-'}
                </td>
                <td className="px-3 py-2 text-sm text-content-secondary text-center">
                  {analysis ? analysis.snr_weight.toFixed(1) : '-'}
                </td>
                <td className="px-3 py-2 text-sm text-content-secondary text-center">
                  {analysis ? (
                    <span className="inline-flex items-center gap-1">
                      {analysis.trail_r_squared.toFixed(3)}
                      {analysis.possibly_trailed && (
                        <span title="Possible star trails detected">
                          <AlertTriangle size={14} className="text-amber-400" />
                        </span>
                      )}
                    </span>
                  ) : '-'}
                </td>
                <td className="px-3 py-2 text-sm text-content-secondary text-center">
                  {analysis?.quality_score != null ? `${(analysis.quality_score * 100).toFixed(0)}%` : '-'}
                </td>
                <td className="w-20 px-2 py-2 text-center">
                  <div className="flex items-center justify-center gap-1">
                    <button
                      onClick={e => handleReveal(e, frame.file_path)}
                      className="inline-flex items-center p-1 text-content-muted hover:text-content bg-surface-hover hover:bg-surface-hover rounded transition-colors"
                      title="Reveal in file explorer"
                    >
                      <FolderOpen size={14} />
                    </button>
                    {!isBlackholed && (
                      <button
                        onClick={e => handleBlackhole(e, frame.file_id)}
                        className="inline-flex items-center p-1 text-content-muted hover:text-error bg-surface-hover hover:bg-surface-hover rounded transition-colors"
                        title="Move to Black Hole"
                      >
                        <Trash2 size={14} />
                      </button>
                    )}
                  </div>
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>

      {frames.length === 0 && (
        <div className="px-4 py-8 text-center text-content-muted">
          <p className="text-sm">No light frames to display</p>
        </div>
      )}
    </div>
  );
}
