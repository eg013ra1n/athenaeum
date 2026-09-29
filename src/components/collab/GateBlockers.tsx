import { useEffect, useRef, useState } from 'react';
import { ChevronDown } from 'lucide-react';
import type { GateBlocker, GateReport } from '../../types/models';

const SOLVE_REASONS = new Set(['no coordinates', 'unknown pixel scale']);

/** `1 frame has…` / `2 frames have…` (and `is`/`are`, `fails`/`fail`) — every
 * count-driven line pluralises both the noun and its verb. */
function frames(n: number, verb: 'have' | 'are' | 'fail'): string {
  const noun = `${n} frame${n === 1 ? '' : 's'}`;
  const singular = { have: 'has', are: 'is', fail: 'fails' }[verb];
  return `${noun} ${n === 1 ? singular : verb}`;
}

function line(b: GateBlocker): string {
  switch (b.kind) {
    case 'analyze': return `${frames(b.frames, 'have')} no analysis`;
    case 'solve': return `${frames(b.frames, 'have')} no coordinates or pixel scale`;
    case 'linkCalibration':
    case 'buildMasters': return `${frames(b.frames, 'are')} not calibrated`;
    case 'mapFilter': return `${b.names.length} filter name${b.names.length === 1 ? '' : 's'} ${b.names.length === 1 ? 'needs' : 'need'} a mapping`;
    case 'threshold': return `${frames(b.frames, 'fail')} a threshold`;
    case 'uuid': return `${frames(b.frames, 'have')} no uuid — re-scan their folder`;
    case 'outsideTarget': return `${frames(b.frames, 'are')} outside the target`;
    default: return '';
  }
}

const BTN = 'inline-flex items-center gap-1 rounded border border-border px-2 py-0.5 text-xs text-content-secondary transition-colors hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-50';

/** Spec 2026-09-28 §7.2 — "What blocks publishing": one line per cause with
 * one button. `attest` rides on the calibration line; `buildMasters` folds
 * into the calibration line (same navigation) — a project with no
 * `linkCalibration` blocker but a `buildMasters` one still gets the single
 * calibration line, rather than rendering nothing. */
