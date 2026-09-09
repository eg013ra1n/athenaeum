import { X } from 'lucide-react';
import type { ImagingLocation } from '../types/models';

interface SkyFieldPanelProps {
  location: ImagingLocation;
  onClose: () => void;
  onOpenObject: (frameSetId: number) => void;
}

export function SkyFieldPanel({ location, onClose, onOpenObject }: SkyFieldPanelProps) {
  const dates = location.dateRange.map(date => date?.split('T')[0] || 'Unknown');

  return (
    <aside
      aria-labelledby="sky-field-title"
      className="absolute top-3 right-3 bottom-3 z-10 w-80 max-w-[calc(100%-1.5rem)] overflow-y-auto rounded-lg border border-border bg-surface-elevated p-4 shadow-lg"
    >
      <div className="flex items-start justify-between gap-3 mb-4">
        <div className="min-w-0">
          <p className="text-xs text-content-muted mb-1">Field details</p>
          <h3 id="sky-field-title" className="font-semibold text-content break-words">
            {location.objectName || '[No Name]'}
          </h3>
        </div>
        <button
          type="button"
          aria-label="Close field details"
          onClick={onClose}
          className="p-1 rounded text-content-muted hover:bg-surface-hover focus:outline-none focus:ring-2 focus:ring-accent"
        >
          <X size={18} />
        </button>
      </div>
      <dl className="grid grid-cols-2 gap-x-3 gap-y-3 text-sm">
        <dt className="text-content-muted">Exposures</dt>
        <dd className="text-content">{location.frameCount.toLocaleString()}</dd>
        <dt className="text-content-muted">Integration time</dt>
        <dd className="text-content">{(location.totalExposure / 3600).toFixed(2)} h</dd>
        <dt className="text-content-muted">Filters</dt>
        <dd className="text-content break-words">{location.filters.join(', ') || 'Unknown'}</dd>
        <dt className="text-content-muted">First observation</dt>
        <dd className="text-content">{dates[0]}</dd>
        <dt className="text-content-muted">Last observation</dt>
        <dd className="text-content">{dates[1]}</dd>
        <dt className="text-content-muted">Cameras</dt>
        <dd className="text-content break-words">{location.cameras || 'Unknown'}</dd>
      </dl>
      <p className="text-xs text-content-muted mt-4">
        Totals use confirmed exposure representatives in this field. The date filter selects fields,
        so these totals are not restricted to the chosen dates.
      </p>
      {location.frameSetId !== null ? (
        <button
          type="button"
          onClick={() => onOpenObject(location.frameSetId!)}
          className="mt-4 w-full px-3 py-2 rounded bg-accent text-on-accent hover:bg-accent-hover focus:outline-none focus:ring-2 focus:ring-accent"
        >
          Open object
        </button>
      ) : (
        <p className="mt-4 text-sm text-content-secondary">
          These frames are not organized into an object yet.
        </p>
      )}
    </aside>
  );
}
