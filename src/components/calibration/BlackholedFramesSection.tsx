import { useState, useCallback, useMemo } from 'react';
import { ChevronDown, ChevronRight, RotateCw, Eye, AlertTriangle } from 'lucide-react';
import { api } from '../../api';
import { ConfirmDialog } from '../ConfirmDialog';
import type { FrameAnalysis } from '../../types/models';
import type { EnrichedLightFrame } from './LightsAnalysisTable';

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

interface BlackholedFramesSectionProps {
  frames: EnrichedLightFrame[];
  analysisData?: Map<number, FrameAnalysis>;
  onBlink?: (frameIds: number[]) => void;
}

export function BlackholedFramesSection({ frames, analysisData, onBlink }: BlackholedFramesSectionProps) {
  const [expanded, setExpanded] = useState(false);
  const [restoring, setRestoring] = useState<number | null>(null);
  const [restoringAll, setRestoringAll] = useState(false);
  const [showRestoreAllConfirm, setShowRestoreAllConfirm] = useState(false);

  const hasBeta = useMemo(() => {
    if (!analysisData) return false;
    for (const frame of frames) {
      const a = analysisData.get(frame.frame_id);
      if (a?.median_beta != null) return true;
    }
    return false;
  }, [frames, analysisData]);

  const handleRestore = useCallback(async (fileId: number) => {
    setRestoring(fileId);
    try {
      await api.invoke('restore_from_black_hole', { fileId });
    } catch (err) {
      console.error('Failed to restore from blackhole:', err);
    } finally {
      setRestoring(null);
    }
  }, []);

  const handleRestoreAll = useCallback(async () => {
    setShowRestoreAllConfirm(false);
    setRestoringAll(true);
    try {
      for (const frame of frames) {
        await api.invoke('restore_from_black_hole', { fileId: frame.file_id });
      }
    } catch (err) {
      console.error('Failed to restore all from blackhole:', err);
    } finally {
      setRestoringAll(false);
    }
  }, [frames]);

  const handleBlinkAll = useCallback(() => {
    onBlink?.(frames.map(f => f.frame_id));
  }, [frames, onBlink]);

  if (frames.length === 0) return null;

  return (
    <div className="mt-3 border border-error/40 rounded-lg overflow-hidden">
      <button
        onClick={() => setExpanded(prev => !prev)}
        className="w-full flex items-center gap-2 px-3 py-2 bg-error/15 hover:bg-error/25 transition-colors text-left"
      >
        {expanded ? <ChevronDown size={14} className="text-error" /> : <ChevronRight size={14} className="text-error" />}
        <span className="text-sm font-semibold text-error">
          Blackholed
        </span>
        <span className="text-xs font-medium text-error/70">
          ({frames.length} frame{frames.length !== 1 ? 's' : ''})
        </span>
        {expanded && (
          <span className="ml-auto flex items-center gap-2" onClick={e => e.stopPropagation()}>
            {onBlink && (
              <button
                onClick={handleBlinkAll}
                className="inline-flex items-center gap-1 px-2 py-0.5 text-xs text-accent hover:text-accent bg-accent/10 hover:bg-accent/20 rounded transition-colors"
                title="Blink all blackholed frames"
              >
                <Eye size={12} />
                Blink All
              </button>
            )}
            <button
              onClick={() => setShowRestoreAllConfirm(true)}
              disabled={restoringAll}
              className="inline-flex items-center gap-1 px-2 py-0.5 text-xs text-success hover:text-success bg-success/10 hover:bg-success/20 rounded transition-colors disabled:opacity-50"
              title="Restore all blackholed frames"
            >
              <RotateCw size={12} className={restoringAll ? 'animate-spin' : ''} />
              Restore All
            </button>
          </span>
        )}
      </button>
      {expanded && (
        <div className="max-h-64 overflow-y-auto">
        <table className="w-full" role="table">
          <thead className="bg-surface sticky top-0 z-10">
            <tr>
              {/* Identity group — no tint */}
              <th scope="col" className="px-1.5 py-1.5 text-left text-xs font-semibold text-content-secondary">Date/Time</th>
              <th scope="col" className="px-1.5 py-1.5 text-center text-xs font-semibold text-content-secondary">Camera</th>
              <th scope="col" className="px-1.5 py-1.5 text-center text-xs font-semibold text-content-secondary">Filter</th>
              {/* Exposure group — gold tint */}
              <th scope="col" className="px-1.5 py-1.5 text-center bg-warning/10 text-xs font-semibold text-content-secondary">Exposure</th>
              {/* Image Quality group — frost blue tint */}
              <th scope="col" className="px-1.5 py-1.5 text-center bg-accent/10 text-xs font-semibold text-content-secondary">Stars</th>
              <th scope="col" className="px-1.5 py-1.5 text-center bg-accent/10 text-xs font-semibold text-content-secondary">FWHM</th>
              <th scope="col" className="px-1.5 py-1.5 text-center bg-accent/10 text-xs font-semibold text-content-secondary">Eccentricity</th>
              {/* Signal group — green tint */}
              <th scope="col" className="px-1.5 py-1.5 text-center bg-success/10 text-xs font-semibold text-content-secondary">SNR</th>
              <th scope="col" className="px-1.5 py-1.5 text-center bg-success/10 text-xs font-semibold text-content-secondary">Frame SNR</th>
              <th scope="col" className="px-1.5 py-1.5 text-center bg-success/10 text-xs font-semibold text-content-secondary">PSF Signal</th>
              <th scope="col" className="px-1.5 py-1.5 text-center bg-success/10 text-xs font-semibold text-content-secondary">SNR Weight</th>
              {/* Tracking group — orange tint */}
              <th scope="col" className="px-1.5 py-1.5 text-center bg-orange/10 text-xs font-semibold text-content-secondary">Trail R²</th>
              {/* Beta column — only shown when Moffat data exists */}
              {hasBeta && (
                <th scope="col" className="px-1.5 py-1.5 text-center bg-accent/10 text-xs font-semibold text-content-secondary">Moffat β</th>
              )}
              {/* Action */}
              <th scope="col" className="w-20 px-1.5 py-1.5 text-center text-xs font-semibold text-content-secondary">Actions</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {frames.map((frame, idx) => {
              const analysis = analysisData?.get(frame.frame_id);
              return (
                <tr
                  key={frame.frame_id}
                  className={`${idx % 2 === 0 ? 'bg-surface-elevated' : 'bg-surface'} hover:bg-surface-hover transition-colors`}
                >
                  {/* Identity group — no tint */}
                  <td className="px-1.5 py-1 text-sm font-mono text-content-secondary">{formatDateTime(frame.date_obs)}</td>
                  <td className="px-1.5 py-1 text-sm text-content-secondary text-center">{frame.camera}</td>
                  <td className="px-1.5 py-1 text-sm text-content-secondary text-center">{frame.filter ?? '-'}</td>
                  {/* Exposure group — gold tint */}
                  <td className="px-1.5 py-1 text-sm text-content-secondary text-center bg-warning/5">{frame.exptime !== null ? `${frame.exptime}s` : '-'}</td>
                  {/* Image Quality group — frost blue tint */}
                  <td className="px-1.5 py-1 text-sm text-content-secondary text-center bg-accent/5">{analysis ? analysis.stars_detected : '-'}</td>
                  <td className="px-1.5 py-1 text-sm text-content-secondary text-center bg-accent/5">{analysis ? analysis.median_fwhm.toFixed(2) : '-'}</td>
                  <td className="px-1.5 py-1 text-sm text-content-secondary text-center bg-accent/5">{analysis ? analysis.median_eccentricity.toFixed(3) : '-'}</td>
                  {/* Signal group — green tint */}
                  <td className="px-1.5 py-1 text-sm text-content-secondary text-center bg-success/5">{analysis ? analysis.median_snr.toFixed(1) : '-'}</td>
                  <td className="px-1.5 py-1 text-sm text-content-secondary text-center bg-success/5">{analysis ? analysis.frame_snr.toFixed(1) : '-'}</td>
                  <td className="px-1.5 py-1 text-sm text-content-secondary text-center bg-success/5">{analysis ? analysis.psf_signal.toFixed(1) : '-'}</td>
                  <td className="px-1.5 py-1 text-sm text-content-secondary text-center bg-success/5">{analysis ? analysis.snr_weight.toFixed(1) : '-'}</td>
                  {/* Tracking group — orange tint */}
                  <td className="px-1.5 py-1 text-sm text-content-secondary text-center bg-orange/5">
                    {analysis ? (
                      <span className="inline-flex items-center gap-1">
                        {analysis.trail_r_squared.toFixed(3)}
                        {analysis.possibly_trailed && (
                          <span title={analysis.trail_r_squared >= 0.3
                            ? "Directional trail detected (RA drift)"
                            : "Guiding issue (wind/vibration)"
                          }>
                            <AlertTriangle size={14} className="text-warning" />
                          </span>
                        )}
                      </span>
                    ) : '-'}
                  </td>
                  {/* Beta column — only shown when Moffat data exists */}
                  {hasBeta && (
                    <td className="px-1.5 py-1 text-sm text-content-secondary text-center bg-accent/5">
                      {analysis?.median_beta != null ? analysis.median_beta.toFixed(2) : '-'}
                    </td>
                  )}
                  {/* Action */}
                  <td className="w-20 px-1.5 py-1 text-center">
                    <button
                      onClick={() => handleRestore(frame.file_id)}
                      disabled={restoring === frame.file_id || restoringAll}
                      className="inline-flex items-center gap-1 px-2 py-0.5 text-xs text-success hover:text-success bg-success/10 hover:bg-success/20 rounded transition-colors disabled:opacity-50"
                      title="Restore from blackhole"
                    >
                      <RotateCw size={12} className={restoring === frame.file_id ? 'animate-spin' : ''} />
                      Restore
                    </button>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
        </div>
      )}

      <ConfirmDialog
        isOpen={showRestoreAllConfirm}
        title="Restore All Blackholed Frames"
        message={`Are you sure you want to restore ${frames.length} frame${frames.length !== 1 ? 's' : ''} from the blackhole?`}
        confirmText="Restore All"
        onConfirm={handleRestoreAll}
        onCancel={() => setShowRestoreAllConfirm(false)}
      />
    </div>
  );
}
