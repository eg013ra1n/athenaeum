import { useEffect, useRef, useState } from 'react';
import { Info } from 'lucide-react';
import { Button, DialogShell } from '../ui';
import { formatBytes } from '../stacking/formatBytes';
import { useMcpPlan } from '../../contexts/McpPlanContext';
import { PlanView } from './PlanView';
import { chooseOne, initialSelection, selectionProblems, toggleGroup, toggleItem, totals } from './planSelection';
import type { McpPlanView } from '../../types/mcp';

/**
 * The plan window (docs/mcp/README.md, "The plan window"): the one place where the user looks over what the
 * assistant prepared, ticks, picks the copy that stays, and presses Apply or Cancel. Rendered once, in `Layout`.
 */
export function PlanWindow() {
  const { plan } = useMcpPlan();
  // A new plan id is a new dialog: its selection and its initial focus start over.
  return plan ? <PlanDialog key={plan.planId} plan={plan} /> : null;
}

/** Apply stays disabled this long after the window shows a waiting plan, so a click aimed elsewhere cannot apply it. */
const APPLY_ARM_MS = 1000;

const files = (n: number) => `${n} file${n === 1 ? '' : 's'}`;

function PlanDialog({ plan }: { plan: McpPlanView }) {
  const { busy, apply, cancel, dismiss, openNewer } = useMcpPlan();
  const kind = plan.state.kind;
  const preparing = kind === 'preparing';

  // The selection starts from the plan; it starts over once when a preparing plan gets its items.
  const [store, setStore] = useState(() => ({ preparing, sel: initialSelection(plan) }));
  let sel = store.sel;
  if (store.preparing !== preparing) {
    sel = initialSelection(plan);
    setStore({ preparing, sel });
  }
  const update = (f: (s: typeof sel) => typeof sel) => setStore({ preparing, sel: f(sel) });

  // The banner is for a replaced plan only; a newer plan waits unseen while this one applies.
  const superseded = kind === 'replaced';
  const waiting = kind === 'waiting';
  const canAnswer = (waiting || preparing) && !busy;
  const sum = totals(plan, sel);
  const problems = waiting ? selectionProblems(plan, sel) : [];

  // Nothing pressable has the initial focus: the window opens by itself over any page, so a Space or Enter meant for
  // something else must press nothing. The title line takes it (`data-autofocus`, not a tab stop of its own).
  const mode = waiting || preparing ? 'cancel' : kind === 'applying' ? 'busy' : 'close';
  const titleRef = useRef<HTMLSpanElement>(null);
  const first = useRef(true);
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    // The button that was pressed is gone (Cancel/Apply -> Close): focus returns to the title, not to another button.
    titleRef.current?.focus();
  }, [mode]);

  // Apply arms one second after the window shows a waiting plan (on open, and when a preparing plan turns waiting).
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    setArmed(false);
    if (!waiting) return;
    const t = setTimeout(() => setArmed(true), APPLY_ARM_MS);
    return () => clearTimeout(t);
  }, [waiting]);

  const footer = (
    <>
      <div className="mr-auto min-w-0 self-center">
        {(waiting || kind === 'applying') && (
          <p className="font-medium text-content-secondary">
            {files(sum.items)} · {formatBytes(sum.bytes)}
          </p>
        )}
        {problems.length > 0 && (
          <ul className="text-warning" role="status">
            {problems.map((p) => (
              <li key={p}>{p}</li>
            ))}
          </ul>
        )}
      </div>
      <span className="contents">
      {mode === 'cancel' && (
        <>
          <Button disabled={busy} onClick={() => void cancel()}>
            Cancel
          </Button>
          <Button
            variant="primary"
            disabled={!waiting || !armed || !canAnswer || superseded || problems.length > 0}
            onClick={() => void apply({ action: 'apply', selection: sel })}
          >
            Apply
          </Button>
        </>
      )}
      {mode === 'busy' && (
        <Button variant="primary" disabled>
          Apply
        </Button>
      )}
      {mode === 'close' && (
        <>
          <Button onClick={dismiss}>
            Close
          </Button>
          {/* A replaced plan keeps a dead Apply, so it is plain that it cannot be applied. */}
          {kind === 'replaced' && (
            <Button variant="primary" disabled>
              Apply
            </Button>
          )}
        </>
      )}
      </span>
    </>
  );

  return (
    <DialogShell
      size="lg"
      title={
        <span ref={titleRef} data-autofocus tabIndex={-1} className="outline-none">
          {plan.title}
        </span>
      }
      closeOnScrim={mode === 'close'}
      busy={busy || kind === 'applying'}
      // Escape, the cross and a click outside decline a plan that still waits; a finished plan just closes.
      onClose={mode === 'cancel' ? () => void cancel() : dismiss}
      footer={footer}
    >
      <div className="mb-3 flex flex-wrap items-baseline justify-between gap-x-4 gap-y-0.5 text-[11.5px] text-content-muted">
        <span className="min-w-0 break-all">{`${plan.source.build} ${plan.source.version} · catalog ${plan.source.catalog}`}</span>
        {plan.expiresAt && waiting && <span>Expires {plan.expiresAt}</span>}
      </div>
      {superseded && (
        <div role="status" className="mb-3 flex items-center gap-2 rounded-md border border-warning/50 bg-warning-muted px-2.5 py-2 text-content">
          <Info size={14} className="shrink-0 text-warning" aria-hidden />
          <span className="min-w-0 flex-1">The assistant prepared a newer plan</span>
          <Button size="sm" variant="primary" onClick={() => void openNewer()}>
            Open the new plan
          </Button>
        </div>
      )}
      <PlanView
        plan={plan}
        selection={sel}
        readOnly={!waiting || superseded || busy}
        onToggleItem={(g, i) => update((s) => toggleItem(plan, s, g, i))}
        onToggleGroup={(g) => update((s) => toggleGroup(plan, s, g))}
        onChoose={(g, i) => update((s) => chooseOne(plan, s, g, i))}
      />
    </DialogShell>
  );
}
