import { useState, type ReactNode } from 'react';
import { ArrowRight, ChevronDown, ChevronRight, Crosshair, Lock, Pencil } from 'lucide-react';
import { Checkbox } from '../settings/Checkbox';
import { counted, fmt, groupLock, humanValue, splitFacts, type Fact } from './planLabels';
import type { PlanGroup, PlanItem } from '../../types/mcp';

/**
 * The dense table of a metadata or plate-solve plan (the Missing Metadata table's canon): a group row with the folder
 * or the value once, one muted line of what the group's frames share, then one 12 px line per frame. What a whole
 * group has in common is said on the group row, never on its frames.
 */

/** Frames shown per group before "+N more". */
const SHOWN = 12;

const CB = 'w-[34px] shrink-0 text-center';
const COLS: Record<'metadata' | 'plate_solve', string> = {
  plate_solve: 'grid-cols-[34px_minmax(0,1fr)_230px]',
  metadata: 'grid-cols-[34px_minmax(0,1.1fr)_minmax(0,1fr)_230px]',
};

type TableKind = 'metadata' | 'plate_solve';

const upper = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);
const noValue = (v: string) => (v === '(none)' ? 'none' : v);

function Cell({ children, className = '' }: { children?: ReactNode; className?: string }) {
  return <div className={`min-w-0 px-1.5 ${className}`}>{children}</div>;
}

/** The sticky header row: the select-all box and the column names. */
export function PlanTableHead({ kind, checked, mixed, disabled, onToggle }: { kind: TableKind; checked: boolean; mixed: boolean; disabled: boolean; onToggle: () => void }) {
  const th = 'px-1.5 py-1.5 text-left text-xs font-semibold text-content-secondary whitespace-nowrap';
  return (
    <div data-testid="plan-table-head" className={`sticky top-0 z-[3] grid items-center bg-surface-hover ${COLS[kind]}`}>
      <div className={`${CB} flex justify-center py-1.5`}>
        <Checkbox size="sm" checked={checked} indeterminate={mixed} disabled={disabled} onChange={onToggle} ariaLabel="Select all" />
      </div>
      <div className={th}>File</div>
      {kind === 'metadata' && <div className={th}>Folder</div>}
      <div className={th}>{kind === 'metadata' ? 'Now → Proposed' : 'Position hint'}</div>
    </div>
  );
}

/** The folder of a plate-solve group: the last two folders bright, the rest of the path muted, cut from the left. */
function FolderPath({ path }: { path: string }) {
  const parts = path.split('/');
  const tail = parts.slice(-2).join('/');
  const head = path.slice(0, path.length - tail.length);
  return (
    <span title={path} className="min-w-0 overflow-hidden text-ellipsis whitespace-nowrap text-left font-mono text-xs font-medium text-content [direction:rtl]">
      <bdi>
        <span className="text-content-faint">{head}</span>
        {tail}
      </bdi>
    </span>
  );
}

/** What the unlocked frames of a group all say as their evidence; `null` when they differ or none says one. */
function sharedEvidence(group: PlanGroup): string | null {
  const live = group.items.filter((i) => i.locked === null);
  const first = live[0]?.evidence ?? null;
  return first !== null && live.every((i) => i.evidence === first) ? first : null;
}

/** "Position hint none / on 9 frames / on every frame". */
function hintSummary(group: PlanGroup): string {
  const n = group.items.filter((i) => hasHint(i)).length;
  if (n === 0) return 'none';
  if (n === group.items.length) return group.items.length === 1 ? 'on this frame' : 'on every frame';
  return `on ${counted(n, 'frame', 'frames')}`;
}
const hasHint = (i: PlanItem) => {
  const h = i.values.coordinates;
  return !!h && h !== 'none';
};

export interface PlanTableGroupProps {
  kind: TableKind;
  group: PlanGroup;
  facts: { key: string; label: string }[];
  included: string[];
  readOnly: boolean;
  open: boolean;
  onToggleOpen: (groupId: string) => void;
  onToggleItem: (groupId: string, itemId: string) => void;
  onToggleGroup: (groupId: string) => void;
}

