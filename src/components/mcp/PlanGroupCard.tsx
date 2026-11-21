import type { ReactNode } from 'react';
import { AlertTriangle, ChevronDown, ChevronRight, Lock, Trash2 } from 'lucide-react';
import { Checkbox } from '../settings/Checkbox';
import { Chip } from '../ui';
import { formatBytes, imagetypClass } from '../duplicates/presentation';
import { splitFacts, stayLabel, type Fact } from './planLabels';
import type { PlanGroup, PlanItem } from '../../types/mcp';

/**
 * One group of a Black Hole plan, drawn like a group of the Duplicates page (`DuplicateGroupCard`): a header row that
 * opens the group, then one row per copy with the marker that says what happens to THAT copy from the current
 * selection. What the copies share is said once for the group.
 */
export interface PlanGroupCardProps {
  group: PlanGroup;
  /** The columns the plan names, for the facts on a row. */
  facts: { key: string; label: string }[];
  chosen: string | null;
  included: string[];
  readOnly: boolean;
  open: boolean;
  onToggleOpen: (groupId: string) => void;
  onToggleItem: (groupId: string, itemId: string) => void;
  onToggleGroup: (groupId: string) => void;
  onChoose: (groupId: string, itemId: string) => void;
}

const isDay = (v: string) => /^\d{4}-\d{2}-\d{2}/.test(v);
const dim = 'flex-shrink-0 text-border';

/** The evidence the copies that may move all share; said once on the group. */
function sharedEvidence(group: PlanGroup, chosen: string | null): string | null {
  const moving = group.items.filter((i) => i.id !== chosen && i.evidence !== null);
  const first = moving[0]?.evidence ?? null;
  return first !== null && moving.every((i) => i.evidence === first) ? first : null;
}

export function PlanGroupCard({ group, facts, chosen, included, readOnly, open, onToggleOpen, onToggleItem, onToggleGroup, onChoose }: PlanGroupCardProps) {
  const tickable = group.items.filter((i) => i.locked === null && i.id !== chosen);
  const ticked = tickable.filter((i) => included.includes(i.id)).length;
  const bodyId = `plan-${group.id}`;
  const { shared, own } = splitFacts(facts, group);
  // The header says the frame type and the night once, as the Duplicates header does; the rest is said in the body.
  const type = shared.find((f) => f.key === 'IMAGETYP');
  const date = shared.find((f) => f.key === 'DATE-OBS');
  const bodyShared = shared.filter((f) => f.key !== 'IMAGETYP' && f.key !== 'DATE-OBS');
  const sized = group.items.some((i) => i.sizeBytes != null);
  const movedBytes = group.items.filter((i) => included.includes(i.id)).reduce((n, i) => n + (i.sizeBytes ?? 0), 0);
  const unitBytes = group.items.find((i) => i.id === chosen)?.sizeBytes ?? group.items[0]?.sizeBytes ?? null;
  const needsKeeper = group.chooseOne !== null && chosen === null;
  const evidence = sharedEvidence(group, chosen);
  const unreachable = group.note !== null && /reach/i.test(group.note);

  return (
    <div className={open ? 'bg-surface-hover/40' : ''}>
      <div className="flex items-center gap-2 px-3 py-2 transition-colors hover:bg-surface-hover">
        <Checkbox
          size="sm"
          checked={tickable.length > 0 && ticked === tickable.length}
          indeterminate={ticked > 0 && ticked < tickable.length}
          disabled={readOnly || tickable.length === 0}
          onChange={() => onToggleGroup(group.id)}
          ariaLabel={`Include all in ${group.title}`}
        />
        <button
          type="button"
          aria-expanded={open}
          aria-controls={bodyId}
          aria-label={`${open ? 'Collapse' : 'Expand'} ${group.title}`}
          onClick={() => onToggleOpen(group.id)}
          className="flex min-w-0 flex-1 items-center gap-2 rounded text-left focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
        >
          {open ? <ChevronDown size={16} className="flex-shrink-0" /> : <ChevronRight size={16} className="flex-shrink-0" />}
          {type && (
            <span className={`inline-flex h-5 flex-shrink-0 items-center rounded border px-1.5 text-[10px] font-semibold uppercase tracking-wide ${imagetypClass(type.value)}`}>
              {type.value}
            </span>
          )}
          <span className="min-w-0 truncate font-mono text-xs text-content" title={group.title}>
            {group.title}
          </span>
          {date && <span className="flex-shrink-0 font-mono text-[11px] text-content-muted">{isDay(date.value) ? date.value.slice(0, 10) : date.value}</span>}
          {sized && unitBytes !== null && (
            <>
              <span className={dim}>·</span>
              <span className="flex-shrink-0 text-xs font-semibold text-content">{group.items.length}</span>
              <span className="flex-shrink-0 text-[11px] text-content-muted">×</span>
              <span className="flex-shrink-0 text-[11px] text-content-muted">{formatBytes(unitBytes)}</span>
              {movedBytes > 0 && (
                <>
                  <span className={dim}>·</span>
                  <span className="flex-shrink-0 whitespace-nowrap text-[11px] text-error/80">{formatBytes(movedBytes)} to move</span>
                </>
              )}
            </>
          )}
        </button>
        {/* Like the Duplicates header, a group says nothing while nothing is marked, unless it still needs its copy that stays. */}
        {needsKeeper ? (
          <span className="inline-flex h-6 flex-shrink-0 items-center gap-1 whitespace-nowrap rounded-full border border-warning/50 bg-warning-muted px-2 text-[11px] text-warning">
            <AlertTriangle size={12} aria-hidden />
            Pick the copy that stays
          </span>
        ) : (
          ticked > 0 && (
            <span className="inline-flex h-6 flex-shrink-0 items-center gap-1 whitespace-nowrap rounded-full border border-error/50 bg-error-muted px-2 text-[11px] text-error">
              <Trash2 size={12} aria-hidden />
              Move {ticked} of {group.items.length}
            </span>
          )
        )}
      </div>

      {open && (
        <div id={bodyId} className="px-3 pb-2.5 pt-0.5">
          {group.note && <p className={`mb-1 ml-1 text-[11px] ${unreachable ? 'text-warning' : 'text-content-faint'}`}>{group.note}</p>}
          {evidence && <p className="mb-1 ml-1 text-[11px] text-content-faint">{evidence}</p>}
          {bodyShared.length > 0 && <FactLine facts={bodyShared} />}
          {group.items.map((item) => (
            <Row
              key={item.id}
              group={group}
              item={item}
              extra={own(item)}
              sharedEvidence={evidence}
              isChosen={item.id === chosen}
              isIncluded={included.includes(item.id)}
              readOnly={readOnly}
              onToggleItem={onToggleItem}
              onChoose={onChoose}
            />
          ))}
        </div>
      )}
    </div>
  );
}

