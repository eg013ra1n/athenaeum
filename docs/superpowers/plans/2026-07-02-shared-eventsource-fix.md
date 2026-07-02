# Shared-EventSource Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Multiplex every `api.listen()` registration in the web frontend over ONE shared `EventSource` per tab, fixing the reproduced bug where per-listener SSE connections saturate the browser's 6-connections-per-origin HTTP/1.1 pool and starve all other requests (second tab blocked; same tab self-blocked after startup).

**Architecture:** Client-only change in `src/api/http.ts`. A module-level registry (`Map<eventName, Set<Registration>>`) tracks live listeners; one lazily-created `EventSource` carries the stream (the server already broadcasts ALL events with names on the single `/api/events` channel — `crates/athenaeum-web/src/routes/mod.rs:268` — so one connection loses nothing). One DOM dispatcher per event name fans out to registrations, each isolated in try/catch. Connection closes when the last registration unlistens and is rebuilt if `setApiKey` changes the URL. No backend, schema, or `ApiBackend`-interface changes.

**Tech Stack:** TypeScript, browser `EventSource`, existing Axum SSE backend (untouched).

## Global Constraints

- **No new npm deps. No backend/Rust changes** (Task 4 touches only a compose comment).
- **`ApiBackend` signature frozen:** `listen<T>(event, handler) => Promise<UnlistenFn>` — zero changes to `src/api/tauri.ts`, `src/api/index.ts`, or any consumer.
- **Never-swallow:** JSON-parse failures and throwing handlers must `console.error` with the event name.
- **API key stays memory-only** (no localStorage/sessionStorage) — project decision from W4-T13.
- **Handler isolation:** one throwing handler must not prevent other handlers of the same event from running (today's 13 independent connections give this for free; the shared dispatcher must preserve it).
- **Duplicate-registration safety:** the same function reference registered twice for the same event must survive one unlisten (wrap each registration in its own object; never store raw handler refs in the Set).
- Git: author preconfigured (eg013ra1n) — do NOT override, no Co-Authored-By. Branch `0.2.2`, do NOT push (controller pushes after review).
- Baseline evidence (2026-07-02 session, tip `9209e80c`): one tab on `/files` = exactly 6 established sockets (`lsof`), fetch-race BLOCKED >4s in BOTH tabs, tab 2 stuck blank inside WebAuthGate. 13 `listen()` sites in always-mounted hooks alone.

---

### Task 1: Verification harness + RED baseline

The frontend has no unit-test framework (repo gates are `tsc` + builds — do NOT add one). The failing test is a live two-tab reproduction driven over the Chrome DevTools Protocol. This task materializes the harness and records the RED result pre-fix.

**Files:**
- Create: `/tmp/athenaeum-sse-repro/cdp_eval.py` (harness tooling — NOT committed to the repo)
- Create: `/tmp/athenaeum-sse-repro/checks.sh` (NOT committed)

**Interfaces:**
- Produces: `run_checks` procedure used verbatim by Task 3 (GREEN run).

- [ ] **Step 1: Build the web bundle and prepare dirs**

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum
npm run build:web
mkdir -p /tmp/athenaeum-sse-repro/scanroot
cp rustafits/tests/mono.fits /tmp/athenaeum-sse-repro/scanroot/
cargo build -p athenaeum-web
```

- [ ] **Step 2: Write the CDP client** to `/tmp/athenaeum-sse-repro/cdp_eval.py`:

```python
#!/usr/bin/env python3
"""Minimal CDP websocket client: evaluate a JS expression in page targets."""
import base64, json, os, socket, struct, sys, urllib.request
from urllib.parse import urlparse


def ws_connect(ws_url):
    u = urlparse(ws_url)
    sock = socket.create_connection((u.hostname, u.port), timeout=15)
    key = base64.b64encode(os.urandom(16)).decode()
    sock.sendall((
        f"GET {u.path} HTTP/1.1\r\nHost: {u.hostname}:{u.port}\r\n"
        "Upgrade: websocket\r\nConnection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    ).encode())
    resp = b""
    while b"\r\n\r\n" not in resp:
        resp += sock.recv(4096)
    assert b"101" in resp.split(b"\r\n")[0], resp
    return sock


def ws_send(sock, payload):
    header = bytearray([0x81])
    mask = os.urandom(4)
    n = len(payload)
    if n < 126:
        header.append(0x80 | n)
    elif n < 65536:
        header.append(0x80 | 126); header += struct.pack(">H", n)
    else:
        header.append(0x80 | 127); header += struct.pack(">Q", n)
    header += mask
    sock.sendall(bytes(header) + bytes(b ^ mask[i % 4] for i, b in enumerate(payload)))


def _recv_exact(sock, n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise ConnectionError("closed")
        buf += chunk
    return buf


def ws_recv(sock):
    hdr = _recv_exact(sock, 2)
    length = hdr[1] & 0x7F
    if length == 126:
        length = struct.unpack(">H", _recv_exact(sock, 2))[0]
    elif length == 127:
        length = struct.unpack(">Q", _recv_exact(sock, 8))[0]
    return _recv_exact(sock, length)


def evaluate(ws_url, expression):
    sock = ws_connect(ws_url)
    ws_send(sock, json.dumps({
        "id": 1, "method": "Runtime.evaluate",
        "params": {"expression": expression, "returnByValue": True, "awaitPromise": True},
    }).encode())
    while True:
        msg = json.loads(ws_recv(sock))
        if msg.get("id") == 1:
            sock.close()
            return msg["result"]["result"].get("value")


if __name__ == "__main__":
    expr = sys.argv[1]
    targets = json.load(urllib.request.urlopen("http://127.0.0.1:9222/json"))
    for t in targets:
        if t["type"] != "page":
            continue
        val = evaluate(t["webSocketDebuggerUrl"], expr)
        print(f"=== {t['url']} ===")
        print(json.dumps(val, indent=1) if isinstance(val, (dict, list)) else val)
```

- [ ] **Step 3: Write the check script** to `/tmp/athenaeum-sse-repro/checks.sh`:

```bash
#!/bin/bash
# Two-tab SSE starvation checks. Run from the athenaeum repo root.
set -u
R=/tmp/athenaeum-sse-repro
REPO=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum

echo "── start server"
ATHENAEUM_DB_PATH=$R/repro.db ATHENAEUM_PORT=3199 \
  ATHENAEUM_ALLOWED_PATHS=$R ATHENAEUM_STATIC_DIR=$REPO/dist \
  nohup $REPO/target/debug/athenaeum-web > $R/server.log 2>&1 &
sleep 2

echo "── open tab 1"
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  --headless=new --remote-debugging-port=9222 \
  --user-data-dir=$R/chrome-profile http://127.0.0.1:3199/ > /dev/null 2>&1 &
sleep 8

echo "── CHECK A: sockets held by ONE tab (want: 1–2, red baseline: 6)"
lsof -nP -i TCP:3199 | grep ESTABLISHED | grep -ci google

echo "── open tab 2"
curl -s -X PUT "http://127.0.0.1:9222/json/new?http://127.0.0.1:3199/" > /dev/null
sleep 8

echo "── CHECK B: total sockets with TWO tabs (want: 2–4, red baseline: 6 pinned)"
lsof -nP -i TCP:3199 | grep ESTABLISHED | grep -ci google

echo "── CHECK C: fetch-race in BOTH tabs (want: FETCH OK 200 twice, red: BLOCKED)"
python3 $R/cdp_eval.py "Promise.race([fetch('/api/get_scan_roots',{method:'POST',headers:{'Content-Type':'application/json'},body:'{}'}).then(r=>'FETCH OK '+r.status), new Promise(res=>setTimeout(()=>res('FETCH BLOCKED >4s'),4000))]).then(f=>({f, body: document.body.innerText.slice(0,80)}))"
```

```bash
chmod +x /tmp/athenaeum-sse-repro/checks.sh
```

- [ ] **Step 4: Run the harness and record RED**

Run: `bash /tmp/athenaeum-sse-repro/checks.sh`
Expected (pre-fix): CHECK A = `6`; CHECK B = `6`; CHECK C = `FETCH BLOCKED >4s` in both tabs, tab-2 `body` empty.

- [ ] **Step 5: Tear down** (leave the harness files for Task 3)

```bash
pkill -f 'remote-debugging-port=9222'; pkill -f 'target/debug/athenaeum-web'
rm -rf /tmp/athenaeum-sse-repro/chrome-profile /tmp/athenaeum-sse-repro/repro.db
```

No commit — this task produces evidence, not repo changes.

### Task 2: Multiplex `listen()` over one shared EventSource

**Files:**
- Modify: `src/api/http.ts` (only file; replace the `listen` implementation and extend `setApiKey`; `invoke`/`probeAuth`/`BASE_URL` untouched)

**Interfaces:**
- Consumes: `ApiBackend`/`UnlistenFn` from `src/api/tauri.ts` (unchanged).
- Produces: identical public surface — `httpApi.listen<T>(event, handler): Promise<UnlistenFn>`, `setApiKey(key)`. Task 3 relies on: exactly one `/api/events` connection per tab while ≥1 registration is live; connection closes at zero registrations; rebuilt on key change.

- [ ] **Step 1: Replace the shared-connection section of `src/api/http.ts`.** Change `setApiKey` (lines 11-13) to:

```ts
export function setApiKey(key: string | null): void {
  apiKey = key;
  // If the shared stream is already open with a different key, rebuild it
  // so the new credential takes effect. Registrations survive the swap.
  if (sharedSource && sourceKey !== apiKey) {
    ensureSource();
  }
}
```

Then insert, between `probeAuth` and `export const httpApi`, the shared-connection machinery:

```ts
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
```

- [ ] **Step 2: Replace the `listen` method body** inside `httpApi` (the whole current implementation at lines 57-80) with:

```ts
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
```

- [ ] **Step 3: Typecheck and build both targets**

Run: `npx tsc --noEmit` → expected: clean.
Run: `npm run build:web` → expected: `✓ built` (chunk-size warning pre-exists).
Run: `npx vite build` → expected: `✓ built` (desktop bundle sanity; http.ts is tree-shaken there but must still compile).

- [ ] **Step 4: Commit**

```bash
git add src/api/http.ts
git commit -m "fix(web-ui): multiplex all SSE listeners over one shared EventSource per tab"
```

### Task 3: GREEN verification (two-tab, event flow, reconnect)

**Files:** none (uses the Task 1 harness verbatim; repo unchanged).

**Interfaces:**
- Consumes: Task 1's `/tmp/athenaeum-sse-repro/{cdp_eval.py,checks.sh}`; Task 2's committed fix.

- [ ] **Step 1: Rebuild the bundle with the fix and re-run the harness**

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum && npm run build:web
bash /tmp/athenaeum-sse-repro/checks.sh
```

Expected (GREEN): CHECK A = `1` or `2` (1 SSE + possibly one lingering keep-alive); CHECK B = `2`–`4`; CHECK C = `FETCH OK 200` in BOTH tabs and tab-2 `body` non-empty (sidebar text, e.g. `ATHENAEUM…`).

- [ ] **Step 2: Event-flow proof — a real backend event must reach a handler over the shared connection.** With the harness still up:

```bash
R=/tmp/athenaeum-sse-repro
# register the scan root (id printed in the response)
curl -s -X POST http://127.0.0.1:3199/api/add_scan_root \
  -H 'Content-Type: application/json' -d "{\"path\": \"$R/scanroot\"}"
# fire a scan (blocks until done; emits scan-progress/scan-complete via SSE)
curl -s -X POST http://127.0.0.1:3199/api/start_scan_with_progress \
  -H 'Content-Type: application/json' -d '{"rootId": 1}'
# within 5s of completion, the scan toast must be visible in BOTH tabs
python3 $R/cdp_eval.py "document.body.innerText.includes('Scan finished') || document.body.innerText.includes('No new files')"
```

Expected: scan returns a result JSON (1 file processed); the final evaluate prints `true` for **both** tabs (`useScanProgress`'s `scan-complete` handler → `notify()` toast). Run the evaluate immediately — the toast auto-dismisses after 5s; if you miss the window, re-run the scan (second run yields the `No new files` title, also asserted).

- [ ] **Step 3: Auto-reconnect proof.** Kill ONLY the server, restart it, and confirm the shared connection re-establishes without a reload:

```bash
pkill -f 'target/debug/athenaeum-web'; sleep 2
R=/tmp/athenaeum-sse-repro; REPO=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum
ATHENAEUM_DB_PATH=$R/repro.db ATHENAEUM_PORT=3199 ATHENAEUM_ALLOWED_PATHS=$R \
  ATHENAEUM_STATIC_DIR=$REPO/dist nohup $REPO/target/debug/athenaeum-web > $R/server2.log 2>&1 &
sleep 10   # EventSource default retry is a few seconds
lsof -nP -i TCP:3199 | grep ESTABLISHED | grep -ci google
```

Expected: count returns to the CHECK-B level (one SSE per tab re-established by the browser's built-in retry; dispatchers persist on the same EventSource object, so no re-attach is involved).

- [ ] **Step 4: Auth-mode spot check.** Restart the stack with `ATHENAEUM_API_KEY=testkey123` (add it to the server env line), reload tab 1 (`curl -s -X PUT "http://127.0.0.1:9222/json/new?http://127.0.0.1:3199/"` for a fresh tab is fine), then submit the key through the gate:

```bash
python3 /tmp/athenaeum-sse-repro/cdp_eval.py "(async()=>{const i=document.querySelector('input');if(!i)return 'no gate';const s=Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype,'value').set;s.call(i,'testkey123');i.dispatchEvent(new Event('input',{bubbles:true}));await new Promise(r=>setTimeout(r,100));const b=[...document.querySelectorAll('button')].find(b=>/submit|connect|unlock|enter/i.test(b.textContent)||b.type==='submit');b&&b.click();await new Promise(r=>setTimeout(r,2500));return document.body.innerText.slice(0,60)})()"
```

Expected: the gated tab renders the app (sidebar text), and `lsof` shows its single SSE socket — i.e. the shared connection was created with `?api_key=` AFTER `setApiKey` ran (gate mounts children only post-auth).

- [ ] **Step 5: Tear down everything**

```bash
pkill -f 'remote-debugging-port=9222'; pkill -f 'target/debug/athenaeum-web'
rm -rf /tmp/athenaeum-sse-repro
```

No commit — record all outputs in the task report / SDD ledger.

### Task 4: Docker docs — HTTP/2 reverse-proxy headroom note

**Files:**
- Modify: `docker/docker-compose.example.yml` (comment block only)

**Interfaces:** none.

- [ ] **Step 1: Add the note.** In the commented env-var documentation block (where `ATHENAEUM_API_KEY` is documented, around lines 29-40), append:

```yaml
      # The web UI shares one SSE connection per browser tab. Plain HTTP/1.1
      # allows ~6 connections per origin browser-wide, so ~6 simultaneous
      # tabs is the ceiling. If you need more, terminate TLS in front of
      # Athenaeum (any HTTPS reverse proxy): browsers then use HTTP/2 toward
      # the proxy (~100 streams) while the proxy speaks HTTP/1.1 to the app.
```

(Adjust leading whitespace to match the surrounding comment block exactly.)

- [ ] **Step 2: Commit**

```bash
git add docker/docker-compose.example.yml
git commit -m "docs(docker): note HTTP/2 reverse-proxy option for many-tab SSE headroom"
```

---

## Self-Review (completed 2026-07-02)

- **Coverage:** root-cause fix (Task 2), red/green acceptance for the exact reported symptom (Tasks 1/3), event-flow + reconnect + auth-interplay regressions (Task 3 steps 2-4), residual-limit documentation (Task 4). The rejected alternatives (HTTP/2 h2c, WebSocket, SharedWorker, fetch-SSE) are recorded in the session research summary and the SDD ledger — deliberately NOT in scope.
- **Placeholders:** none — every code block is complete and paths are absolute.
- **Type consistency:** `Registration`, `sharedSource`, `sourceKey`, `registrations`, `dispatchers`, `eventsUrl`, `makeDispatcher`, `attachDispatcher`, `ensureSource`, `teardownIfIdle` are defined once (Task 2 Step 1) and referenced with identical names in Step 2 and in `setApiKey`.
- **Known accepted behaviors:** (a) React StrictMode dev double-mount can briefly drop registrations to zero → one close/reopen churn cycle, dev-only, harmless; (b) events arriving during an auto-reconnect gap are lost — identical in kind to the pre-fix per-listener behavior, and handlers refetch authoritative DB state; (c) ~6 simultaneous tabs still saturate the pool — documented in Task 4, escape hatch is the HTTPS proxy.
