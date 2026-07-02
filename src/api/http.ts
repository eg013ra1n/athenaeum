// HTTP API implementation — wraps fetch/SSE for web (Docker) mode
import type { ApiBackend, UnlistenFn } from './tauri';

const BASE_URL = import.meta.env.VITE_API_BASE_URL || '';

// Opt-in API-key auth (backend: ATHENAEUM_API_KEY, see routes/auth.rs).
// Held in memory only — a page refresh re-prompts by design, so this must
// never be persisted to localStorage/sessionStorage.
let apiKey: string | null = null;

export function setApiKey(key: string | null): void {
  apiKey = key;
  // If the shared stream is already open with a different key, rebuild it
  // so the new credential takes effect. Registrations survive the swap.
  if (sharedSource && sourceKey !== apiKey) {
    ensureSource();
  }
}

/** Cheap authenticated POST used by WebAuthGate to check whether the
 * configured key is accepted. Maps a 401 to 'unauthorized'; any other
 * completed response (including other 4xx/5xx) proves the request got
 * past the auth middleware, so it's treated as 'ok'. Network failures
 * (server unreachable) are left to throw so the caller can distinguish
 * "wrong key" from "can't reach the server". */
export async function probeAuth(): Promise<'ok' | 'unauthorized'> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' };
  if (apiKey) {
    headers['X-API-Key'] = apiKey;
  }
  const res = await fetch(`${BASE_URL}/api/get_scan_roots`, {
    method: 'POST',
    headers,
    body: JSON.stringify({}),
  });
  return res.status === 401 ? 'unauthorized' : 'ok';
}

// ── Shared SSE connection ────────────────────────────────────────────────
// ONE EventSource per tab, multiplexing every listen() registration.
// Browsers cap plain-HTTP/1.1 connections at 6 per origin ACROSS TABS, and
// SSE streams never complete — per-listener EventSources (13+ at steady
// state) saturated the pool and starved every other request, including a
// second tab's initial fetches. The server broadcasts all named events on
// the single /api/events channel, so one connection carries everything.
// Design: docs/superpowers/plans/2026-07-02-shared-eventsource-fix.md

/** One listen() call. Wrapping the handler keeps duplicate registrations
 * of the same function distinct, so unlistening one leaves the other. */
type Registration = { handler: (payload: unknown) => void };

let sharedSource: EventSource | null = null;
/** The apiKey the current sharedSource URL was built with. */
let sourceKey: string | null = null;
const registrations = new Map<string, Set<Registration>>();
const dispatchers = new Map<string, (e: MessageEvent) => void>();

function eventsUrl(): string {
  return apiKey
    ? `${BASE_URL}/api/events?api_key=${encodeURIComponent(apiKey)}`
    : `${BASE_URL}/api/events`;
}

function makeDispatcher(event: string): (e: MessageEvent) => void {
  return (e: MessageEvent) => {
    let payload: unknown;
    try {
      payload = JSON.parse(e.data);
    } catch (err) {
      console.error(`[sse] bad JSON payload for '${event}':`, err);
      return;
    }
    const regs = registrations.get(event);
    if (!regs) return;
    for (const reg of regs) {
      try {
        reg.handler(payload);
      } catch (err) {
        // One throwing handler must not starve the others (each had its
        // own connection before multiplexing; keep that isolation).
        console.error(`[sse] handler for '${event}' threw:`, err);
      }
    }
  };
}

function attachDispatcher(event: string): void {
  if (!sharedSource || dispatchers.has(event)) return;
  const dispatcher = makeDispatcher(event);
  dispatchers.set(event, dispatcher);
  sharedSource.addEventListener(event, dispatcher);
}

/** (Re)create the shared connection if absent or built with a stale key,
 * re-attaching a dispatcher for every registered event name. */
function ensureSource(): void {
  if (sharedSource && sourceKey === apiKey) return;
  if (sharedSource) {
    sharedSource.close();
    sharedSource = null;
  }
  dispatchers.clear();
  sharedSource = new EventSource(eventsUrl());
  sourceKey = apiKey;
  for (const event of registrations.keys()) {
    attachDispatcher(event);
  }
}

function teardownIfIdle(): void {
  if (registrations.size > 0 || !sharedSource) return;
  sharedSource.close();
  sharedSource = null;
  sourceKey = null;
  dispatchers.clear();
}

export const httpApi: ApiBackend = {
  async invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    const headers: Record<string, string> = { 'Content-Type': 'application/json' };
    if (apiKey) {
      headers['X-API-Key'] = apiKey;
    }
    const res = await fetch(`${BASE_URL}/api/${command}`, {
      method: 'POST',
      headers,
      body: JSON.stringify(args ?? {}),
    });
    if (!res.ok) {
      const text = await res.text();
      throw text || `HTTP ${res.status}`;
    }
    const ct = res.headers.get('content-type') ?? '';
    if (ct.startsWith('image/') || ct === 'application/octet-stream') {
      const buf = await res.arrayBuffer();
      return new Uint8Array(buf) as unknown as T;
    }
    return res.json() as Promise<T>;
  },

  listen<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
    const reg: Registration = { handler: handler as (payload: unknown) => void };
    let regs = registrations.get(event);
    if (!regs) {
      regs = new Set();
      registrations.set(event, regs);
    }
    regs.add(reg);
    ensureSource();
    attachDispatcher(event);

    const unlisten: UnlistenFn = () => {
      const set = registrations.get(event);
      if (!set) return;
      set.delete(reg);
      if (set.size === 0) {
        registrations.delete(event);
        const dispatcher = dispatchers.get(event);
        if (dispatcher && sharedSource) {
          sharedSource.removeEventListener(event, dispatcher);
        }
        dispatchers.delete(event);
      }
      teardownIfIdle();
    };
    return Promise.resolve(unlisten);
  },
};
