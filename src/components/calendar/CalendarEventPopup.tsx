import { useEffect, useRef } from 'react';
import { X, Map, Eye, Clock, Camera } from 'lucide-react';
import { format, parseISO } from 'date-fns';
import type { CalendarDayEvent, CalendarFrameSetSummary, CalendarUnorganizedGroup } from '../../types/models';

interface CalendarEventPopupProps {
  date: string;
  events: CalendarDayEvent;
  anchorElement: HTMLElement;
  onClose: () => void;
  onNavigateToSkyAtlas: (ra: number, dec: number) => void;
  onNavigateToFrameSet: (frameSetId: number) => void;
}

function formatExposure(seconds: number): string {
  if (seconds < 60) {
    return `${seconds.toFixed(0)}s`;
  }
  const minutes = seconds / 60;
  if (minutes < 60) {
    return `${minutes.toFixed(1)}m`;
  }
  const hours = minutes / 60;
  return `${hours.toFixed(1)}h`;
}

function FrameSetCard({
  frameSet,
  onSkyAtlas,
  onDetails,
}: {
  frameSet: CalendarFrameSetSummary;
  onSkyAtlas: () => void;
  onDetails: () => void;
}) {
  const hasCoordinates = frameSet.ra !== null && frameSet.dec !== null;
  const displayName = frameSet.objectName || frameSet.name || 'Unnamed';

  return (
    <div className="bg-gray-700 rounded-lg p-3 space-y-2">
      <div className="font-medium text-gray-100 truncate" title={displayName}>
        {displayName}
      </div>
      <div className="flex items-center gap-3 text-xs text-gray-400">
        <span className="flex items-center gap-1">
          <Camera size={12} />
          {frameSet.frameCount} frames
        </span>
        <span className="flex items-center gap-1">
          <Clock size={12} />
          {formatExposure(frameSet.totalExposureSeconds)}
        </span>
        {frameSet.filters.length > 0 && (
          <span className="text-gray-500">
            {frameSet.filters.filter(f => f).join(', ')}
          </span>
        )}
      </div>
      <div className="flex items-center gap-2 pt-1">
        <button
          onClick={onDetails}
          className="flex-1 flex items-center justify-center gap-1.5 px-2 py-1.5 bg-blue-600 hover:bg-blue-500 text-white rounded text-xs font-medium transition-colors"
        >
          <Eye size={12} />
          Details
        </button>
        <button
          onClick={onSkyAtlas}
          disabled={!hasCoordinates}
          className={`
            flex-1 flex items-center justify-center gap-1.5 px-2 py-1.5 rounded text-xs font-medium transition-colors
            ${hasCoordinates
              ? 'bg-gray-600 hover:bg-gray-500 text-white'
              : 'bg-gray-600 text-gray-400 cursor-not-allowed'}
          `}
          title={hasCoordinates ? 'Show in SkyAtlas' : 'No coordinates available'}
        >
          <Map size={12} />
          SkyAtlas
        </button>
      </div>
    </div>
  );
}

function UnorganizedCard({
  group,
  onSkyAtlas,
}: {
  group: CalendarUnorganizedGroup;
  onSkyAtlas: () => void;
}) {
  const hasCoordinates = group.ra !== null && group.dec !== null;
  const displayName = group.objectName || 'Unlocated Frames';

  return (
    <div className="bg-gray-700/50 rounded-lg p-3 space-y-2 border border-yellow-500/20">
      <div className="font-medium text-gray-200 truncate flex items-center gap-2" title={displayName}>
        <span className="w-2 h-2 rounded-full bg-yellow-500" />
        {displayName}
      </div>
      <div className="flex items-center gap-3 text-xs text-gray-400">
        <span className="flex items-center gap-1">
          <Camera size={12} />
          {group.frameCount} frames
        </span>
        <span className="flex items-center gap-1">
          <Clock size={12} />
          {formatExposure(group.totalExposureSeconds)}
        </span>
      </div>
      {hasCoordinates && (
        <div className="pt-1">
          <button
            onClick={onSkyAtlas}
            className="w-full flex items-center justify-center gap-1.5 px-2 py-1.5 bg-blue-600 hover:bg-blue-500 text-white rounded text-xs font-medium transition-colors"
          >
            <Map size={12} />
            Show in SkyAtlas
          </button>
        </div>
      )}
    </div>
  );
}

