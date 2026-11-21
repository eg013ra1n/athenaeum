import { useState, type ElementType, type ReactNode } from 'react';
import { ArrowRight, Check, ChevronDown, ChevronRight, Crosshair, Lock, Pencil, Trash2 } from 'lucide-react';
import { Checkbox } from '../settings/Checkbox';
import { formatBytes, imagetypClass } from '../duplicates/presentation';
import { humanLabel, humanValue, splitFacts, type Fact } from './planLabels';
import type { McpPlanView, PlanGroup, PlanItem } from '../../types/mcp';

/**
 * One group of a plan, drawn like a group of the Duplicates page (`DuplicateGroupCard`): a header row that opens the
 * group, then one row per file with the marker that says what happens to THAT file from the current selection.
 */
export interface PlanGroupCardProps {
  kind: McpPlanView['kind'];
  group: PlanGroup;
  /** The columns the plan names, for the facts on a row. */
  facts: { key: string; label: string }[];
  chosen: string | null;
  included: string[];
  readOnly: boolean;
  defaultOpen: boolean;
  onToggleItem: (groupId: string, itemId: string) => void;
  onToggleGroup: (groupId: string) => void;
  onChoose: (groupId: string, itemId: string) => void;
}

/** What a ticked row does, per kind: the pill of the header and the marker of the row use these words. */
const KIND_WORDS: Record<McpPlanView['kind'], { pill: string; marker: string; left: string; icon: ElementType }> = {
  black_hole: { pill: 'Move', marker: 'To the Black Hole', left: 'Stays', icon: Trash2 },
  metadata: { pill: 'Fill', marker: 'Will be filled', left: 'Left empty', icon: Pencil },
  plate_solve: { pill: 'Solve', marker: 'Will be solved', left: 'Not solved', icon: Crosshair },
};

const isDay = (v: string) => /^\d{4}-\d{2}-\d{2}/.test(v);

