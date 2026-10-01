import { useEffect, useRef, useState, type JSX } from 'react';
import { ChevronDown } from 'lucide-react';
import type { FrameVM } from './frames';
import { useOverlayEscape } from '../../ui/useOverlayEscape';

export interface ReasonGroupActionProps {
  kind: string; // the Reason group's key (a BLOCKER_ORDER kind or '')
  rows: FrameVM[]; // the group's frames
  solveBusy: boolean;
  analyzeBusy: ReadonlySet<number>;
  onSolve: (frameIds: number[]) => void;
  onAnalyze: (setId: number) => void;
  onOpenCalibration: (setId: number) => void;
  onMapFilters: () => void;
}

const BTN =
  'inline-flex items-center gap-1 rounded border border-border px-2 py-0.5 text-xs text-content-secondary transition-colors hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-50';
const MUTED = 'text-xs text-content-muted';

interface SetEntry {
  id: number;
  name: string;
}

/** Distinct `setId`s among the group's rows, named from the row itself
 *  (`setName ?? Set #id`) — precise per-group data, never the project-wide
 *  gate (spec ruling: Held back's actions work from the group's own rows). */
function setsOf(rows: FrameVM[]): SetEntry[] {
  const seen = new Map<number, string>();
  for (const r of rows) {
    if (r.setId !== null && !seen.has(r.setId)) seen.set(r.setId, r.setName ?? `Set #${r.setId}`);
  }
  return Array.from(seen, ([id, name]) => ({ id, name }));
}

/**
 * Held back's per-Reason-group fix button (Task 10, spec 2026-09-30). Sits
 * on a Reason group's header (`ProjectFrameTable`'s `groupAction`) and
 * replaces the retired project-wide blocker line for the same cause — same
 * set-picker menu behaviour (Escape / outside-click close), reused verbatim
 * from it, but fed the group's OWN rows so the count and the offered
 * sets are exactly this group's, not the whole project's gate.
 */
export default function ReasonGroupAction({
  kind, rows, solveBusy, analyzeBusy, onSolve, onAnalyze, onOpenCalibration, onMapFilters,
}: ReasonGroupActionProps): JSX.Element | null {
  const [menuFor, setMenuFor] = useState<string | null>(null);
  const menuRef = useRef<HTMLSpanElement>(null);

  // Escape or a click outside the open menu closes it — the ONE open menu at
  // a time `menuFor` tracks, so one ref (attached only to whichever
  // `setPicker` call is currently open) is enough (the retired blocker line's pattern).
  // Escape goes through the overlay stack as a popover: over a docked side
  // panel it closes the menu alone.
  useOverlayEscape(menuFor !== null, 'popover', () => setMenuFor(null));
  // If the rows refresh while a menu is open and its picker collapses to a
  // single button (or vanishes), nothing renders for `menuFor` any more:
  // close it, so no invisible popover holds the stack or swallows Escape.
  useEffect(() => {
    if (menuFor !== null && !menuRef.current) setMenuFor(null);
  });
  useEffect(() => {
    if (!menuFor) return;
    const onPointerDown = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) setMenuFor(null);
    };
    document.addEventListener('mousedown', onPointerDown);
    return () => {
      document.removeEventListener('mousedown', onPointerDown);
    };
  }, [menuFor]);

  const setPicker = (
    key: string,
    label: string,
    sets: SetEntry[],
    pick: (id: number) => void,
    isBusy: (id: number) => boolean = () => false,
  ) =>
    sets.length === 1 ? (
      <button type="button" className={BTN} disabled={isBusy(sets[0].id)} onClick={() => pick(sets[0].id)}>
        {label}
      </button>
    ) : (
      <span className="relative" ref={menuFor === key ? menuRef : undefined}>
        <button type="button" className={BTN} onClick={() => setMenuFor(menuFor === key ? null : key)}>
          {label} <ChevronDown size={11} />
        </button>
        {menuFor === key && (
          <ul role="menu" className="absolute z-10 mt-1 min-w-[8rem] rounded border border-border bg-surface p-1 text-xs shadow">
            {sets.map((s) => (
              <li key={s.id} role="none">
                <button
                  type="button"
                  role="menuitem"
                  disabled={isBusy(s.id)}
                  className="block w-full whitespace-nowrap rounded px-2 py-1 text-left hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-50"
                  onClick={() => {
                    if (isBusy(s.id)) return;
                    setMenuFor(null);
                    pick(s.id);
                  }}
                >
                  {s.name}
                  {isBusy(s.id) ? ' (busy…)' : ''}
                </button>
              </li>
            ))}
          </ul>
        )}
      </span>
    );

  switch (kind) {
    case 'analyze': {
      const sets = setsOf(rows);
      if (sets.length === 0) return null;
      return setPicker('analyze', 'Analyze', sets, onAnalyze, (id) => analyzeBusy.has(id));
    }
    case 'solve': {
      const ids = rows.map((r) => r.frameId).filter((id): id is number => id !== null);
      if (ids.length === 0) return null;
      return (
        <button type="button" className={BTN} disabled={solveBusy} onClick={() => onSolve(ids)}>
          Solve {rows.length}
        </button>
      );
    }
    case 'linkCalibration':
    case 'buildMasters':
    case 'attest': {
      const sets = setsOf(rows);
      if (sets.length === 0) return null;
      return (
        <span className="inline-flex items-center gap-1">
          {setPicker('cal', 'Open calibration', sets, onOpenCalibration)}
          {setPicker('attest', 'Attest as calibrated…', sets, onOpenCalibration)}
        </span>
      );
    }
    case 'mapFilter':
      return (
        <button type="button" className={BTN} onClick={onMapFilters}>
          Map filters
        </button>
      );
    case 'threshold':
      return <span className={MUTED}>quality — the frames themselves</span>;
    case 'uuid':
      return <span className={MUTED}>re-scan the folder</span>;
    case 'outsideTarget':
      return <span className={MUTED}>outside the target radius</span>;
    default:
      return null;
  }
}
