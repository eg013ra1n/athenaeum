import { Camera, Telescope, Calendar } from 'lucide-react';

interface EquipmentHeaderProps {
  cameras: string[];
  telescopes: string[];
  dateRange: [string, string] | null;
}

/**
 * Equipment summary header showing cameras, telescopes, and session date range
 */
export function EquipmentHeader({ cameras, telescopes, dateRange }: EquipmentHeaderProps) {
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
            <span className="text-content">{formatDateRange(dateRange)}</span>
          </div>
        )}
      </div>
    </div>
  );
}

/**
 * Format a date range for display
 */
function formatDateRange(dateRange: [string, string]): string {
  const [start, end] = dateRange;

  // Try to parse ISO dates
  try {
    const startDate = new Date(start);
    const endDate = new Date(end);

    const startStr = startDate.toLocaleDateString('en-US', {
      month: 'short',
      day: 'numeric',
      year: 'numeric',
    });
    const endStr = endDate.toLocaleDateString('en-US', {
      month: 'short',
      day: 'numeric',
      year: 'numeric',
    });

    if (startStr === endStr) {
      return startStr;
    }

    // Calculate number of nights
    const diffTime = Math.abs(endDate.getTime() - startDate.getTime());
    const diffDays = Math.ceil(diffTime / (1000 * 60 * 60 * 24)) + 1;

    return `${startStr} - ${endStr} (${diffDays} nights)`;
  } catch {
    // Fallback to raw display
    if (start === end) {
      return start;
    }
    return `${start} to ${end}`;
  }
}
