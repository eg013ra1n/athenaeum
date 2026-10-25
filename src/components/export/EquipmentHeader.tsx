import { Camera, Telescope, Calendar } from 'lucide-react';
import { formatSessions } from './sessionsLine';

interface EquipmentHeaderProps {
  cameras: string[];
  telescopes: string[];
  dateRange: [string, string] | null;
  /** Imaging nights of the exported lights, as the catalog counts them
   *  (`ExportSummary.nightCount`) — never derived from the span here. */
  nightCount: number;
}

/**
 * Equipment summary header showing cameras, telescopes, and session date range
 */
export function EquipmentHeader({ cameras, telescopes, dateRange, nightCount }: EquipmentHeaderProps) {
  return (
    <div className="bg-surface-elevated border border-border rounded-lg p-4">
      <h3 className="text-xs font-medium text-content-muted uppercase tracking-wide mb-3">
        Equipment Used
      </h3>
      <div className="flex flex-wrap gap-6 text-sm">
        {/* Cameras */}
        {cameras.length > 0 && (
          <div className="flex items-center gap-2">
            <Camera size={16} className="text-accent" />
            <span className="text-content-muted">Cameras:</span>
            <span className="text-content">{cameras.join(', ')}</span>
          </div>
        )}

        {/* Telescopes */}
        {telescopes.length > 0 && (
          <div className="flex items-center gap-2">
            <Telescope size={16} className="text-purple" />
            <span className="text-content-muted">Telescopes:</span>
            <span className="text-content">{telescopes.join(', ')}</span>
          </div>
        )}

        {/* Date Range */}
        {dateRange && (
          <div className="flex items-center gap-2">
            <Calendar size={16} className="text-warning" />
            <span className="text-content-muted">Sessions:</span>
            <span className="text-content">{formatSessions(dateRange, nightCount)}</span>
          </div>
        )}
      </div>
    </div>
  );
}
