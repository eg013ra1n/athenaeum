import React from 'react';
import {
  ScanSearch,
  Target,
  Play,
  Camera,
  Calendar,
  Tag,
  X,
  RefreshCw,
} from 'lucide-react';

export type FilterChip = 'coordinates' | 'object' | 'date' | 'camera' | 'type';

export interface EligibleCounts {
  plateSolve: number;
  findObject: number;
  blink: number;
  setCamera: number;
  setDate: number;
  setType: number;
}

interface MissingMetadataToolbarProps {
  activeChips: Set<FilterChip>;
  onToggleChip: (chip: FilterChip) => void;

  selectedCount: number;
  eligible: EligibleCounts;

  loading: boolean;
  onRefresh: () => void;

  onPlateSolve: () => void;
  onFindObject: () => void;
  onBlink: () => void;
  onSetCamera: () => void;
  onSetDate: () => void;
  onSetType: () => void;
  onClearSelection: () => void;
}

const CHIPS: { key: FilterChip; label: string }[] = [
  { key: 'coordinates', label: 'Coordinates' },
  { key: 'object', label: 'Object' },
  { key: 'date', label: 'Date' },
  { key: 'camera', label: 'Camera' },
  { key: 'type', label: 'Type' },
];

interface ActionButtonProps {
  icon: React.ReactNode;
  label: string;
  subLabel: string;
  onClick: () => void;
  disabled: boolean;
  disabledTitle?: string;
  colorClass?: string;
}

const ActionButton: React.FC<ActionButtonProps> = ({
  icon,
  label,
  subLabel,
  onClick,
  disabled,
  disabledTitle,
  colorClass = 'bg-accent hover:bg-accent-hover text-white',
}) => (
  <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
    <button
      onClick={onClick}
      disabled={disabled}
      title={disabled && disabledTitle ? disabledTitle : undefined}
      className={`h-7 px-2.5 inline-flex items-center justify-center gap-1.5 text-xs font-medium rounded-lg transition-colors ${
        disabled
          ? 'bg-surface-hover text-content-muted cursor-not-allowed opacity-50'
          : colorClass
      }`}
    >
      {icon}
      {label}
    </button>
    <span className="text-[10px] text-content-muted leading-tight">{subLabel}</span>
  </div>
);

export const MissingMetadataToolbar: React.FC<MissingMetadataToolbarProps> = ({
  activeChips,
  onToggleChip,
  selectedCount,
  eligible,
  loading,
  onRefresh,
  onPlateSolve,
  onFindObject,
  onBlink,
  onSetCamera,
  onSetDate,
  onSetType,
  onClearSelection,
}) => {
  return (
    <div className="flex items-start gap-2 flex-wrap flex-shrink-0 pb-3 border-b border-border">
      {/* Refresh — sits before the filter chips */}
      <button
        onClick={onRefresh}
        disabled={loading}
        title="Refresh"
        className="h-7 px-2.5 inline-flex items-center gap-1 text-xs font-medium rounded-lg border border-border bg-surface-hover hover:bg-surface-elevated text-content-secondary hover:text-content transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
      >
        <RefreshCw size={12} className={loading ? 'animate-spin' : ''} />
        {loading ? 'Loading…' : 'Refresh'}
      </button>

      <span className="text-border h-7 flex items-center">|</span>

      {/* Filter chips */}
      <div className="flex items-center gap-1.5 flex-shrink-0">
        {CHIPS.map(chip => {
          const active = activeChips.has(chip.key);
          return (
            <button
              key={chip.key}
              onClick={() => onToggleChip(chip.key)}
              className={`h-7 px-3 text-xs rounded-lg border transition-colors ${
                active
                  ? 'bg-accent/20 border-accent text-accent'
                  : 'bg-surface-hover border-border text-content-secondary hover:text-content'
              }`}
            >
              {chip.label}
            </button>
          );
        })}
      </div>

      <span className="text-border h-7 flex items-center">|</span>

      {/* Analysis actions: Plate Solve, Find Object, Blink */}
      <ActionButton
        icon={<ScanSearch size={12} />}
        label="Plate Solve"
        subLabel={selectedCount > 0 ? `${eligible.plateSolve} of ${selectedCount}` : 'Select frames'}
        onClick={onPlateSolve}
        disabled={eligible.plateSolve === 0}
        disabledTitle="No selected frames are LIGHT frames missing coordinates"
      />
      <ActionButton
        icon={<Target size={12} />}
        label="Find Object"
        subLabel={selectedCount > 0 ? `${eligible.findObject} of ${selectedCount}` : 'Select frames'}
        onClick={onFindObject}
        disabled={eligible.findObject === 0}
        disabledTitle="No selected frames are LIGHT frames with coordinates but missing object name"
        colorClass="bg-accent hover:bg-accent-hover text-white"
      />
      <ActionButton
        icon={<Play size={12} />}
        label="Blink"
        subLabel={selectedCount > 0 ? `${eligible.blink} of ${selectedCount}` : 'Select frames'}
        onClick={onBlink}
        disabled={eligible.blink === 0}
        disabledTitle="No frames selected"
        colorClass="bg-cyan-600 hover:bg-cyan-700 text-white"
      />

      <span className="text-border h-7 flex items-center">|</span>

      {/* Edit actions: Set Camera, Set Date, Set Frame Type */}
      <ActionButton
        icon={<Camera size={12} />}
        label="Set Camera"
        subLabel={selectedCount > 0 ? `${eligible.setCamera} of ${selectedCount}` : 'Select frames'}
        onClick={onSetCamera}
        disabled={eligible.setCamera === 0}
        disabledTitle="No selected frames are missing a camera (instrume)"
        colorClass="bg-surface-hover border border-border text-content-secondary hover:text-content"
      />
      <ActionButton
        icon={<Calendar size={12} />}
        label="Set Date"
        subLabel={selectedCount > 0 ? `${eligible.setDate} of ${selectedCount}` : 'Select frames'}
        onClick={onSetDate}
        disabled={eligible.setDate === 0}
        disabledTitle="No selected frames are missing a date"
        colorClass="bg-surface-hover border border-border text-content-secondary hover:text-content"
      />
      <ActionButton
        icon={<Tag size={12} />}
        label="Set Type"
        subLabel={selectedCount > 0 ? `${eligible.setType} of ${selectedCount}` : 'Select frames'}
        onClick={onSetType}
        disabled={eligible.setType === 0}
        disabledTitle="No selected frames are missing a frame type"
        colorClass="bg-surface-hover border border-border text-content-secondary hover:text-content"
      />

      <span className="text-border h-7 flex items-center">|</span>

      {/* Selection counter */}
      <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
        <div className="flex h-7">
          <button
            onClick={onClearSelection}
            disabled={selectedCount === 0}
            title="Clear selection"
            className="h-7 px-2.5 inline-flex items-center justify-center text-xs font-medium bg-surface-hover hover:bg-surface-elevated text-content-secondary rounded-l-lg border border-r-0 border-border transition-colors disabled:opacity-30 disabled:cursor-default"
          >
            {selectedCount}
          </button>
          <button
            onClick={onClearSelection}
            disabled={selectedCount === 0}
            title="Clear selection"
            className="w-7 h-7 inline-flex items-center justify-center text-xs font-medium bg-surface-hover hover:bg-surface-elevated text-content-secondary rounded-r-lg border border-border transition-colors disabled:opacity-30 disabled:cursor-default"
          >
            <X size={10} />
          </button>
        </div>
        <span className="text-[10px] text-content-muted leading-tight">Selected</span>
      </div>

    </div>
  );
};
