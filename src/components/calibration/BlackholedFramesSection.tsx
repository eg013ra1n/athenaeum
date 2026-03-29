import { useState, useCallback } from 'react';
import { ChevronDown, ChevronRight, RotateCw } from 'lucide-react';
import { api } from '../../api';
import type { EnrichedLightFrame } from './LightsAnalysisTable';

interface BlackholedFramesSectionProps {
  frames: EnrichedLightFrame[];
}

export function BlackholedFramesSection({ frames }: BlackholedFramesSectionProps) {
  const [expanded, setExpanded] = useState(false);
  const [restoring, setRestoring] = useState<number | null>(null);

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

  if (frames.length === 0) return null;

  return (
    <div className="mt-3 border border-error/20 rounded-lg overflow-hidden opacity-70">
      <button
        onClick={() => setExpanded(prev => !prev)}
        className="w-full flex items-center gap-2 px-3 py-2 bg-error/5 hover:bg-error/10 transition-colors text-left"
      >
        {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
        <span className="text-sm font-medium text-content-secondary">
          Blackholed
        </span>
        <span className="text-xs text-content-muted">
          ({frames.length} frame{frames.length !== 1 ? 's' : ''})
        </span>
      </button>
      {expanded && (
        <table className="w-full">
          <thead className="bg-surface">
            <tr className="text-xs text-content-muted">
              <th className="px-3 py-1.5 text-left font-medium">Filename</th>
              <th className="px-3 py-1.5 text-left font-medium">Date</th>
              <th className="px-3 py-1.5 text-left font-medium">Camera</th>
              <th className="px-3 py-1.5 text-left font-medium">Filter</th>
              <th className="px-3 py-1.5 text-right font-medium">Exposure</th>
              <th className="px-3 py-1.5 text-center font-medium">Action</th>
            </tr>
          </thead>
          <tbody>
            {frames.map(frame => (
              <tr key={frame.frame_id} className="border-t border-border/30 text-sm text-content-muted">
                <td className="px-3 py-1.5 truncate max-w-[200px]" title={frame.filename}>
                  {frame.filename}
                </td>
                <td className="px-3 py-1.5 whitespace-nowrap">
                  {frame.date_obs ? new Date(frame.date_obs).toLocaleString(undefined, {
                    month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit'
                  }) : '-'}
                </td>
                <td className="px-3 py-1.5">{frame.camera ?? '-'}</td>
                <td className="px-3 py-1.5">{frame.filter ?? '-'}</td>
                <td className="px-3 py-1.5 text-right">
                  {frame.exptime != null ? `${frame.exptime}s` : '-'}
                </td>
                <td className="px-3 py-1.5 text-center">
                  <button
                    onClick={() => handleRestore(frame.file_id)}
                    disabled={restoring === frame.file_id}
                    className="inline-flex items-center gap-1 px-2 py-0.5 text-xs text-success hover:text-success bg-success/10 hover:bg-success/20 rounded transition-colors disabled:opacity-50"
                    title="Restore from blackhole"
                  >
                    <RotateCw size={12} className={restoring === frame.file_id ? 'animate-spin' : ''} />
                    Restore
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