export function PlanTableGroup({ kind, group, facts, included, readOnly, open, onToggleOpen, onToggleItem, onToggleGroup }: PlanTableGroupProps) {
  const [shown, setShown] = useState(SHOWN);
  const locked = groupLock(group);
  const tickable = group.items.filter((i) => i.locked === null);
  const ticked = tickable.filter((i) => included.includes(i.id)).length;
  const bodyId = `plan-${group.id}`;
  const Icon = kind === 'metadata' ? Pencil : Crosshair;
  const word = kind === 'metadata' ? 'Fill' : 'Solve';

  // A group of one says its facts on the group row too; the others say what all their frames share.
  const split = splitFacts(facts, group);
  const single = group.items.length === 1 && group.items[0] ? splitFacts(facts, { ...group, items: [group.items[0], group.items[0]] }) : null;
  const shared: Fact[] = single ? single.shared : split.shared;
  const own = (item: PlanItem): Fact[] => (single ? [] : split.own(item));
  const evidence = sharedEvidence(group);

  const factBits: ReactNode[] = shared.map((f) => (
    <span key={f.key}>
      {f.label} <b className="font-normal text-content-secondary">{noValue(f.value)}</b>
    </span>
  ));
  if (kind === 'plate_solve') {
    factBits.push(
      <span key="hint">
        Position hint <b className="font-normal text-content-secondary">{hintSummary(group)}</b>
      </span>,
    );
  }
  if (!locked && evidence) factBits.push(<span key="why">{kind === 'metadata' ? `Why: ${evidence}` : upper(evidence)}</span>);
  if (locked && group.note) factBits.push(<span key="note">{upper(group.note)}</span>);

  // A metadata group "Object: M31" reads "Object → M31"; one with nothing proposed keeps its words.
  const heading = kind === 'metadata' && !locked ? group.title.replace(/^([^:→]+): (.+)$/, '$1 → $2') : group.title;
  const rows = group.items.slice(0, shown);
  const more = group.items.length - rows.length;

  return (
    <div>
      <div className={`grid grid-cols-[34px_minmax(0,1fr)] items-start border-y border-border ${locked ? 'bg-surface-hover' : 'bg-accent/15'} py-[5px]`}>
        <div className={`${CB} flex justify-center pt-[5px]`}>
          <Checkbox
            size="sm"
            checked={tickable.length > 0 && ticked === tickable.length}
            indeterminate={ticked > 0 && ticked < tickable.length}
            disabled={readOnly || tickable.length === 0}
            onChange={() => onToggleGroup(group.id)}
            ariaLabel={`Include all in ${group.title}`}
          />
        </div>
        <div className="min-w-0 pr-2">
          <div className="flex min-w-0 items-center gap-2">
            <button
              type="button"
              aria-expanded={open}
              aria-controls={bodyId}
              aria-label={`${open ? 'Collapse' : 'Expand'} ${group.title}`}
              onClick={() => onToggleOpen(group.id)}
              className="inline-flex flex-none text-content-faint hover:text-content focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
            >
              {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
            </button>
            {kind === 'plate_solve' ? (
              <FolderPath path={group.title} />
            ) : (
              <span className="whitespace-nowrap text-xs font-semibold text-content" title={group.title}>
                {heading}
              </span>
            )}
            <span className="whitespace-nowrap text-xs text-content-muted">
              <b className="font-medium text-content">{fmt(group.items.length)}</b> {group.items.length === 1 ? 'frame' : 'frames'}
            </span>
            {locked ? (
              <span
                title={locked}
                className="ml-auto inline-flex h-6 min-w-0 max-w-[280px] flex-none items-center gap-1 rounded-full border border-border bg-surface-hover px-2 text-[11px] text-content-muted"
              >
                <Lock size={12} className="flex-none" aria-hidden />
                <span className="truncate">{locked}</span>
              </span>
            ) : (
              <span
                className={`ml-auto inline-flex h-6 flex-none items-center gap-1 whitespace-nowrap rounded-full border px-2 text-[11px] ${
                  ticked === 0 ? 'border-border bg-surface-hover text-content-muted' : 'border-accent/40 bg-accent/15 text-accent'
                }`}
              >
                <Icon size={12} aria-hidden />
                {word} {fmt(ticked)} of {fmt(tickable.length)}
              </span>
            )}
          </div>
          {factBits.length > 0 && (
            <div className="ml-6 mt-0.5 text-[11px] text-content-faint">
              {factBits.map((b, i) => (
                <span key={i}>
                  {i > 0 && <span aria-hidden className="mx-1.5 text-border">·</span>}
                  {b}
                </span>
              ))}
            </div>
          )}
        </div>
      </div>

      {open && (
        <div id={bodyId}>
          {rows.map((item, i) => (
            <TableRow
              key={item.id}
              kind={kind}
              group={group}
              item={item}
              index={i}
              own={own(item)}
              showEvidence={!locked && evidence === null}
              groupLocked={locked !== null}
              isIncluded={included.includes(item.id)}
              readOnly={readOnly}
              onToggleItem={onToggleItem}
            />
          ))}
          {more > 0 && (
            <div className="bg-surface py-[3px] pl-10 text-xs">
              <button type="button" onClick={() => setShown(group.items.length)} className="px-1.5 py-px text-[11px] leading-[1.4] text-accent hover:underline">
                +{fmt(more)} more {more === 1 ? 'frame' : 'frames'} in this {kind === 'plate_solve' ? 'folder' : 'group'}
              </button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function LockChip({ reason }: { reason: string }) {
  return (
    <span title={reason} className="inline-flex max-w-full items-center gap-1 rounded-[3px] bg-surface-hover px-1.5 text-[11px] leading-[18px] text-content-muted">
      <Lock size={10} className="flex-none" aria-hidden />
      <span className="truncate">{reason}</span>
    </span>
  );
}

function TableRow({
  kind, group, item, index, own, showEvidence, groupLocked, isIncluded, readOnly, onToggleItem,
}: {
  kind: TableKind;
  group: PlanGroup;
  item: PlanItem;
  index: number;
  own: Fact[];
  /** The frames do not agree on their evidence, so each says its own. */
  showEvidence: boolean;
  groupLocked: boolean;
  isIncluded: boolean;
  readOnly: boolean;
  onToggleItem: (groupId: string, itemId: string) => void;
}) {
  const locked = item.locked !== null;
  const off = !locked && !isIncluded;
  const dim = <span className="text-content-faint">—</span>;
  const lastCell = (): ReactNode => {
    if (kind === 'plate_solve') {
      if (locked) return groupLocked ? null : <LockChip reason={item.locked ?? ''} />;
      return hasHint(item) ? <span className="whitespace-nowrap text-content-secondary">{item.values.coordinates}</span> : null;
    }
    const field = item.values.field ?? '';
    const now = humanValue(field, item.values.current ?? '');
    const proposed = humanValue(field, item.values.proposed ?? '');
    return (
      <span className="flex min-w-0 items-center whitespace-nowrap">
        <span className="text-content-faint">{now}</span>
        <ArrowRight size={11} className="mx-1 flex-none text-content-faint" aria-hidden />
        {locked ? (groupLocked ? dim : <LockChip reason={item.locked ?? ''} />) : <span className="font-medium text-content">{proposed}</span>}
      </span>
    );
  };
  return (
    <div
      data-testid="plan-row"
      className={`grid items-center border-b border-border/30 py-1 text-xs hover:bg-surface-hover ${COLS[kind]} ${index % 2 ? 'bg-surface' : 'bg-surface-elevated'}`}
    >
      <div className={`${CB} flex justify-center`}>
        <Checkbox size="sm" checked={isIncluded && !locked} disabled={readOnly || locked} onChange={() => onToggleItem(group.id, item.id)} ariaLabel={`Include ${item.fileName}`} />
      </div>
      <Cell className={`truncate ${off || locked ? 'text-content-muted' : 'text-content-secondary'}`}>
        <span title={item.fileName}>{item.fileName}</span>
        {own.map((f) => (
          <span key={f.key} className="ml-2 text-[11px] text-content-faint">
            {f.label} {noValue(f.value)}
          </span>
        ))}
        {showEvidence && !locked && item.evidence && <span className="ml-2 text-[11px] text-content-faint">{item.evidence}</span>}
      </Cell>
      {kind === 'metadata' && (
        <Cell className="truncate text-[11px] text-content-faint">
          <span title={item.folder}>{`…/${item.folder.split('/').slice(-3).join('/')}`}</span>
        </Cell>
      )}
      <Cell>{lastCell()}</Cell>
    </div>
  );
}
