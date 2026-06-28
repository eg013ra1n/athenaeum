import React, { memo, useMemo } from "react";
import {
  Loader2,
  CheckSquare,
  Square,
  Trash2,
  FolderOpen,
  ArrowUp,
  ArrowDown,
  CheckCheck,
  ArrowLeftRight,
} from "lucide-react";
import { useNavigate } from "react-router-dom";
import type { FrameListProps, SortField } from "./types";

function formatTime(dateStr: string | null | undefined): string {
  if (!dateStr) return "--/--/-- --:--:--";
  try {
    const d = new Date(dateStr);
    const y = d.getFullYear();
    const m = String(d.getMonth() + 1).padStart(2, '0');
    const day = String(d.getDate()).padStart(2, '0');
    const h = String(d.getHours()).padStart(2, '0');
    const min = String(d.getMinutes()).padStart(2, '0');
    const s = String(d.getSeconds()).padStart(2, '0');
    return `${y}-${m}-${day} ${h}:${min}:${s}`;
  } catch { return "--/--/-- --:--:--"; }
}

function SortLabel({ field, label, current, direction, onClick }: {
  field: SortField; label: string; current: SortField; direction: 'asc' | 'desc';
  onClick: (field: SortField) => void;
}) {
  const isActive = current === field;
  return (
    <button
      onClick={() => onClick(field)}
      className={`text-[10px] font-medium transition-colors flex items-center gap-1 px-1.5 h-6 rounded bg-surface-elevated border border-border ${
        isActive
          ? 'text-accent border-accent/40'
          : 'text-content-muted hover:text-content-secondary hover:bg-surface-hover'
      }`}
    >
      {label}
      {isActive && (direction === 'asc' ? <ArrowUp size={10} /> : <ArrowDown size={10} />)}
    </button>
  );
}

