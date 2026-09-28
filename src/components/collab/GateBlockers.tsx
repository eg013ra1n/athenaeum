import { useState } from 'react';
import { ChevronDown } from 'lucide-react';
import type { GateBlocker, GateReport } from '../../types/models';

const SOLVE_REASONS = new Set(['no coordinates', 'unknown pixel scale']);

function line(b: GateBlocker): string {
  switch (b.kind) {
    case 'analyze': return `${b.frames} frames have no analysis`;
    case 'solve': return `${b.frames} frames have no coordinates or pixel scale`;
    case 'linkCalibration':
    case 'buildMasters': return `${b.frames} frames are not calibrated`;
    case 'mapFilter': return `${b.names.length} filter name${b.names.length === 1 ? '' : 's'} need a mapping`;
    case 'threshold': return `${b.frames} frames fail a threshold`;
    case 'uuid': return `${b.frames} frames have no uuid — re-scan their folder`;
    case 'outsideTarget': return `${b.frames} frames are outside the target`;
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
  const calFrames = calBlockers.reduce((n, b) => Math.max(n, b.frames), 0);
  const solveIds = gate.rows.filter((r) => r.failures.some((f) => SOLVE_REASONS.has(f))).map((r) => r.frameId);

  const setPicker = (key: string, label: string, sets: number[], pick: (id: number) => void) =>
    sets.length === 1 ? (
      <button type="button" className={BTN} onClick={() => pick(sets[0])}>{label}</button>
    ) : (
      <span className="relative">
        <button type="button" className={BTN} onClick={() => setMenuFor(menuFor === key ? null : key)}>{label} <ChevronDown size={11} /></button>
        {menuFor === key && (
          <ul role="menu" className="absolute z-10 mt-1 rounded border border-border bg-surface p-1 text-xs shadow">
            {sets.map((s) => (
              <li key={s} role="menuitem" className="cursor-pointer rounded px-2 py-1 hover:bg-surface-hover" onClick={() => { setMenuFor(null); pick(s); }}>Set #{s}</li>
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
              <span>{isCalibration ? `${calFrames} frames are not calibrated` : line(b)}</span>
              {b.kind === 'analyze' && setPicker('analyze', 'Analyze', b.sets, onAnalyze)}
              {b.kind === 'solve' && (
                <button type="button" className={BTN} disabled={solveBusy} onClick={() => onSolve(solveIds)}>Solve {b.frames} frames</button>
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
