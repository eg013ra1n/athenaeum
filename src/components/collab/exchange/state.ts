// Pure reducer for the app-root live collab exchange state (wave 2, Task 8).
//
// Two inputs feed one `ExchangeState`: the `get_collab_exchange` snapshot
// (catalog-backed, carries `waitingForPublisher` and device names) and the
// `collab-exchange-progress` event stream (device ids only, no names, no
// `waitingForPublisher` — that field is carried over from whatever the state
// already knows). `CollabExchangeContext.tsx` is the only caller; kept here,
// pure and dependency-free, so it can be unit-tested without React.
//
// Fix round 1 (controller ruling): the backend's `ExchangeMeter::snapshot`
// keeps an idle flow for IDLE_DROP = 60s and reports it `moving: false,
// rateBps: 0, inFlight: []` — the snapshot answer can carry these ghosts, not
// just live events. `applySnapshot` now filters with the same `isLiveFlow`
// rule `applyProgress` uses, for both the global and a scoped answer, so a
// quiet project never lingers in `projects`. The per-project figures that
// must survive a project having no live flow (`toGo`, `waitingForPublisher`)
// move to their own `summary` map, which only ever gains/updates entries —
// never deleted by a quiet snapshot, a quiet event, or `clearFlows`.

import type {
  CollabExchangeProgress,
  DeviceNameView,
  ExchangeSnapshot,
  FlowView,
  ProjectFlows,
} from '../../../types/models';

/** Longest rate-history ring kept per flow (spec: last 40 `rateBps` samples). */
const RATE_HISTORY_MAX = 40;

/** The two per-project figures that outlive a project's flows going idle. */
export interface ProjectSummary {
  toGo: number;
  waitingForPublisher: number | null;
}

export interface ExchangeState {
  /** By `projectId`. Only projects with at least one LIVE flow are present. */
  projects: Record<string, ProjectFlows>;
  /** By `nameKey(projectId, device)`. */
  names: Record<string, DeviceNameView>;
  /** By `flowKey(flow)`, oldest first, capped at `RATE_HISTORY_MAX`. */
  rates: Record<string, number[]>;
  /** By `projectId`. Survives a project's flows going idle/quiet — never
   * cleared by a quiet snapshot, a quiet progress event, or `clearFlows`. */
  summary: Record<string, ProjectSummary>;
}

export const EMPTY_EXCHANGE: ExchangeState = { projects: {}, names: {}, rates: {}, summary: {} };

export function nameKey(projectId: string, device: string): string {
  return `${projectId}|${device}`;
}

export function flowKey(f: FlowView): string {
  return `${f.projectId}|${f.direction}|${f.device}`;
}

/** A flow is "live" (worth keeping/showing as a flow row) while it is moving,
 * has an in-flight item, or is still reporting a nonzero rate. Idle flows —
 * the meter's 60s "just finished" ghosts — fail this and are dropped from
 * `projects`, though their project's `summary` entry is unaffected. */
function isLiveFlow(f: FlowView): boolean {
  return f.moving || f.inFlight.length > 0 || f.rateBps > 0;
}

function filterLive(p: ProjectFlows): ProjectFlows {
  return { ...p, recv: p.recv.filter(isLiveFlow), send: p.send.filter(isLiveFlow) };
}

function hasLiveFlow(p: ProjectFlows): boolean {
  return p.recv.length > 0 || p.send.length > 0;
}

/** Seeds a ring only for a flow that doesn't have one yet — an existing ring
 * (built from live progress events) is never reset by a later snapshot. */