/** Frame list sidebar for BlinkViewer with two-line rows and sort controls */
export const FrameList: React.FC<FrameListProps> = memo(function FrameList({
  frames,
  currentIndex,
  selectedFrames,
  blackholedFileIds,
  loadingIndices,
  analysisMap,
  sortField,
  sortDirection,
  onSortChange,
  onFrameClick,
  onCheckboxClick,
  onSelectAll,
  onClearSelection,
  onInvertSelection,
}) {
  const navigate = useNavigate();

  // Compute sorted display order (array of original indices)
  const sortedIndices = useMemo(() => {
    const indices = frames.map((_, i) => i);
    indices.sort((a, b) => {
      const fa = frames[a];
      const fb = frames[b];
      const aa = fa.frame?.id ? analysisMap.get(fa.frame.id) : undefined;
      const ab = fb.frame?.id ? analysisMap.get(fb.frame.id) : undefined;

      let cmp = 0;
      switch (sortField) {
        case 'time':
          cmp = (fa.frame?.date_obs ?? '').localeCompare(fb.frame?.date_obs ?? '');
          break;
        case 'filter':
          cmp = (fa.frame?.filter ?? '').localeCompare(fb.frame?.filter ?? '');
          break;
        case 'exptime':
          cmp = (fa.frame?.exptime ?? 0) - (fb.frame?.exptime ?? 0);
          break;
        case 'fwhm':
          cmp = (aa?.median_fwhm ?? Infinity) - (ab?.median_fwhm ?? Infinity);
          break;
        case 'eccentricity':
          cmp = (aa?.median_eccentricity ?? Infinity) - (ab?.median_eccentricity ?? Infinity);
          break;
        case 'frame_snr':
          cmp = (aa?.frame_snr ?? -Infinity) - (ab?.frame_snr ?? -Infinity);
          break;
      }
      return sortDirection === 'asc' ? cmp : -cmp;
    });
    return indices;
  }, [frames, analysisMap, sortField, sortDirection]);

  const hasMultipleFilters = useMemo(() => {
    const vals = new Set(frames.map(f => f.frame?.filter ?? ''));
    return vals.size > 1;
  }, [frames]);

  const hasMultipleExptimes = useMemo(() => {
    const vals = new Set(frames.map(f => f.frame?.exptime ?? 0));
    return vals.size > 1;
  }, [frames]);

  const hasAnyAnalysis = analysisMap.size > 0;

  return (
    <div className="bg-surface flex flex-col h-full">
      {/* Header: selection split button + sort controls */}
      <div className="px-2 py-1.5 border-b border-border flex items-center gap-2">
        {/* Selection split button: [Select All | Count/Unselect | Invert] */}
        <div className="flex h-6 flex-shrink-0">
          <button
            onClick={onSelectAll}
            className="px-1.5 inline-flex items-center justify-center text-[10px] font-medium bg-surface-elevated hover:bg-surface-hover text-content-secondary rounded-l border border-r-0 border-border transition-colors"
            title="Select all"
          >
            <CheckCheck size={10} />
          </button>
          <button
            onClick={onClearSelection}
            disabled={selectedFrames.size === 0}
            className="px-1.5 inline-flex items-center justify-center text-[10px] font-medium bg-surface-elevated hover:bg-surface-hover text-content-secondary border-y border-border transition-colors disabled:opacity-30 disabled:cursor-default min-w-[20px]"
            title="Clear selection"
          >
            {selectedFrames.size}
          </button>
          <button
            onClick={onInvertSelection}
            className="px-1.5 inline-flex items-center justify-center text-[10px] font-medium bg-surface-elevated hover:bg-surface-hover text-content-secondary rounded-r border border-l-0 border-border transition-colors"
            title="Invert selection"
          >
            <ArrowLeftRight size={10} />
          </button>
        </div>

        {/* Divider */}
        <div className="w-px h-4 bg-border flex-shrink-0" />

        {/* Sort labels */}
        <div className="flex items-center gap-1 flex-wrap">
          <SortLabel field="time" label="Time" current={sortField} direction={sortDirection} onClick={onSortChange} />
          {hasMultipleFilters && <SortLabel field="filter" label="Filter" current={sortField} direction={sortDirection} onClick={onSortChange} />}
          {hasMultipleExptimes && <SortLabel field="exptime" label="Exp" current={sortField} direction={sortDirection} onClick={onSortChange} />}
          {hasAnyAnalysis && <>
            <SortLabel field="fwhm" label="FWHM" current={sortField} direction={sortDirection} onClick={onSortChange} />
            <SortLabel field="eccentricity" label="Ecc" current={sortField} direction={sortDirection} onClick={onSortChange} />
            <SortLabel field="frame_snr" label="SNR" current={sortField} direction={sortDirection} onClick={onSortChange} />
          </>}
        </div>
      </div>

      {/* Scrollable frame list */}
      <div className="flex-1 overflow-y-auto select-none">
        {sortedIndices.map((index) => {
            const frame = frames[index];
            const isSelected = selectedFrames.has(index);
            const isCurrent = index === currentIndex;
            const isBlackholed = frame.file.id ? blackholedFileIds.has(frame.file.id) : false;
            const analysis = frame.frame?.id ? analysisMap.get(frame.frame.id) : undefined;

            // Two independent visual channels so the three states never hide
            // each other:
            //   • Background tint encodes *status* — selected (warning) takes
            //     precedence over blackholed (error), then a faint accent wash
            //     for a plain current row, then the default.
            //   • "Current" (the frame on the canvas) adds an inset accent ring
            //     ON TOP of whatever tint is in effect, so the displayed frame
            //     is always identifiable — including when it's deleted/selected.
            let rowClasses = "px-2 py-2 text-xs cursor-pointer transition-colors border-b border-border/40 last:border-b-0";
            if (isSelected) {
              rowClasses += " bg-warning/10 text-warning";
            } else if (isBlackholed) {
              rowClasses += " bg-error/5 text-content-muted";
            } else if (isCurrent) {
              rowClasses += " bg-accent/10 text-content";
            } else {
              rowClasses += " bg-surface-elevated text-content-secondary hover:bg-surface-hover";
            }
            if (isCurrent) {
              rowClasses += " ring-2 ring-inset ring-accent";
            }

            return (
              <div
                key={frame.file.id ?? index}
                onClick={(e) => onFrameClick(index, e)}
                className={rowClasses}
                title={frame.file.filename}
              >
                <div className="flex items-center gap-1.5 min-h-[18px]">
                  {isBlackholed ? (
                    <button onClick={(e) => onCheckboxClick(index, e)} className="flex-shrink-0 p-0.5 -ml-0.5">
                      {isSelected ? <CheckSquare size={13} /> : <Trash2 size={13} />}
                    </button>
                  ) : (
                    <button onClick={(e) => onCheckboxClick(index, e)} className="flex-shrink-0 p-0.5 -ml-0.5">
                      {isSelected ? <CheckSquare size={13} /> : <Square size={13} />}
                    </button>
                  )}
                  <span className="font-mono">{formatTime(frame.frame?.date_obs)}</span>
                  {hasMultipleFilters && frame.frame?.filter && (
                    <span className="font-bold text-content-secondary">{frame.frame.filter}</span>
                  )}
                  {hasMultipleExptimes && frame.frame?.exptime != null && (
                    <span className="font-bold text-content-secondary">{frame.frame.exptime}s</span>
                  )}
                  {hasAnyAnalysis && analysis && (
                    <>
                      <span className={`font-bold ${fwhmColor(analysis.median_fwhm)}`}>{analysis.median_fwhm.toFixed(2)}px</span>
                      <span className={`font-bold ${eccColor(analysis.median_eccentricity)}`}>{analysis.median_eccentricity.toFixed(2)}</span>
                      <span className="font-bold text-content-muted">{analysis.frame_snr.toFixed(1)}dB</span>
                    </>
                  )}
                  {loadingIndices.has(index) && (
                    <Loader2 className="animate-spin flex-shrink-0 ml-auto" size={11} />
                  )}
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      navigate('/files', {
                        state: { reveal: { path: frame.file.path, token: Date.now() } },
                      });
                    }}
                    className="ml-auto p-1 rounded transition-colors flex-shrink-0 text-content-muted hover:text-content hover:bg-surface-hover"
                    title="Locate in file browser"
                  >
                    <FolderOpen size={12} />
                  </button>
                </div>
              </div>
            );
          })}
      </div>
    </div>
  );
});

function fwhmColor(fwhm: number): string {
  if (fwhm <= 2.5) return "text-success/80";
  if (fwhm <= 4.0) return "text-warning/80";
  return "text-error/80";
}

function eccColor(ecc: number): string {
  if (ecc <= 0.5) return "text-success/80";
  if (ecc <= 0.7) return "text-warning/80";
  return "text-error/80";
}
