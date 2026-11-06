// The Frames table's toolbar and status chips (spec §6.2), on the Analysis tab's labelled-toolbar pieces:
// Selection · Blink · Exclude · Include | Group by · Units · Outliers · Columns | Search, then one row of chips.
import { CircleMinus, CirclePlus, Columns3, TriangleAlert } from 'lucide-react';
import {
  BlinkToolButton,
  ToolButton,
  ToolDivider,
  ToolSelection,
  ToolSplit,
  ToolToggle,
} from '../toolbar/LabeledToolbar';
import { FilterChip } from '../ui/FilterChip';
import type { GroupMode, StatusFilter } from './frameView';

const GROUP_MODE_LABEL: Record<GroupMode, string> = {
  group: 'Group',
  night: 'Night',
  publisher: 'Publisher',
  status: 'Status',
  none: 'None',
};
const GROUP_MODE_TITLE: Record<GroupMode, string> = {
  group: 'Group by stacking group',
  night: 'Group by imaging night',
  publisher: 'Group by publisher',
  status: 'Group by status',
  none: 'No grouping',
};

const STATUS_CHIPS: { value: StatusFilter; label: string }[] = [
  { value: 'all', label: 'All' },
  { value: 'included', label: 'Included' },
  { value: 'regFailed', label: 'Reg failed' },
  { value: 'excludedByRun', label: 'Excluded by the run' },
  { value: 'outOfNextRun', label: 'Out of next run' },
  { value: 'notStackable', label: 'Not stackable here' },
];

/** An action's label: `Exclude (3 of 5)` when it would touch only part of the selection. */
export function actionLabel(verb: string, eligible: number, selected: number): string {
  return eligible > 0 && eligible < selected ? `${verb} (${eligible} of ${selected})` : verb;
}

export interface FramesToolbarProps {
  isProject: boolean;
  selectedCount: number;
  visibleCount: number;
  onClearSelection: () => void;
  onInvertSelection: () => void;
  onBlink: () => void;
  /** How many selected rows each action would touch. */
  excludeCount: number;
  includeCount: number;
  onExclude: () => void;
  onInclude: () => void;
  /** A run is active: Exclude and Include are disabled, Blink is not. */
  disabled: boolean;
  groupModes: GroupMode[];
  groupMode: GroupMode;
  onGroupMode: (mode: GroupMode) => void;
  /** Whether any group has a pixel scale; the Units toggle shows only then. */
  showUnits: boolean;
  arcsec: boolean;
  onToggleArcsec: () => void;
  outliersOnly: boolean;
  onToggleOutliers: () => void;
  columns: boolean;
  onToggleColumns: () => void;
  query: string;
  onQuery: (query: string) => void;
  status: StatusFilter;
  counts: Record<StatusFilter, number>;
  onStatus: (status: StatusFilter) => void;
}

export function FramesToolbar(p: FramesToolbarProps) {
  const chips = p.isProject ? STATUS_CHIPS : STATUS_CHIPS.filter((c) => c.value !== 'notStackable');
  return (
    <div className="space-y-2">
      <div role="toolbar" aria-label="Frames toolbar" className="flex items-start gap-2 flex-wrap">
        <ToolSelection count={p.selectedCount} onClear={p.onClearSelection} onInvert={p.onInvertSelection} />
        <BlinkToolButton selectedCount={p.selectedCount} visibleCount={p.visibleCount} onClick={p.onBlink} />
        <ToolButton
          label={actionLabel('Exclude', p.excludeCount, p.selectedCount)}
          title="Leave the selected frames out of the next run"
          onClick={p.onExclude}
          disabled={p.disabled || p.excludeCount === 0}
        >
          <CircleMinus size={12} />
        </ToolButton>
        <ToolButton
          label={actionLabel('Include', p.includeCount, p.selectedCount)}
          title="Put the selected frames back into the next run"
          onClick={p.onInclude}
          disabled={p.disabled || p.includeCount === 0}
        >
          <CirclePlus size={12} />
        </ToolButton>
        <ToolDivider />
        <ToolSplit
          label="Group by"
          value={p.groupMode}
          onChange={p.onGroupMode}
          options={p.groupModes.map((mode) => ({
            value: mode,
            title: GROUP_MODE_TITLE[mode],
            content: GROUP_MODE_LABEL[mode],
          }))}
        />
        {p.showUnits && (
          <ToolToggle
            label="Units"
            title="Toggle FWHM between pixels and arcseconds"
            on={p.arcsec}
            onClick={p.onToggleArcsec}
          >
            {p.arcsec ? '"' : 'px'}
          </ToolToggle>
        )}
        <ToolToggle label="Outliers" title="Show outlier frames only" on={p.outliersOnly} onClick={p.onToggleOutliers}>
          <TriangleAlert size={12} className="mx-auto" />
        </ToolToggle>
        <ToolToggle label="Columns" title="Show the registration columns" on={p.columns} onClick={p.onToggleColumns}>
          <Columns3 size={12} className="mx-auto" />
        </ToolToggle>
        <ToolDivider />
        <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
          <input
            type="search"
            value={p.query}
            onChange={(e) => p.onQuery(e.target.value)}
            aria-label="Search"
            placeholder={p.isProject ? 'File or publisher' : 'File name'}
            className="h-7 w-44 px-2 text-xs bg-surface-hover border border-border rounded-lg text-content placeholder:text-content-faint focus:outline-none focus:border-accent"
          />
          <span className="text-[10px] text-content-muted leading-tight">Search</span>
        </div>
      </div>
      <div className="flex flex-wrap gap-1.5">
        {chips.map((c) => (
          <FilterChip
            key={c.value}
            filter={c.label}
            count={p.counts[c.value]}
            on={p.status === c.value}
            onClick={() => p.onStatus(c.value)}
            dot={false}
          />
        ))}
      </div>
    </div>
  );
}
