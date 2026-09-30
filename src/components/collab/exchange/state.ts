// Pure reducer for the app-root live collab exchange state (wave 2, Task 8).
//
// Two inputs feed one `ExchangeState`: the `get_collab_exchange` snapshot
// (catalog-backed, carries `waitingForPublisher` and device names) and the
// `collab-exchange-progress` event stream (device ids only, no names, no
// `waitingForPublisher` — that field is carried over from whatever the state
// already knows). `CollabExchangeContext.tsx` is the only caller; kept here,
// pure and dependency-free, so it can be unit-tested without React.

import type {
  CollabExchangeProgress,
  DeviceNameView,
  ExchangeSnapshot,
  FlowView,
  ProjectFlows,
} from '../../../types/models';

/** Longest rate-history ring kept per flow (spec: last 40 `rateBps` samples). */
const RATE_HISTORY_MAX = 40;

export interface ExchangeState {
  /** By `projectId`. Only projects with at least one live flow are present. */
  projects: Record<string, ProjectFlows>;
  /** By `nameKey(projectId, device)`. */
  names: Record<string, DeviceNameView>;
  /** By `flowKey(flow)`, oldest first, capped at `RATE_HISTORY_MAX`. */
  rates: Record<string, number[]>;
}

export const EMPTY_EXCHANGE: ExchangeState = { projects: {}, names: {}, rates: {} };

export function nameKey(projectId: string, device: string): string {
  return `${projectId}|${device}`;
}

export function flowKey(f: FlowView): string {
  return `${f.projectId}|${f.direction}|${f.device}`;
}

/** A flow is "live" (worth keeping/showing) while it is moving, has an
 * in-flight item, or is still reporting a nonzero rate. */
function isLiveFlow(f: FlowView): boolean {
  return f.moving || f.inFlight.length > 0 || f.rateBps > 0;
}

function seedRates(rates: Record<string, number[]>, flows: FlowView[]): void {
  for (const f of flows) {
    rates[flowKey(f)] = [f.rateBps];
  }
}

function appendRate(rates: Record<string, number[]>, f: FlowView): void {
  const key = flowKey(f);
  const prev = rates[key];
  const next = prev ? [...prev, f.rateBps] : [f.rateBps];
  rates[key] = next.length > RATE_HISTORY_MAX ? next.slice(next.length - RATE_HISTORY_MAX) : next;
}

/**
 * Apply a `get_collab_exchange` snapshot answer.
 *
 * `scope === null` is the global no-arg answer: it replaces `projects`
 * wholesale (the command only returns projects that currently have flows).
 * `scope === <projectId>` is a single-project refetch: only that project's
 * entry is replaced — set from the answer if it names that project, dropped
 * otherwise. Names always merge in, regardless of scope. Rates are reseeded
 * (one sample per flow, from its current `rateBps`) for every flow in the
 * project(s) this call touches; flows of untouched projects keep their ring.
 */
export function applySnapshot(
  s: ExchangeState,
  snap: ExchangeSnapshot,
  scope: string | null,
): ExchangeState {
  const rates = { ...s.rates };
  let projects: Record<string, ProjectFlows>;

  if (scope === null) {
    projects = {};
    for (const p of snap.projects) {
      projects[p.projectId] = p;
      seedRates(rates, [...p.recv, ...p.send]);
    }
    // Full replace: drop rate rings belonging to projects no longer present.
    for (const key of Object.keys(rates)) {
      const owner = key.split('|')[0];
      if (!(owner in projects)) delete rates[key];
    }
  } else {
    projects = { ...s.projects };
    const old = projects[scope];
    if (old) {
      for (const f of [...old.recv, ...old.send]) delete rates[flowKey(f)];
    }
    delete projects[scope];
    for (const p of snap.projects) {
      if (p.projectId === scope) {
        projects[p.projectId] = p;
        seedRates(rates, [...p.recv, ...p.send]);
      }
    }
  }

  const names = { ...s.names };
  for (const n of snap.names) {
    names[nameKey(n.projectId, n.device)] = n;
  }

  return { projects, names, rates };
}

/**
 * Apply one `collab-exchange-progress` event. Per project in the event: keep
 * only live flows (see `isLiveFlow`); no flow left removes the project
 * entirely; otherwise the project is set with `waitingForPublisher` carried
 * over from whatever the state already had for it (the event's own value is
 * always `null`). Every kept flow's `rateBps` is appended to its ring.
 * Projects absent from the event — their flows and rings — are untouched.
 */
export function applyProgress(s: ExchangeState, ev: CollabExchangeProgress): ExchangeState {
  const projects = { ...s.projects };
  const rates = { ...s.rates };

  for (const p of ev.projects) {
    const recv = p.recv.filter(isLiveFlow);
    const send = p.send.filter(isLiveFlow);
    if (recv.length === 0 && send.length === 0) {
      delete projects[p.projectId];
      continue;
    }
    const waitingForPublisher = projects[p.projectId]?.waitingForPublisher ?? null;
    projects[p.projectId] = { ...p, recv, send, waitingForPublisher };
    for (const f of [...recv, ...send]) appendRate(rates, f);
  }

  return { ...s, projects, rates };
}

/** Empties live flow state on a `collab-live-status` transition away from
 * `'live'` (the runtime sends no quiet payload when it stops). Names survive
 * a reconnect. */
export function clearFlows(s: ExchangeState): ExchangeState {
  return { projects: {}, names: s.names, rates: {} };
}

/** `nameKey`s for every flow currently in state that has no `names` entry —
 * de-duplicated, in first-seen order. */
export function unknownDevices(s: ExchangeState): string[] {
  const seen = new Set<string>();
  const result: string[] = [];
  for (const p of Object.values(s.projects)) {
    for (const f of [...p.recv, ...p.send]) {
      const key = nameKey(f.projectId, f.device);
      if (s.names[key] || seen.has(key)) continue;
      seen.add(key);
      result.push(key);
    }
  }
  return result;
}

/** Display label for one peer: the member name when known, else `null`; the
 * device name when known, else the short id (first 8 chars). */
export function peerLabel(
  s: ExchangeState,
  projectId: string,
  device: string,
): { member: string | null; device: string } {
  const n = s.names[nameKey(projectId, device)];
  return { member: n?.memberName ?? null, device: n?.deviceName ?? device.slice(0, 8) };
}

/** Sidebar/indicator summary: current rate by direction, summed across every
 * live project, and whether anything is live at all. */
export function exchangeTotals(s: ExchangeState): { recvBps: number; sendBps: number; active: boolean } {
  let recvBps = 0;
  let sendBps = 0;
  let active = false;
  for (const p of Object.values(s.projects)) {
    if (p.recv.length > 0 || p.send.length > 0) active = true;
    for (const f of p.recv) recvBps += f.rateBps;
    for (const f of p.send) sendBps += f.rateBps;
  }
  return { recvBps, sendBps, active };
}
