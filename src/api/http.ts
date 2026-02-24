// HTTP API implementation — wraps fetch/SSE for web (Docker) mode
import type { ApiBackend, UnlistenFn } from './tauri';

const BASE_URL = import.meta.env.VITE_API_BASE_URL || '';

export const httpApi: ApiBackend = {
  async invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    const res = await fetch(`${BASE_URL}/api/${command}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
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
    const source = new EventSource(`${BASE_URL}/api/events`);

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