export default function GateBlockers({
  gate, solveBusy, analyzeBusy, onMapFilters, onOpenCalibration, onSolve, onAnalyze,
}: {
  gate: GateReport;
  solveBusy: boolean;
  analyzeBusy: Set<number>;
  onMapFilters: () => void;
  onOpenCalibration: (setId: number) => void;
  onSolve: (frameIds: number[]) => void;
  onAnalyze: (setId: number) => void;
}) {
  const [menuFor, setMenuFor] = useState<string | null>(null);
  const menuRef = useRef<HTMLSpanElement>(null);

  // Escape or a click outside the open menu closes it — the ONE open menu at
  // a time `menuFor` tracks, so one ref (attached only to whichever
  // `setPicker` call is currently open) is enough.
  useEffect(() => {
    if (!menuFor) return;
    const onKeyDown = (e: KeyboardEvent) => { if (e.key === 'Escape') setMenuFor(null); };
    const onPointerDown = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) setMenuFor(null);
    };
    document.addEventListener('keydown', onKeyDown);
    document.addEventListener('mousedown', onPointerDown);
    return () => {
      document.removeEventListener('keydown', onKeyDown);
      document.removeEventListener('mousedown', onPointerDown);
    };
  }, [menuFor]);

  const hasLinkCalibration = gate.blockers.some((b) => b.kind === 'linkCalibration');
  // `buildMasters` only carries the calibration line when there is no
  // `linkCalibration` blocker to carry it instead — the two never both
  // render their own line.
  const shown = gate.blockers.filter((b) => {
    if (b.kind === 'attest') return false;
    if (b.kind === 'buildMasters') return !hasLinkCalibration;
    return true;
  });
  if (shown.length === 0) return null;
  const calBlockers = gate.blockers.filter((b) => b.kind === 'linkCalibration' || b.kind === 'buildMasters');
  const calSets = Array.from(new Set(calBlockers.flatMap((b) => b.sets)));
  // `linkCalibration` and `buildMasters` count DISJOINT frame sets (a frame
  // is either unlinked or linked-but-masterless, never counted under both),
  // so the max of the two undercounts whenever both are non-empty. The
  // backend's `attest` blocker (`derive_blockers`, `collab/gate.rs`) is added
  // alongside EITHER of them and carries the true union — use it, falling
  // back to the max only if it is somehow absent.
  const attestBlocker = gate.blockers.find((b) => b.kind === 'attest');
  const calFrames = attestBlocker?.frames ?? calBlockers.reduce((n, b) => Math.max(n, b.frames), 0);
  const solveIds = gate.rows.filter((r) => r.failures.some((f) => SOLVE_REASONS.has(f))).map((r) => r.frameId);

  const setPicker = (
    key: string,
    label: string,
    sets: number[],
    pick: (id: number) => void,
    isBusy: (id: number) => boolean = () => false,
  ) =>
    sets.length === 1 ? (
      <button type="button" className={BTN} disabled={isBusy(sets[0])} onClick={() => pick(sets[0])}>{label}</button>
    ) : (
      <span className="relative" ref={menuFor === key ? menuRef : undefined}>
        <button type="button" className={BTN} onClick={() => setMenuFor(menuFor === key ? null : key)}>{label} <ChevronDown size={11} /></button>
        {menuFor === key && (
          <ul role="menu" className="absolute z-10 mt-1 rounded border border-border bg-surface p-1 text-xs shadow">
            {sets.map((s) => (
              <li key={s} role="none">
                <button
                  type="button"
                  role="menuitem"
                  disabled={isBusy(s)}
                  className="block w-full rounded px-2 py-1 text-left hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-50"
                  onClick={() => { if (isBusy(s)) return; setMenuFor(null); pick(s); }}
                >
                  {/* `GateBlocker.sets` (models.ts) is `Array<number>` — ids
                      only, no name. Showing the set's actual name here would
                      need the backend to carry it; no backend change this
                      wave, so `Set #id` stays until it does. */}
                  Set #{s}{isBusy(s) ? ' (busy…)' : ''}
                </button>
              </li>
            ))}
          </ul>
        )}
      </span>
    );

  let calibrationRendered = false;
  return (
    <div className="rounded border border-border bg-surface p-3 text-sm">
      <p className="mb-2 font-medium text-content">What blocks publishing</p>
      <ul className="space-y-1.5">
        {shown.map((b) => {
          const isCalibration = b.kind === 'linkCalibration' || b.kind === 'buildMasters';
          if (isCalibration) {
            if (calibrationRendered) return null;
            calibrationRendered = true;
          }
          return (
            <li key={b.kind} className="flex flex-wrap items-center gap-2 text-content-secondary">
              <span>{isCalibration ? `${frames(calFrames, 'are')} not calibrated` : line(b)}</span>
              {b.kind === 'analyze' && setPicker('analyze', 'Analyze', b.sets, onAnalyze, (id) => analyzeBusy.has(id))}
              {b.kind === 'solve' && (
                <button type="button" className={BTN} disabled={solveBusy} onClick={() => onSolve(solveIds)}>Solve {b.frames} frame{b.frames === 1 ? '' : 's'}</button>
              )}
              {isCalibration && (
                <>
                  {setPicker('cal', 'Open calibration', calSets, onOpenCalibration)}
                  {setPicker('attest', 'Attest as calibrated…', calSets, onOpenCalibration)}
                </>
              )}
              {b.kind === 'mapFilter' && <button type="button" className={BTN} onClick={onMapFilters}>Map filters</button>}
              {b.kind === 'analyze' && b.sets.some((s) => analyzeBusy.has(s)) && <span className="text-xs text-content-muted">analyzing…</span>}
            </li>
          );
        })}
      </ul>
    </div>
  );
}