function seedRatesIfMissing(rates: Record<string, number[]>, flows: FlowView[]): void {
  for (const f of flows) {
    const key = flowKey(f);
    if (!(key in rates)) rates[key] = [f.rateBps];
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
 * wholesale (after the live-flow filter — the meter can still report a
 * recently-finished project with idle-only flows). `scope === <projectId>` is
 * a single-project refetch: only that project's `projects` entry is
 * replaced — set (post-filter) if the answer names that project and has a
 * live flow left, dropped otherwise. Names always merge in, regardless of
 * scope. `summary[projectId]` is written for every project PRESENT in the
 * answer (live or not) — a scoped answer that does not name the project at
 * all (never observed from the current backend, which always echoes the
 * requested id, but handled defensively) leaves that summary untouched.
 * Rate rings are seeded only for a flow that has none yet; an existing ring
 * is never reset by a snapshot, only appended to by later progress events.
 */
export function applySnapshot(
  s: ExchangeState,
  snap: ExchangeSnapshot,
  scope: string | null,
): ExchangeState {
  const rates = { ...s.rates };
  const summary = { ...s.summary };
  let projects: Record<string, ProjectFlows>;

  if (scope === null) {
    projects = {};
    for (const raw of snap.projects) {
      summary[raw.projectId] = { toGo: raw.toGo, waitingForPublisher: raw.waitingForPublisher };
      const p = filterLive(raw);
      if (!hasLiveFlow(p)) continue;
      projects[p.projectId] = p;
      seedRatesIfMissing(rates, [...p.recv, ...p.send]);
    }
    // Full replace: dropping rings of projects no longer live is fine — they
    // reseed from scratch (single sample) the next time that project's flow
    // starts moving again.
    for (const key of Object.keys(rates)) {
      const owner = key.split('|')[0];
      if (!(owner in projects)) delete rates[key];
    }
  } else {
    projects = { ...s.projects };
    const old = projects[scope];
    delete projects[scope];
    let restored = false;
    for (const raw of snap.projects) {
      if (raw.projectId !== scope) continue;
      summary[raw.projectId] = { toGo: raw.toGo, waitingForPublisher: raw.waitingForPublisher };
      const p = filterLive(raw);
      if (!hasLiveFlow(p)) continue;
      projects[p.projectId] = p;
      seedRatesIfMissing(rates, [...p.recv, ...p.send]);
      restored = true;
    }
    if (old && !restored) {
      for (const f of [...old.recv, ...old.send]) delete rates[flowKey(f)];
    }
  }

  const names = { ...s.names };
  for (const n of snap.names) {
    names[nameKey(n.projectId, n.device)] = n;
  }

  return { projects, names, rates, summary };
}

/**
 * Apply one `collab-exchange-progress` event. Per project in the event: keep
 * only live flows (see `isLiveFlow`); no flow left removes the project from
 * `projects` — but `summary[projectId]` is always written (`toGo` from the
 * event, `waitingForPublisher` carried over from what the state already had,
 * since the event's own value is always `null`), so a quiet event never
 * deletes a summary entry. Every kept flow's `rateBps` is appended to its
 * ring. Projects absent from the event — their flows, rings and summary —
 * are untouched.
 */
export function applyProgress(s: ExchangeState, ev: CollabExchangeProgress): ExchangeState {
  const projects = { ...s.projects };
  const rates = { ...s.rates };
  const summary = { ...s.summary };

  for (const p of ev.projects) {
    const recv = p.recv.filter(isLiveFlow);
    const send = p.send.filter(isLiveFlow);

    const waitingForPublisher =
      summary[p.projectId]?.waitingForPublisher ?? projects[p.projectId]?.waitingForPublisher ?? null;
    summary[p.projectId] = { toGo: p.toGo, waitingForPublisher };

    if (recv.length === 0 && send.length === 0) {
      delete projects[p.projectId];
      continue;
    }
    projects[p.projectId] = { ...p, recv, send, waitingForPublisher };
    for (const f of [...recv, ...send]) appendRate(rates, f);
  }

  return { ...s, projects, rates, summary };
}

/** Empties live flow state on a `collab-live-status` transition away from
 * `'live'` (the runtime sends no quiet payload when it stops). Names and
 * `summary` survive a reconnect — neither is flow data. */
export function clearFlows(s: ExchangeState): ExchangeState {
  return { projects: {}, names: s.names, rates: {}, summary: s.summary };
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
 * live project, and whether anything is actually transferring right now
 * (some flow `moving` or with an in-flight item — NOT merely a nonzero rate,
 * so a residual idle/ghost flow, already excluded from `projects`, can never
 * hold the indicator lit; and not merely "a project entry exists", since
 * `projects` only ever holds live flows in the first place). */
export function exchangeTotals(s: ExchangeState): { recvBps: number; sendBps: number; active: boolean } {
  let recvBps = 0;
  let sendBps = 0;
  let active = false;
  for (const p of Object.values(s.projects)) {
    for (const f of p.recv) {
      recvBps += f.rateBps;
      if (f.moving || f.inFlight.length > 0) active = true;
    }
    for (const f of p.send) {
      sendBps += f.rateBps;
      if (f.moving || f.inFlight.length > 0) active = true;
    }
  }
  return { recvBps, sendBps, active };
}
