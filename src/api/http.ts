// HTTP API implementation — wraps fetch/SSE for web (Docker) mode
import type { ApiBackend, UnlistenFn } from './tauri';

const BASE_URL = import.meta.env.VITE_API_BASE_URL || '';

// Opt-in API-key auth (backend: ATHENAEUM_API_KEY, see routes/auth.rs).
// Held in memory only — a page refresh re-prompts by design, so this must
// never be persisted to localStorage/sessionStorage.
let apiKey: string | null = null;

export function setApiKey(key: string | null): void {
  apiKey = key;
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
    const eventsUrl = apiKey
      ? `${BASE_URL}/api/events?api_key=${encodeURIComponent(apiKey)}`
      : `${BASE_URL}/api/events`;
    const source = new EventSource(eventsUrl);

    const onMessage = (e: MessageEvent) => {
      if (e.type === event) {
        handler(JSON.parse(e.data) as T);
      }
    };

    // SSE sends named events — listen for the specific event type
    source.addEventListener(event, onMessage);

    const unlisten: UnlistenFn = () => {
      source.removeEventListener(event, onMessage);
      // Close SSE connection if no more listeners would remain
      // In practice, the component cleanup will handle this
      source.close();
    };

    return Promise.resolve(unlisten);
  },
};