/** `Label value` pairs on one muted line: the label faint, the value a step brighter. */
function FactLine({ facts }: { facts: Fact[] }) {
  return (
    <div className="mb-1 ml-1 flex flex-wrap gap-x-3 gap-y-0.5 text-[11px]">
      {facts.map((f) => (
        <span key={f.key}>
          <span className="text-content-faint">{f.label}</span> <span className="text-content-secondary">{f.value}</span>
        </span>
      ))}
    </div>
  );
}

function Row({
  group, item, extra, sharedEvidence: groupEvidence, isChosen, isIncluded, readOnly, onToggleItem, onChoose,
}: {
  group: PlanGroup;
  item: PlanItem;
  /** Facts of this copy that differ from its group's. */
  extra: Fact[];
  sharedEvidence: string | null;
  isChosen: boolean;
  isIncluded: boolean;
  readOnly: boolean;
  onToggleItem: (groupId: string, itemId: string) => void;
  onChoose: (groupId: string, itemId: string) => void;
}) {
  const locked = item.locked !== null;
  const acts = isIncluded && !isChosen && !locked;
  const canKeep = group.chooseOne !== null && !isChosen && item.candidate;
  const modified = item.values.modified ?? null;
  // The copy's own words, only where they differ from what the group says.
  const own = [...extra.map((f) => `${f.label} ${f.value}`), ...(!locked && item.evidence && item.evidence !== groupEvidence ? [item.evidence] : [])];

  let lab: ReactNode = null;
  // A copy that cannot move shows the lock, even when it is the one that stays (a master with calibration links).
  if (locked)
    lab = (
      <>
        <Lock size={13} className="text-content-faint" aria-hidden />
        <span className="sr-only">Locked</span>
      </>
    );
  else if (isChosen) lab = <span className="text-success">Keep</span>;
  else if (acts)
    lab = (
      <>
        <Trash2 size={13} aria-hidden />
        <span className="sr-only">To the Black Hole</span>
      </>
    );

  return (
    <div
      data-testid="plan-row"
      className={`flex items-center gap-2 rounded border px-2 py-[5px] transition-colors ${
        acts ? 'border-error/50 bg-error-muted/40' : 'border-transparent hover:bg-surface-hover'
      }`}
    >
      <Checkbox
        size="sm"
        checked={isIncluded}
        disabled={readOnly || locked || isChosen}
        onChange={() => onToggleItem(group.id, item.id)}
        ariaLabel={`Include ${item.fileName}`}
      />
      <span className={`inline-flex w-12 flex-none items-center text-xs font-medium ${acts ? 'text-error' : ''}`}>{lab}</span>
      <div className="min-w-0 flex-1">
        <span
          title={`${item.folder}/${item.fileName}`}
          className={`block overflow-hidden text-ellipsis whitespace-nowrap text-left font-mono text-xs [direction:rtl] ${acts ? 'text-content-muted line-through' : 'text-content'}`}
        >
          <bdi>
            <span className="text-content-faint">{item.folder}/</span>
            <span>{item.fileName}</span>
          </bdi>
        </span>
        {own.length > 0 && <div className="truncate text-[11px] text-content-faint">{own.join(' · ')}</div>}
      </div>
      {modified && <span className="flex-none whitespace-nowrap text-[11px] text-content-muted">{modified}</span>}
      <span className="flex min-w-24 flex-none items-center justify-end gap-1.5">
        {isChosen && !locked && <Chip tone="ok">Stays</Chip>}
        {locked && item.locked && (
          <Chip tone="info" title={item.locked}>
            <span aria-hidden>{stayLabel(item.locked)}</span>
            <span className="sr-only">{item.locked}</span>
          </Chip>
        )}
        {canKeep && (
          <button
            type="button"
            disabled={readOnly}
            aria-label={`${group.chooseOne}: ${item.fileName}`}
            onClick={() => onChoose(group.id, item.id)}
            className="flex-shrink-0 px-1.5 py-px text-[11px] leading-[1.4] text-accent hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent disabled:cursor-not-allowed disabled:opacity-45 disabled:no-underline"
          >
            Keep this one
          </button>
        )}
      </span>
    </div>
  );
}