export function PlanGroupCard({ kind, group, facts, chosen, included, readOnly, defaultOpen, onToggleItem, onToggleGroup, onChoose }: PlanGroupCardProps) {
  const [open, setOpen] = useState(defaultOpen);
  const words = KIND_WORDS[kind];
  const tickable = group.items.filter((i) => i.locked === null && i.id !== chosen);
  const ticked = tickable.filter((i) => included.includes(i.id)).length;
  const bodyId = `plan-${group.id}`;
  const { shared, own } = splitFacts(facts, group);
  // The header says the frame type and the night once (as the Duplicates header does); the rest is said in the body.
  const headerKeys = kind === 'black_hole' ? ['IMAGETYP', 'DATE-OBS'] : [];
  const type = shared.find((f) => f.key === 'IMAGETYP');
  const date = shared.find((f) => f.key === 'DATE-OBS');
  const bodyShared = shared.filter((f) => !headerKeys.includes(f.key));
  const sized = group.items.some((i) => i.sizeBytes != null);
  const movedBytes = group.items.filter((i) => included.includes(i.id)).reduce((n, i) => n + (i.sizeBytes ?? 0), 0);
  const unitBytes = group.items.find((i) => i.id === chosen)?.sizeBytes ?? group.items[0]?.sizeBytes ?? null;
  const PillIcon = words.icon;

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
          onClick={() => setOpen((o) => !o)}
          className="flex min-w-0 flex-1 items-center gap-2 rounded text-left focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
        >
          {open ? <ChevronDown size={16} className="flex-shrink-0" /> : <ChevronRight size={16} className="flex-shrink-0" />}
          {type && (
            <span className={`inline-flex h-5 flex-shrink-0 items-center rounded border px-1.5 text-[10px] font-semibold uppercase tracking-wide ${imagetypClass(type.value)}`}>
              {type.value}
            </span>
          )}
          <span
            className={`min-w-0 truncate text-xs text-content ${kind === 'black_hole' ? 'font-mono' : 'font-semibold'}`}
            title={group.title}
          >
            {group.title}
          </span>
          {date && (
            <span className="flex-shrink-0 font-mono text-[11px] text-content-muted">{isDay(date.value) ? date.value.slice(0, 10) : date.value}</span>
          )}
          {kind === 'black_hole' && sized && unitBytes !== null && (
            <>
              <span className="flex-shrink-0 text-border">·</span>
              <span className="flex-shrink-0 text-xs font-semibold">{group.items.length}</span>
              <span className="flex-shrink-0 text-[11px] text-content-muted">×</span>
              <span className="flex-shrink-0 text-[11px] text-content-muted">{formatBytes(unitBytes)}</span>
              {movedBytes > 0 && (
                <>
                  <span className="flex-shrink-0 text-border">·</span>
                  <span className="flex-shrink-0 text-[11px] text-error/80">{formatBytes(movedBytes)} to move</span>
                </>
              )}
            </>
          )}
        </button>
        {/* Like the Duplicates header, a Black Hole group says nothing while nothing is marked; the others always say how many. */}
        {(ticked > 0 || kind !== 'black_hole') && (
          <span
            className={`inline-flex h-6 flex-shrink-0 items-center gap-1 rounded-full border px-2 text-[11px] ${
              ticked === 0
                ? 'border-border bg-surface-hover text-content-muted'
                : kind === 'black_hole'
                  ? 'border-error/50 bg-error-muted text-error'
                  : 'border-accent/40 bg-accent/15 text-accent'
            }`}
          >
            <PillIcon size={12} />
            {words.pill} {ticked} of {tickable.length}
          </span>
        )}
      </div>

      {open && (
        <div id={bodyId} className="space-y-1 px-3 pb-3 pt-1">
          {group.note && <p className="text-[11px] text-content-muted">{group.note}</p>}
          {bodyShared.length > 0 && <FactLine facts={bodyShared} />}
          {group.items.map((item) => (
            <Row
              key={item.id}
              kind={kind}
              group={group}
              item={item}
              facts={own(item)}
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
function FactLine({ facts, children }: { facts: Fact[]; children?: ReactNode }) {
  return (
    <div className="flex flex-wrap gap-x-3 gap-y-0.5 text-[11px]">
      {facts.map((f) => (
        <span key={f.key}>
          <span className="text-content-faint">{f.label}</span> <span className="text-content-secondary">{f.value}</span>
        </span>
      ))}
      {children}
    </div>
  );
}

function Row({
  kind,
  group,
  item,
  facts,
  isChosen,
  isIncluded,
  readOnly,
  onToggleItem,
  onChoose,
}: {
  kind: McpPlanView['kind'];
  group: PlanGroup;
  item: PlanItem;
  facts: Fact[];
  isChosen: boolean;
  isIncluded: boolean;
  readOnly: boolean;
  onToggleItem: (groupId: string, itemId: string) => void;
  onChoose: (groupId: string, itemId: string) => void;
}) {
  const words = KIND_WORDS[kind];
  const locked = item.locked !== null;
  const acts = isIncluded && !isChosen && !locked;
  const destructive = kind === 'black_hole';

  // The marker says what happens to THIS row from the current selection.
  let marker: ReactNode;
  if (acts) {
    marker = (
      <span className={`inline-flex items-center gap-1 ${destructive ? 'text-error' : 'text-accent'}`}>
        {destructive ? <Trash2 size={13} /> : <Check size={13} />}
        {words.marker}
      </span>
    );
  } else if (isChosen) {
    marker = (
      <span className="inline-flex items-center gap-1 text-success">
        <Check size={13} />
        Stays
      </span>
    );
  } else if (locked) {
    marker = (
      <span className="inline-flex items-center gap-1 text-content-muted">
        <Lock size={12} />
        Locked
      </span>
    );
  } else {
    marker = <span className={destructive ? 'text-content-secondary' : 'text-content-muted'}>{words.left}</span>;
  }

  const size = item.sizeBytes != null ? formatBytes(item.sizeBytes) : null;
  const modified = item.values.modified ?? null;
  const canKeep = group.chooseOne !== null && !isChosen && item.candidate;

  return (
    <div
      data-testid="plan-row"
      className={`flex items-start gap-2 rounded border px-2 py-1.5 transition-colors ${
        acts ? (destructive ? 'border-error/40 bg-error-muted/40' : 'border-accent/40 bg-accent/10') : 'border-transparent hover:bg-surface-hover'
      }`}
    >
      <Checkbox
        size="sm"
        checked={isIncluded}
        disabled={readOnly || locked || isChosen}
        onChange={() => onToggleItem(group.id, item.id)}
        ariaLabel={`Include ${item.fileName}`}
      />
      <span className="w-36 flex-shrink-0 text-xs font-medium leading-4">{marker}</span>
      <div className="min-w-0 flex-1 space-y-0.5">
        <div
          className={`truncate font-mono text-xs ${acts && destructive ? 'text-content-muted line-through' : 'text-content'}`}
          title={item.fileName}
        >
          {item.fileName}
        </div>
        <div className="truncate font-mono text-[11px] text-content-muted" title={item.folder}>
          {item.folder}
        </div>
        {kind === 'metadata' && <Change item={item} />}
        {kind === 'plate_solve' && <SolveLines item={item} />}
        {facts.length > 0 && <FactLine facts={facts} />}
        {locked && (
          <div className="flex items-start gap-1 text-[11px] text-content-muted">
            <Lock size={11} className="mt-[2px] shrink-0" aria-hidden />
            <span>{item.locked}</span>
          </div>
        )}
        {!locked && item.evidence && <div className="text-[11px] text-content-muted">{item.evidence}</div>}
      </div>
      {(size || modified) && (
        <div className="flex-shrink-0 text-right text-[11px] text-content-muted">
          {size && <div>{size}</div>}
          {modified && <div>{modified}</div>}
        </div>
      )}
      {canKeep && (
        <button
          type="button"
          disabled={readOnly}
          aria-label={`${group.chooseOne}: ${item.fileName}`}
          onClick={() => onChoose(group.id, item.id)}
          className="flex-shrink-0 rounded border border-border px-2 py-0.5 text-[11px] text-content-secondary hover:border-accent/50 hover:text-accent focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent disabled:cursor-not-allowed disabled:opacity-50"
        >
          Keep this one
        </button>
      )}
    </div>
  );
}

/** A metadata change: the field, what the frame holds now, and the value the plan would write. */
function Change({ item }: { item: PlanItem }) {
  const field = item.values.field ?? '';
  const current = item.values.current ?? '';
  const proposed = item.values.proposed ?? '';
  return (
    <div className="flex flex-wrap items-center gap-x-1.5 text-[11px]">
      <span className="text-content-faint">{humanLabel(field)}</span>
      <span className="text-content-muted">{humanValue(field, current)}</span>
      <ArrowRight size={11} className="text-content-faint" aria-hidden />
      <span className="font-medium text-content">{humanValue(field, proposed)}</span>
    </div>
  );
}

/** A plate-solve row: the position the frame holds now, and what a solve would write to it. */
function SolveLines({ item }: { item: PlanItem }) {
  const hint = item.values.coordinates;
  const noName = item.values.object === '(none)';
  return (
    <div className="flex flex-wrap gap-x-3 gap-y-0.5 text-[11px]">
      {hint && (
        <span>
          <span className="text-content-faint">Position hint</span> <span className="text-content-secondary">{hint}</span>
        </span>
      )}
      <span>
        <span className="text-content-faint">A solve writes</span>{' '}
        <span className="text-content-secondary">{noName ? 'its coordinates and the name of the object in its field' : 'its coordinates'}</span>
      </span>
    </div>
  );
}