export function CalendarEventPopup({
  date,
  events,
  anchorElement,
  onClose,
  onNavigateToSkyAtlas,
  onNavigateToFrameSet,
}: CalendarEventPopupProps) {
  const popupRef = useRef<HTMLDivElement>(null);

  // Close on escape key
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        onClose();
      }
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [onClose]);

  // Close on click outside
  useEffect(() => {
    const handleClickOutside = (e: MouseEvent) => {
      if (popupRef.current && !popupRef.current.contains(e.target as Node)) {
        onClose();
      }
    };
    // Delay adding the listener to avoid immediate close
    const timer = setTimeout(() => {
      window.addEventListener('click', handleClickOutside);
    }, 0);
    return () => {
      clearTimeout(timer);
      window.removeEventListener('click', handleClickOutside);
    };
  }, [onClose]);

  // Position the popup near the anchor element
  useEffect(() => {
    if (!popupRef.current || !anchorElement) return;

    const popup = popupRef.current;
    const anchor = anchorElement.getBoundingClientRect();
    const viewportWidth = window.innerWidth;
    const viewportHeight = window.innerHeight;

    // Default position: below and to the right of the anchor
    let left = anchor.left;
    let top = anchor.bottom + 8;

    // Adjust if popup would go off-screen
    const popupRect = popup.getBoundingClientRect();

    if (left + popupRect.width > viewportWidth - 20) {
      left = viewportWidth - popupRect.width - 20;
    }
    if (left < 20) {
      left = 20;
    }

    if (top + popupRect.height > viewportHeight - 20) {
      // Position above the anchor instead
      top = anchor.top - popupRect.height - 8;
    }

    popup.style.left = `${left}px`;
    popup.style.top = `${top}px`;
  }, [anchorElement]);

  const displayDate = format(parseISO(date), 'EEEE, MMMM d, yyyy');

  return (
    <div
      ref={popupRef}
      className="fixed z-50 w-80 max-h-[70vh] overflow-y-auto bg-gray-800 rounded-lg shadow-xl border border-gray-600"
      style={{ left: 0, top: 0 }}
    >
      {/* Header */}
      <div className="sticky top-0 bg-gray-800 border-b border-gray-700 p-3 flex items-center justify-between">
        <h4 className="font-semibold text-gray-100">{displayDate}</h4>
        <button
          onClick={onClose}
          className="p-1 hover:bg-gray-700 rounded transition-colors"
        >
          <X size={16} />
        </button>
      </div>

      {/* Content */}
      <div className="p-3 space-y-4">
        {/* Frame Sets Section */}
        {events.frameSets.length > 0 && (
          <div className="space-y-2">
            <h5 className="text-xs font-medium text-gray-400 uppercase tracking-wide">
              Frame Sets
            </h5>
            <div className="space-y-2">
              {events.frameSets.map((frameSet) => (
                <FrameSetCard
                  key={frameSet.id}
                  frameSet={frameSet}
                  onSkyAtlas={() => {
                    if (frameSet.ra !== null && frameSet.dec !== null) {
                      onNavigateToSkyAtlas(frameSet.ra, frameSet.dec);
                    }
                  }}
                  onDetails={() => onNavigateToFrameSet(frameSet.id)}
                />
              ))}
            </div>
          </div>
        )}

        {/* Unorganized Section */}
        {events.unorganizedGroups.length > 0 && (
          <div className="space-y-2">
            <h5 className="text-xs font-medium text-gray-400 uppercase tracking-wide">
              Unorganized
            </h5>
            <div className="space-y-2">
              {events.unorganizedGroups.map((group) => (
                <UnorganizedCard
                  key={group.id}
                  group={group}
                  onSkyAtlas={() => {
                    if (group.ra !== null && group.dec !== null) {
                      onNavigateToSkyAtlas(group.ra, group.dec);
                    }
                  }}
                />
              ))}
            </div>
          </div>
        )}

        {/* Summary */}
        <div className="pt-2 border-t border-gray-700 text-xs text-gray-400">
          Total: {events.totalFrameCount} frames | {formatExposure(events.totalExposureSeconds)}
        </div>
      </div>
    </div>
  );
}
