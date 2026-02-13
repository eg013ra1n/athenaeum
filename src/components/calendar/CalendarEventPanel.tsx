import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Map, Eye, Clock, Camera, Plus, Calendar } from 'lucide-react';
import { format, parseISO } from 'date-fns';
import type { CalendarDayEvent, CalendarFrameSetSummary, CalendarUnorganizedGroup } from '../../types/models';

interface CalendarEventPanelProps {
  selectedDate: string | null;
  events: CalendarDayEvent | null;
  onNavigateToSkyChart: (ra: number, dec: number) => void;
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
  onSkyChart,
  onDetails,
}: {
  frameSet: CalendarFrameSetSummary;
  onSkyChart: () => void;
  onDetails: () => void;
}) {
  const hasCoordinates = frameSet.ra !== null && frameSet.dec !== null;
  const displayName = frameSet.objectName || frameSet.name || 'Unnamed';

  return (
    <div className="bg-surface-hover rounded-lg p-3 space-y-2">
      <div className="font-medium text-content truncate" title={displayName}>
        {displayName}
      </div>
      <div className="flex items-center gap-3 text-xs text-content-muted">
        <span className="flex items-center gap-1">
          <Camera size={12} />
          {frameSet.frameCount} frames
        </span>
        <span className="flex items-center gap-1">
          <Clock size={12} />
          {formatExposure(frameSet.totalExposureSeconds)}
        </span>
        {frameSet.filters.length > 0 && (
          <span className="text-content-muted">
            {frameSet.filters.filter(f => f).join(', ')}
          </span>
        )}
      </div>
      <div className="flex items-center gap-2 pt-1">
        <button
          onClick={onDetails}
          className="flex-1 flex items-center justify-center gap-1.5 px-2 py-1.5 bg-accent hover:bg-accent-hover text-surface rounded text-xs font-medium transition-colors"
        >
          <Eye size={12} />
          Details
        </button>
        <button
          onClick={onSkyChart}
          disabled={!hasCoordinates}
          className={`
            flex-1 flex items-center justify-center gap-1.5 px-2 py-1.5 rounded text-xs font-medium transition-colors
            ${hasCoordinates
              ? 'bg-surface-hover hover:brightness-110 text-content'
              : 'bg-surface-hover text-content-muted cursor-not-allowed'}
          `}
          title={hasCoordinates ? 'Show in Sky Chart' : 'No coordinates available'}
        >
          <Map size={12} />
          Sky Chart
        </button>
      </div>
    </div>
  );
}

function UnorganizedCard({
  group,
  onSkyChart,
  onCreateFrameset,
}: {
  group: CalendarUnorganizedGroup;
  onSkyChart: () => void;
  onCreateFrameset: () => void;
}) {
  const hasCoordinates = group.ra !== null && group.dec !== null;
  const displayName = group.objectName || 'Unlocated Frames';

  return (
    <div className="bg-surface-hover/50 rounded-lg p-3 space-y-2 border border-warning/20">
      <div className="font-medium text-content truncate flex items-center gap-2" title={displayName}>
        <span className="w-2 h-2 rounded-full bg-warning" />
        {displayName}
      </div>
      <div className="flex items-center gap-3 text-xs text-content-muted">
        <span className="flex items-center gap-1">
          <Camera size={12} />
          {group.frameCount} frames
        </span>
        <span className="flex items-center gap-1">
          <Clock size={12} />
          {formatExposure(group.totalExposureSeconds)}
        </span>
      </div>
      <div className="pt-1 space-y-2">
        <button
          onClick={onCreateFrameset}
          className="w-full flex items-center justify-center gap-1.5 px-2 py-1.5 bg-success hover:brightness-110 text-white rounded text-xs font-medium transition-colors"
        >
          <Plus size={12} />
          Create Frame Set
        </button>
        {hasCoordinates && (
          <button
            onClick={onSkyChart}
            className="w-full flex items-center justify-center gap-1.5 px-2 py-1.5 bg-surface-hover hover:brightness-110 text-content rounded text-xs font-medium transition-colors"
          >
            <Map size={12} />
            Show in Sky Chart
          </button>
        )}
      </div>
    </div>
  );
}

