// src/components/calibration/ReassignFramePanel.tsx
import { useState, useEffect, useCallback, useRef, useMemo } from 'react';
import { Wrench, Eye } from 'lucide-react';
import type { FrameAnalysis } from '../../types/models';
import type { LightRow } from './CalibrationTableView';
import type { EnrichedLightFrame } from './LightsAnalysisTable';

type SortField = 'date' | 'fwhm' | 'ecc' | 'snr';
type SortDir = 'asc' | 'desc';

interface ReassignFramePanelProps {
  lightRow: LightRow | null;
  analysisData?: Map<number, FrameAnalysis>;
  onManualCalibration: (frameIds: number[]) => void;
  onBlink?: (frameIds: number[]) => void;
}

// ── Date formatting ──────────────────────────────────────────────────────────

const pad = (n: number) => String(n).padStart(2, '0');

function formatDateTime(dateStr: string | null): string {
  if (!dateStr) return '—';
  const d = new Date(dateStr);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

// ── Component ────────────────────────────────────────────────────────────────

export function ReassignFramePanel({
  lightRow,
  analysisData,
  onManualCalibration,
  onBlink,
}: ReassignFramePanelProps) {
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set());
  const lastClickedIndexRef = useRef<number | null>(null);

  // Reset selection when lightRow changes
  useEffect(() => {
    setSelectedIds(new Set());
    lastClickedIndexRef.current = null;
  }, [lightRow?.rowKey]);

  const frames: EnrichedLightFrame[] = lightRow?.frames ?? [];

  // Sort state
  const [sortField, setSortField] = useState<SortField>('date');
  const [sortDir, setSortDir] = useState<SortDir>('asc');

  const handleSort = useCallback((field: SortField) => {
    if (field === sortField) {
      setSortDir(d => d === 'asc' ? 'desc' : 'asc');
    } else {
      setSortField(field);
      setSortDir('asc');
    }
  }, [sortField]);

  const sortedFrames = useMemo(() => {
    const getValue = (f: EnrichedLightFrame): number | null => {
      const a = analysisData?.get(f.frame_id);
      switch (sortField) {
        case 'date': return f.date_obs ? new Date(f.date_obs).getTime() : null;
        case 'fwhm': return a?.median_fwhm ?? null;
        case 'ecc': return a?.median_eccentricity ?? null;
        case 'snr': return a?.frame_snr ?? null;
      }
    };
    return [...frames].sort((a, b) => {
      const av = getValue(a);
      const bv = getValue(b);
      if (av == null && bv == null) return 0;
      if (av == null) return 1;
      if (bv == null) return -1;
      return sortDir === 'asc' ? av - bv : bv - av;
    });
  }, [frames, sortField, sortDir, analysisData]);

  // Select-all / none / indeterminate logic
  const allSelected = frames.length > 0 && selectedIds.size === frames.length;
  const someSelected = selectedIds.size > 0 && selectedIds.size < frames.length;

  const handleSelectAll = useCallback(() => {
    if (allSelected) {
      setSelectedIds(new Set());
    } else {
      setSelectedIds(new Set(frames.map(f => f.frame_id)));
    }
    lastClickedIndexRef.current = null;
  }, [allSelected, frames]);

  const handleRowCheck = useCallback((frame: EnrichedLightFrame, index: number, e: React.MouseEvent) => {
    setSelectedIds(prev => {
      const next = new Set(prev);

      if (e.shiftKey && lastClickedIndexRef.current != null) {
        // Range select
        const lo = Math.min(lastClickedIndexRef.current, index);
        const hi = Math.max(lastClickedIndexRef.current, index);
        // Determine whether to select or deselect based on the newly clicked item
        const isSelecting = !prev.has(frame.frame_id);
        for (let i = lo; i <= hi; i++) {
          const id = sortedFrames[i].frame_id;
          if (isSelecting) {
            next.add(id);
          } else {
            next.delete(id);
          }
        }
      } else {
        // Simple toggle
        if (next.has(frame.frame_id)) {
          next.delete(frame.frame_id);
        } else {
          next.add(frame.frame_id);
        }
        lastClickedIndexRef.current = index;
      }

      return next;
    });

    if (!e.shiftKey) {
      lastClickedIndexRef.current = index;
    }
  }, [sortedFrames]);

  const handleManualCalibration = useCallback(() => {
    if (selectedIds.size === 0) return;
    onManualCalibration(Array.from(selectedIds));
  }, [selectedIds, onManualCalibration]);

  const handleBlink = useCallback(() => {
    if (selectedIds.size === 0 || !onBlink) return;
    onBlink(Array.from(selectedIds));
  }, [selectedIds, onBlink]);

  // ── Empty state ────────────────────────────────────────────────────────────

  if (!lightRow) {
    return (
      <div className="flex flex-col h-full border-l border-border/40 bg-surface">
        <div className="px-3 py-2 border-b border-border/40 bg-surface-elevated flex-shrink-0">
          <span className="text-xs font-semibold uppercase tracking-wider text-content-muted">
            Re-assign Frames
          </span>
        </div>
        <div className="flex-1 flex items-center justify-center">
          <p className="text-xs text-content-muted italic text-center px-4">
            Click a light row to see individual frames
          </p>
        </div>
      </div>
    );
  }

  // ── Frame group label ──────────────────────────────────────────────────────

  const groupLabel = [
    lightRow.filter ?? 'No filter',
    lightRow.exptime != null ? `${lightRow.exptime}s` : null,
  ].filter(Boolean).join(' · ');

  return (
    <div className="flex flex-col h-full border-l border-border/40 bg-surface min-w-0">
      {/* Header */}
      <div className="px-3 py-2 border-b border-border/40 bg-surface-elevated flex-shrink-0 flex items-center justify-between gap-2">
        <div className="min-w-0">
          <div className="text-xs font-semibold uppercase tracking-wider text-content-muted">
            Re-assign Frames
          </div>
          <div className="text-xs text-content-secondary truncate mt-0.5" title={groupLabel}>
            {groupLabel}
          </div>
        </div>
        <div className="flex items-center gap-1.5 flex-shrink-0">
          <button
            onClick={handleManualCalibration}
            disabled={selectedIds.size === 0}
            className="inline-flex items-center gap-1.5 px-2 py-1 text-xs font-medium rounded transition-colors
              disabled:opacity-40 disabled:cursor-not-allowed
              bg-accent/15 text-accent hover:bg-accent/25 disabled:hover:bg-accent/15"
            title={selectedIds.size === 0 ? 'Select frames to assign calibration' : `Assign calibration to ${selectedIds.size} frame${selectedIds.size !== 1 ? 's' : ''}`}
          >
            <Wrench size={11} />
            Manual Cal
            {selectedIds.size > 0 && (
              <span className="ml-0.5 font-bold">{selectedIds.size}</span>
            )}
          </button>
          {onBlink && (
            <button
              onClick={handleBlink}
              disabled={selectedIds.size === 0}
              className="inline-flex items-center gap-1.5 px-2 py-1 text-xs font-medium rounded transition-colors
                disabled:opacity-40 disabled:cursor-not-allowed
                bg-accent/15 text-accent hover:bg-accent/25 disabled:hover:bg-accent/15"
              title={selectedIds.size === 0 ? 'Select frames to blink' : `Blink ${selectedIds.size} frame${selectedIds.size !== 1 ? 's' : ''}`}
            >
              <Eye size={11} />
              Blink
            </button>
          )}
        </div>
      </div>

      {/* Table */}
      <div className="flex-1 overflow-y-auto">
        {frames.length === 0 ? (
          <div className="px-4 py-3 text-xs text-content-muted italic">No frames in this group.</div>
        ) : (
          <table className="w-full" style={{ borderCollapse: 'separate', borderSpacing: 0 }}>
            <thead>
              <tr className="border-b border-border/40">
                <th className="px-1.5 py-1.5 sticky top-0 z-10 bg-surface-elevated w-7">
                  <input
                    type="checkbox"
                    checked={allSelected}
                    ref={el => {
                      if (el) el.indeterminate = someSelected;
                    }}
                    onChange={handleSelectAll}
                    className="accent-accent cursor-pointer"
                    aria-label="Select all frames"
                  />
                </th>
                {(['date', 'fwhm', 'ecc', 'snr'] as SortField[]).map(field => {
                  const label = field === 'date' ? 'Date / Time' : field === 'fwhm' ? 'FWHM' : field === 'ecc' ? 'Ecc' : 'SNR';
                  const active = sortField === field;
                  return (
                    <th
                      key={field}
                      onClick={() => handleSort(field)}
                      className="px-1.5 py-1.5 text-left text-xs font-semibold text-content-secondary cursor-pointer select-none whitespace-nowrap hover:text-content transition-colors sticky top-0 z-10 bg-surface-elevated"
                    >
                      {label}
                      {active && <span className="ml-0.5 text-accent">{sortDir === 'asc' ? '↑' : '↓'}</span>}
                    </th>
                  );
                })}
              </tr>
            </thead>
            <tbody>
              {sortedFrames.map((frame, index) => {
                const analysis = analysisData?.get(frame.frame_id);
                const isChecked = selectedIds.has(frame.frame_id);
                return (
                  <tr
                    key={frame.frame_id}
                    onClick={e => handleRowCheck(frame, index, e)}
                    className={`border-b border-border/20 cursor-pointer select-none hover:bg-surface-hover/50 transition-colors ${isChecked ? 'bg-accent/10' : ''}`}
                    style={{ height: 28 }}
                  >
                    <td className="px-1.5 py-1" onClick={e => e.stopPropagation()}>
                      <input
                        type="checkbox"
                        checked={isChecked}
                        onChange={e => handleRowCheck(frame, index, e as unknown as React.MouseEvent)}
                        className="accent-accent cursor-pointer"
                        aria-label={`Select frame ${frame.frame_id}`}
                      />
                    </td>
                    <td className="px-1.5 py-1 text-xs font-mono text-content-secondary whitespace-nowrap">
                      {formatDateTime(frame.date_obs)}
                    </td>
                    <td className="px-1.5 py-1 text-xs text-content-secondary tabular-nums">
                      {analysis ? analysis.median_fwhm.toFixed(2) : '—'}
                    </td>
                    <td className="px-1.5 py-1 text-xs text-content-secondary tabular-nums">
                      {analysis ? analysis.median_eccentricity.toFixed(2) : '—'}
                    </td>
                    <td className="px-1.5 py-1 text-xs text-content-secondary tabular-nums">
                      {analysis ? analysis.frame_snr.toFixed(1) : '—'}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}
