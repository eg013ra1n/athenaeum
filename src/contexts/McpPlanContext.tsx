import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { api } from '../api';
import { useNotifications } from './NotificationContext';
import type { McpPlanView, PlanAnswer } from '../types/mcp';

/**
 * The plan window's state (docs/mcp/README.md, "The plan window"): the one plan on screen, kept current from
 * `mcp-plan-updated {planId}` events and `get_mcp_plan`, and the two answers.
 *
 * The plan on screen is never swapped under the cursor. An event for another plan only records that a newer one
 * waits (`newerPlanId`); the window offers it as "Open the new plan". A plan opened while no window is shown is
 * not a swap and opens at once.
 *
 * An applying plan never locks the app: Close hides the window and the apply carries on (the plan stays tracked
 * here); the window opens again with the report when the plan reaches its end state. While a plan is
 * `preparing`, `waiting` or `applying` it is also re-read every 3 s and when the tab becomes visible, because the
 * web build can drop events (an EventSource reconnect, a lagged broadcast).
 */
interface McpPlanContextValue {
  /** The plan the window shows; `null` when the window is closed or hidden while the plan applies. */
  plan: McpPlanView | null;
  /** A plan newer than the shown one is waiting (the shown one is replaced, or finished). */
  newerPlanId: string | null;
  /** An answer is in flight. */
  busy: boolean;
  apply: (answer: Extract<PlanAnswer, { action: 'apply' }>) => Promise<void>;
  cancel: () => Promise<void>;
  /** Close the window; a newer plan, if one waits, opens in its place. While a plan applies it only hides the window. */
  dismiss: () => void;
  openNewer: () => Promise<void>;
}

const McpPlanContext = createContext<McpPlanContextValue | null>(null);

/** A plan that still needs the user (or is being applied): the kinds that open a closed window. */
function undecided(plan: McpPlanView): boolean {
  const k = plan.state.kind;
  return k === 'preparing' || k === 'waiting' || k === 'applying';
}

/** How often a plan that is not finished is re-read, in case an event was lost. */
const REREAD_MS = 3000;

export function McpPlanProvider({ children }: { children: ReactNode }) {
  const { notify } = useNotifications();
  const notifyRef = useRef(notify);
  notifyRef.current = notify;

  const [plan, setPlanState] = useState<McpPlanView | null>(null);
  const [newerPlanId, setNewerState] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // The window is hidden while its plan applies; the plan is still tracked and shows its report when it ends.
  const [hidden, setHidden] = useState(false);
  const planRef = useRef<McpPlanView | null>(null);
  const newerRef = useRef<string | null>(null);
  // Per plan id: the newest request whose answer was used, so a slow answer never overwrites a newer one.
  const seqRef = useRef(0);
  const usedRef = useRef(new Map<string, number>());

  const setPlan = useCallback((p: McpPlanView | null) => {
    planRef.current = p;
    setPlanState(p);
  }, []);
  const setNewer = useCallback((id: string | null) => {
    newerRef.current = id;
    setNewerState(id);
  }, []);

  /** Take a fetched plan into the state, unless a newer answer for the same plan was already used. */
  const take = useCallback(
    (view: McpPlanView, seq: number) => {
      if ((usedRef.current.get(view.planId) ?? 0) > seq) return;
      usedRef.current.set(view.planId, seq);
      const cur = planRef.current;
      if (cur && cur.planId === view.planId) {
        setPlan(view);
        // A hidden plan that is no longer applying has ended: the window opens again with its report.
        if (view.state.kind !== 'applying') setHidden(false);
        if (view.state.kind === 'replaced' && view.replacedBy) setNewer(view.replacedBy);
      } else if (cur) {
        if (undecided(view)) setNewer(view.planId);
        else if (newerRef.current === view.planId) setNewer(null);
      } else if (undecided(view)) {
        setPlan(view);
      }
    },
    [setPlan, setNewer],
  );

  const read = useCallback(
    async (planId: string | null) => {
      const seq = ++seqRef.current;
      try {
        const view = await api.invoke<McpPlanView | null>('get_mcp_plan', { planId });
        if (view) take(view, seq);
        return view;
      } catch (err) {
        console.error('[mcp-plan] get_mcp_plan failed:', err);
        return null;
      }
    },
    [take],
  );

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<{ planId: string }>('mcp-plan-updated', (p) => {
        if (cancelled) return;
        void read(p.planId);
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[mcp-plan] listen failed:', err));
    // A page opened later shows the plan that waits.
    void read(null);
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [read]);

  // Never trust events alone: re-read the plan that is on screen (or hidden) while it is not finished.
  const pollId = plan && undecided(plan) ? plan.planId : null;
  useEffect(() => {
    if (!pollId) return;
    const timer = setInterval(() => void read(pollId), REREAD_MS);
    const onVisible = () => {
      if (document.visibilityState === 'visible') void read(pollId);
    };
    document.addEventListener('visibilitychange', onVisible);
    return () => {
      clearInterval(timer);
      document.removeEventListener('visibilitychange', onVisible);
    };
  }, [pollId, read]);

  const answer = useCallback(
    async (a: PlanAnswer, failTitle: string): Promise<McpPlanView | null> => {
      const cur = planRef.current;
      if (!cur) return null;
      const seq = ++seqRef.current;
      setBusy(true);
      try {
        const view = await api.invoke<McpPlanView>('answer_mcp_plan', { planId: cur.planId, answer: a });
        take(view, seq);
        return view;
      } catch (err) {
        console.error('[mcp-plan] answer_mcp_plan failed:', err);
        notifyRef.current({ title: failTitle, detail: String(err), tone: 'warning', hasErrors: true, kind: 'generic' });
        return null;
      } finally {
        setBusy(false);
      }
    },
    [take],
  );

  const openNewer = useCallback(async () => {
    const cur = planRef.current;
    const target = newerRef.current ?? cur?.replacedBy ?? null;
    const view = await api
      .invoke<McpPlanView | null>('get_mcp_plan', { planId: target })
      .catch((err) => {
        console.error('[mcp-plan] get_mcp_plan failed:', err);
        return null;
      });
    setNewer(null);
    setHidden(false);
    if (view) {
      usedRef.current.set(view.planId, ++seqRef.current);
      setPlan(view);
    } else {
      setPlan(null);
    }
  }, [setNewer, setPlan]);

  const dismiss = useCallback(() => {
    // A plan that applies keeps going: Close only hides the window, and the report opens it again.
    if (planRef.current?.state.kind === 'applying') {
      setHidden(true);
      return;
    }
    setHidden(false);
    if (newerRef.current) void openNewer();
    else setPlan(null);
  }, [openNewer, setPlan]);

  const apply = useCallback(
    async (a: Extract<PlanAnswer, { action: 'apply' }>) => {
      await answer(a, 'Could not apply the plan');
    },
    [answer],
  );

  const cancel = useCallback(async () => {
    const view = await answer({ action: 'cancel' }, 'Could not cancel the plan');
    if (view) dismiss();
  }, [answer, dismiss]);

  const value = useMemo(
    () => ({ plan: hidden ? null : plan, newerPlanId, busy, apply, cancel, dismiss, openNewer }),
    [plan, hidden, newerPlanId, busy, apply, cancel, dismiss, openNewer],
  );
  return <McpPlanContext.Provider value={value}>{children}</McpPlanContext.Provider>;
}

export function useMcpPlan(): McpPlanContextValue {
  const ctx = useContext(McpPlanContext);
  if (!ctx) throw new Error('useMcpPlan must be used within McpPlanProvider');
  return ctx;
}