export function CalendarEventPanel({
  selectedDate,
  events,
  onNavigateToSkyChart,
  onNavigateToFrameSet,
}: CalendarEventPanelProps) {
  // State for frame set creation
  const [creatingFromGroup, setCreatingFromGroup] = useState<CalendarUnorganizedGroup | null>(null);
  const [frameSetName, setFrameSetName] = useState('');
  const [isCreating, setIsCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  const [createSuccess, setCreateSuccess] = useState(false);

  // Handler for creating frame set
  const handleCreateFrameset = async () => {
    if (!creatingFromGroup || !frameSetName.trim()) return;

    setIsCreating(true);
    setCreateError(null);

    try {
      await invoke<number>('create_frame_set_from_selection', {
        name: frameSetName.trim(),
        frame_ids: creatingFromGroup.frameIds,
        description: ''
      });

      // Success
      setCreateSuccess(true);
      setTimeout(() => {
        setCreatingFromGroup(null);
        setFrameSetName('');
        setCreateSuccess(false);
      }, 1500);
    } catch (err) {
      setCreateError(String(err));
    } finally {
      setIsCreating(false);
    }
  };

  // Empty state - no day selected
  if (!selectedDate || !events) {
    return (
      <div className="h-full bg-surface-elevated rounded-lg border border-border flex flex-col items-center justify-center text-content-muted p-6">
        <Calendar size={48} className="mb-4 opacity-50" />
        <p className="text-center">Select a day to view details</p>
      </div>
    );
  }

  const displayDate = format(parseISO(selectedDate), 'EEEE, MMMM d, yyyy');

  return (
    <div className="h-full bg-surface-elevated rounded-lg border border-border flex flex-col overflow-hidden">
      {/* Header */}
      <div className="flex-shrink-0 bg-surface-elevated border-b border-border p-4">
        <h4 className="font-semibold text-content">{displayDate}</h4>
      </div>

      {/* Scrollable content */}
      <div className="flex-1 overflow-y-auto p-4 space-y-4">
        {/* Frame Sets Section */}
        {events.frameSets.length > 0 && (
          <div className="space-y-2">
            <h5 className="text-xs font-medium text-content-muted uppercase tracking-wide">
              Frame Sets
            </h5>
            <div className="space-y-2">
              {events.frameSets.map((frameSet) => (
                <FrameSetCard
                  key={frameSet.id}
                  frameSet={frameSet}
                  onSkyChart={() => {
                    if (frameSet.ra !== null && frameSet.dec !== null) {
                      onNavigateToSkyChart(frameSet.ra, frameSet.dec);
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
            <h5 className="text-xs font-medium text-content-muted uppercase tracking-wide">
              Unorganized
            </h5>
            <div className="space-y-2">
              {events.unorganizedGroups.map((group) => (
                <UnorganizedCard
                  key={group.id}
                  group={group}
                  onSkyChart={() => {
                    if (group.ra !== null && group.dec !== null) {
                      onNavigateToSkyChart(group.ra, group.dec);
                    }
                  }}
                  onCreateFrameset={() => {
                    setCreatingFromGroup(group);
                    setFrameSetName(group.objectName || '');
                    setCreateError(null);
                    setCreateSuccess(false);
                  }}
                />
              ))}
            </div>
          </div>
        )}

        {/* Summary */}
        <div className="pt-2 border-t border-border text-xs text-content-muted">
          Total: {events.totalFrameCount} frames | {formatExposure(events.totalExposureSeconds)}
        </div>
      </div>

      {/* Frame Set Creation Form */}
      {creatingFromGroup && (
        <div className="flex-shrink-0 p-4 bg-surface border-t border-border">
          {createSuccess ? (
            <div className="text-center text-success py-2">
              Frame set created successfully!
            </div>
          ) : (
            <>
              <h5 className="text-sm font-medium text-content mb-2">
                Create Frame Set from {creatingFromGroup.frameCount} frames
              </h5>
              <input
                type="text"
                value={frameSetName}
                onChange={(e) => setFrameSetName(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && frameSetName.trim() && !isCreating) {
                    handleCreateFrameset();
                  }
                }}
                placeholder="Enter frame set name..."
                className="w-full px-3 py-2 bg-surface-hover border border-border rounded text-sm text-content focus:outline-none focus:ring-1 focus:ring-accent mb-2"
                autoFocus
              />
              {createError && (
                <p className="text-error text-xs mb-2">{createError}</p>
              )}
              <div className="flex gap-2">
                <button
                  onClick={() => {
                    setCreatingFromGroup(null);
                    setFrameSetName('');
                    setCreateError(null);
                  }}
                  className="flex-1 px-3 py-1.5 bg-surface-hover hover:bg-surface-hover text-content rounded text-sm transition-colors"
                >
                  Cancel
                </button>
                <button
                  onClick={handleCreateFrameset}
                  disabled={!frameSetName.trim() || isCreating}
                  className="flex-1 px-3 py-1.5 bg-success hover:brightness-110 disabled:opacity-50 disabled:cursor-not-allowed text-white rounded text-sm font-medium transition-colors"
                >
                  {isCreating ? 'Creating...' : 'Create'}
                </button>
              </div>
            </>
          )}
        </div>
      )}
    </div>
  );
}
