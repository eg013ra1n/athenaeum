# Collab Project Page — Pixel-Accurate UI (Wave 5.5) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild the collab project page (six tabs, frame and member cards, the collab dialogs) so it matches the approved mockup within 1 px on the same data, on a shared compact UI layer that the rest of the app can reuse.

**Architecture:** A dev harness renders the real page on the mockup's own data next to the mockup (Task 1). The foundation comes next: tokens, font, palettes and formatters (Task 2), then `src/components/ui/` primitives that each mirror one mockup CSS rule (Tasks 3–5). The shared table engine is restyled (Task 6). Finally the page shell, the side panels, every tab and every collab dialog are rebuilt on those primitives (Tasks 7–15). Every task ends with a side-by-side harness measurement. Presentation only: no Rust, command, event or model change.

**Tech Stack:** React 18 + TypeScript, Tailwind 3 (arbitrary values), vitest + @testing-library/react, Node ≥ 20 for the harness, Vite in web mode (`VITE_TARGET=web`).

**Spec:** `docs/superpowers/specs/2026-09-30-collab-project-ui-pixel-design.md` (sections cited as §N). Visual reference: `docs/superpowers/research/2026-09-29-collab-project-layout-mockup.html` (after Task 1, in standards mode).

## Global Constraints

- Presentation only: no change under `crates/`, no command, no event, no change to `src/types/models.ts` (spec U7, §15).
- Design tokens only. Raw colours are allowed ONLY in: `tailwind.config.js` token definitions, `src/utils/filterColors.ts` (the one filter palette, §4.3), `src/components/collab/project/memberColors.ts` (the member palette, §4.4), the table-row state rgba values listed in §5.1, and the dialog scrim/shadow values in §7.
- Exact mockup values live inside the `src/components/ui/` primitives; call sites use the primitives, not re-typed pixel values.
- No `@tauri-apps/*` import outside `src/api/`. Every `api.listen` uses the StrictMode-safe pattern from CLAUDE.md.
- Never swallow errors: `console.error` before anything else. User-visible action failures go through `notify()`, not new banners.
- UI strings in English. Do not name any third-party project in code, comments, docs or commit messages.
- No `println!`-style debug output; no `console.log` left in production code (the harness scripts may print).
- Page root of the project page: `13px / 1.4`, `tabular-nums` (§3.2). Other pages keep their sizes.
- Reference viewport for every harness check: 1440 × 900, app sidebar collapsed, mockup "Design notes" off (§2.1). Tolerance: 1 px, except the deviations D1–D7 in §19.
- Commit after each task with the repo's trailer lines (`Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and the session line), as user `eg013ra1n`.
- Verification commands:
  - `npx vitest run <paths>` for the task's tests;
  - `npx tsc --noEmit -p .` before every commit;
  - `npx vitest run` (the full suite) at Tasks 5, 10 and 16.

## Review Focus

1. **Long names** — a 60-character file name, a 30-character member or device name, a 60-character project title. They must truncate with an ellipsis (tables, header) or break by character (panel title); rows never grow and columns never move. Pinned in Task 6 (table truncation test) and Task 7 (header title test).
2. **Many filters** — 12 distinct filters, including unmapped raw names. The Members table scrolls horizontally inside its box, Overview goal rows stay one line each, and unknown filters keep a stable colour. Pinned in Task 2 (`filterColors` stability test) and Task 13 (12-filter Members test).
3. **Narrow window with the side panel open** — at 1100 px, Frame shrinks to its 220 px minimum, then the table scrolls horizontally; the panel stays 400 px. Pinned in Task 4 (`PanelLayout` test) and Task 6 (the Frame `<col>` minimum-width test).
4. **Empty project** — no own frames, no library frames, no flows, no sessions, no members' data. Every card and table shows its `EmptyState` line; no NaN, no blank boxes. Pinned in Task 9 (Overview empty test), Task 13 (Members empty test) and Task 14 (Exchange empty test).
5. **Live states** — connecting, reconnecting, unreachable, signed out, storage unavailable, off. The pill keeps its geometry and label for each; Sync is disabled when off. Pinned in Task 7 (`LivePill` state test).

---

### Task 1: Dev harness and the standards-mode reference

**Files:**
- Modify: `docs/superpowers/research/2026-09-29-collab-project-layout-mockup.html` (prepend one line)
- Create: `scripts/ui-harness/fixtures.mjs`
- Create: `scripts/ui-harness/server.mjs`
- Create: `scripts/ui-harness/run.mjs`
- Create: `scripts/ui-harness/measure.js`
- Create: `scripts/ui-harness/README.md`
- Modify: `package.json` (one script)

**Interfaces:**
- Produces: `npm run ui:harness`, which serves the web app at `http://localhost:1430/projects/p-m31` on the mockup's data. `HARNESS_SCENARIO=long|filters|empty|offline` switches the fixture set. `scripts/ui-harness/measure.js` is a geometry dump to run on both pages.
- Produces: `buildFixtures(scenario)` in `fixtures.mjs`, returning `{ handlers }` — a map of command name to `(args) => response`.

- [ ] **Step 1: Put the mockup in standards mode**

Prepend exactly `<!doctype html>` as the first line of `docs/superpowers/research/2026-09-29-collab-project-layout-mockup.html` (the file currently starts with `<title>Collab Project Layout</title>`).

Run: `head -2 docs/superpowers/research/2026-09-29-collab-project-layout-mockup.html`
Expected: line 1 `<!doctype html>`, line 2 `<title>Collab Project Layout</title>`.

- [ ] **Step 2: Write `scripts/ui-harness/fixtures.mjs`**

The mockup stays the one source of the sample data. This module evaluates the mockup's own data generator (the script from `<script>` up to `const COLS=`) in a `vm` sandbox, then maps it to the app's API shapes.

```js
// Dev-only UI harness fixtures: the approved mockup's deterministic sample data
// (seed 20260929 — 3,719 frames, 8 members) mapped to the app's API shapes.
// The mockup HTML is evaluated, never copied, so it stays the one source.
import fs from 'node:fs';
import vm from 'node:vm';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const MOCKUP = path.join(ROOT, 'docs/superpowers/research/2026-09-29-collab-project-layout-mockup.html');

function mockupData() {
  const html = fs.readFileSync(MOCKUP, 'utf8');
  const start = html.indexOf('<script>') + '<script>'.length;
  const end = html.indexOf('const COLS=', start);
  if (start < 8 || end < 0) throw new Error('mockup script markers not found');
  // The script opens an IIFE `(function(){'use strict'; …`; close it here and return the data.
  const src = `${html.slice(start, end)}\nreturn { MEMBERS, FRAMES, GOALS };\n})()`;
  return vm.runInNewContext(src, { Math, Date, JSON, Object, Array, Set, Map, String, Number });
}

const PID = 'p-m31';
const NOW = '2026-09-29T10:00:00Z';
const LAST_SEEN = { now: NOW, '3 days ago': '2026-09-26T10:00:00Z', '5 h ago': '2026-09-29T05:00:00Z', '1 day ago': '2026-09-28T10:00:00Z' };
// The mockup's reason codes → the gate's real failure kinds (`BLOCKER_ORDER`).
const KIND = { solve: 'solve', analyze: 'analyze', map: 'mapFilter', flat: 'linkCalibration', fwhm: 'threshold', ecc: 'threshold', stars: 'threshold' };

export function buildFixtures(scenario = 'default') {
  const { MEMBERS, FRAMES: ALL, GOALS } = mockupData();
  let FRAMES = ALL;
  if (scenario === 'empty') FRAMES = [];
  if (scenario === 'long') {
    FRAMES = ALL.map((f, i) => (i % 7 === 0 ? { ...f, name: `Light_M31_${f.filter}_${f.exp}s_${f.night.replace(/-/g, '')}_very_long_session_name_${i}.fits` } : f));
    MEMBERS[1] = { ...MEMBERS[1], name: 'Konstantin Konstantinopolsky', devices: [{ n: 'konstantin-observatory-workstation', on: true }] };
  }
  if (scenario === 'filters') {
    const extra = ['S2 6nm', 'O3 3nm', 'CLS', 'L-eXtreme'];
    FRAMES = ALL.map((f, i) => (!f.own && i % 11 === 0 ? { ...f, filter: extra[i % extra.length] } : f));
  }
  const MBY = Object.fromEntries(MEMBERS.map((m) => [m.id, m]));
  const hex = (s) => Buffer.from(s.padEnd(32, '0')).toString('hex').slice(0, 64);
  const devId = (mid, i) => hex(`${mid}-dev-${i}`);
  const acc = (id) => `acc-${id}`;
  const online = (mid) => MBY[mid].online;
  const uuid = (f) => `f-${String(f.id).padStart(5, '0')}`;
  const others = (f) => (f.holders || []).filter((h) => h !== 'you');
  const inProject = FRAMES.filter((f) => f.pub);
  const published = inProject.filter((f) => f.pub === 'published');

  const card = {
    projectId: PID, slug: 'm31-deep-field-2026', title: scenario === 'long' ? 'M31 Deep Field 2026 — autumn campaign with the extended team and guests' : 'M31 Deep Field 2026',
    dataRole: 'send_receive', coordinator: true, canModerate: true, requireApproval: true,
    pendingFrames: FRAMES.filter((f) => !f.own && f.pub === 'pending').length,
    projectStatus: 'active', targetName: 'M31', targetRaDeg: 10.6847, targetDecDeg: 41.2687, targetRadiusDeg: 1.5,
    membershipVersion: 8, linkedSets: 1, candidates: 0, publishable: FRAMES.filter((f) => f.own && f.seg === 'ready').length,
    autoReplicate: true, autoPublish: true, fetchedAt: NOW,
    publishingDevice: { deviceId: devId('you', 0), name: 'This Mac' }, publishingHere: true,
  };
  const detail = {
    card,
    members: MEMBERS.map((m) => ({ displayName: m.name, dataRole: (m.data || m.role) === 'Contributor' ? 'send' : 'send_receive', coordinator: m.role === 'Coordinator' })),
    thresholdsVersion: 3,
    thresholds: [
      { metricKey: 'fwhm', op: 'lte', value: 3.0 },
      { metricKey: 'eccentricity', op: 'lte', value: 0.55 },
      { metricKey: 'stars', op: 'gte', value: 150 },
      { metricKey: 'trailed', op: 'reject_if', value: true },
    ],
    links: [{ framesSetId: 1, name: 'M31 · 2026 autumn', lightCount: FRAMES.filter((f) => f.own).length, distanceDeg: 0.12, withinRadius: true }],
    portalBase: 'https://portal.example',
    goals: Object.fromEntries(Object.entries(GOALS).map(([f, h]) => [f, h * 3600])),
  };
  const localStateOf = (f) => {
    if (f.own) return f.disk === 'missing' ? 'own_missing' : f.disk === 'changed' ? 'own_changed' : 'own_held';
    return { have: 'held', downloading: 'wanted', queued: 'wanted', missing: 'wanted', notkept: 'not_kept' }[f.mine] || 'idle';
  };
  const frameView = (f) => ({
    frameUuid: uuid(f), fileName: f.name, publisher: MBY[f.publisher].name, publisherAccountId: acc(f.publisher),
    own: f.own, filter: f.filter, exptimeSec: f.exp, dateObs: `${f.night}T22:00:00`,
    state: f.pub === 'excluded' ? 'published' : f.pub, accepted: f.pub !== 'excluded',
    acceptedReason: f.pub === 'excluded' ? 'Trailed stars' : null,
    localState: localStateOf(f), onDisk: f.own ? f.disk !== 'missing' : f.mine === 'have',
    holdersOnline: f.mine === 'missing' ? 0 : others(f).filter(online).length, holdersTotal: others(f).length,
    waitingForPublisher: !f.own && f.mine === 'missing' && f.publisher === 'irina', newVersionWaiting: false,
    byteSize: f.size, contentVersion: f.version || 1, lastError: null, fwhmArcsec: f.fwhm, eccentricity: f.ecc,
    starsDetected: f.stars, camera: f.camera, telescope: null, medianSnr: f.snr, night: f.night,
    contributorState: null, contributorReason: null,
    receivedAt: !f.own && f.mine === 'have' ? `${f.publishedAt.replace(' ', 'T')}Z` : null,
    receivedFromDevice: !f.own && f.mine === 'have' ? devId(f.publisher, 0) : null,
    receivedFromMember: !f.own && f.mine === 'have' ? MBY[f.publisher].name : null,
  });
  const rulesOf = (f) => [
    { metricKey: 'fwhm', label: 'FWHM', value: `${f.fwhm.toFixed(2)}″`, needs: '≤ 3.00″', pass: f.fwhm <= 3 },
    { metricKey: 'eccentricity', label: 'eccentricity', value: f.ecc.toFixed(2), needs: '≤ 0.55', pass: f.ecc <= 0.55 },
    { metricKey: 'stars', label: 'stars', value: String(f.stars), needs: '≥ 150', pass: f.stars >= 150 },
    { metricKey: 'trailed', label: 'trailed', value: 'no', needs: 'not trailed', pass: true },
  ];
  const ownRow = (f) => ({
    frameId: f.id, frameUuid: uuid(f), fileName: f.name, setId: 1, setName: f.set, night: f.night, filter: f.filter,
    filterMapped: !f.reasons.some((r) => r.code === 'map'), camera: f.camera, exptimeSec: f.exp, byteSize: f.size,
    fwhmArcsec: f.fwhm, eccentricity: f.ecc, starsDetected: f.stars, medianSnr: f.snr, segment: f.seg,
    contributorState: f.seg === 'published' ? (f.pub === 'pending' ? 'pendingApproval' : f.pub === 'rejected' ? 'rejected' : 'published') : f.seg === 'held' ? 'failsGate' : 'notPublished',
    contributorReason: f.reasons[0]?.detail ?? null,
    failures: f.reasons.map((r) => ({ kind: KIND[r.code], text: r.detail })),
    contentVersion: f.pub ? f.version || 1 : null, pubState: f.pub ? (f.pub === 'excluded' ? 'published' : f.pub) : null,
    acceptedReason: f.pub === 'excluded' ? 'Trailed stars' : null,
    holdersOnline: f.pub ? others(f).filter(online).length : null, holdersTotal: f.pub ? others(f).length : null,
    localState: f.pub ? localStateOf(f) : null, publishedAt: f.publishedAt ?? null, lastError: null, rules: rulesOf(f),
    path: `/Volumes/Astro/M31/2026-autumn/${f.night}/LIGHT/${f.name}`, accepted: f.pub ? f.pub !== 'excluded' : null,
  });
  const med = (a) => { const s = [...a].sort((x, y) => x - y); const k = s.length >> 1; return s.length ? (s.length % 2 ? s[k] : (s[k - 1] + s[k]) / 2) : null; };
  const memberSummary = MEMBERS.map((m) => {
    const mine = published.filter((f) => f.publisher === m.id);
    const secondsByFilter = {};
    for (const f of mine) secondsByFilter[f.filter] = (secondsByFilter[f.filter] || 0) + f.exp;
    const held = inProject.filter((f) => (f.holders || []).includes(m.id));
    const cams = {};
    for (const f of mine) (cams[`${f.camera}|${f.filter}`] ||= []).push(f);
    return {
      accountId: acc(m.id), displayName: m.name, dataRole: (m.data || m.role) === 'Contributor' ? 'send' : 'send_receive',
      coordinator: m.role === 'Coordinator',
      devices: m.devices.map((d, i) => ({ device: devId(m.id, i), name: d.n, online: d.on })),
      online: scenario === 'offline' ? false : m.online, lastSeenAt: LAST_SEEN[m.seen] ?? NOW, publishedFrames: mine.length, secondsByFilter,
      qualityByCamera: Object.entries(cams).map(([k, fs]) => ({ camera: k.split('|')[0], filter: k.split('|')[1], frames: fs.length, medianFwhm: med(fs.map((f) => f.fwhm)), medianEcc: med(fs.map((f) => f.ecc)) })),
      holdsFrames: held.length, holdsBytes: held.reduce((a, f) => a + f.size, 0), holdsShare: inProject.length ? held.length / inProject.length : 0,
    };
  });
  const downloading = FRAMES.filter((f) => f.mine === 'downloading');
  const flow = (mid, direction, items, rate, completed, bytes) => ({
    projectId: PID, device: devId(mid, 0), direction, bytesSession: bytes, rateBps: rate, etaSecs: 540, moving: true, completed,
    inFlight: items.map((f) => ({ frameUuid: uuid(f), fileName: f.name, size: f.size, done: Math.round((f.size * (f.progress || 40)) / 100) })),
  });
  const ownPub = FRAMES.filter((f) => f.own && f.pub);
  const exchange = scenario === 'empty' ? { projects: [], names: [] } : {
    projects: [{
      projectId: PID,
      recv: [flow('kostya', 'recv', downloading.filter((f) => f.publisher === 'kostya').slice(0, 2), 33.3e6, 37, 4.8e9), flow('masha', 'recv', downloading.filter((f) => f.publisher === 'masha').slice(0, 1), 10.1e6, 20, 1.4e9)],
      send: [flow('kostya', 'send', ownPub.slice(0, 2), 18.0e6, 21, 2.7e9), flow('andrei', 'send', ownPub.slice(2, 3), 5.6e6, 7, 0.873e9)],
      toGo: FRAMES.filter((f) => ['downloading', 'queued', 'missing'].includes(f.mine)).length,
      waitingForPublisher: FRAMES.filter((f) => f.mine === 'missing' && f.publisher === 'irina').length,
    }],
    names: MEMBERS.flatMap((m) => m.devices.map((d, i) => ({ projectId: PID, device: devId(m.id, i), memberName: m.name, deviceName: d.n }))),
  };
  const moderation = FRAMES.filter((f) => !f.own && f.pub === 'pending').map((f) => ({
    frameUuid: uuid(f), fileName: f.name, publisher: MBY[f.publisher].name, publisherAccountId: acc(f.publisher), filter: f.filter,
    exptimeSec: f.exp, fwhmArcsec: f.fwhm, createdAt: `${f.publishedAt.replace(' ', 'T')}Z`,
  }));
  const holdersFor = (frameUuid) => {
    const f = FRAMES.find((x) => uuid(x) === frameUuid);
    if (!f) return null;
    return (f.holders || []).map((h) => ({ memberName: MBY[h].name, deviceName: MBY[h].devices[0].n, device: devId(h, 0), deviceShort: devId(h, 0).slice(0, 8), online: online(h), isPublisher: h === f.publisher, contentVersion: f.version || 1 }));
  };
  const live = scenario === 'offline'
    ? { state: 'reconnecting', retryInSecs: 12, since: NOW, storage: 'available', storageReason: null, watcherDegraded: false, networkVolume: false }
    : { state: 'live', retryInSecs: null, since: NOW, storage: 'available', storageReason: null, watcherDegraded: false, networkVolume: false };
  const sessions = scenario === 'empty' ? [] : [
    { id: 3, projectId: PID, projectTitle: card.title, startedAt: '2026-09-28T22:48:00Z', finishedAt: '2026-09-28T22:54:56Z', frames: 90, bytes: 7.9e9, failed: 0, sources: [{ device: devId('masha', 0), memberName: 'Masha', deviceName: 'Masha MacBook', bytes: 4.1e9 }, { device: devId('olga', 0), memberName: 'Olga', deviceName: 'olga-home', bytes: 3.8e9 }] },
    { id: 2, projectId: PID, projectTitle: card.title, startedAt: '2026-09-28T10:54:00Z', finishedAt: '2026-09-28T10:56:13Z', frames: 68, bytes: 4.1e9, failed: 2, sources: [{ device: devId('masha', 0), memberName: 'Masha', deviceName: 'Masha MacBook', bytes: 4.1e9 }] },
  ];
  const handlers = {
    list_collab_projects: () => [card],
    refresh_collab_projects: () => [card],
    get_collab_project_detail: () => detail,
    list_collab_frames: () => inProject.map(frameView),
    list_project_own_frames: () => FRAMES.filter((f) => f.own).map(ownRow),
    get_collab_member_summary: () => memberSummary,
    get_collab_exchange: () => exchange,
    list_collab_moderation: () => moderation,
    list_collab_attention: () => ({ changed: [], awaitingChoice: [], notKept: [], otherFiles: [] }),
    get_collaboration_dir: () => '/Volumes/Astro/Collaboration',
    get_collab_live_status: () => live,
    get_collab_storage_status: () => ({ state: 'available', reason: null, root: '/Volumes/Astro/Collaboration', watcherDegraded: false, networkVolume: false, replace: null, unknownDevice: null }),
    list_collab_receive_sessions: () => sessions,
    list_collab_link_suggestions: () => [],
    get_export_dir: () => '/Volumes/Astro/Exports',
    get_collab_frame_holders: (args) => holdersFor(args.frameUuid),
    account_status: () => ({ signedIn: true, email: 'you@example.org', deviceId: devId('you', 0), capability: 'full', hubUrl: 'https://hub.example' }),
    get_compute_queue: () => [],
    get_scan_roots: () => [],
    initialize_database: () => null,
    get_setting: (args) => args.defaultValue ?? null,
  };
  return { handlers, counts: { frames: FRAMES.length, members: MEMBERS.length } };
}

if (process.argv[2] === '--check') {
  const { counts, handlers } = buildFixtures('default');
  if (counts.frames !== 3719 || counts.members !== 8) throw new Error(`unexpected fixture counts ${JSON.stringify(counts)}`);
  console.log('fixtures ok', counts, 'library rows', handlers.list_collab_frames().length);
}
```

- [ ] **Step 3: Check the fixtures**

Run: `node scripts/ui-harness/fixtures.mjs --check`
Expected: `fixtures ok { frames: 3719, members: 8 } library rows <N>`, where N is greater than 3000.

- [ ] **Step 4: Write `scripts/ui-harness/server.mjs`**

```js
// Dev-only mock of the web API for the UI harness: POST /api/<command> from
// fixtures.mjs, an idle SSE channel, CORS for the Vite origin. An unknown
// command is logged once and answered with [] (list_*) or null.
import http from 'node:http';
import { buildFixtures } from './fixtures.mjs';

const scenario = process.env.HARNESS_SCENARIO || 'default';
const { handlers } = buildFixtures(scenario);
const unknown = new Set();
const CORS = { 'Access-Control-Allow-Origin': '*', 'Access-Control-Allow-Headers': 'Content-Type, X-API-Key', 'Access-Control-Allow-Methods': 'POST, GET, OPTIONS' };

http.createServer((req, res) => {
  if (req.method === 'OPTIONS') { res.writeHead(204, CORS); res.end(); return; }
  const url = new URL(req.url, 'http://harness');
  if (url.pathname === '/api/events') {
    res.writeHead(200, { ...CORS, 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache', Connection: 'keep-alive' });
    res.write(': harness\n\n');
    const t = setInterval(() => res.write(': ping\n\n'), 15000);
    req.on('close', () => clearInterval(t));
    return;
  }
  const cmd = url.pathname.replace(/^\/api\//, '');
  let body = '';
  req.on('data', (c) => { body += c; });
  req.on('end', () => {
    const args = body ? JSON.parse(body) : {};
    const h = handlers[cmd];
    let out;
    if (h) out = h(args);
    else {
      if (!unknown.has(cmd)) { unknown.add(cmd); console.log(`[harness] unknown command ${cmd} ${body}`); }
      out = /^list_/.test(cmd) ? [] : null;
    }
    res.writeHead(200, { ...CORS, 'Content-Type': 'application/json' });
    res.end(JSON.stringify(out ?? null));
  });
}).listen(8790, '127.0.0.1', () => console.log(`[harness] api on :8790 (scenario ${scenario})`));
```

- [ ] **Step 5: Write `scripts/ui-harness/run.mjs` and the npm script**

```js
// Starts the harness API and a web-mode Vite on :1430; Ctrl-C stops both.
import { spawn } from 'node:child_process';

const api = spawn(process.execPath, ['scripts/ui-harness/server.mjs'], { stdio: 'inherit', env: process.env });
const vite = spawn('npx', ['vite', '--port', '1430', '--strictPort'], {
  stdio: 'inherit',
  env: { ...process.env, VITE_TARGET: 'web', VITE_API_BASE_URL: 'http://127.0.0.1:8790' },
});
const stop = () => { api.kill(); vite.kill(); process.exit(0); };
process.on('SIGINT', stop);
process.on('SIGTERM', stop);
console.log('[harness] open http://localhost:1430/projects/p-m31');
```

In `package.json` `scripts`, add `"ui:harness": "node scripts/ui-harness/run.mjs"` next to `dev:web`.

- [ ] **Step 6: Write `scripts/ui-harness/measure.js`**

```js
// Paste into the page (Chrome javascript tool or DevTools) on BOTH the mockup and
// the harness; diff the two results by text. Geometry in CSS px, rounded.
(() => {
  const vis = (e) => e.offsetParent !== null || getComputedStyle(e).position === 'fixed';
  const r = (e) => {
    const b = e.getBoundingClientRect();
    const cs = getComputedStyle(e);
    return { x: Math.round(b.left), y: Math.round(b.top), w: Math.round(b.width), h: Math.round(b.height), fs: cs.fontSize, fw: cs.fontWeight, color: cs.color, bg: cs.backgroundColor };
  };
  const txt = (e) => e.textContent.replace(/\s+/g, ' ').trim().slice(0, 48);
  const pick = (sel, n = 60) => [...document.querySelectorAll(sel)].filter(vis).slice(0, n).map((e) => ({ t: txt(e), ...r(e) }));
  const rows = [...document.querySelectorAll('tbody tr')].filter(vis);
  return JSON.stringify({
    th: pick('th'),
    rows: rows.slice(0, 8).map((e) => ({ t: txt(e), ...r(e) })),
    cells: rows[3] ? [...rows[3].children].map((e) => ({ t: txt(e), ...r(e) })) : [],
    headings: pick('h1,h2,h3'),
    tabs: pick('[role=tab]'),
    buttons: pick('button', 40),
  });
})();
```

- [ ] **Step 7: Write `scripts/ui-harness/README.md`**

```markdown
# UI harness (dev only)

Renders the real collab project page on the approved mockup's sample data, so it
can be compared with `docs/superpowers/research/2026-09-29-collab-project-layout-mockup.html`
at the same viewport.

    npm run ui:harness                         # default data
    HARNESS_SCENARIO=long npm run ui:harness   # long names; also: filters, empty, offline

Open http://localhost:1430/projects/p-m31 at 1440 × 900 with the app sidebar
collapsed, and the mockup (served from any static server, "Design notes" off) at
the same size. Run `measure.js` on both pages and diff the JSON by text.
Tolerance: 1 px (spec §17.2). Nothing here ships in the app build.
```

- [ ] **Step 8: Smoke the harness**

Run: `npm run ui:harness` in the background. Then `curl -s -X POST http://127.0.0.1:8790/api/get_collab_project_detail -d '{}' | head -c 120`
Expected: JSON starting with `{"card":{"projectId":"p-m31"`. Open `http://localhost:1430/projects/p-m31`: the current page renders with 8 members and the Moderation badge. Stop the harness.

- [ ] **Step 9: Commit**

```bash
git add docs/superpowers/research/2026-09-29-collab-project-layout-mockup.html scripts/ui-harness package.json
git commit -m "chore(ui-harness): dev harness on the mockup's data; mockup in standards mode"
```

---

### Task 2: Foundation — tokens, font, gutter, palettes, formatters

**Files:**
- Modify: `tailwind.config.js`
- Modify: `src/index.css:5-6` (`:root` font-family)
- Modify: `src/components/Layout.tsx:174` (the content scroll container)
- Modify: `src/utils/filterColors.ts`
- Create: `src/utils/filterColors.test.ts`
- Modify: `src/components/collab/project/memberColors.ts`
- Modify: `src/components/collab/project/memberColors.test.ts`
- Modify: `src/components/collab/format.ts`
- Create or modify: `src/components/collab/format.test.ts`

**Interfaces:**
- Produces Tailwind tokens:
  - `text-content-faint`, `text-content-ghost`;
  - `border-line`, `border-line-soft`, `border-line-plain`;
  - `bg-table-head`, `bg-table-group`, `bg-table-group-l1`, `bg-table-group-hover`;
  - `bg-teal` / `text-teal`;
  - `font-mono` = the spec stack.
- Produces `memberColor(accountId: string, all: { accountId: string; displayName: string }[], selfAccountId: string | null): string` (a hex colour) and `MEMBER_PALETTE: readonly string[]` in `memberColors.ts`. `memberTone` is removed; its callers move to `memberColor` in their own tasks, and this task updates any caller that would otherwise stop compiling by passing `null` for `selfAccountId`.
- Produces `formatSize(bytes: number): string` and `formatDurationPadded(seconds: number): string` in `collab/format.ts`.

- [ ] **Step 1: Write the failing tests**

`src/utils/filterColors.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { getFilterColor } from './filterColors';

describe('filter palette (Nord, spec §4.3)', () => {
  it('maps the canonical filters and their aliases', () => {
    expect(getFilterColor('L')).toBe('#e5e9f0');
    expect(getFilterColor('Lum')).toBe('#e5e9f0');
    expect(getFilterColor('R')).toBe('#bf616a');
    expect(getFilterColor('G')).toBe('#a3be8c');
    expect(getFilterColor('B')).toBe('#5e81ac');
    expect(getFilterColor('Ha')).toBe('#d08770');
    expect(getFilterColor('H-alpha')).toBe('#d08770');
    expect(getFilterColor('OIII')).toBe('#88c0d0');
    expect(getFilterColor('O3')).toBe('#88c0d0');
    expect(getFilterColor('SII')).toBe('#b48ead');
    expect(getFilterColor('S2')).toBe('#b48ead');
    expect(getFilterColor('OSC')).toBe('#ebcb8b');
  });
  it('gives an unknown filter a stable colour across calls (review focus 2)', () => {
    const a = getFilterColor('L-eXtreme');
    expect(getFilterColor('L-eXtreme')).toBe(a);
    expect(getFilterColor('CLS')).not.toBe('');
  });
});
```

Add to `memberColors.test.ts` (replacing the `memberTone` cases):

```ts
import { describe, expect, it } from 'vitest';
import { MEMBER_PALETTE, memberColor } from './memberColors';

const all = [
  { accountId: 'a-kostya', displayName: 'Kostya' },
  { accountId: 'a-you', displayName: 'Vilen' },
  { accountId: 'a-andrei', displayName: 'Andrei' },
];

describe('memberColor (spec §4.4)', () => {
  it('always gives this account the accent colour', () => {
    expect(memberColor('a-you', all, 'a-you')).toBe('#88c0d0');
  });
  it('gives the others the remaining palette by displayName order', () => {
    expect(memberColor('a-andrei', all, 'a-you')).toBe(MEMBER_PALETTE[1]);
    expect(memberColor('a-kostya', all, 'a-you')).toBe(MEMBER_PALETTE[2]);
  });
  it('without a known self, everyone takes palette order from index 0', () => {
    expect(memberColor('a-andrei', all, null)).toBe(MEMBER_PALETTE[0]);
  });
  it('an unknown account gets the first slot, never undefined', () => {
    expect(memberColor('nobody', all, 'a-you')).toBe(MEMBER_PALETTE[0]);
  });
});
```

`src/components/collab/format.test.ts` (add these cases; keep any existing ones):

```ts
import { describe, expect, it } from 'vitest';
import { formatDurationPadded, formatSize } from './format';

describe('mockup formatters', () => {
  it('formatSize uses decimal units like the mockup', () => {
    expect(formatSize(121_920_698)).toBe('122 MB');
    expect(formatSize(4_900_000_000)).toBe('4.9 GB');
    expect(formatSize(1_250_000_000_000)).toBe('1.25 TB');
    expect(formatSize(950_000)).toBe('950 KB');
  });
  it('formatDurationPadded pads minutes after hours', () => {
    expect(formatDurationPadded(3 * 3600 + 2 * 60)).toBe('3h 02m');
    expect(formatDurationPadded(18 * 60)).toBe('18m');
    expect(formatDurationPadded(45 * 3600 + 57 * 60)).toBe('45h 57m');
    expect(formatDurationPadded(0)).toBe('0m');
  });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `npx vitest run src/utils/filterColors.test.ts src/components/collab/project/memberColors.test.ts src/components/collab/format.test.ts`
Expected: FAIL. The colours differ, and `memberColor` / `formatSize` / `formatDurationPadded` are not exported.

- [ ] **Step 3: Tokens**

In `tailwind.config.js` `theme.extend`:
- add `fontFamily.mono: ['ui-monospace', '"SF Mono"', 'Menlo', 'Consolas', 'monospace']`;
- inside `colors.content`, add `faint: 'rgba(216, 222, 233, 0.62)'` and `ghost: 'rgba(216, 222, 233, 0.38)'`;
- add the new colour entries:

```js
line: {
  DEFAULT: 'rgba(76, 86, 106, 0.55)', // mockup --line — card borders, separators
  soft: 'rgba(76, 86, 106, 0.28)',    // frame-table row separator
  plain: 'rgba(76, 86, 106, 0.30)',   // plain-table row separator
},
table: {
  head: '#333a47',
  group: '#323946',
  'group-l1': '#353c4a',
  'group-hover': '#3a4251',
},
teal: { DEFAULT: '#8fbcbb' }, // nord7
```

Change the four `muted` alphas (`success`, `warning`, `error`, `info`) from `0.25` to `0.22` and update their comments to `@ 22%`.

- [ ] **Step 4: Font and gutter**

In `src/index.css` `:root`, replace the `font-family` line with
`font-family: system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;`.

Append to `src/index.css` (spec §14, app-wide):

```css
@layer base {
  button:focus-visible, select:focus-visible, input:focus-visible, textarea:focus-visible, [role='tab']:focus-visible {
    @apply outline outline-2 outline-offset-1 outline-accent;
  }
}
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after { transition: none !important; animation: none !important; }
}
```

In `src/components/Layout.tsx`, change the content scroll container
`<div ref={contentRef} className="flex-1 overflow-auto">` to
`<div ref={contentRef} className="flex-1 overflow-auto [scrollbar-gutter:stable]">`.

- [ ] **Step 5: Filter palette**

In `src/utils/filterColors.ts`, change the colours only; the alias keys and the unknown palette stay:
- narrowband: Ha `'#d08770'`, OIII `'#88c0d0'`, SII `'#b48ead'`;
- broadband: R `'#bf616a'`, G `'#a3be8c'`, B `'#5e81ac'`, L `'#e5e9f0'`;
- add `[['osc', 'rgb', 'color'], '#ebcb8b']` to broadband;
- in `getFilterColor`, the `f.includes('ha')` / `'oiii'` / `'sii'` returns take the same three new values, and the `!filter` fallback becomes `'#e5e9f0'`.

- [ ] **Step 6: Member palette**

Replace the body of `memberColors.ts` (keep the file's top doc comment, updated to say "hex palette per spec §4.4; this account is always accent"):

```ts
/** Spec §4.4 — the mockup's member colours, in order. */
export const MEMBER_PALETTE = [
  '#88c0d0', '#a3be8c', '#b48ead', '#ebcb8b', '#d08770', '#81a1c1', '#8fbcbb', '#bf616a',
] as const;

/**
 * A member's colour: this account (`selfAccountId`) is always the accent; the
 * others take the remaining palette in displayName-then-accountId order,
 * cycling. With no known self, everyone takes the palette from slot 0.
 */
export function memberColor(
  accountId: string,
  all: { accountId: string; displayName: string }[],
  selfAccountId: string | null,
): string {
  if (selfAccountId !== null && accountId === selfAccountId) return MEMBER_PALETTE[0];
  const others = [...all]
    .filter((m) => m.accountId !== selfAccountId)
    .sort((a, b) => a.displayName.localeCompare(b.displayName) || a.accountId.localeCompare(b.accountId));
  const idx = others.findIndex((m) => m.accountId === accountId);
  if (idx === -1) return MEMBER_PALETTE[0];
  const offset = selfAccountId !== null ? 1 : 0;
  const span = MEMBER_PALETTE.length - offset;
  return MEMBER_PALETTE[offset + (idx % span)];
}
```

Then find the callers of the removed `memberTone` (`grep -rn memberTone src`). Each one used a Tailwind class (`bg-…`) from it. Convert each call site to `style={{ backgroundColor: memberColor(m.accountId, members, null) }}`. The real `selfAccountId` arrives in Task 7, and Tasks 9, 13 and 14 rewrite these call sites anyway.

- [ ] **Step 7: Formatters**

Append to `src/components/collab/format.ts`:

```ts
/** The mockup's `fmtB` — decimal units, as the project page shows sizes
 *  ("122 MB", "4.9 GB", "1.25 TB"). The binary `formatBytes` above keeps its
 *  non-project callers (spec §15). */
export function formatSize(n: number): string {
  if (!Number.isFinite(n) || n < 0) return '—';
  if (n >= 1e12) return `${(n / 1e12).toFixed(2)} TB`;
  if (n >= 1e9) return `${(n / 1e9).toFixed(1)} GB`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(0)} MB`;
  return `${(n / 1e3).toFixed(0)} KB`;
}

/** The mockup's `fmtDur` — minutes zero-padded after hours ("3h 02m", "18m"). */
export function formatDurationPadded(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return '0m';
  const h = Math.floor(seconds / 3600);
  const m = Math.round((seconds % 3600) / 60);
  return h ? `${h}h ${String(m).padStart(2, '0')}m` : `${m}m`;
}
```

- [ ] **Step 8: Run the tests and the typecheck**

Run: `npx vitest run src/utils/filterColors.test.ts src/components/collab/project/memberColors.test.ts src/components/collab/format.test.ts && npx tsc --noEmit -p .`
Expected: PASS, no type errors.

- [ ] **Step 9: Harness check**

Start the harness. On Overview, the filter dots and member segments now show the mockup's colours, the font is the system font, and switching tabs no longer shifts the header right edge. Record in the ledger: `measure.js` `tabs[0].x` is identical on Overview and Exchange.

- [ ] **Step 10: Commit**

```bash
git add tailwind.config.js src/index.css src/components/Layout.tsx src/utils/filterColors.ts src/utils/filterColors.test.ts src/components/collab/project/memberColors.ts src/components/collab/project/memberColors.test.ts src/components/collab/format.ts src/components/collab/format.test.ts
git add -u src
git commit -m "feat(ui): mockup tokens, system font, stable gutter, Nord filter and member palettes, mockup formatters"
```

---

### Task 3: Primitives — controls and marks

**Files:**
- Create in `src/components/ui/`: `Button.tsx`, `Pill.tsx`, `Chip.tsx`, `Seg.tsx`, `SegmentTiles.tsx`, `Card.tsx`, `KV.tsx`, `Dots.tsx` (`FilterDot`, `MemberDot`, `StatusDot`), `Bars.tsx` (`Bar`, `ProgressBar`), `Sparkline.tsx`, `FilterChip.tsx`, `EmptyState.tsx`, `Field.tsx` (`Select`, `TextInput`), `index.ts`
- Test: `src/components/ui/primitives.test.tsx`

**Interfaces:**
- Produces (all exported from `src/components/ui/index.ts`):
  - `Button({ variant?: 'default'|'primary'|'danger'|'dangerPrimary'|'link', size?: 'md'|'sm', ...ButtonHTMLAttributes })`
  - `Pill({ children, dot?: ReactNode, as?: 'span'|'button', ...rest })`
  - `Chip({ tone: 'ok'|'warn'|'err'|'info'|'mute', children, title? })`
  - `Seg<T extends string>({ options: { value: T; label: ReactNode }[], value: T, onChange(v: T) })`
  - `SegmentTiles<T extends string>({ tiles: { value: T; n: number | string; label: string; tone: 'accent'|'success'|'warning'|'content' }[], value: T, onChange(v: T) })`
  - `Card({ title?: ReactNode, subtitle?: ReactNode, action?: ReactNode, children, className? })`
  - `KV({ items: [ReactNode, ReactNode][] })`
  - `FilterDot({ filter: string })`
  - `MemberDot({ color: string })`
  - `StatusDot({ state: 'online'|'offline'|'live'|'warn'|'error' })`
  - `Bar({ segments: { value: number; color?: string; className?: string; title?: string }[], total?: number, className? })` — a segment takes a hex `color` (member colours) or a token `className` (`bg-success`, …)
  - `ProgressBar({ percent: number, color?: string, className? })`
  - `Sparkline({ values: number[], color: string })`
  - `FilterChip({ filter: string, count: number, on: boolean, onClick() })`
  - `EmptyState({ children })`
  - `Select(SelectHTMLAttributes)`, `TextInput(InputHTMLAttributes)`

- [ ] **Step 1: Write the failing tests**

`src/components/ui/primitives.test.tsx`:

```tsx
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { Bar, Button, Card, Chip, EmptyState, FilterChip, KV, ProgressBar, Seg, SegmentTiles, Sparkline } from '.';

describe('ui primitives', () => {
  it('Button variants carry the mockup classes', () => {
    render(<><Button>Plain</Button><Button variant="primary">Go</Button><Button size="sm">Small</Button></>);
    expect(screen.getByText('Plain').className).toContain('text-[12px]');
    expect(screen.getByText('Go').className).toContain('bg-accent');
    expect(screen.getByText('Small').className).toContain('text-[11px]');
  });
  it('Chip tones map to the muted backgrounds', () => {
    render(<Chip tone="err">missing</Chip>);
    expect(screen.getByText('missing').className).toContain('bg-error-muted');
  });
  it('Card renders title, subtitle and action in one header', () => {
    render(<Card title="Receiving" subtitle="43.4 MB/s" action={<button>x</button>}>body</Card>);
    const h = screen.getByRole('heading', { name: /Receiving/ });
    expect(h).toHaveTextContent('43.4 MB/s');
    expect(h.className).toContain('text-[13px]');
  });
  it('KV renders a definition list with faint terms', () => {
    render(<KV items={[['Night', '2026-08-31 · Mon']]} />);
    expect(screen.getByText('Night').tagName).toBe('DT');
    expect(screen.getByText('2026-08-31 · Mon').tagName).toBe('DD');
  });
  it('Seg and SegmentTiles report the chosen value', () => {
    const onSeg = vi.fn();
    const onTile = vi.fn();
    render(<>
      <Seg options={[{ value: 'a', label: 'A' }, { value: 'b', label: 'B' }]} value="a" onChange={onSeg} />
      <SegmentTiles tiles={[{ value: 'ready', n: 136, label: 'Ready to publish', tone: 'accent' }, { value: 'held', n: 390, label: 'Held back', tone: 'warning' }]} value="ready" onChange={onTile} />
    </>);
    fireEvent.click(screen.getByText('B'));
    fireEvent.click(screen.getByText('Held back'));
    expect(onSeg).toHaveBeenCalledWith('b');
    expect(onTile).toHaveBeenCalledWith('held');
    expect(screen.getByRole('button', { name: /136 Ready to publish/ })).toHaveAttribute('aria-pressed', 'true');
  });
  it('Bar sizes its segments as shares of the total', () => {
    const { container } = render(<Bar segments={[{ value: 3, color: '#a3be8c' }, { value: 1, color: '#bf616a' }]} />);
    const segs = container.querySelectorAll('i');
    expect((segs[0] as HTMLElement).style.width).toBe('75%');
    expect((segs[1] as HTMLElement).style.width).toBe('25%');
  });
  it('ProgressBar clamps to 0–100', () => {
    const { container } = render(<ProgressBar percent={140} />);
    expect((container.querySelector('i') as HTMLElement).style.width).toBe('100%');
  });
  it('Sparkline draws a line for 2+ samples and nothing for fewer', () => {
    const { container, rerender } = render(<Sparkline values={[1, 3, 2]} color="#88c0d0" />);
    expect(container.querySelectorAll('polyline')).toHaveLength(2);
    rerender(<Sparkline values={[1]} color="#88c0d0" />);
    expect(container.querySelector('polyline')).toBeNull();
  });
  it('FilterChip shows count and dims at zero', () => {
    render(<FilterChip filter="Ha" count={0} on={false} onClick={() => {}} />);
    expect(screen.getByRole('button', { name: /Ha 0/ }).className).toContain('opacity-40');
  });
  it('EmptyState uses the faint 12.5px line', () => {
    render(<EmptyState>Nothing is moving.</EmptyState>);
    expect(screen.getByText('Nothing is moving.').className).toContain('text-[12.5px]');
  });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `npx vitest run src/components/ui/primitives.test.tsx`
Expected: FAIL — `Cannot find module '.'`.

- [ ] **Step 3: Implement the primitives**

`Button.tsx`:

```tsx
import type { ButtonHTMLAttributes } from 'react';

/** Mockup `.btn` / `.btn.pri` / `.btn.sm` / `.btn[disabled]` / `.linkbtn`. */
export type ButtonVariant = 'default' | 'primary' | 'danger' | 'dangerPrimary' | 'link';
export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: 'md' | 'sm';
}
const SIZE = { md: 'px-2.5 py-1 text-[12px]', sm: 'px-[7px] py-px text-[11px]' } as const;
const VARIANT: Record<Exclude<ButtonVariant, 'link'>, string> = {
  default: 'border-border text-content-secondary hover:bg-surface-hover',
  primary: 'border-accent bg-accent font-semibold text-surface hover:border-accent-hover hover:bg-accent-hover',
  danger: 'border-error/60 text-error hover:bg-error/10',
  dangerPrimary: 'border-error bg-error font-semibold text-surface hover:brightness-110',
};
export function Button({ variant = 'default', size = 'md', className = '', type = 'button', ...rest }: ButtonProps) {
  const cls =
    variant === 'link'
      ? `leading-[1.4] text-accent hover:underline disabled:cursor-not-allowed disabled:opacity-45 disabled:no-underline ${size === 'sm' ? 'text-[11px]' : 'text-[12px]'}`
      : `inline-flex items-center gap-1.5 whitespace-nowrap rounded border leading-[1.4] transition-colors disabled:cursor-not-allowed disabled:opacity-45 ${SIZE[size]} ${VARIANT[variant]}`;
  return <button type={type} className={`${cls} ${className}`} {...rest} />;
}
```

`Pill.tsx`:

```tsx
import type { HTMLAttributes, ReactNode } from 'react';

/** Mockup `.pill` — 11.5px on elev, radius 999, padding 1×8. */
export function Pill({ children, dot, as = 'span', className = '', ...rest }: HTMLAttributes<HTMLElement> & { dot?: ReactNode; as?: 'span' | 'button' }) {
  const cls = `inline-flex items-center gap-[5px] rounded-full bg-surface-elevated px-2 py-px text-[11.5px] leading-[1.4] text-content-muted ${as === 'button' ? 'hover:bg-surface-hover disabled:cursor-not-allowed' : ''} ${className}`;
  if (as === 'button') return <button type="button" className={cls} {...rest}>{dot}{children}</button>;
  return <span className={cls} {...rest}>{dot}{children}</span>;
}
```

`Chip.tsx`:

```tsx
import type { ReactNode } from 'react';

/** Mockup `.chip` + `.c-ok/.c-warn/.c-err/.c-info/.c-mute` — 11px, line 18px, radius 3. */
export type ChipTone = 'ok' | 'warn' | 'err' | 'info' | 'mute';
const TONE: Record<ChipTone, string> = {
  ok: 'bg-success-muted text-success',
  warn: 'bg-warning-muted text-warning',
  err: 'bg-error-muted text-error',
  info: 'bg-info-muted text-info',
  mute: 'bg-surface-hover text-content-muted',
};
export function Chip({ tone, children, title, className = '' }: { tone: ChipTone; children: ReactNode; title?: string; className?: string }) {
  return (
    <span title={title} className={`inline-flex items-center gap-1 whitespace-nowrap rounded-[3px] px-1.5 text-[11px] leading-[18px] ${TONE[tone]} ${className}`}>
      {children}
    </span>
  );
}
```

`Seg.tsx`:

```tsx
import type { ReactNode } from 'react';

/** Mockup `.seg` — bordered segmented control. */
export function Seg<T extends string>({ options, value, onChange }: { options: { value: T; label: ReactNode }[]; value: T; onChange: (v: T) => void }) {
  return (
    <span className="inline-flex overflow-hidden rounded-[5px] border border-border">
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          aria-pressed={o.value === value}
          onClick={() => onChange(o.value)}
          className={`border-r border-border px-2.5 py-[3px] leading-[1.4] last:border-r-0 ${o.value === value ? 'bg-surface-elevated text-content' : 'text-content-faint'}`}
        >
          {o.label}
        </button>
      ))}
    </span>
  );
}
```

`SegmentTiles.tsx`:

```tsx
/** Mockup `.segs button` — My-frames segment tiles (number 18px, label 12px). */
const TONE = { accent: 'text-accent', success: 'text-success', warning: 'text-warning', content: 'text-content' } as const;
export function SegmentTiles<T extends string>({ tiles, value, onChange }: {
  tiles: { value: T; n: number | string; label: string; tone: keyof typeof TONE }[];
  value: T;
  onChange: (v: T) => void;
}) {
  return (
    <div className="flex flex-wrap gap-2">
      {tiles.map((t) => {
        const on = t.value === value;
        return (
          <button
            key={t.value}
            type="button"
            aria-pressed={on}
            onClick={() => onChange(t.value)}
            className={`flex min-w-[150px] flex-col items-start gap-px rounded-md border px-3.5 py-[7px] text-left leading-[1.4] ${on ? 'border-accent bg-accent/[0.08]' : 'border-border hover:bg-surface-hover'}`}
          >
            <span className={`text-[18px] font-semibold ${TONE[t.tone]}`}>{typeof t.n === 'number' ? t.n.toLocaleString('en-US') : t.n}</span>
            <span className={`text-[12px] ${on ? 'text-accent' : 'text-content-faint'}`}>{t.label}</span>
          </button>
        );
      })}
    </div>
  );
}
```

`Card.tsx`:

```tsx
import type { ReactNode } from 'react';

/** Mockup `.card` + `.card h2` + `.sub`. */
export function Card({ title, subtitle, action, children, className = '' }: { title?: ReactNode; subtitle?: ReactNode; action?: ReactNode; children?: ReactNode; className?: string }) {
  return (
    <section className={`rounded-lg border border-line bg-surface-elevated px-4 py-3.5 ${className}`}>
      {title !== undefined && (
        <h2 className="mb-2.5 flex items-center gap-2 text-[13px] font-semibold text-content">
          <span>{title}</span>
          {subtitle !== undefined && <span className="text-[12px] font-normal text-content-faint">{subtitle}</span>}
          {action !== undefined && <span className="ml-auto text-[12px] font-normal">{action}</span>}
        </h2>
      )}
      {children}
    </section>
  );
}
```

`KV.tsx`:

```tsx
import { Fragment, type ReactNode } from 'react';

/** Mockup `.kv` — grid auto/1fr, gap 3/14, 12px, faint terms. */
export function KV({ items }: { items: [ReactNode, ReactNode][] }) {
  return (
    <dl className="grid grid-cols-[auto_1fr] gap-x-3.5 gap-y-[3px] text-[12px]">
      {items.map(([k, v], i) => (
        <Fragment key={i}>
          <dt className="text-content-faint">{k}</dt>
          <dd className="m-0 min-w-0 text-content-secondary">{v}</dd>
        </Fragment>
      ))}
    </dl>
  );
}
```

`Dots.tsx`:

```tsx
import { getFilterColor } from '../../utils/filterColors';

/** Mockup `.fdot` — 8px, radius 2, 5px right margin. */
export function FilterDot({ filter }: { filter: string }) {
  return <span aria-hidden className="mr-[5px] inline-block h-2 w-2 shrink-0 rounded-[2px] align-[0px]" style={{ backgroundColor: getFilterColor(filter) }} />;
}
/** Mockup `.dot` in a member's colour. */
export function MemberDot({ color }: { color: string }) {
  return <span aria-hidden className="inline-block h-[7px] w-[7px] shrink-0 rounded-full" style={{ backgroundColor: color }} />;
}
const STATE = {
  online: 'bg-success',
  offline: 'bg-border',
  live: 'bg-success shadow-[0_0_0_3px_rgba(163,190,140,0.18)]',
  warn: 'bg-warning',
  error: 'bg-error',
} as const;
/** Mockup `.dot` / `.live-dot`. */
export function StatusDot({ state }: { state: keyof typeof STATE }) {
  return <span aria-hidden className={`inline-block h-[7px] w-[7px] shrink-0 rounded-full ${STATE[state]}`} />;
}
```

`Bars.tsx`:

```tsx
/** Mockup `.bar` — 8px stacked bar on surface-hover. */
export function Bar({ segments, total, className = '' }: { segments: { value: number; color?: string; className?: string; title?: string }[]; total?: number; className?: string }) {
  const t = total ?? segments.reduce((a, s) => a + s.value, 0);
  return (
    <span className={`flex h-2 w-full overflow-hidden rounded-[2px] bg-surface-hover ${className}`}>
      {t > 0 && segments.filter((s) => s.value > 0).map((s, i) => (
        <i
          key={i}
          title={s.title}
          className={`block h-full ${s.className ?? ''}`}
          style={{ width: `${(s.value / t) * 100}%`, ...(s.color ? { backgroundColor: s.color } : {}) }}
        />
      ))}
    </span>
  );
}
/** Mockup `.pbar` — 4px progress. */
export function ProgressBar({ percent, color, className = '' }: { percent: number; color?: string; className?: string }) {
  const p = Math.max(0, Math.min(100, Number.isFinite(percent) ? percent : 0));
  return (
    <span className={`block h-1 overflow-hidden rounded-[2px] bg-surface-hover ${className}`}>
      <i className={`block h-full ${color ? '' : 'bg-accent'}`} style={{ width: `${p}%`, ...(color ? { backgroundColor: color } : {}) }} />
    </span>
  );
}
```

`Sparkline.tsx`:

```tsx
/** Mockup `spark()` — 120×26 area + line, fill 12%. Needs ≥ 2 samples. */
export function Sparkline({ values, color }: { values: number[]; color: string }) {
  if (values.length < 2) return <svg width="120" height="26" aria-hidden />;
  const max = Math.max(...values) * 1.1 || 1;
  const pts = values.map((v, i) => `${((i / (values.length - 1)) * 120).toFixed(1)},${(24 - (v / max) * 22).toFixed(1)}`).join(' ');
  return (
    <svg width="120" height="26" viewBox="0 0 120 26" aria-hidden>
      <polyline points={`0,25 ${pts} 120,25`} fill={color} fillOpacity={0.12} stroke="none" />
      <polyline points={pts} fill="none" stroke={color} strokeWidth={1.4} />
    </svg>
  );
}
```

`FilterChip.tsx`:

```tsx
import { FilterDot } from './Dots';

/** Mockup `.fchip` (+ `.k` count, `.on`, `.zero`). */
export function FilterChip({ filter, count, on, onClick }: { filter: string; count: number; on: boolean; onClick: () => void }) {
  return (
    <button
      type="button"
      aria-pressed={on}
      onClick={onClick}
      className={`inline-flex items-center gap-[5px] rounded-full border px-2 py-0.5 text-[12px] leading-[1.4] ${on ? 'border-accent bg-accent/[0.12] text-content' : 'border-border text-content-muted'} ${count === 0 ? 'opacity-40' : ''}`}
    >
      <FilterDot filter={filter} />
      {filter} <span className="text-[10.5px] text-content-faint">{count}</span>
    </button>
  );
}
```

`EmptyState.tsx`:

```tsx
import type { ReactNode } from 'react';

/** Mockup `.empty`. */
export function EmptyState({ children }: { children: ReactNode }) {
  return <p className="py-2.5 text-[12.5px] text-content-faint">{children}</p>;
}
```

`Field.tsx`:

```tsx
import { forwardRef, type InputHTMLAttributes, type SelectHTMLAttributes } from 'react';

/** Mockup `select,input[type=text]` — 26px, elev, border, radius 4, padding 3×6. */
const FIELD = 'h-[26px] rounded border border-border bg-surface-elevated px-1.5 py-[3px] text-[12px] text-content placeholder:text-content-faint';
export const Select = forwardRef<HTMLSelectElement, SelectHTMLAttributes<HTMLSelectElement>>(function Select({ className = '', ...rest }, ref) {
  return <select ref={ref} className={`${FIELD} ${className}`} {...rest} />;
});
export const TextInput = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement>>(function TextInput({ className = '', type = 'text', ...rest }, ref) {
  return <input ref={ref} type={type} className={`${FIELD} ${className}`} {...rest} />;
});
```

`index.ts`:

```ts
export * from './Button';
export * from './Pill';
export * from './Chip';
export * from './Seg';
export * from './SegmentTiles';
export * from './Card';
export * from './KV';
export * from './Dots';
export * from './Bars';
export * from './Sparkline';
export * from './FilterChip';
export * from './EmptyState';
export * from './Field';
```

- [ ] **Step 4: Run the tests and the typecheck**

Run: `npx vitest run src/components/ui/primitives.test.tsx && npx tsc --noEmit -p .`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/components/ui
git commit -m "feat(ui): compact primitives mirroring the mockup CSS"
```

---

### Task 4: Overlays — `DialogShell`, `Popover`, `PanelLayout` / `SidePanel`

**Files:**
- Create: `src/components/ui/DialogShell.tsx`, `src/components/ui/Popover.tsx`, `src/components/ui/SidePanel.tsx`
- Modify: `src/components/ui/index.ts`
- Test: `src/components/ui/overlays.test.tsx`

**Interfaces:**
- Produces:
  - `DialogShell({ title: ReactNode, size?: 'sm'|'md', onClose(): void, busy?: boolean, footer?: ReactNode, children })`. The title element labels the dialog (`aria-labelledby`); an element with `data-autofocus` inside gets the initial focus. A 440/560 px window; Esc and scrim close it unless `busy`; focus is trapped and restored.
  - `Popover({ open: boolean, onClose(): void, className?: string, children })`. Absolutely positioned under its relative parent; closes on outside mousedown and on Esc.
  - `PanelLayout({ panel: ReactNode | null, children })`. When `panel` is set, the body becomes `grid-cols-[minmax(0,1fr)_400px] gap-3.5`.
  - `SidePanel({ title: ReactNode, onClose(): void, children, label: string })`. Sticky, 400 px, `elev`, `line` border, radius 8, padding 16×18 (bottom 30), height = viewport below its top, own scroll; Esc closes it.

- [ ] **Step 1: Write the failing tests**

`src/components/ui/overlays.test.tsx`:

```tsx
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { useState } from 'react';
import { DialogShell, PanelLayout, Popover, SidePanel } from '.';

describe('DialogShell', () => {
  it('Esc and scrim close; clicks inside do not', () => {
    const onClose = vi.fn();
    render(<DialogShell title="Export" onClose={onClose}><p>body</p></DialogShell>);
    fireEvent.click(screen.getByText('body'));
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.keyDown(document, { key: 'Escape' });
    fireEvent.click(screen.getByTestId('dialog-scrim'));
    expect(onClose).toHaveBeenCalledTimes(2);
  });
  it('does not close while busy', () => {
    const onClose = vi.fn();
    render(<DialogShell title="Export" onClose={onClose} busy><p>body</p></DialogShell>);
    fireEvent.keyDown(document, { key: 'Escape' });
    fireEvent.click(screen.getByTestId('dialog-scrim'));
    expect(onClose).not.toHaveBeenCalled();
  });
  it('is a labelled modal dialog and focuses inside', () => {
    render(<DialogShell title="Publish to M31" onClose={() => {}} footer={<button>OK</button>}><input aria-label="x" /></DialogShell>);
    const d = screen.getByRole('dialog', { name: 'Publish to M31' });
    expect(d).toHaveAttribute('aria-modal', 'true');
    expect(d.contains(document.activeElement)).toBe(true);
  });
  it('traps Tab at the last focusable element', () => {
    render(<DialogShell title="T" onClose={() => {}} footer={<button>Last</button>}><button>First</button></DialogShell>);
    const last = screen.getByText('Last');
    last.focus();
    fireEvent.keyDown(document, { key: 'Tab' });
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Close' }));
  });
});

describe('Popover', () => {
  it('closes on Esc and on an outside mousedown', () => {
    const onClose = vi.fn();
    render(<div><span>outside</span><div className="relative"><Popover open onClose={onClose}><label>col</label></Popover></div></div>);
    fireEvent.keyDown(document, { key: 'Escape' });
    fireEvent.mouseDown(screen.getByText('outside'));
    expect(onClose).toHaveBeenCalledTimes(2);
  });
});

describe('PanelLayout + SidePanel', () => {
  function Harness() {
    const [open, setOpen] = useState(true);
    return (
      <PanelLayout panel={open ? <SidePanel title="Light_0003.fits" label="Frame details" onClose={() => setOpen(false)}>kv</SidePanel> : null}>
        <table><tbody><tr><td>row</td></tr></tbody></table>
      </PanelLayout>
    );
  }
  it('splits into a 400px panel column while open and closes on Esc (review focus 3)', () => {
    const { container } = render(<Harness />);
    expect((container.firstChild as HTMLElement).className).toContain('grid-cols-[minmax(0,1fr)_400px]');
    expect(screen.getByRole('complementary', { name: 'Frame details' })).toBeInTheDocument();
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('complementary')).toBeNull();
    expect((container.firstChild as HTMLElement).className).not.toContain('grid-cols');
  });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `npx vitest run src/components/ui/overlays.test.tsx`
Expected: FAIL — `DialogShell` is not exported.

- [ ] **Step 3: Implement**

`DialogShell.tsx`:

```tsx
import { useEffect, useId, useRef, type ReactNode } from 'react';
import { X } from 'lucide-react';
import { Button } from './Button';

const FOCUSABLE = 'button:not([disabled]),[href],input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])';

/** Spec §7 — the one modal shell: scrim, centred window, header, body, footer. */
export function DialogShell({ title, size = 'sm', onClose, busy = false, footer, children }: {
  title: ReactNode;
  size?: 'sm' | 'md';
  onClose: () => void;
  busy?: boolean;
  footer?: ReactNode;
  children: ReactNode;
}) {
  const titleId = useId();
  const ref = useRef<HTMLDivElement>(null);
  const busyRef = useRef(busy);
  busyRef.current = busy;
  const closeRef = useRef(onClose);
  closeRef.current = onClose;

  useEffect(() => {
    const opener = document.activeElement as HTMLElement | null;
    const first = ref.current?.querySelector<HTMLElement>(`[data-autofocus],${FOCUSABLE}`);
    (first ?? ref.current)?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        if (!busyRef.current) closeRef.current();
        return;
      }
      if (e.key !== 'Tab' || !ref.current) return;
      const items = [...ref.current.querySelectorAll<HTMLElement>(FOCUSABLE)];
      if (items.length === 0) return;
      const i = items.indexOf(document.activeElement as HTMLElement);
      if (!e.shiftKey && (i === items.length - 1 || i === -1)) { e.preventDefault(); items[0].focus(); }
      else if (e.shiftKey && i <= 0) { e.preventDefault(); items[items.length - 1].focus(); }
    };
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('keydown', onKey);
      opener?.focus?.();
    };
  }, []);

  return (
    <div
      data-testid="dialog-scrim"
      className="fixed inset-0 z-50 flex items-center justify-center bg-[rgba(46,52,64,0.6)]"
      onClick={() => { if (!busyRef.current) onClose(); }}
    >
      <div
        ref={ref}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        onClick={(e) => e.stopPropagation()}
        className={`max-w-[92vw] rounded-lg border border-border bg-surface-elevated px-4 py-3.5 text-[12.5px] leading-[1.4] text-content-muted shadow-[0_8px_24px_rgba(0,0,0,0.35)] ${size === 'md' ? 'w-[560px]' : 'w-[440px]'}`}
      >
        <div className="mb-2.5 flex items-start gap-2">
          <h2 id={titleId} className="min-w-0 flex-1 text-[13px] font-semibold text-content">{title}</h2>
          <Button size="sm" aria-label="Close" onClick={onClose} disabled={busy}><X size={12} /></Button>
        </div>
        <div>{children}</div>
        {footer !== undefined && <div className="mt-3.5 flex justify-end gap-2">{footer}</div>}
      </div>
    </div>
  );
}
```

`Popover.tsx`:

```tsx
import { useEffect, useRef, type ReactNode } from 'react';

/** Mockup `.pop` — absolutely positioned under its `relative` parent. */
export function Popover({ open, onClose, children, className = '' }: { open: boolean; onClose: () => void; children: ReactNode; className?: string }) {
  const ref = useRef<HTMLDivElement>(null);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  useEffect(() => {
    if (!open) return undefined;
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') closeRef.current(); };
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.parentElement?.contains(e.target as Node)) closeRef.current();
    };
    document.addEventListener('keydown', onKey);
    document.addEventListener('mousedown', onDown);
    return () => {
      document.removeEventListener('keydown', onKey);
      document.removeEventListener('mousedown', onDown);
    };
  }, [open]);
  if (!open) return null;
  return (
    <div ref={ref} className={`absolute right-0 top-full z-20 mt-1 grid min-w-[170px] gap-1 rounded-md border border-border bg-surface-elevated px-2.5 py-2 shadow-[0_8px_24px_rgba(0,0,0,0.35)] ${className}`}>
      {children}
    </div>
  );
}
```

`SidePanel.tsx`:

```tsx
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import { X } from 'lucide-react';
import { Button } from './Button';

/** Spec §6.1 — a tab body that grows a 400px docked panel column while `panel` is set. */
export function PanelLayout({ panel, children }: { panel: ReactNode | null; children: ReactNode }) {
  return (
    <div className={panel ? 'grid grid-cols-[minmax(0,1fr)_400px] items-start gap-3.5' : ''}>
      <div className="min-w-0">{children}</div>
      {panel}
    </div>
  );
}

/** Spec §6.1 — sticky, own scroll, height = the viewport below its top edge. */
export function SidePanel({ title, label, onClose, children }: { title: ReactNode; label: string; onClose: () => void; children: ReactNode }) {
  const ref = useRef<HTMLElement>(null);
  const [height, setHeight] = useState<number | undefined>(undefined);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  useLayoutEffect(() => {
    const fit = () => {
      if (!ref.current) return;
      const top = Math.max(0, ref.current.getBoundingClientRect().top);
      setHeight(Math.max(240, window.innerHeight - top - 16));
    };
    fit();
    window.addEventListener('resize', fit);
    return () => window.removeEventListener('resize', fit);
  }, []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || e.defaultPrevented) return;
      if (document.querySelector('[role="dialog"]')) return; // an open dialog owns Escape
      closeRef.current();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, []);
  return (
    <aside
      ref={ref}
      aria-label={label}
      style={height ? { height } : undefined}
      className="sticky top-0 overflow-auto rounded-lg border border-line bg-surface-elevated px-[18px] pb-[30px] pt-4 text-[12px] leading-[1.4]"
    >
      <div className="flex items-start gap-2">
        <div className="min-w-0 flex-1">{title}</div>
        <Button size="sm" aria-label="Close" onClick={onClose}><X size={12} /></Button>
      </div>
      {children}
    </aside>
  );
}
```

Add `export * from './DialogShell'; export * from './Popover'; export * from './SidePanel';` to `index.ts`.

- [ ] **Step 4: Run the tests and the typecheck**

Run: `npx vitest run src/components/ui/overlays.test.tsx && npx tsc --noEmit -p .`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/components/ui
git commit -m "feat(ui): DialogShell, Popover and the docked SidePanel layout"
```

---

### Task 5: App-wide `Checkbox`, `ConfirmDialog`, `AlertDialog`

**Files:**
- Modify: `src/components/settings/Checkbox.tsx`
- Modify: `src/components/ConfirmDialog.tsx`
- Modify: `src/components/AlertDialog.tsx`
- Test: `src/components/settings/Checkbox.test.tsx` (create if absent), `src/components/ConfirmDialog.test.tsx` (create)

**Interfaces:**
- Consumes: `DialogShell`, `Button` (Tasks 3–4).
- Produces: unchanged public props for all three components (their callers are untouched). `Checkbox` keeps `CheckboxProps` and gains the mockup `.cb` box: 13 px, radius 3, `bg-surface` border `border-border`; checked = accent fill with a surface-coloured tick; indeterminate is not exposed here, because the table checkboxes (Task 6) render their own `.cb` box.

- [ ] **Step 1: Write the failing tests**

`src/components/ConfirmDialog.test.tsx`:

```tsx
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { ConfirmDialog } from './ConfirmDialog';

describe('ConfirmDialog on DialogShell', () => {
  it('renders as a labelled dialog with Cancel and the confirm action', () => {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(<ConfirmDialog isOpen title="Stop keeping 3 frames?" message={'line 1\nline 2'} confirmText="Stop keeping" confirmDanger onConfirm={onConfirm} onCancel={onCancel} />);
    expect(screen.getByRole('dialog', { name: 'Stop keeping 3 frames?' })).toBeInTheDocument();
    expect(screen.getByText(/line 1/).className).toContain('whitespace-pre-line');
    const confirm = screen.getByRole('button', { name: 'Stop keeping' });
    expect(confirm.className).toContain('bg-error');
    fireEvent.click(confirm);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onConfirm).toHaveBeenCalledTimes(1);
    expect(onCancel).toHaveBeenCalledTimes(1);
  });
  it('renders nothing when closed', () => {
    render(<ConfirmDialog isOpen={false} title="t" message="m" onConfirm={() => {}} onCancel={() => {}} />);
    expect(screen.queryByRole('dialog')).toBeNull();
  });
});
```

`src/components/settings/Checkbox.test.tsx` — add, or create with:

```tsx
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { Checkbox } from './Checkbox';

describe('Checkbox (mockup .cb)', () => {
  it('keeps a real checkbox input and draws the 13px box', () => {
    const onChange = vi.fn();
    render(<Checkbox checked={false} onChange={onChange} label="Trust these publishers" />);
    const input = screen.getByRole('checkbox', { name: 'Trust these publishers' });
    fireEvent.click(input);
    expect(onChange).toHaveBeenCalledWith(true);
    expect(screen.getByTestId('cb-box').className).toContain('h-[13px]');
  });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `npx vitest run src/components/ConfirmDialog.test.tsx src/components/settings/Checkbox.test.tsx`
Expected: FAIL — there is no dialog role and no `cb-box`.

- [ ] **Step 3: Implement**

`ConfirmDialog.tsx` body (the props interface stays):

```tsx
import { Button, DialogShell } from './ui';

export const ConfirmDialog: React.FC<ConfirmDialogProps> = ({
  isOpen, title, message, onConfirm, onCancel, confirmText = 'Confirm', cancelText = 'Cancel', confirmDanger = false,
}) => {
  if (!isOpen) return null;
  return (
    <DialogShell
      title={title}
      onClose={onCancel}
      footer={
        <>
          <Button onClick={onCancel}>{cancelText}</Button>
          <Button variant={confirmDanger ? 'dangerPrimary' : 'primary'} onClick={onConfirm} data-autofocus>{confirmText}</Button>
        </>
      }
    >
      <p className="whitespace-pre-line">{message}</p>
    </DialogShell>
  );
};
```

`AlertDialog.tsx`: keep its props. Render `DialogShell` with the title prefixed by the variant icon (`AlertCircle` for error in `text-error`, `AlertTriangle` for warning in `text-warning`, `Info` for info in `text-info`, all at size 14 and inline). Body `<p className="whitespace-pre-line">{message}</p>`. Footer: `showCloseButton !== false` gives `<Button variant="primary" onClick={onClose} data-autofocus>OK</Button>`.

`Checkbox.tsx`: keep the label/description layout. Replace the visible box with a visually hidden real `<input type="checkbox" className="peer sr-only">` inside the `<label>`. After it, render
`<span data-testid="cb-box" aria-hidden className="relative inline-block h-[13px] w-[13px] shrink-0 rounded-[3px] border border-border bg-surface peer-checked:border-accent peer-checked:bg-accent peer-focus-visible:outline peer-focus-visible:outline-2 peer-focus-visible:outline-accent peer-disabled:opacity-45" />`.
The tick is `<svg>` 9×9 (`<path d="M1.5 4.5 3.5 6.5 7.5 2" stroke="currentColor" strokeWidth="2" fill="none"/>`, `text-surface`), absolutely centred inside the box and shown only when checked (`{checked && …}`). `role="switch"` keeps its current switch rendering, untouched. `size` `sm`/`md` changes only the label font (`text-[12px]` / `text-sm`), never the box.

- [ ] **Step 4: Run the full suite and the typecheck**

Run: `npx vitest run && npx tsc --noEmit -p .`
Expected: PASS. If a caller's test asserted the old ConfirmDialog markup (a `bg-success` class, `h3`), update that assertion to the dialog role and button names; the behaviour is unchanged.

- [ ] **Step 5: Harness check**

In the harness, open Moderation. The "Trust these publishers" `Checkbox` box measures 13 × 13 px (inspect `[data-testid=cb-box]`). The `ConfirmDialog` look is covered by the unit test here; its on-page check happens in Task 12 (Reject) and Task 15 (dialogs).

- [ ] **Step 6: Commit**

```bash
git add src/components/settings/Checkbox.tsx src/components/settings/Checkbox.test.tsx src/components/ConfirmDialog.tsx src/components/ConfirmDialog.test.tsx src/components/AlertDialog.tsx
git add -u src
git commit -m "feat(ui): Checkbox, ConfirmDialog and AlertDialog on the mockup shell (app-wide)"
```

---

### Task 6: Frame-table engine (`ProjectFrameTable` + `frames.tsx` cells)

**Files:**
- Modify: `src/components/collab/project/table/ProjectFrameTable.tsx` (markup and layout; the facet/group/sort/selection/windowing logic stays as is)
- Modify: `src/components/collab/project/frames.tsx` (cell and group renderers only, `COLUMNS`/`GROUPS` data unchanged)
- Create: `src/components/collab/project/MemberColors.tsx`
- Modify: `src/components/collab/project/table/ProjectFrameTable.test.tsx`, `src/components/collab/project/frames.test.tsx`

**Interfaces:**
- Consumes: `Button`, `Chip`, `FilterChip`, `FilterDot`, `MemberDot`, `StatusDot`, `Bar`, `ProgressBar`, `Popover`, `Select`, `TextInput`, `EmptyState` (Task 3–4); `formatSize`, `formatDurationPadded` (Task 2).
- Produces:
  - `MemberColorsProvider({ members: { accountId: string; displayName: string }[], selfAccountId: string | null, children })` and `useMemberColor(): (accountIdOrName: string | null) => string`. The hook looks up by accountId first, then by displayName, and falls back to `MEMBER_PALETTE[0]`. Without a provider it returns `MEMBER_PALETTE[0]` for everything.
  - New `ProjectFrameTableProps` fields:
    - `activeKey?: string | null` — the row whose side panel is open; it gets the active background;
    - `groupRowExtra?: ReactNode` — rendered left of "Columns ⚙" (Library puts Export for WBPP there).
  - `export function tableMinWidth(columns: ColumnDef<FrameVM>[]): number` = 32 + 220 (Frame minimum) + the sum of the other columns' widths.

- [ ] **Step 1: Write the failing tests**

Add to `ProjectFrameTable.test.tsx` (reuse the file's existing `renderTable`/fixture helpers; `rows(n)` = n `FrameVM` rows):

```tsx
it('lays out with a fixed colgroup from the column definitions, independent of the rendered rows', () => {
  const { container } = renderTable({ tableId: 'library', rows: rows(400) });
  const table = container.querySelector('table')!;
  expect(table.className).toContain('table-fixed');
  const cols = [...container.querySelectorAll('col')].map((c) => (c as HTMLElement).style.width);
  expect(cols[0]).toBe('32px');
  expect(cols[1]).toBe(''); // Frame takes the rest
  expect(cols).toContain('108px'); // Publisher
  const scroller = container.querySelector('[data-testid="frame-table-scroll"]')!;
  fireEvent.scroll(scroller, { target: { scrollTop: 5000 } });
  expect([...container.querySelectorAll('col')].map((c) => (c as HTMLElement).style.width)).toEqual(cols);
});

it('sets the table min-width so Frame never drops below 220px (review focus 3)', () => {
  const { container } = renderTable({ tableId: 'library', rows: rows(3) });
  const table = container.querySelector('table') as HTMLElement;
  const expected = 32 + 220 + [108, 98, 70, 122, 82, 66, 60, 88, 168, 78].reduce((a, b) => a + b, 0);
  expect(table.style.minWidth).toBe(`${expected}px`);
});

it('right-aligns numeric headers and truncates cells instead of wrapping (review focus 1)', () => {
  const long = rows(1).map((r) => ({ ...r, fileName: `${'Light_M31_Ha_300s_'.repeat(4)}0001.fits` }));
  renderTable({ tableId: 'library', rows: long });
  expect(screen.getByRole('columnheader', { name: /Size/ }).className).toContain('text-right');
  const cell = screen.getByText(/0001\.fits/).closest('td')!;
  expect(cell.className).toContain('whitespace-nowrap');
  expect(cell.className).toContain('text-ellipsis');
});

it('marks the active row', () => {
  const r = rows(2);
  renderTable({ tableId: 'library', rows: r, activeKey: r[1].key });
  expect(screen.getByText(r[1].fileName).closest('tr')!.className).toContain('bg-accent/[0.16]');
});

it('renders groupRowExtra next to Columns', () => {
  renderTable({ tableId: 'library', rows: rows(2), groupRowExtra: <button>Export for WBPP</button> });
  expect(screen.getByRole('button', { name: 'Export for WBPP' })).toBeInTheDocument();
});
```

If `renderTable` does not forward extra props, extend it to spread `Partial<ProjectFrameTableProps>` over its defaults.

Add to `frames.test.tsx`:

```tsx
it('formats cells like the mockup: "180 s", decimal sizes, padded group durations', () => {
  const vm = fromLibrary(lib({ exptimeSec: 180, byteSize: 121_920_698 }), new Map());
  render(<>{COLUMNS.exp.cell(vm)}|{COLUMNS.size.cell(vm)}|{COLUMNS.exp.renderAggregate!([vm, vm, vm])}</>);
  expect(screen.getByText(/180 s/)).toBeInTheDocument();
  expect(screen.getByText(/122 MB/)).toBeInTheDocument();
  expect(screen.getByText(/9m/)).toBeInTheDocument();
});

it('status and disk cells use Chip tones', () => {
  const vm = fromOwn(own({ segment: 'published', pubState: 'pending', localState: 'own_missing', holdersTotal: 1 }));
  render(<>{COLUMNS.status.cell(vm)}{COLUMNS.disk.cell(vm)}</>);
  expect(screen.getByText('pending').className).toContain('bg-warning-muted');
  expect(screen.getByText('missing').className).toContain('bg-error-muted');
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `npx vitest run src/components/collab/project/table/ProjectFrameTable.test.tsx src/components/collab/project/frames.test.tsx`
Expected: FAIL — there is no `table-fixed`, no `<col>`, and cells read "180s" and binary sizes.

- [ ] **Step 3: `MemberColors.tsx`**

```tsx
import { createContext, useCallback, useContext, useMemo, type ReactNode } from 'react';
import { MEMBER_PALETTE, memberColor } from './memberColors';

type Lookup = (accountIdOrName: string | null) => string;
const Ctx = createContext<Lookup>(() => MEMBER_PALETTE[0]);

/** Spec §4.4 — one member → colour lookup for the whole project page. */
export function MemberColorsProvider({ members, selfAccountId, children }: {
  members: { accountId: string; displayName: string }[];
  selfAccountId: string | null;
  children: ReactNode;
}) {
  const byName = useMemo(() => new Map(members.map((m) => [m.displayName, m.accountId])), [members]);
  const lookup = useCallback<Lookup>(
    (key) => {
      if (!key) return MEMBER_PALETTE[0];
      const accountId = members.some((m) => m.accountId === key) ? key : byName.get(key);
      return accountId ? memberColor(accountId, members, selfAccountId) : MEMBER_PALETTE[0];
    },
    [members, byName, selfAccountId],
  );
  return <Ctx.Provider value={lookup}>{children}</Ctx.Provider>;
}

export function useMemberColor(): Lookup {
  return useContext(Ctx);
}
```

- [ ] **Step 4: Cells and group labels in `frames.tsx`**

- Delete the local `chip()` / `Tone` / `filterDot()` helpers. Map the old tones to `Chip` tones: `success→ok`, `warning→warn`, `error→err`, `muted→mute`. Replace every `<span className={chip(t)}>x</span>` with `<Chip tone={…}>x</Chip>`, and every `filterDot(f)` with `<FilterDot filter={f} />`.
- `statusTone` returns `ChipTone`: `published→'ok'`, `rejected→'err'`, `excluded→'mute'`, default `'warn'`.
- `dash()` → `<span className="text-content-ghost">—</span>`.
- `name.cell` → `<span className="font-mono text-[12px]">…</span>`. Keep the excluded chip (`<Chip tone="mute" className="ml-1.5">excluded</Chip>`).
- `publisher.cell` → `<PublisherName accountId={v.publisherAccountId} name={v.publisher} />`, defined in this file:

```tsx
function PublisherName({ accountId, name }: { accountId: string | null; name: string | null }) {
  const colorOf = useMemberColor();
  if (!name) return <span className="text-content-ghost">—</span>;
  return <span className="inline-flex items-center gap-[5px]"><MemberDot color={colorOf(accountId ?? name)} />{name}</span>;
}
```

  `publisher.renderAggregate`: one publisher → `<PublisherName accountId={rows[0].publisherAccountId} name={rows[0].publisher} />`, otherwise `` `${n} members` ``.
- `exp.cell` → `` num(`${v.exptimeSec} s`) ``; `exp.renderAggregate` → `num(formatDurationPadded(sum(...)))`.
- `size.cell` / `size.renderAggregate` → `formatSize`.
- `disk.cell` "yes" → `<span className="text-content-faint">yes</span>`. `publishedAt` / `submitted` → `text-content-faint`.
- `device.cell`:
  - have → `<span className="inline-flex items-center gap-[5px] text-success"><StatusDot state="online" />have</span>`;
  - queued → `<span className="text-content-faint">○ queued</span>`;
  - downloading → `<span className="inline-flex w-full items-center gap-1.5"><ProgressBar percent={v.progress ?? 0} className="w-[60px]" /><span className="text-[11px] text-accent">{v.progress ?? 0}%</span></span>`;
  - missing → `<Chip tone="err" title={v.missingWhy ?? undefined}>missing</Chip>` then `<span className="ml-1.5 text-[11px] text-content-faint">{v.missingWhy}</span>`;
  - notKept / notReplicated → `text-content-faint`;
  - needsChoice / changed → `text-warning` (unchanged).
- `device.renderAggregate` →

```tsx
<span className="inline-flex w-full items-center gap-1.5">
  <Bar className="w-[80px]" segments={[
    { value: counts.have, className: 'bg-success', title: `${counts.have} have` },
    { value: counts.downloading, className: 'bg-accent', title: `${counts.downloading} downloading` },
    { value: counts.queued, className: 'bg-accent-muted', title: `${counts.queued} queued` },
    { value: counts.missing, className: 'bg-error', title: `${counts.missing} missing` },
    { value: counts.notKept, className: 'bg-border', title: `${counts.notKept} not kept` },
  ]} total={rows.length} />
  <span className="text-[11px]">{text}</span>
</span>
```

  The `text` logic stays: "N missing" in `text-error`, else "N to go" in `text-accent`, else `have/total` in `text-content-faint`.
- `GROUPS.publisher.renderLabel` → `<PublisherName accountId={null} name={k || null} />` (the lookup falls back from name). `GROUPS.filter.renderLabel` → `<span className="inline-flex items-center"><FilterDot filter={k} />{k}</span>`.

- [ ] **Step 5: The table markup in `ProjectFrameTable.tsx`**

Replace the three class constants and the JSX; the handlers, memos and effects stay. The structure, top to bottom:

```tsx
<div className="flex flex-col text-[13px] leading-[1.4]">
  {/* Filter row — mockup .toolbar */}
  <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1.5 py-2">
    <span className="-mr-1 text-[11.5px] text-content-faint">Filter</span>
    {presentFilters.map((f) => (
      <FilterChip key={f} filter={f} count={counts.filters.get(f) ?? 0} on={facets.filters.includes(f)} onClick={() => toggleFilterChip(f)} />
    ))}
    <span aria-hidden className="h-[18px] w-px bg-line" />
    {/* each existing <select> becomes <Select …> with the same props; the search <input> becomes <TextInput className="w-[150px]" …> */}
    <Button variant="link" size="sm" disabled={active === 0} onClick={clearFacets}>Clear filters</Button>
  </div>
  {/* Group row */}
  <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1.5 pb-2">
    <span className="-mr-1 text-[11.5px] text-content-faint">Group by</span>
    {/* Select, then "▸" as <span className="text-[11.5px] text-content-faint">▸</span>, then Select */}
    <Button variant="link" disabled={levelDefs.length === 0} onClick={() => setExpanded(allGroupIds(tree))}>Expand all</Button>
    <Button variant="link" disabled={levelDefs.length === 0} onClick={() => setExpanded([])}>Collapse all</Button>
    <span className="flex-1" />
    {props.groupRowExtra}
    <div className="relative">
      <Button onClick={() => setColumnsOpen((o) => !o)}>Columns ⚙</Button>
      <Popover open={columnsOpen} onClose={() => setColumnsOpen(false)}>
        {config.columns.filter((id) => id !== 'name').map((id) => (
          <label key={id} className="flex cursor-pointer items-center gap-2 whitespace-nowrap text-[12px] text-content-muted">
            <CbBox checked={visibleColIds.includes(id)} onChange={() => toggleColumn(id)} label={COLUMNS[id].label} />
            {COLUMNS[id].label}
          </label>
        ))}
      </Popover>
    </div>
  </div>
  {/* Totals — mockup .totals */}
  <div className="flex flex-wrap items-center gap-x-3.5 gap-y-1.5 rounded-t-md border border-b-0 border-line bg-surface-elevated px-2.5 py-[7px] text-[12px] text-content-muted">
    <span>Showing <strong className="font-semibold text-content">{filtered.length.toLocaleString('en-US')}</strong> of {rows.length.toLocaleString('en-US')} frames</span>
    <span>Σ <strong className="font-semibold text-content">{formatDurationPadded(totalExp)}</strong></span>
    <strong className="font-semibold text-content">{formatSize(totalBytes)}</strong>
    <span>FWHM x̃ <strong className="font-semibold text-content">{medFwhm === null ? '—' : `${medFwhm.toFixed(2)}″`}</strong></span>
    {/* the selection note keeps its logic, restyled text-accent 12px, "Clear" as <Button variant="link" size="sm"> */}
    <span className="flex-1" />
    {/* actions: <Button variant={a.primary ? 'primary' : 'default'} disabled={disabled} onClick=…>{a.busy && <Loader2 size={12} className="animate-spin" />}{actionLabel(…)}</Button> */}
    {toolbarExtra}
  </div>
  {/* Table box — mockup .tscroll */}
  <div ref={scrollRef} data-testid="frame-table-scroll" onScroll={…} style={{ height: fillHeight }} className="overflow-auto rounded-b-md border border-line bg-surface">
    <table className="w-full table-fixed border-separate border-spacing-0" style={{ minWidth: tableMinWidth(columns) }}>
      <colgroup>
        <col style={{ width: 32 }} />
        {columns.map((c) => <col key={c.id} style={c.id === 'name' ? undefined : { width: c.width }} />)}
      </colgroup>
      <thead>…</thead>
      <tbody>…</tbody>
    </table>
  </div>
</div>
```

- `fillHeight`: `const fillHeight = useFillHeight(scrollEl, 360)`, a hook in this file:

```ts
/** Spec §5.2 item 4 — the table box reaches the window bottom, minimum `min`px. */
function useFillHeight(el: HTMLElement | null, min: number): number | undefined {
  const [h, setH] = useState<number | undefined>(undefined);
  useLayoutEffect(() => {
    if (!el) return undefined;
    const fit = () => setH(Math.max(min, Math.floor(window.innerHeight - el.getBoundingClientRect().top - 24)));
    fit();
    window.addEventListener('resize', fit);
    return () => window.removeEventListener('resize', fit);
  }, [el, min]);
  return h;
}
```

- `tableMinWidth` (exported): `32 + 220 + columns.filter((c) => c.id !== 'name').reduce((a, c) => a + c.width, 0)`.
- Header cell:

```tsx
<th
  key={col.id}
  scope="col"
  onClick={() => onSortClick(col.id)}
  className={`sticky top-0 z-[2] h-[30px] cursor-pointer select-none overflow-hidden text-ellipsis whitespace-nowrap border-b border-border bg-table-head px-2 text-[11.5px] font-medium hover:text-content ${col.numeric ? 'text-right' : 'text-left'} ${sort.col === col.id ? 'text-accent' : 'text-content-faint'}`}
>
  {col.label}{sort.col === col.id ? (sort.dir === 1 ? ' ↑' : ' ↓') : ''}
</th>
```

  The checkbox header cell uses the same classes, without sort, and holds `<CbBox state={headerState} … label="Select all shown" />`.
- Shared cell classes: `const TD = 'h-7 overflow-hidden text-ellipsis whitespace-nowrap border-b border-line-soft px-2'`.
- Frame row:
  - `<tr className={`cursor-pointer ${active ? 'bg-accent/[0.16]' : selected ? 'bg-accent/[0.08]' : ''} hover:bg-[rgba(67,76,94,0.55)]`}>`;
  - cells `${TD} text-content-secondary ${col.numeric ? 'text-right' : ''}`;
  - the first cell's `paddingLeft` = `8 + depth * 16`.
- Group row:
  - `<tr className={`cursor-pointer ${node.depth === 0 ? 'bg-table-group' : 'bg-table-group-l1'} hover:bg-table-group-hover`}>`;
  - the label cell: `${TD} font-medium text-content`, with `<span className="inline-block w-3.5 text-content-faint">{isOpen ? '▾' : '▸'}</span>`, then the label, then `<span className="ml-1.5 text-[11px] font-normal text-content-faint">{node.rows.length} fr</span>`;
  - aggregate cells: `${TD} font-normal text-content-muted ${col.numeric ? 'text-right' : ''}`.
- `CbBox` (in this file) replaces `HeaderCheckbox` and the raw inputs. It is a real `<input type="checkbox" className="peer sr-only" aria-label={label}>` inside a `<label>` with the mockup `.cb` box:
  - box: `inline-block h-[13px] w-[13px] rounded-[3px] border border-border bg-surface align-[-2px]`;
  - checked: `border-accent bg-accent` plus the tick svg;
  - `some`: `border-accent-muted bg-accent-muted` plus a 7×2 `bg-content` bar;
  - it sets `indeterminate` on the input via a ref, as `HeaderCheckbox` did.
- Empty table (no rows at all): `<EmptyState>{emptyText}</EmptyState>`. "No frames match these filters" row: `text-[12.5px] text-content-faint`, with "Clear filters" as `<Button variant="link">`.

- [ ] **Step 6: Run the tests and the typecheck**

Run: `npx vitest run src/components/collab/project && npx tsc --noEmit -p .`
Expected: PASS. Existing assertions on old class names or on "180s" / "116.3 MB" text get updated to the new formats in the same step; their behaviour assertions stay.

- [ ] **Step 7: Harness check**

Library tab in both pages at 1440 × 900. Run `measure.js` on each and compare:
- every `th` `x` and `w`, and the header height (30);
- row pitch (29);
- the cell font (13px, `rgb(229, 233, 240)`);
- the group row background (`rgb(50, 57, 70)`).

Scroll the app table by 5,000 px and re-measure the `th` — no column moves. Record the diff (expect ≤ 1 px everywhere except D2, the table height).

- [ ] **Step 8: Commit**

```bash
git add src/components/collab/project
git commit -m "feat(collab): fixed-layout frame table on the mockup's geometry, member colours, mockup cell formats"
```

---

### Task 7: Page shell — header, meta line, tabs, panels, member colours

**Files:**
- Modify: `src/pages/ProjectDetail.tsx`
- Modify: `src/pages/Projects.tsx` (header only)
- Modify: `src/components/collab/CollabLiveStatus.tsx` (add the pill variant)
- Create: `src/components/collab/project/MetaLine.tsx`
- Delete: `src/components/collab/AutoReplicateBar.tsx` and its test (its toggle moves into `MetaLine`); `AutoPublishSwitch` stays for the frame set page, but the project page no longer renders it (`MetaLine` owns it here)
- Test: `src/pages/ProjectDetail.test.tsx`, `src/components/collab/CollabLiveStatus.test.tsx`, `src/components/collab/project/MetaLine.test.tsx`

**Interfaces:**
- Consumes: `Pill`, `StatusDot`, `Chip`, `Button`, `PanelLayout` (Tasks 3–4), `MemberColorsProvider` (Task 6).
- Produces:
  - `CollabLiveStatus({ compact?: boolean, variant?: 'default' | 'pill', syncedAt?: string | null })`. `variant="pill"` renders ONE `<Pill as="button">`: a `StatusDot` (live → `live`, storage warning → `warn`, unreachable / storage unavailable → `error`, others → `offline`), then the label. When live and storage is available the label is `Live · synced N s ago`, with N from `syncedAt` ticking every second; `s ago` becomes `m ago` from 60 s. Otherwise the label is `liveStatusLabel(...)`. Clicking runs `collab_sync_now`; while syncing the label is `Syncing…`; the pill is disabled when `off`. The owner link keeps rendering as `<Button variant="link" size="sm">` after the pill. `PERIODIC_ONLY` goes to the pill `title`.
  - `MetaLine({ card: ProjectCard, canReceive: boolean, onChanged(): void, onSwitchHere(): void, switchBusy: boolean })`. It renders the §8 row 2 line and owns the two toggle writes (`set_project_auto_publish`, `set_project_auto_replicate`). A failure goes to `console.error` + `notify({ title: 'Could not change auto-publish' | 'Could not change auto-replicate', detail, kind: 'project', tone: 'warning', hasErrors: true })`.
  - `ProjectDetail` resolves `selfAccountId` (see Step 4) and wraps the page body in `MemberColorsProvider`. The frame-table tabs render inside `PanelLayout`, with the frame panel (Task 8) as `panel`.

- [ ] **Step 1: Write the failing tests**

`CollabLiveStatus.test.tsx` (add; mock `api` as the file already does):

```tsx
it.each([
  [{ state: 'connecting' }, 'Connecting…', 'offline'],
  [{ state: 'reconnecting', retryInSecs: 12 }, 'Reconnecting in 12 s', 'offline'],
  [{ state: 'unreachable' }, 'Hub unreachable — retrying', 'error'],
  [{ state: 'signedOut' }, 'Signed out', 'offline'],
  [{ state: 'live', storage: 'unavailable', storageReason: 'path_missing' }, 'Online · storage unavailable (the Collaboration folder is missing)', 'error'],
])('pill variant shows %o as one pill (review focus 5)', async (patch, label) => {
  mockStatus({ state: 'live', retryInSecs: null, since: NOW, storage: 'available', storageReason: null, watcherDegraded: false, networkVolume: false, ...patch });
  render(<CollabLiveStatus variant="pill" syncedAt={NOW} />);
  const pill = await screen.findByRole('button', { name: new RegExp(label.replace(/[()]/g, '\\$&')) });
  expect(pill.className).toContain('rounded-full');
});

it('pill variant reads "Live · synced N s ago" and runs Sync on click', async () => {
  vi.useFakeTimers({ now: new Date('2026-09-29T10:00:04Z') });
  mockStatus({ state: 'live', retryInSecs: null, since: NOW, storage: 'available', storageReason: null, watcherDegraded: false, networkVolume: false });
  render(<CollabLiveStatus variant="pill" syncedAt="2026-09-29T10:00:00Z" />);
  const pill = await screen.findByRole('button', { name: 'Live · synced 4 s ago' });
  fireEvent.click(pill);
  expect(api.invoke).toHaveBeenCalledWith('collab_sync_now');
  vi.useRealTimers();
});

it('pill is disabled when collaboration is off', async () => {
  mockStatus({ state: 'off', retryInSecs: null, since: NOW, storage: 'notSet', storageReason: null, watcherDegraded: false, networkVolume: false });
  render(<CollabLiveStatus variant="pill" syncedAt={NOW} />);
  expect(await screen.findByRole('button', { name: 'Collaboration is off' })).toBeDisabled();
});
```

(`mockStatus` sets `api.invoke`'s `get_collab_live_status` answer; add it next to the file's existing mocks if absent. `NOW = '2026-09-29T10:00:00Z'`.)

`MetaLine.test.tsx`:

```tsx
it('reads like the mockup and flips auto-publish on click', async () => {
  render(<MetaLine card={card({ publishingHere: true, autoPublish: true, autoReplicate: true })} canReceive onChanged={onChanged} onSwitchHere={() => {}} switchBusy={false} />);
  expect(screen.getByText('Publishing from this device')).toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: 'Auto-publish on' }));
  await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('set_project_auto_publish', { projectId: 'p', enabled: false }));
  expect(onChanged).toHaveBeenCalled();
});

it('names the other publishing device and offers to switch', () => {
  render(<MetaLine card={card({ publishingHere: false, publishingDevice: { deviceId: 'd', name: 'kostya-obs' } })} canReceive onChanged={() => {}} onSwitchHere={onSwitch} switchBusy={false} />);
  expect(screen.getByText('Publishing from kostya-obs')).toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: 'Publish from here' }));
  expect(onSwitch).toHaveBeenCalled();
});

it('hides auto-replicate for a send-only member', () => {
  render(<MetaLine card={card({})} canReceive={false} onChanged={() => {}} onSwitchHere={() => {}} switchBusy={false} />);
  expect(screen.queryByRole('button', { name: /Auto-replicate/ })).toBeNull();
});

it('a failed toggle notifies and never flips the label', async () => {
  vi.mocked(api.invoke).mockRejectedValueOnce(new Error('db locked'));
  renderWithNotifications(<MetaLine card={card({ autoReplicate: true })} canReceive onChanged={onChanged} onSwitchHere={() => {}} switchBusy={false} />);
  fireEvent.click(screen.getByRole('button', { name: 'Auto-replicate on' }));
  expect(await screen.findByText('Could not change auto-replicate')).toBeInTheDocument();
  expect(onChanged).not.toHaveBeenCalled();
});
```

(`card(patch)` builds a `ProjectCard` with `projectId: 'p'`. `renderWithNotifications` wraps in `NotificationProvider` + `ToastStack`, as `CollabAttention.test.tsx` does.)

`ProjectDetail.test.tsx` (add):

```tsx
it('header follows the app pattern: HistoryNav, 24px bold title, muted subtitle, role chip, live pill, portal link', async () => {
  renderPage();
  const h = await screen.findByRole('heading', { level: 2, name: /M31 Deep Field 2026/ });
  expect(h.className).toContain('text-2xl');
  expect(h.className).toContain('font-bold');
  expect(screen.getByText(/M31 · r 1\.5° · 2 members/)).toBeInTheDocument();
  expect(screen.getByText('coordinator')).toBeInTheDocument();
  expect(screen.getByRole('button', { name: /Manage on portal/ })).toBeInTheDocument();
  expect(screen.queryByText('Auto-download contributions')).toBeNull();
});

it('a long project title truncates on one line (review focus 1)', async () => {
  renderPage({ title: 'M31 Deep Field 2026 — autumn campaign with the extended team and guests' });
  const h = await screen.findByRole('heading', { level: 2, name: /autumn campaign/ });
  expect(h.className).toContain('truncate');
});

it('tab counts read "136 ready" / "N to go" / pending, as pills', async () => {
  renderPage();
  expect(await screen.findByRole('tab', { name: /My frames 1 ready/ })).toBeInTheDocument();
  expect(screen.getByRole('tab', { name: /Moderation 3/ })).toBeInTheDocument();
});

it('closes the frame panel on a tab change', async () => {
  renderPage();
  fireEvent.click(await screen.findByRole('tab', { name: /Library/ }));
  fireEvent.click(await screen.findByText('light_001.fits'));
  expect(screen.getByRole('complementary', { name: 'Frame details' })).toBeInTheDocument();
  fireEvent.click(screen.getByRole('tab', { name: /Members/ }));
  expect(screen.queryByRole('complementary', { name: 'Frame details' })).toBeNull();
});
```

Adapt `renderPage()` and its fixture to the file's existing helpers. The counts in the names follow that fixture: one ready frame, three pending, two members.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `npx vitest run src/pages/ProjectDetail.test.tsx src/components/collab/CollabLiveStatus.test.tsx src/components/collab/project/MetaLine.test.tsx`
Expected: FAIL.

- [ ] **Step 3: `CollabLiveStatus` pill variant and `MetaLine`**

- `CollabLiveStatus`: add the `variant` / `syncedAt` props. Keep all state, effects and `syncNow`. In the `variant === 'pill'` branch render:

```tsx
<span className="inline-flex items-center gap-2">
  <Pill
    as="button"
    onClick={() => void syncNow()}
    disabled={syncing || off}
    title={[periodicOnly ? PERIODIC_ONLY : null, off ? 'Collaboration is off — sign in and set a Collaboration folder first' : 'Sync now: reconnect to the hub and check every project'].filter(Boolean).join(' · ')}
    dot={<StatusDot state={dotState(status)} />}
  >
    {syncing ? 'Syncing…' : pillLabel(status, elapsed, syncedAt ?? null, now)}
  </Pill>
  {showOwnerLink && <Button variant="link" size="sm" onClick={requestOpen}>{pending === 'replace' ? 'Replace a device…' : 'Resolve the folder owner…'}</Button>}
</span>
```

  Here `now` is a `Date.now()` state ticked by a 1 s interval while `variant === 'pill'`. The two pure helpers are exported for tests:

```ts
export function dotState(s: Status): 'live' | 'warn' | 'error' | 'offline' {
  if (s.state === 'live') return s.storage === 'unavailable' ? 'error' : s.storage === 'readOnly' ? 'warn' : 'live';
  return s.state === 'unreachable' ? 'error' : 'offline';
}
export function pillLabel(s: Status, elapsed: number, syncedAt: string | null, now: number): string {
  if (s.state === 'live' && s.storage === 'available' && syncedAt) {
    const secs = Math.max(0, Math.round((now - Date.parse(syncedAt)) / 1000));
    return `Live · synced ${secs < 60 ? `${secs} s` : `${Math.round(secs / 60)} m`} ago`;
  }
  return liveStatusLabel(s, elapsed);
}
```

- `MetaLine.tsx`:

```tsx
export default function MetaLine({ card, canReceive, onChanged, onSwitchHere, switchBusy }: {
  card: ProjectCard; canReceive: boolean; onChanged: () => void; onSwitchHere: () => void; switchBusy: boolean;
}) {
  const { notify } = useNotifications();
  const [busy, setBusy] = useState<'publish' | 'replicate' | null>(null);
  const flip = async (which: 'publish' | 'replicate', enabled: boolean) => {
    setBusy(which);
    try {
      await api.invoke(which === 'publish' ? 'set_project_auto_publish' : 'set_project_auto_replicate', { projectId: card.projectId, enabled });
      onChanged();
    } catch (err) {
      console.error(`[projects] set auto-${which} failed:`, err);
      notify({ title: `Could not change auto-${which === 'publish' ? 'publish' : 'replicate'}`, detail: err instanceof Error ? err.message : String(err), kind: 'project', tone: 'warning', hasErrors: true });
    } finally {
      setBusy(null);
    }
  };
  const toggle = 'text-content-faint underline-offset-2 hover:text-accent hover:underline disabled:cursor-not-allowed disabled:opacity-45';
  return (
    <div className="mt-1 flex flex-wrap items-center gap-1.5 text-[12px] leading-[1.4] text-content-faint">
      <Monitor size={11} aria-hidden />
      <span>{card.publishingHere ? 'Publishing from this device' : card.publishingDevice ? `Publishing from ${deviceLabel(card.publishingDevice.name)}` : 'Nobody is publishing to this project yet'}</span>
      {!card.publishingHere && card.publishingDevice && (
        <Button variant="link" size="sm" onClick={onSwitchHere} disabled={switchBusy}>Publish from here</Button>
      )}
      <span aria-hidden>·</span>
      <button type="button" className={toggle} disabled={busy !== null} onClick={() => void flip('publish', !card.autoPublish)}
        title="Passing frames publish automatically as scans, analysis and links change.">
        Auto-publish {card.autoPublish ? 'on' : 'off'}
      </button>
      {canReceive && (
        <>
          <span aria-hidden>·</span>
          <button type="button" className={toggle} disabled={busy !== null} onClick={() => void flip('replicate', !card.autoReplicate)}
            title="New approved contributions download automatically. Every member who has a frame helps distribute it.">
            Auto-replicate {card.autoReplicate ? 'on' : 'off'}
          </button>
        </>
      )}
    </div>
  );
}
```

- [ ] **Step 4: `ProjectDetail` shell**

- **Self account.** Add

```ts
const [deviceId, setDeviceId] = useState<string | null>(null);
useEffect(() => {
  api.invoke<AccountStatus>('account_status')
    .then((s) => setDeviceId(s.deviceId))
    .catch((err) => console.error('[projects] account_status failed:', err));
}, []);
const selfAccountId = useMemo(() => resolveSelfAccount(frames, members, deviceId), [frames, members, deviceId]);
```

  with the exported pure helper

```ts
/** This account: a published own frame's publisher, else the member whose devices include this device. */
export function resolveSelfAccount(frames: ProjectFrameView[] | null, members: MemberSummary[] | null, deviceId: string | null): string | null {
  const own = frames?.find((f) => f.own)?.publisherAccountId;
  if (own) return own;
  if (!deviceId || !members) return null;
  return members.find((m) => m.devices.some((d) => d.device === deviceId))?.accountId ?? null;
}
```

  Also load members on mount: today they load only when the Members tab mounts. Call `get_collab_member_summary` from the shell's `loadDetail` sibling, `loadMembers`, and pass the result into `MembersTab` as its data (Task 13 changes `MembersTab` to take `members` as a prop). Until Task 13, keep `onMembers` working.
- **Page root.** `<div className="space-y-0 p-6 text-[13px] leading-[1.4] [font-variant-numeric:tabular-nums]">` wrapped in `<MemberColorsProvider members={members ?? []} selfAccountId={selfAccountId}>`.
- **Header row 1:**

```tsx
<div className="flex flex-wrap items-center gap-x-3 gap-y-2">
  <HistoryNav fallback="/projects" />
  <h2 className="min-w-0 truncate text-2xl font-bold text-content">{c.title}</h2>
  <span className="text-sm font-normal text-content-muted">◎ {c.targetName} · r {c.targetRadiusDeg.toFixed(1)}° · {detail.members.length} members</span>
  {c.coordinator && <Chip tone="info">coordinator</Chip>}
  <span className="ml-auto flex items-center gap-3.5">
    <CollabLiveStatus variant="pill" syncedAt={c.fetchedAt} />
    <Button variant="link" onClick={() => void openPortal(portalPath)}>Manage on portal ↗</Button>
  </span>
</div>
<MetaLine card={c} canReceive={canReceive} onChanged={() => void loadDetail()} onSwitchHere={() => setSwitchConfirm(true)} switchBusy={publishing.switchBusy} />
```

- Remove the old publishing line, the `AutoReplicateBar` block, `publishedBytes`, `BADGE`, and the `Target`/`Monitor`/`ExternalLink` imports that fall out of use.
- **Tabs:**

```tsx
<div role="tablist" className="mt-3.5 flex gap-0.5 overflow-x-auto border-b border-border">
  {tabs.map((t) => (
    <button key={t} type="button" role="tab" aria-selected={activeTab === t} onClick={() => selectTab(t)}
      className={`-mb-px inline-flex items-center gap-1.5 whitespace-nowrap border-b-2 px-3.5 py-2 ${activeTab === t ? 'border-accent font-semibold text-content' : 'border-transparent text-content-faint hover:text-content-secondary'}`}>
      {TAB_LABEL[t]}
      {badge[t] && badge[t]!.n > 0 && (
        <span className={`rounded-full px-[5px] text-[10.5px] font-medium ${badge[t]!.warn ? 'bg-warning-muted text-warning' : 'bg-surface-hover text-content-muted'}`}>{badge[t]!.text}</span>
      )}
    </button>
  ))}
</div>
```

  with `badge` =

```ts
{
  mine: { n: readyCount, text: `${readyCount} ready` },
  library: { n: toCome, text: `${toCome} to go` },
  moderation: { n: c.pendingFrames, text: String(c.pendingFrames), warn: true },
}
```

  `selectTab(t)` = `setDrawer(null); setTab(t)`.
- **Tab body:** `<div className="pt-3.5">`. The Mine / Library / Moderation tabs render inside

```tsx
<PanelLayout panel={drawerFrame ? <FramePanel … /> : null}>…the tab…</PanelLayout>
```

  `FramePanel` arrives in Task 8. Until then, render the existing `FrameDrawer` as the panel content, so the layout works. Pass `activeKey={drawerFrame?.key ?? null}` down to each tab's `ProjectFrameTable` (add an `activeKey` prop to `MyFramesTab`, `LibraryTab` and `ModerationTab` that forwards it).
- **`Projects.tsx` header:** `<HistoryNav />` then `<h2 className="text-2xl font-bold">Projects</h2>`, the icon removed; `CollabLiveStatus compact` stays on the right.

- [ ] **Step 5: Run the tests and the typecheck**

Run: `npx vitest run src/pages src/components/collab && npx tsc --noEmit -p .`
Expected: PASS. Tests that asserted the removed `AutoReplicateBar` move to `MetaLine.test.tsx` (the same behaviours); delete `AutoReplicateBar.test.tsx` together with the component.

- [ ] **Step 6: Harness check**

Overview in both pages:
- measure the tab positions (`tabs[].x/w`), the underline (2 px, `rgb(136, 192, 208)`), the count pills (10.5px);
- the meta line (12px, faint) and the pill (11.5px, dot with a glow ring);
- the title (24px bold, D1).

Scenario `offline`: the pill reads "Reconnecting in 12 s" with the same height. Scenario `long`: the title truncates, the pill stays on row 1.

- [ ] **Step 7: Commit**

```bash
git add -A src/pages src/components/collab
git commit -m "feat(collab): project header on the app pattern, live pill with sync, meta-line toggles, pill tab counts, docked panel layout"
```

---

### Task 8: Frame panel (replaces `FrameDrawer`)

**Files:**
- Rename: `src/components/collab/project/FrameDrawer.tsx` → `FramePanel.tsx`, `FrameDrawer.test.tsx` → `FramePanel.test.tsx`
- Modify: `src/pages/ProjectDetail.tsx` (import and usage)

**Interfaces:**
- Consumes: `SidePanel`, `KV`, `Chip`, `FilterDot`, `MemberDot`, `StatusDot`, `Button` (Tasks 3–4); `useMemberColor` (Task 6); `formatSize` (Task 2).
- Produces: `FramePanel({ projectId, frame: FrameVM, canModerate, onClose, onChanged, thresholdsVersion: number | null })`, the same props as `FrameDrawer` plus `thresholdsVersion`. It renders a `SidePanel` with `label="Frame details"`. The holders effect and listener, restore, copy-path and `ExcludeDialog` logic stay exactly as they are.

- [ ] **Step 1: Write the failing tests** (rename the test file, then add):

```tsx
it('title block: mono file name and status chips', () => {
  renderPanel(baseFrame({ own: null, lib: lib(), pubState: 'published', device: 'have' }));
  expect(screen.getByText('light_001.fits').className).toContain('font-mono');
  expect(screen.getByText('published').className).toContain('bg-success-muted');
  expect(screen.getByText('have')).toBeInTheDocument();
});

it('Frame section is a KV with the mockup labels', () => {
  renderPanel(baseFrame({ night: '2026-08-31', exptimeSec: 180, byteSize: 121_920_698, contentVersion: 2 }));
  for (const label of ['Publisher', 'Night', 'Filter', 'Camera', 'Exposure', 'Size', 'Version']) {
    expect(screen.getByText(label).tagName).toBe('DT');
  }
  expect(screen.getByText('2026-08-31 · Mon')).toBeInTheDocument();
  expect(screen.getByText('180 s')).toBeInTheDocument();
  expect(screen.getByText('122 MB')).toBeInTheDocument();
  expect(screen.getByText('v2 · v1 superseded')).toBeInTheDocument();
});

it('Metrics: SNR to one decimal, no Zero point row', () => {
  renderPanel(baseFrame({ snr: 23.054622650146484 }));
  expect(screen.getByText('23.1')).toBeInTheDocument();
  expect(screen.queryByText('Zero point')).toBeNull();
});

it('Gate is a 4-column grid with the thresholds version and precondition rows', () => {
  renderPanel(baseFrame({ own: own({ failures: [{ kind: 'solve', text: 'No WCS' }], rules: [{ metricKey: 'fwhm', label: 'FWHM', value: '3.42″', needs: '≤ 3.00″', pass: false }] }) }), { thresholdsVersion: 3 });
  expect(screen.getByRole('heading', { name: /Gate thresholds v3/ })).toBeInTheDocument();
  expect(screen.getByText('Plate-solved')).toBeInTheDocument();
  expect(screen.getByText('3.42″')).toBeInTheDocument();
  expect(screen.getAllByText('✕').length).toBeGreaterThanOrEqual(2);
});

it('Who holds it: "N online of M", publisher chip, device on the right', async () => {
  holdersAnswer([{ memberName: 'Andrei', deviceName: 'andrei-pc', device: 'd1', deviceShort: 'd1', online: true, isPublisher: true, contentVersion: 1 }, { memberName: 'Olga', deviceName: 'olga-home', device: 'd2', deviceShort: 'd2', online: false, isPublisher: false, contentVersion: 1 }]);
  renderPanel(baseFrame({ lib: lib() }));
  expect(await screen.findByRole('heading', { name: /Who holds it 1 online of 2/ })).toBeInTheDocument();
  expect(screen.getByText('publisher')).toBeInTheDocument();
  expect(screen.getByText('andrei-pc').className).toContain('ml-auto');
});
```

- [ ] **Step 2: Run to verify they fail**

Run: `npx vitest run src/components/collab/project/FramePanel.test.tsx`
Expected: FAIL.

- [ ] **Step 3: Implement the render** (the logic stays; only the JSX below replaces the `<aside>` tree):

```tsx
const colorOf = useMemberColor();
const H3 = 'mb-1.5 mt-4 text-[13px] font-semibold text-content';
const status = frame.own
  ? frame.own.segment === 'held' ? <Chip tone="warn">held back</Chip>
    : frame.own.segment === 'ready' ? <Chip tone="info">ready</Chip>
    : <Chip tone={statusTone(effectiveStatus(frame))}>{effectiveStatus(frame)}</Chip>
  : <><Chip tone={statusTone(effectiveStatus(frame))}>{effectiveStatus(frame)}</Chip> {COLUMNS.device.cell(frame)}</>;
const preconditions: [string, boolean][] = frame.own ? [
  ['Plate-solved', !frame.own.failures.some((f) => f.kind === 'solve')],
  ['Analyzed', !frame.own.failures.some((f) => f.kind === 'analyze')],
  ['Filter mapped', frame.filterMapped],
] : [];

return (
  <SidePanel
    label="Frame details"
    onClose={onClose}
    title={<>
      <div className="break-all font-mono text-[12.5px] text-content">{frame.fileName}</div>
      <div className="mt-1.5 flex flex-wrap items-center gap-1.5">{status}</div>
    </>}
  >
    <h3 className={H3}>Frame</h3>
    <KV items={[
      ...(frame.publisher ? [['Publisher', <span className="inline-flex items-center gap-[5px]"><MemberDot color={colorOf(frame.publisherAccountId ?? frame.publisher)} />{frame.publisher}</span>] as [ReactNode, ReactNode]] : []),
      ...(frame.night ? [['Night', nightLabel(frame.night)] as [ReactNode, ReactNode]] : []),
      ['Filter', <span className="inline-flex items-center"><FilterDot filter={frame.filter} />{frame.filter}{!frame.filterMapped && <span className="ml-1 text-content-faint">(unmapped)</span>}</span>],
      ['Camera', frame.camera === '' ? 'Unknown camera' : frame.camera],
      ...(frame.exptimeSec !== null ? [['Exposure', `${frame.exptimeSec} s`] as [ReactNode, ReactNode]] : []),
      ...(frame.byteSize !== null ? [['Size', formatSize(frame.byteSize)] as [ReactNode, ReactNode]] : []),
      ...(frame.contentVersion !== null ? [['Version', frame.contentVersion > 1 ? `v${frame.contentVersion} · v${frame.contentVersion - 1} superseded` : `v${frame.contentVersion}`] as [ReactNode, ReactNode]] : []),
      ...(frame.publishedAt ? [['Published', formatTimestamp(frame.publishedAt)] as [ReactNode, ReactNode]] : []),
      ...(frame.own ? [['Object', <>{frame.setName ?? '—'}{frame.setId !== null && <> · <Link to={`/objects/${frame.setId}`} className="text-accent hover:underline">Open object</Link></>}</>] as [ReactNode, ReactNode]] : []),
    ]} />

    <h3 className={H3}>Metrics</h3>
    <KV items={[
      ['FWHM', frame.fwhm !== null ? `${frame.fwhm.toFixed(2)}″` : '—'],
      ['Eccentricity', frame.ecc !== null ? frame.ecc.toFixed(2) : '—'],
      ['Stars', frame.stars !== null ? String(frame.stars) : '—'],
      ['SNR', frame.snr !== null ? frame.snr.toFixed(1) : '—'],
    ]} />

    {frame.own && (<>
      <h3 className={H3}>Gate {thresholdsVersion !== null && <span className="font-normal text-content-faint">thresholds v{thresholdsVersion}</span>}</h3>
      <div className="grid grid-cols-[1fr_auto_auto_auto] items-center gap-x-3 gap-y-[3px] text-[12px]">
        <span className="text-[11px] text-content-faint">Rule</span><span className="text-[11px] text-content-faint">Value</span><span className="text-[11px] text-content-faint">Needs</span><span />
        {frame.own.rules.map((r) => (
          <Fragment key={r.metricKey}>
            <span className="text-content-secondary">{r.label}</span>
            <span className="text-content-secondary">{r.value ?? '—'}</span>
            <span className="text-content-faint">{r.needs}</span>
            <span className={r.pass === true ? 'text-success' : r.pass === false ? 'text-error' : 'text-content-faint'}>{r.pass === true ? '✓' : r.pass === false ? '✕' : '—'}</span>
          </Fragment>
        ))}
        {preconditions.map(([label, ok]) => (
          <Fragment key={label}>
            <span className="text-content-secondary">{label}</span>
            <span className="text-content-secondary">{ok ? 'yes' : 'no'}</span>
            <span className="text-content-faint">required</span>
            <span className={ok ? 'text-success' : 'text-error'}>{ok ? '✓' : '✕'}</span>
          </Fragment>
        ))}
      </div>
      {frame.own.lastError && <p className="mt-1.5 text-[12px] text-error">{frame.own.lastError}</p>}
    </>)}

    {holdersFor && (<>
      <h3 className={H3}>Who holds it {Array.isArray(holders) && <span className="font-normal text-content-faint">{holders.filter((h) => h.online).length} online of {holders.length}</span>}</h3>
      {holders === 'loading' && <EmptyState>Loading…</EmptyState>}
      {holders === 'error' && <p className="text-[12.5px] text-error">Could not load holders — see console.</p>}
      {Array.isArray(holders) && holders.length === 0 && <EmptyState>Nobody else holds it yet.</EmptyState>}
      {Array.isArray(holders) && holders.map((h, i) => (
        <div key={i} className="flex items-center gap-2 py-0.5 text-[12.5px]">
          <StatusDot state={h.online ? 'online' : 'offline'} />
          <MemberDot color={colorOf(h.memberName)} />
          <span className="text-content-secondary">{h.memberName ?? 'Unknown member'}</span>
          {h.isPublisher && <Chip tone="mute">publisher</Chip>}
          {h.contentVersion !== frame.contentVersion && <Chip tone="warn">v{h.contentVersion}</Chip>}
          <span className="ml-auto text-content-faint">{h.deviceName ?? h.deviceShort}</span>
        </div>
      ))}
    </>)}

    {(frame.own?.path || frame.lib?.receivedAt) && <h3 className={H3}>On this device</h3>}
    {frame.own?.path && (
      <div className="flex items-start gap-1.5">
        <span className="break-all font-mono text-[11.5px] text-content-faint">{frame.own.path}</span>
        <Button size="sm" aria-label="Copy path" onClick={copyPath}><Copy size={11} /></Button>
      </div>
    )}
    {frame.lib?.receivedAt && (
      <p className="text-[12px] text-content-muted">Received {formatTimestamp(frame.lib.receivedAt)} from {frame.lib.receivedFromMember ?? (frame.lib.receivedFromDevice === 'local' ? "this device's files" : (frame.lib.receivedFromDevice?.slice(0, 8) ?? 'unknown'))}</p>
    )}

    {showExclusionSection && (
      <div className="mt-4">
        {frame.excluded ? (
          <div className="rounded border border-warning/40 bg-warning-muted px-2.5 py-2 text-[12px] text-warning">
            Excluded — {frame.acceptedReason}
            {canModerate && <div className="mt-1.5"><Button onClick={() => void doRestore()} disabled={restoring}>{restoring && <Loader2 size={12} className="animate-spin" />}Restore</Button></div>}
            {restoreError && <p className="mt-1 text-error">{restoreError}</p>}
          </div>
        ) : (
          <Button variant="danger" onClick={() => setExcludeOpen(true)}>Exclude…</Button>
        )}
      </div>
    )}
    {excludeOpen && <ExcludeDialog projectId={projectId} frames={[frame]} onClose={() => setExcludeOpen(false)} onDone={() => onChanged()} />}
  </SidePanel>
);
```

`nightLabel(n)` = `` `${n} · ${WEEKDAY[new Date(`${n}T00:00:00Z`).getUTCDay()]}` ``. Export `WEEKDAY`, `statusTone` and `effectiveStatus` from `frames.tsx`. Drop the drawer's own Escape listener: `SidePanel` handles Escape and yields to an open dialog.

- [ ] **Step 4: Wire it in `ProjectDetail`**

Replace the `FrameDrawer` usage in the `PanelLayout` panel slot with
`<FramePanel key={drawerFrame.key} projectId={id} frame={drawerFrame} canModerate={canModerate} thresholdsVersion={detail.thresholdsVersion} onClose={() => setDrawer(null)} onChanged={…same…} />`.
A second click on the active row closes it: in the tabs' `onOpen`, `setDrawer((d) => (d?.key === vm.key ? null : vm))`.

- [ ] **Step 5: Run the tests and the typecheck**

Run: `npx vitest run src/components/collab/project src/pages && npx tsc --noEmit -p .`
Expected: PASS.

- [ ] **Step 6: Harness check**

Library → click `Light_M31_OSC_180s_20260831_0003.fits` in both pages. Compare:
- the panel title font (mono 12.5px), the chip row, the section titles (13px/600) with a 16px top margin;
- the KV label column (faint 12px) and value column x, the holder row height;
- panel width 400 (D3: docked, not overlaid — the header and tabs stay visible).

Record the diff.

- [ ] **Step 7: Commit**

```bash
git add -A src/components/collab/project src/pages/ProjectDetail.tsx
git commit -m "feat(collab): frame panel on the mockup's drawer structure, docked beside the table"
```

---

### Task 9: Overview

**Files:**
- Create: `src/components/collab/project/attention.ts`, `src/components/collab/project/attention.test.ts`
- Modify: `src/components/collab/project/OverviewTab.tsx`, `OverviewTab.test.tsx`
- Modify: `src/pages/ProjectDetail.tsx` (the `openAttention` navigation and the new props)

**Interfaces:**
- Consumes: `Card`, `Chip`, `Button`, `EmptyState`, `FilterDot`, `MemberDot` (Task 3); `useMemberColor` (Task 6); `formatSize`, `formatDurationPadded`, `formatRate` (Task 2 / existing); `BLOCKER_ORDER`, `EMPTY_FACETS`, `type Facets` (`table/model.ts`).
- Produces:

```ts
export type AttentionTarget =
  | { kind: 'segment'; segment: 'ready' | 'published' | 'held'; state?: string }
  | { kind: 'tab'; tab: 'library' | 'moderation'; state?: string };
export interface AttentionItem {
  key: string;
  tone: 'warn' | 'err';
  count: number;
  title: string;
  detail: string | null;
  action: string; // button label
  target: AttentionTarget;
}
export function deriveAttention(input: {
  own: OwnFrameRow[];
  library: ProjectFrameView[];
  members: MemberSummary[];
  canModerate: boolean;
  pending: number;
  now: number;
}): AttentionItem[];
export function offlineFor(iso: string | null, now: number): string; // "5 h", "1 day", "3 days"
```

- `OverviewTab` props change: it drops `libraryToCome` and adds `library: ProjectFrameView[] | null` and `onAttention(target: AttentionTarget): void`.
- In `ProjectDetail`, `openAttention(target)` writes the table facet session key before switching: `collab.${id}.${segment}.facets` for a segment, `collab.${id}.library.facets` for Library, each set to `{ ...EMPTY_FACETS, state: target.state ?? null }`. It then sets the segment/tab.

- [ ] **Step 1: Write the failing tests** — `attention.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { deriveAttention, offlineFor } from './attention';

const NOW = Date.parse('2026-09-29T10:00:00Z');
const held = (kind: string, night = '2026-09-26', filter = 'Ha') => ({ segment: 'held', failures: [{ kind, text: kind }], night, filter, filterMapped: kind !== 'mapFilter' }) as never;
const pub = (p: Partial<Record<string, unknown>>) => ({ segment: 'published', failures: [], holdersTotal: 1, localState: 'own_held', byteSize: 1e9, ...p }) as never;

describe('deriveAttention', () => {
  it('one row per held-back cause in blocker order, with the mockup copy', () => {
    const items = deriveAttention({ own: [held('solve'), held('solve'), held('mapFilter', '2026-09-25', 'S2 6nm')], library: [], members: [], canModerate: false, pending: 0, now: NOW });
    expect(items.map((i) => i.key)).toEqual(['solve', 'mapFilter']);
    expect(items[0]).toMatchObject({ count: 2, tone: 'warn', title: '2 frames from 2026-09-26 are not plate-solved', detail: 'They cannot be published until solved.', action: 'Review', target: { kind: 'segment', segment: 'held', state: 'solve' } });
    expect(items[1]).toMatchObject({ title: '1 frame with an unmapped filter “S2 6nm”', action: 'Map' });
  });
  it('one-copy and not-on-disk rows for own published frames', () => {
    const items = deriveAttention({ own: [pub({ holdersTotal: 0, byteSize: 3.9e9 }), pub({ localState: 'own_missing' })], library: [], members: [], canModerate: false, pending: 0, now: NOW });
    expect(items.find((i) => i.key === 'single')).toMatchObject({ tone: 'err', title: '1 published frame exists in one copy only · 3.9 GB', target: { kind: 'segment', segment: 'published', state: 'single' } });
    expect(items.find((i) => i.key === 'disk')).toMatchObject({ tone: 'err', target: { kind: 'segment', segment: 'published', state: 'disk' } });
  });
  it('missing-here row names the offline publishers and how long', () => {
    const library = [{ own: false, state: 'published', localState: 'wanted', holdersOnline: 0, publisherAccountId: 'a-irina', publisher: 'Irina' }] as never;
    const members = [{ accountId: 'a-irina', displayName: 'Irina', online: false, lastSeenAt: '2026-09-28T10:00:00Z' }] as never;
    const items = deriveAttention({ own: [], library, members, canModerate: false, pending: 0, now: NOW });
    expect(items[0]).toMatchObject({ key: 'missing', title: '1 frame missing here because its holders are offline', detail: 'Irina offline 1 day.', target: { kind: 'tab', tab: 'library', state: 'missing' } });
  });
  it('approval row only for a moderator, naming first publishers', () => {
    const library = [{ own: false, state: 'pending', publisher: 'Irina' }, { own: false, state: 'pending', publisher: 'Pavel' }] as never;
    expect(deriveAttention({ own: [], library, members: [], canModerate: false, pending: 2, now: NOW })).toEqual([]);
    expect(deriveAttention({ own: [], library, members: [], canModerate: true, pending: 2, now: NOW })[0]).toMatchObject({ title: '2 frames wait for your approval', detail: 'First publications by Irina and Pavel.', action: 'Moderate' });
  });
  it('empty input → no rows (review focus 4)', () => {
    expect(deriveAttention({ own: [], library: [], members: [], canModerate: true, pending: 0, now: NOW })).toEqual([]);
  });
  it('offlineFor buckets hours and days', () => {
    expect(offlineFor('2026-09-29T05:00:00Z', NOW)).toBe('5 h');
    expect(offlineFor('2026-09-26T10:00:00Z', NOW)).toBe('3 days');
    expect(offlineFor(null, NOW)).toBe('a while');
  });
});
```

`OverviewTab.test.tsx` — add, keeping the passing behaviour tests and updating the old attention texts:

```tsx
it('renders the four cards like the mockup', async () => {
  renderOverview();
  expect(screen.getByRole('heading', { name: /Integration toward goal published, accepted frames · by member/ })).toBeInTheDocument();
  expect(screen.getByRole('heading', { name: 'My contribution' })).toBeInTheDocument();
  expect(screen.getByRole('button', { name: /136 ready to publish/ })).toBeInTheDocument();
  expect(screen.getByText(/8h 16m/)).toBeInTheDocument(); // "**8h 16m** of 40h"
  expect(screen.getByText('FWHM ≤ 3.00″')).toBeInTheDocument();
  expect(screen.getByText('Reject trailed frames')).toBeInTheDocument();
});

it('an attention row calls onAttention with its target', () => {
  const onAttention = vi.fn();
  renderOverview({ onAttention, own: [heldRow('solve')] });
  fireEvent.click(screen.getByRole('button', { name: 'Review' }));
  expect(onAttention).toHaveBeenCalledWith({ kind: 'segment', segment: 'held', state: 'solve' });
});

it('an empty project shows empty states, never NaN (review focus 4)', () => {
  renderOverview({ own: [], members: [], library: [], goals: null });
  expect(screen.getByText('No integration yet.')).toBeInTheDocument();
  expect(screen.getByText('Nothing needs your attention.')).toBeInTheDocument();
  expect(document.body.textContent).not.toMatch(/NaN/);
});
```

- [ ] **Step 2: Run to verify they fail**

Run: `npx vitest run src/components/collab/project/attention.test.ts src/components/collab/project/OverviewTab.test.tsx`
Expected: FAIL.

- [ ] **Step 3: Implement `attention.ts`**

```ts
import type { MemberSummary, OwnFrameRow, ProjectFrameView } from '../../../types/models';
import { BLOCKER_ORDER } from './table/model';
import { formatSize } from '../format';

// (the AttentionTarget / AttentionItem types from the Interfaces block)

const plural = (n: number, one: string, many: string) => (n === 1 ? one : many);
const listNames = (names: string[]) =>
  names.length <= 1 ? (names[0] ?? '') : `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`;

export function offlineFor(iso: string | null, now: number): string {
  const t = iso ? Date.parse(iso) : NaN;
  if (Number.isNaN(t)) return 'a while';
  const h = Math.max(1, Math.round((now - t) / 3_600_000));
  if (h < 24) return `${h} h`;
  const d = Math.round(h / 24);
  return `${d} ${plural(d, 'day', 'days')}`;
}

const CAUSE: Record<string, { title: (n: number, rows: OwnFrameRow[]) => string; detail: string | null; action: string }> = {
  solve: {
    title: (n, rows) => {
      const nights = new Set(rows.map((r) => r.night));
      const when = nights.size === 1 && rows[0].night ? ` from ${rows[0].night}` : '';
      return `${n} ${plural(n, 'frame', 'frames')}${when} ${plural(n, 'is', 'are')} not plate-solved`;
    },
    detail: 'They cannot be published until solved.',
    action: 'Review',
  },
  analyze: { title: (n) => `${n} ${plural(n, 'frame is', 'frames are')} not analyzed`, detail: 'The gate needs FWHM, eccentricity and stars.', action: 'Analyze' },
  linkCalibration: { title: (n) => `${n} ${plural(n, 'frame has', 'frames have')} no calibration linked`, detail: null, action: 'Review' },
  buildMasters: { title: (n) => `${n} ${plural(n, 'frame waits', 'frames wait')} for masters to be built`, detail: null, action: 'Review' },
  attest: { title: (n) => `${n} ${plural(n, 'frame is', 'frames are')} not calibrated`, detail: null, action: 'Review' },
  mapFilter: {
    title: (n, rows) => {
      const raws = [...new Set(rows.map((r) => r.filter))];
      return raws.length === 1
        ? `${n} ${plural(n, 'frame', 'frames')} with an unmapped filter “${raws[0]}”`
        : `${n} ${plural(n, 'frame', 'frames')} with unmapped filters`;
    },
    detail: 'Map it once; the gate re-checks them.',
    action: 'Map',
  },
  threshold: { title: (n) => `${n} ${plural(n, 'frame fails', 'frames fail')} the quality thresholds`, detail: null, action: 'Review' },
  uuid: { title: (n) => `${n} ${plural(n, 'frame has', 'frames have')} no frame uuid`, detail: 'Re-scan the folder.', action: 'Review' },
  outsideTarget: { title: (n) => `${n} ${plural(n, 'frame is', 'frames are')} outside the target`, detail: null, action: 'Review' },
};

export function deriveAttention({ own, library, members, canModerate, pending, now }: {
  own: OwnFrameRow[]; library: ProjectFrameView[]; members: MemberSummary[]; canModerate: boolean; pending: number; now: number;
}): AttentionItem[] {
  const out: AttentionItem[] = [];
  const heldRows = own.filter((r) => r.segment === 'held');
  for (const kind of BLOCKER_ORDER) {
    const rows = heldRows.filter((r) => r.failures[0]?.kind === kind);
    if (rows.length === 0) continue;
    const c = CAUSE[kind];
    out.push({ key: kind, tone: 'warn', count: rows.length, title: c.title(rows.length, rows), detail: c.detail, action: c.action, target: { kind: 'segment', segment: 'held', state: kind } });
  }
  const pubRows = own.filter((r) => r.segment === 'published' && r.accepted !== false);
  const single = pubRows.filter((r) => r.holdersTotal === 0 && r.localState === 'own_held');
  if (single.length > 0) {
    const bytes = single.reduce((a, r) => a + r.byteSize, 0);
    out.push({ key: 'single', tone: 'err', count: single.length, title: `${single.length} published ${plural(single.length, 'frame exists', 'frames exist')} in one copy only · ${formatSize(bytes)}`, detail: 'If that device is lost, the project loses them.', action: 'Show', target: { kind: 'segment', segment: 'published', state: 'single' } });
  }
  const disk = pubRows.filter((r) => r.localState === 'own_missing' || r.localState === 'own_changed');
  if (disk.length > 0) {
    out.push({ key: 'disk', tone: 'err', count: disk.length, title: `${disk.length} published ${plural(disk.length, 'frame is', 'frames are')} missing or changed on this device`, detail: null, action: 'Show', target: { kind: 'segment', segment: 'published', state: 'disk' } });
  }
  const missing = library.filter((f) => !f.own && f.state === 'published' && f.localState === 'wanted' && f.holdersOnline === 0);
  if (missing.length > 0) {
    const offline = [...new Set(missing.map((f) => f.publisherAccountId))]
      .map((id) => members.find((m) => m.accountId === id))
      .filter((m): m is MemberSummary => !!m && !m.online)
      .map((m) => `${m.displayName} offline ${offlineFor(m.lastSeenAt, now)}`);
    out.push({ key: 'missing', tone: 'err', count: missing.length, title: `${missing.length} ${plural(missing.length, 'frame', 'frames')} missing here because ${plural(missing.length, 'its', 'their')} holders are offline`, detail: offline.length ? `${offline.join(', ')}.` : null, action: 'Show', target: { kind: 'tab', tab: 'library', state: 'missing' } });
  }
  if (canModerate && pending > 0) {
    const pubs = [...new Set(library.filter((f) => f.state === 'pending').map((f) => f.publisher))];
    out.push({ key: 'approval', tone: 'warn', count: pending, title: `${pending} ${plural(pending, 'frame waits', 'frames wait')} for your approval`, detail: pubs.length ? `First publications by ${listNames(pubs)}.` : null, action: 'Moderate', target: { kind: 'tab', tab: 'moderation' } });
  }
  return out;
}
```

- [ ] **Step 4: Rebuild `OverviewTab`** (keep its integration maths; replace the JSX):

```tsx
<div className="grid grid-cols-[minmax(0,1.5fr)_minmax(0,1fr)] items-start gap-3.5 max-[900px]:grid-cols-1">
  <Card title="Integration toward goal" subtitle="published, accepted frames · by member">
    {filters.length === 0 ? <EmptyState>No integration yet.</EmptyState> : filters.map((f) => (
      <div key={f} className="my-[9px] grid grid-cols-[54px_1fr_150px] items-center gap-2.5">
        <span className="inline-flex items-center font-semibold text-content"><FilterDot filter={f} />{f}</span>
        <span className="relative h-3.5 rounded-[3px] bg-surface-hover">
          <span className="flex h-full overflow-hidden rounded-[3px]">
            {segs.map((m) => <i key={m.accountId} className="block h-full" style={{ width: `${(sec(m) / scale) * 100}%`, backgroundColor: colorOf(m.accountId) }} />)}
          </span>
          {goal !== null && <span aria-hidden className="absolute -bottom-[3px] -top-[3px] w-0.5 bg-content" style={{ left: `calc(${(goal / scale) * 100}% - 1px)` }} />}
        </span>
        <span className="text-right text-[12px] text-content-muted">
          <b className="font-semibold text-content">{formatDurationPadded(total)}</b>
          {goal !== null && <> of {formatDurationPadded(goal).replace(' 00m', '')} · {left! > 0 ? <span className="text-warning">{formatDurationPadded(left!)} to go</span> : <span className="text-success">goal met</span>}</>}
        </span>
      </div>
    ))}
    {members.length > 0 && (
      <div className="mt-2 flex flex-wrap gap-x-3 gap-y-1 text-[11.5px] text-content-faint">
        {members.map((m) => <span key={m.accountId} className="inline-flex items-center gap-[5px]"><MemberDot color={colorOf(m.accountId)} />{m.displayName}</span>)}
      </div>
    )}
  </Card>
  <div className="grid gap-3.5">
    <Card title="My contribution">
      <div className="grid grid-cols-3 gap-2">
        {([['ready', readyCount, 'ready to publish', 'text-accent'], ['published', publishedCount, 'published', 'text-success'], ['held', heldCount, 'held back', 'text-warning']] as const).map(([seg, n, label, tone]) => (
          <button key={seg} type="button" onClick={() => onOpenSegment(seg)} className="rounded-md border border-line px-2.5 py-2 text-left hover:border-accent">
            <span className={`block text-[20px] font-semibold ${tone}`}>{own === null ? '—' : n.toLocaleString('en-US')}</span>
            <span className="text-[11.5px] text-content-faint">{label}</span>
          </button>
        ))}
      </div>
    </Card>
    <Card title="Needs attention">
      {items.length === 0 ? <EmptyState>Nothing needs your attention.</EmptyState> : items.map((it) => (
        <div key={it.key} className="flex items-start gap-2.5 border-t border-line py-[7px] text-[12.5px] first:border-t-0">
          <Chip tone={it.tone}>{it.count.toLocaleString('en-US')}</Chip>
          <span className="flex-1 text-content-secondary">{it.title}{it.detail && <small className="block text-[11.5px] text-content-faint">{it.detail}</small>}</span>
          <Button size="sm" onClick={() => onAttention(it.target)}>{it.action}</Button>
        </div>
      ))}
    </Card>
    <Card title="Exchange now" action={<Button variant="link" onClick={() => onOpenTab('exchange')}>Open Exchange →</Button>}>
      {recv.length === 0 && send.length === 0 ? <EmptyState>Nothing is moving.</EmptyState> : (
        <div className="grid gap-1 text-[12.5px] text-content-secondary">
          {recv.length > 0 && <div>↓ <b className="font-semibold text-content">{formatRate(sumRate(recv))}</b> from {names(recv).join(', ')}</div>}
          {send.length > 0 && <div>↑ <b className="font-semibold text-content">{formatRate(sumRate(send))}</b> to {names(send).join(', ')}</div>}
        </div>
      )}
    </Card>
    <Card title="Quality thresholds" subtitle={thresholdsVersion !== null ? `v${thresholdsVersion} · set by the coordinator` : 'set by the coordinator'}>
      {thresholds.length === 0 ? <EmptyState>No quality rules set.</EmptyState> : thresholds.map((r) => <div key={r.metricKey} className="text-[12.5px] text-content-secondary">{thresholdLine(r)}</div>)}
    </Card>
  </div>
</div>
```

`thresholdLine(r)`:

```ts
function thresholdLine(r: ThresholdRuleView): string {
  const v = typeof r.value === 'number' ? r.value : null;
  if (r.op === 'reject_if' && r.metricKey === 'trailed') return 'Reject trailed frames';
  const op = r.op === 'lte' ? '≤' : r.op === 'gte' ? '≥' : r.op;
  if (r.metricKey === 'fwhm' && v !== null) return `FWHM ${op} ${v.toFixed(2)}″`;
  if (r.metricKey === 'eccentricity' && v !== null) return `Eccentricity ${op} ${v.toFixed(2)}`;
  if (r.metricKey === 'stars' && v !== null) return `Stars ${op} ${v}`;
  return `${r.metricKey} ${op} ${String(r.value)}`;
}
```

For "Exchange now", keep the tab's existing reads of the exchange context: `recv` / `send` are this project's flows (`state.projects[projectId]`). Add `sumRate(flows)` (Σ `rateBps`) and `names(flows)` = the distinct `peerLabel(state, projectId, f.device).member ?? device` values; `ExchangeTab` already has the same two helpers, so move them to `exchange/state.ts` and import them in both tabs.

The goal text follows the mockup exactly: "**8h 16m** of 40h · 31h 44m to go". The goal hours print without minutes when they are whole (`40h`, not `40h 00m`), hence the `replace(' 00m', '')`. Filters sort by `filterOrder`. Members are those with any seconds in any filter, ordered as the member list.

- [ ] **Step 5: Wire in `ProjectDetail`** — pass `library={frames}` and `onAttention={openAttention}`, drop `libraryToCome`, and implement

```ts
const [, setReadyFacets] = useSessionState<Facets>(`collab.${id}.ready.facets`, EMPTY_FACETS);
const [, setHeldFacets] = useSessionState<Facets>(`collab.${id}.held.facets`, EMPTY_FACETS);
const [, setPublishedFacets] = useSessionState<Facets>(`collab.${id}.published.facets`, EMPTY_FACETS);
const [, setLibraryFacets] = useSessionState<Facets>(`collab.${id}.library.facets`, EMPTY_FACETS);
const openAttention = (t: AttentionTarget) => {
  const facets = { ...EMPTY_FACETS, state: t.state ?? null };
  if (t.kind === 'segment') {
    ({ ready: setReadyFacets, held: setHeldFacets, published: setPublishedFacets })[t.segment](facets);
    setSegment(t.segment);
    selectTab('mine');
  } else {
    if (t.tab === 'library') setLibraryFacets(facets);
    selectTab(t.tab);
  }
};
```

Verify the key format against `ProjectFrameTable`'s `collab.${scope}.${tableId}.facets`, where `scope` = project id and `tableId` = segment. If `MyFramesTab` passes a different `tableId`, use its value.

- [ ] **Step 6: Run the tests and the typecheck**

Run: `npx vitest run src/components/collab/project src/pages && npx tsc --noEmit -p .`
Expected: PASS.

- [ ] **Step 7: Harness check**

Overview in both pages. Compare the two columns' x/w (1.5fr/1fr, gap 14), the card padding (14/16), the goal rows (54 / 1fr / 150, 14 px bars, marker), the three tiles (20 px numbers), the attention rows (chip, title 12.5, detail 11.5, button sm) and the thresholds text. Record. Scenario `empty`: every card shows its empty line.

- [ ] **Step 8: Commit**

```bash
git add -A src/components/collab/project src/pages/ProjectDetail.tsx
git commit -m "feat(collab): Overview on the mockup — goal bars, three tiles, per-cause attention rows with actions"
```

---

### Task 10: My frames

**Files:**
- Modify: `src/components/collab/project/MyFramesTab.tsx`, `MyFramesTab.test.tsx`
- Modify: `src/components/collab/LinkObjectDialog.tsx` (it gains the linked-objects list; the shell migration is in Task 15, but the list moves here so nothing is lost when the strip goes)

**Interfaces:**
- Consumes: `SegmentTiles`, `Button` (Task 3); `ProjectFrameTable` with `activeKey` (Task 6).
- Produces: unchanged `MyFramesTab` props, plus `activeKey?: string | null` (added in Task 7). `autoPublish` stays in the props and is unused here (`MetaLine` owns it); remove it together with its `ProjectDetail` pass-through in the same step. `LinkObjectDialog` gains `links: LinkedSetView[]` and `onUnlink?` only if an unlink command already exists (`grep -n "set_collab_link" src`); otherwise it lists the links read-only.

- [ ] **Step 1: Write the failing tests** (in `MyFramesTab.test.tsx`):

```tsx
it('segment tiles and the two buttons sit on one row like the mockup', () => {
  renderTab({ rows: [ready(), published(), held()] });
  expect(screen.getByRole('button', { name: /1 Ready to publish/ })).toHaveAttribute('aria-pressed', 'true');
  expect(screen.getByRole('button', { name: /1 Published/ })).toBeInTheDocument();
  expect(screen.getByRole('button', { name: /1 Held back/ })).toBeInTheDocument();
  expect(screen.getByRole('button', { name: '+ Link an object' })).toBeInTheDocument();
  expect(screen.getByRole('button', { name: 'Recalibrate and republish all' })).toBeInTheDocument();
  expect(screen.queryByText('Linked objects')).toBeNull();
  expect(screen.queryByText('Auto-publish my frames')).toBeNull();
});

it('Ready shows "Publish all N" as the primary action', () => {
  renderTab({ rows: [ready(), ready()] });
  expect(screen.getByRole('button', { name: 'Publish all 2' }).className).toContain('bg-accent');
});
```

- [ ] **Step 2: Run to verify they fail**

Run: `npx vitest run src/components/collab/project/MyFramesTab.test.tsx`
Expected: FAIL.

- [ ] **Step 3: Implement**

- Replace the segment buttons with
  `<SegmentTiles tiles={[{ value: 'ready', n: readyCount, label: 'Ready to publish', tone: 'accent' }, { value: 'published', n: publishedCount, label: 'Published', tone: 'success' }, { value: 'held', n: heldCount, label: 'Held back', tone: 'warning' }]} value={segment} onChange={onSegment} />`.
- The row: `<div className="mb-2.5 flex flex-wrap items-start gap-2">{tiles}<span className="flex-1" /><Button onClick={() => setLinkOpen(true)}>+ Link an object</Button><Button onClick={() => onRequestRepublish(null)} disabled={!canRepublish || republishBusy}>Recalibrate and republish all</Button></div>`.
- Delete the "Linked objects" strip and the `AutoPublishSwitch` render. Pass `links` into `LinkObjectDialog`, which renders them at the top of its body as rows: name, "· N lights", then a `Chip` ok "on target" or a `Chip` warn "outside the target". Under them come the suggestions.
- The publish-error and refusal boxes: `rounded border border-warning/40 bg-warning-muted px-2.5 py-2 text-[12px] text-content`; error text `text-[12.5px] text-error`.
- Every `ProjectFrameTable` here gets `activeKey={activeKey}`. Actions keep their ids and verbs; `Publish` keeps `primary: true`.

- [ ] **Step 4: Run the tests and the typecheck**

Run: `npx vitest run src/components/collab src/pages && npx tsc --noEmit -p .`
Expected: PASS.

- [ ] **Step 5: Harness check**

My frames → Ready in both pages. Compare the tiles (min 150, 18 px numbers, active border), the buttons' right edge, the table header row. Then Held back (the Why held back column at 230, chips) and Published (the Status / Holders / On disk / Published columns). Record.

- [ ] **Step 6: Full suite and commit**

Run: `npx vitest run && npx tsc --noEmit -p .` — Expected: PASS.

```bash
git add -A src/components/collab src/pages
git commit -m "feat(collab): My frames tiles and actions as in the mockup; linked objects move into the Link dialog"
```

---

### Task 11: Library

**Files:**
- Modify: `src/components/collab/project/LibraryTab.tsx`, `LibraryTab.test.tsx`
- Modify: `src/components/collab/CollabAttention.tsx`, `CollabAttention.test.tsx`

**Interfaces:**
- Consumes: `Card`, `Chip`, `Button`, `EmptyState` (Task 3); `ProjectFrameTable` `groupRowExtra` / `activeKey` (Task 6).
- Produces: `CollabAttention` renders ONE `Card` titled "Needs your attention", holding `.att`-style rows grouped under 12 px `content-faint` sub-labels (Changed files · Waiting for your choice · Not kept · Other files). It returns `null` when every list is empty. Its public props and actions are unchanged.

- [ ] **Step 1: Write the failing tests**

`LibraryTab.test.tsx`:

```tsx
it('has no "Project frames" heading and puts Export for WBPP on the group row', async () => {
  renderTab([frame({})]);
  expect(screen.queryByText('Project frames')).toBeNull();
  const exportBtn = await screen.findByRole('button', { name: /Export for WBPP/ });
  expect(exportBtn.closest('div')!.textContent).toContain('Columns');
});
```

`CollabAttention.test.tsx`: update `renders every list` to expect one card heading, "Needs your attention", with the four sub-labels. Keep every action test as is (button names are unchanged).

- [ ] **Step 2: Run to verify they fail**

Run: `npx vitest run src/components/collab/project/LibraryTab.test.tsx src/components/collab/CollabAttention.test.tsx`
Expected: FAIL.

- [ ] **Step 3: Implement**

- `LibraryTab`:
  - remove the heading row;
  - pass `groupRowExtra={<Button onClick={() => setExportOpen(true)}><FolderOutput size={12} />Export for WBPP</Button>}` to the table;
  - the unset-folder banner: `flex items-center gap-2 rounded border border-warning/40 bg-warning-muted px-2.5 py-2 text-[12px] text-content-secondary`, with "Open File Manager" as `<Button size="sm">` inside a `Link`;
  - `keepError`: `text-[12.5px] text-error`.
- `CollabAttention`:
  - replace the `Section` component with sub-blocks inside one `Card` titled "Needs your attention";
  - each list: a 12 px `content-faint` label with a note (`title` attribute), then rows `flex items-center gap-2.5 border-t border-line py-[7px] text-[12.5px] first:border-t-0` — file name in mono 12, detail faint 11.5, buttons `Button size="sm"`, danger buttons `variant="danger"`;
  - bulk buttons ("Re-fetch all", "Stop keeping all", "Keep all again") sit right of their list label;
  - "Other files" keeps its collapsed single row (it is already compact) inside the same card, restyled with the same classes.

- [ ] **Step 4: Run the tests and the typecheck**

Run: `npx vitest run src/components/collab && npx tsc --noEmit -p .`
Expected: PASS.

- [ ] **Step 5: Harness check**

Library in both pages. Compare the filter row (chips 12 px, count 10.5), the selects (26 px), the group row, the totals bar (padding 7/10, 12 px), the `th` positions and the On-this-device cells (dot + "have", the queued text, the progress bar 60 px, the missing chip + reason) and the group aggregate bar (80 px). Record.

- [ ] **Step 6: Commit**

```bash
git add -A src/components/collab
git commit -m "feat(collab): Library layout as in the mockup; attention lists in one compact card"
```

---

### Task 12: Moderation

**Files:**
- Modify: `src/components/collab/project/ModerationTab.tsx`, `ModerationTab.test.tsx`

**Interfaces:**
- Consumes: `DialogShell`, `Button`, `TextInput` (Tasks 3–4); `Checkbox` (Task 5); `ProjectFrameTable` (Task 6).
- Produces: `RejectDialog` renders through `DialogShell` (title "Reject N frame(s)"). Its props and validation (`REASON_MAX`, trimmed non-empty) are unchanged.

- [ ] **Step 1: Write the failing tests**:

```tsx
it('totals bar: Approve all (primary), Reject all, then the trust checkbox', async () => {
  renderTab();
  const approve = await screen.findByRole('button', { name: /Approve all/ });
  expect(approve.className).toContain('bg-accent');
  expect(screen.getByRole('button', { name: /Reject all/ })).toBeInTheDocument();
  expect(screen.getByRole('checkbox', { name: 'Trust these publishers' })).toBeInTheDocument();
});

it('reject opens a DialogShell dialog', async () => {
  renderTab();
  fireEvent.click(await screen.findByRole('button', { name: /Reject all/ }));
  expect(screen.getByRole('dialog', { name: /Reject/ })).toBeInTheDocument();
});

it('Excluded frames is its own 13px section title', async () => {
  renderTab({ library: [excluded()] });
  expect((await screen.findByRole('heading', { name: 'Excluded frames' })).className).toContain('text-[13px]');
});
```

- [ ] **Step 2: Run to verify they fail**

Run: `npx vitest run src/components/collab/project/ModerationTab.test.tsx`
Expected: FAIL.

- [ ] **Step 3: Implement**

- Section titles become `<h3 className="mb-1.5 mt-5 text-[13px] font-semibold text-content first:mt-0">` — "Waiting for review" and "Excluded frames".
- "This project publishes without review." is a `text-[12px] text-content-faint` line.
- The trust checkbox moves into the table's `toolbarExtra` as `<Checkbox checked={trust} onChange={setTrust} label="Trust these publishers" size="sm" />`. Approve keeps `primary: true`, Reject is a default button.
- `RejectDialog` renders
  `<DialogShell title={`Reject ${frames.length} ${frames.length === 1 ? 'frame' : 'frames'}`} onClose={onCancel} busy={busy} footer={<><Button onClick={onCancel} disabled={busy}>Cancel</Button><Button variant="dangerPrimary" disabled={!valid || busy} onClick={() => onReject(trimmed)}>{busy && <Loader2 size={12} className="animate-spin" />}Reject</Button></>}>`.
  The body is a `<textarea>` with the field classes (`rounded border border-border bg-surface px-1.5 py-[3px] text-[12px]`, 4 rows, full width), the counter `text-[11px] text-content-faint` (red when over), and the too-long / empty messages as today. Its own Escape listener goes (`DialogShell` owns Escape and `busy`).
- Result lines `text-[12.5px]`; errors `text-error`.

- [ ] **Step 4: Run the tests and the typecheck**

Run: `npx vitest run src/components/collab && npx tsc --noEmit -p .`
Expected: PASS.

- [ ] **Step 5: Harness check**

Moderation in both pages. Compare the table geometry (the SNR column is visible by default, Publisher ▸ Night grouping) and the totals bar. Open Reject all: the dialog is 440 px wide, the title is 13 px, the buttons are right-aligned. Record.

- [ ] **Step 6: Commit**

```bash
git add -A src/components/collab/project
git commit -m "feat(collab): Moderation as in the mockup; reject reason on the shared dialog shell"
```

---

### Task 13: Members and the member panel

**Files:**
- Modify: `src/components/collab/project/MembersTab.tsx`, `MembersTab.test.tsx`
- Create: `src/components/collab/project/MemberPanel.tsx`
- Modify: `src/pages/ProjectDetail.tsx` (members flow as data)

**Interfaces:**
- Consumes: `PanelLayout`, `SidePanel`, `KV`, `Chip`, `MemberDot`, `StatusDot`, `FilterDot`, `EmptyState` (Tasks 3–4); `useMemberColor` (Task 6); `formatDurationPadded`, `formatSize`; `filterOrder` (`table/model.ts`).
- Produces:
  - `MembersTab({ members: MemberSummary[] | null, error: boolean, projectId: string })` — data comes from the shell (Task 7 loads it); the tab's own fetch and `refreshToken` / `onMembers` are removed, and the shell's peers-changed throttle now reloads members directly.
  - `MemberPanel({ member: MemberSummary, onClose(): void })` — a `SidePanel` labelled "Member details".
  - `export function filterColumns(members: MemberSummary[]): string[]` — the filters with any seconds, in `filterOrder`, with `None` last.

- [ ] **Step 1: Write the failing tests**:

```tsx
it('columns in the mockup order, numeric headers right-aligned', () => {
  render(<MembersTab projectId="p" members={[m('Kostya', { L: 7200 })]} error={false} />);
  const heads = screen.getAllByRole('columnheader').map((h) => h.textContent?.trim());
  expect(heads).toEqual(['Member', 'Role', 'Devices', 'Published ↓', 'L', 'Σ', 'FWHM x̃', 'Holds', 'Last seen']);
  expect(screen.getByRole('columnheader', { name: 'Σ' }).className).toContain('text-right');
});

it('filter cells read "2h 00m" or a ghost dash; None becomes "No filter" last', () => {
  render(<MembersTab projectId="p" members={[m('A', { L: 7200, None: 600 }), m('B', {})]} error={false} />);
  expect(screen.getByRole('columnheader', { name: 'No filter' })).toBeInTheDocument();
  expect(screen.getAllByText('2h 00m')[0]).toBeInTheDocument();
  expect(screen.getAllByText('—')[0].className).toContain('text-content-ghost');
});

it('12 filters scroll horizontally inside the table box (review focus 2)', () => {
  const secs = Object.fromEntries(['L', 'R', 'G', 'B', 'Ha', 'OIII', 'SII', 'OSC', 'CLS', 'S2 6nm', 'O3 3nm', 'L-eXtreme'].map((f) => [f, 600]));
  const { container } = render(<MembersTab projectId="p" members={[m('A', secs)]} error={false} />);
  expect(container.querySelector('[data-testid="members-scroll"]')!.className).toContain('overflow-x-auto');
  expect(screen.getAllByRole('columnheader')).toHaveLength(9 + 11); // 12 filters replace the one "L" column
});

it('a row click opens the member panel with cameras and devices', () => {
  render(<MembersTab projectId="p" members={[m('Kostya', { L: 7200 }, { qualityByCamera: [{ camera: 'ASI6200MM Pro', filter: 'L', frames: 60, medianFwhm: 2.3, medianEcc: 0.4 }] })]} error={false} />);
  fireEvent.click(screen.getByText('Kostya'));
  const panel = screen.getByRole('complementary', { name: 'Member details' });
  expect(within(panel).getByText('ASI6200MM Pro')).toBeInTheDocument();
  expect(within(panel).getByText(/60 fr · x̃ FWHM 2\.30″ · x̃ ecc 0\.40/)).toBeInTheDocument();
});

it('no members → an empty state (review focus 4)', () => {
  render(<MembersTab projectId="p" members={[]} error={false} />);
  expect(screen.getByText('No members yet.')).toBeInTheDocument();
});
```

(`m(name, secondsByFilter, patch?)` builds a `MemberSummary`.)

- [ ] **Step 2: Run to verify they fail**

Run: `npx vitest run src/components/collab/project/MembersTab.test.tsx`
Expected: FAIL.

- [ ] **Step 3: Implement**

- Keep `SortKey` / `valueFor` / `lastSeenRank` / `roleLabel` / `totalSeconds`. Remove the fetch and the inline expansion. Add `const [openId, setOpenId] = useState<string | null>(null)`.
- Table:
  - wrapper: `<div data-testid="members-scroll" className="overflow-x-auto rounded-md border border-line">`;
  - `<table className="w-full border-collapse text-[12.5px]">`;
  - `th`: `whitespace-nowrap border-b border-border px-2 py-1.5 text-left text-[11.5px] font-medium text-content-faint` + `cursor-pointer hover:text-content`, `text-right` for numerics, `text-accent` when sorted;
  - `td`: `whitespace-nowrap border-b border-line-plain px-2 py-1.5`;
  - row: `cursor-pointer hover:bg-[rgba(67,76,94,0.45)]`, plus `bg-accent/[0.16]` when open.
- Cells:
  1. `<span className="inline-flex items-center gap-[5px]"><MemberDot color={colorOf(m.accountId)} /><b className="font-semibold text-content">{m.displayName}</b></span>`;
  2. the role, with `<span className="text-content-faint"> (Processor data)</span>` for a coordinator whose data role is `send_receive` (`(Contributor data)` for `send`);
  3. the devices — `m.devices.map((d) => <span key={d.device} title={d.name ?? d.device.slice(0, 8)} className="mr-0.5 inline-block"><StatusDot state={d.online ? 'online' : 'offline'} /></span>)`;
  4. published, right-aligned `toLocaleString('en-US')`;
  5. one cell per `filterColumns(members)`: `formatDurationPadded(s)` in `text-content-secondary`, or `<span className="text-content-ghost">—</span>`;
  6. Σ: `<b className="font-semibold text-content">{formatDurationPadded(totalSeconds(m))}</b>`;
  7. FWHM x̃: the median of `qualityByCamera` medianFwhm weighted by frames, as `x.xx″`, or a ghost dash;
  8. Holds: `` `${m.holdsFrames.toLocaleString('en-US')} fr · ${formatSize(m.holdsBytes)} ` `` + `<span className="text-content-faint">{Math.round(m.holdsShare * 100)}%</span>`;
  9. Last seen: `m.online ? <span className="text-success">now</span> : <span className="text-content-faint">{formatRelative(m.lastSeenAt, Date.now())}</span>`.
- Filter headers: `<span className="inline-flex items-center"><FilterDot filter={f} />{f === 'None' ? 'No filter' : f}</span>`, right-aligned.
- `filterColumns`: collect keys with `> 0` seconds across members, sort with `filterOrder`, and move `None` to the end.
- Wrap it all in `<PanelLayout panel={open ? <MemberPanel member={open} onClose={() => setOpenId(null)} /> : null}>`.
- `MemberPanel`:
  - title: `<span className="inline-flex items-center gap-[5px] text-[13px] font-semibold text-content"><MemberDot color={colorOf(member.accountId)} />{member.displayName}</span>` + role `Chip`s (mute; `coordinator` info);
  - **Cameras**: `qualityByCamera` grouped by camera — `<b className="font-semibold">{camera}</b>`, then per filter `<FilterDot/>{filter} {frames} fr · x̃ FWHM {x.xx}″ · x̃ ecc {x.xx}` joined with ` · `; `EmptyState` "Nothing published yet.";
  - **Devices**: `KV` of name → "online"/"offline" with a `StatusDot`;
  - **Holds**: `` `${frames} fr · ${formatSize(bytes)} · ${pct} % of the project` ``.
- `ProjectDetail`:
  - pass `members` / `membersError` to `MembersTab`;
  - the peers-changed throttle calls `loadMembers()` instead of bumping `membersRefresh` (delete that state).

- [ ] **Step 4: Run the tests and the typecheck**

Run: `npx vitest run src/components/collab/project src/pages && npx tsc --noEmit -p .`
Expected: PASS.

- [ ] **Step 5: Harness check**

Members in both pages: header x positions, row height (6+… ≈ 29.5 at 12.5px), the devices dots, the Holds / Last seen alignment, no wrapping. Click Kostya: the panel is 400 px, the table narrows, no wrap. Scenario `filters`: horizontal scroll inside the box. Record.

- [ ] **Step 6: Commit**

```bash
git add -A src/components/collab/project src/pages/ProjectDetail.tsx
git commit -m "feat(collab): Members as the mockup's plain table; member details in the docked panel"
```

---

### Task 14: Exchange

**Files:**
- Modify: `src/components/collab/project/ExchangeTab.tsx`, `ExchangeTab.test.tsx`
- Modify: `src/components/collab/exchange/PeerFlowRow.tsx` (+ its test if present). Transfers' `CollabTrafficGroups` also renders `PeerFlowRow`, so it inherits the mockup row, which matches the mockup's Transfers screen. Keep its tests green.

**Interfaces:**
- Consumes: `Card`, `Bar`, `ProgressBar`, `Sparkline`, `Chip`, `MemberDot`, `EmptyState` (Task 3); `useMemberColor` (Task 6); the existing `state.rates[flowKey]` history (40 samples, `exchange/state.ts` `RATE_HISTORY_MAX`) — no new buffer (the spec §16 row is amended in Task 16).
- Produces: `PeerFlowRow({ flow, label, rates, color?: string })`. `tone?: string` becomes `color?: string`, a hex member colour for the avatar. When it is absent, the avatar is `bg-surface-hover` with `text-content`.

- [ ] **Step 1: Write the failing tests** (in `ExchangeTab.test.tsx`):

```tsx
it('flow rows use the mockup grid: avatar, who, bar + caption, rate, sparkline, ETA', async () => {
  renderTab(withFlows());
  const row = await screen.findByRole('button', { name: /from Kostya/ });
  expect(row.className).toContain('grid-cols-[28px_minmax(140px,1.2fr)_minmax(160px,2fr)_90px_128px_70px]');
  expect(within(row).getByText(/37 landed this session · 4\.8 GB/)).toBeInTheDocument();
  expect(within(row).getByText('ETA 9m')).toBeInTheDocument();
  expect(row.querySelector('svg polyline')).not.toBeNull();
});

it('Received sessions: Started, From, Frames, Size, Duration, Avg rate, Outcome', async () => {
  renderTab(withSessions([{ frames: 90, bytes: 7.9e9, failed: 0, startedAt: '2026-09-28T22:48:00Z', finishedAt: '2026-09-28T22:54:56Z' }, { frames: 68, bytes: 4.1e9, failed: 2, startedAt: '2026-09-28T10:54:00Z', finishedAt: '2026-09-28T10:56:13Z' }]));
  expect((await screen.findAllByRole('columnheader')).map((h) => h.textContent)).toEqual(['Started', 'From', 'Frames', 'Size', 'Duration', 'Avg rate', 'Outcome']);
  expect(screen.getByText('landed').className).toContain('bg-success-muted');
  expect(screen.getByText('partial · 2 failed').className).toContain('bg-warning-muted');
  expect(screen.getByText('7m')).toBeInTheDocument();
});

it('idle project: the three empty states (review focus 4)', async () => {
  renderTab(idle());
  expect(await screen.findByText('Nothing is being received.')).toBeInTheDocument();
  expect(screen.getByText('Nothing is being sent.')).toBeInTheDocument();
  expect(screen.getByText('No sessions yet.')).toBeInTheDocument();
});
```

- [ ] **Step 2: Run to verify they fail**

Run: `npx vitest run src/components/collab/project/ExchangeTab.test.tsx`
Expected: FAIL.

- [ ] **Step 3: Implement**

- `PeerFlowRow` render. The outer element is a `<button type="button" aria-expanded={open} onClick={() => setOpen((o) => !o)}` with class `grid w-full grid-cols-[28px_minmax(140px,1.2fr)_minmax(160px,2fr)_90px_128px_70px] items-center gap-2.5 border-t border-line px-1 py-[9px] text-left first:border-t-0 hover:bg-[rgba(67,76,94,0.3)] max-[640px]:grid-cols-[28px_1fr_80px]`. Its children, in order:
  1. avatar `<span className="grid h-[26px] w-[26px] place-items-center rounded-full text-[11px] font-bold text-surface" style={color ? { backgroundColor: color } : undefined}>{initial}</span>`;
  2. who `<span className="min-w-0"><b className="block truncate font-semibold text-content">{isRecv ? '↓ from' : '↑ to'} {label.member ?? label.device}</b><small className="block truncate text-[11.5px] text-content-faint">{label.deviceName ?? label.device} · {flow.inFlight.length} in flight</small></span>`;
  3. bar `<span className="max-[640px]:hidden"><ProgressBar percent={inflightPct} color={isRecv ? undefined : '#a3be8c'} /><small className="text-[11px] text-content-faint">{flow.completed} {isRecv ? 'landed' : 'served'} this session · {formatSize(flow.bytesSession)}</small></span>`, where `inflightPct` = 100 × Σdone / Σsize over `flow.inFlight` (0 when empty);
  4. rate `<span className="text-right font-semibold text-content">{formatRate(flow.rateBps)}</span>`;
  5. `<span className="max-[640px]:hidden"><Sparkline values={rates} color={isRecv ? '#88c0d0' : '#a3be8c'} /></span>`;
  6. `<span className="text-right text-[12px] text-content-faint max-[640px]:hidden">ETA {flow.etaSecs === null ? '—' : formatDurationPadded(flow.etaSecs)}</span>`.

  The expanded in-flight list is `<div className="grid gap-[5px] pb-2 pl-[42px] pt-0.5">`, with rows `grid grid-cols-[minmax(0,1fr)_160px_80px] items-center gap-2.5 text-[12px] text-content-muted`: the mono file name (truncate), `ProgressBar`, and a right-aligned `` `${formatSize(done)} / ${formatSize(size)}` ``.

  The sparkline hexes are the member-palette values from §4.4 (accent / success). Take them from `MEMBER_PALETTE[0]` and `MEMBER_PALETTE[1]`, so there is no new raw colour.
- `ExchangeTab`:
  - **Receiving** `<Card title="Receiving" subtitle={recvSub}>` — rows via `PeerFlowRow` with `color={colorOf(label.member)}`; `EmptyState` "Nothing is being received." when there are no flows (keep "Your role does not receive project data" for a send-only member);
  - **Sending** likewise, with "Nothing is being sent.";
  - **Received** `<Card title="Received" subtitle="sessions · a session ends after 5 min without a landing">` — a plain table (Task 13 classes) with the columns Started (`formatTimestamp`), From (each source: `MemberDot` + member name, comma-separated), Frames, Size (`formatSize`), Duration (`formatDurationPadded((finished − started)/1000)`), Avg rate (`formatRate(bytes / seconds)`), Outcome (`<Chip tone="ok">landed</Chip>` or `<Chip tone="warn">partial · {failed} failed</Chip>`); `EmptyState` "No sessions yet."; the load error `text-[12.5px] text-error`;
  - the cards are stacked with `gap-3.5`.

- [ ] **Step 4: Run the tests and the typecheck**

Run: `npx vitest run src/components/collab src/components/transfers && npx tsc --noEmit -p .`
Expected: PASS (update `CollabTrafficGroups` assertions only where they asserted old classes or text).

- [ ] **Step 5: Harness check**

Exchange in both pages. Compare the peer grid columns (28 / 1.2fr / 2fr / 90 / 128 / 70), the avatar 26 px, the sparkline 120×26, the caption 11 px, and the Received table columns. Record, noting D5: no "queued" count and no "upload streams".

- [ ] **Step 6: Commit**

```bash
git add -A src/components/collab src/components/transfers
git commit -m "feat(collab): Exchange flows and received sessions as in the mockup"
```

---

### Task 15: Collab dialogs on `DialogShell`

**Files:**
- Modify: `src/components/collab/ProjectExportDialog.tsx`, `LinkObjectDialog.tsx`, `FilterMappingDialog.tsx`, `DeviceReplaceDialog.tsx`
- Modify: `src/components/collab/project/ExcludeDialog.tsx`, `RepublishGuardDialog.tsx`
- Create: `src/components/collab/project/PublishConfirmDialog.tsx` (moved out of `ProjectDetail.tsx`'s inline block)
- Modify: `src/pages/ProjectDetail.tsx`
- Tests: each dialog's existing test file (and `PublishConfirmDialog.test.tsx`, new)

**Interfaces:**
- Consumes: `DialogShell`, `Button`, `Select`, `TextInput`, `Chip` (Tasks 3–4); `Checkbox` (Task 5).
- Produces: `PublishConfirmDialog({ title: string, count: number, estimatedBytes: number, needsApproval: boolean, coordinatorName: string, busy: boolean, error: string | null, onConfirm(): void, onCancel(): void })`. Every other dialog keeps its current props.

- [ ] **Step 1: Write the failing tests** — for each of the seven, add one test to its file:

```tsx
it('renders through the shared dialog shell', () => {
  renderDialog();
  const d = screen.getByRole('dialog', { name: /<its title>/ });
  expect(d.className).toContain('rounded-lg');
  expect(d.className).toMatch(/w-\[(440|560)px\]/);
});
```

Use each dialog's own title. The export dialog is `Export “…” for WBPP` (size `md`), the filter mapping `Filter mapping` (`md`), Link `Link an object` (`md`), Exclude `Exclude N frame(s)` (`sm`), the republish guard its current title (`sm`), device replace its current title (`sm`), and publish `Publish to <title>` (`sm`). `PublishConfirmDialog.test.tsx` also checks the three lines, the approval line only when `needsApproval`, and that Publish calls `onConfirm`.

- [ ] **Step 2: Run to verify they fail**

Run: `npx vitest run src/components/collab`
Expected: FAIL on the new tests only.

- [ ] **Step 3: Migrate each dialog**

For every one of them:
- remove its own `fixed inset-0` wrapper, window `div`, header `h2`, close `X` and Escape listener;
- render `<DialogShell title={…} size={…} onClose={…} busy={…} footer={…}>` around the existing body;
- replace buttons with `Button` (Cancel/Close → default; the main action → `primary`; destructive → `dangerPrimary`), selects and text inputs with `Select` / `TextInput`, checkboxes with `Checkbox`;
- set body text to 12.5 px `content-muted` (inherited), notes to `text-[11.5px] text-content-faint`, errors to `text-[12.5px] text-error`.

Keep every behaviour: the busy locks (`busy` prop), `ExcludeDialog`'s outcome line, `DeviceReplaceDialog`'s `escapeClosesAsNotNow`, and `ProjectExportDialog`'s cancel-export wiring. For `DeviceReplaceDialog`, pass `onClose={() => close(escapeClosesAsNotNow)}` so Escape keeps its meaning.

`PublishConfirmDialog` body:

```tsx
<DialogShell title={`Publish to ${title}`} onClose={onCancel} busy={busy} footer={<>
  <Button onClick={onCancel} disabled={busy}>Cancel</Button>
  <Button variant="primary" onClick={onConfirm} disabled={busy} data-autofocus>{busy && <Loader2 size={12} className="animate-spin" />}Publish</Button>
</>}>
  <p>{count} passing {count === 1 ? 'frame' : 'frames'} will be calibrated and announced to the project.</p>
  <p className="mt-1.5 text-[11.5px] text-content-faint">Estimated size ≈ {formatSize(estimatedBytes)} — the exact size is measured when each frame is generated.</p>
  {needsApproval && <p className="mt-1.5 text-[12px] text-warning">This project requires approval — your contribution goes to {coordinatorName} for review.</p>}
  {error && <p className="mt-1.5 text-[12.5px] text-error">{error}</p>}
</DialogShell>
```

In `ProjectDetail`, replace the inline block with `{publishIds && <PublishConfirmDialog title={c.title} count={publishIds.length} estimatedBytes={publishIds.length * APPROX_FRAME_BYTES} needsApproval={needsApproval} coordinatorName={coordinatorName} busy={publishing.publishBusy} error={publishing.publishError} onConfirm={() => void publishing.publish(publishIds)} onCancel={() => setPublishIds(null)} />}`.

- [ ] **Step 4: Run the tests and the typecheck**

Run: `npx vitest run && npx tsc --noEmit -p .`
Expected: PASS.

- [ ] **Step 5: Harness check**

Open, in the harness, Export for WBPP, + Link an object, Recalibrate and republish all, Library → select → Exclude, and Ready → Publish all. Each is 440 or 560 px, has a 13 px title, a 14/16 padding, right-aligned buttons and the same scrim. Record.

- [ ] **Step 6: Commit**

```bash
git add -A src/components/collab src/pages/ProjectDetail.tsx
git commit -m "feat(collab): every collab dialog on the shared shell; publish confirm as its own component"
```

---

### Task 16: Review-focus pass, docs, and the real-window check

**Files:**
- Modify: `docs/superpowers/specs/2026-09-30-collab-project-ui-pixel-design.md` (the §16 rate-history row: "the existing 40-sample `state.rates` history" instead of "a new 60-sample ring buffer"; add a §21 "Implementation notes" line for each ruling taken during execution)
- Modify: `CLAUDE.md` (Frontend Conventions: one line)
- Modify: `docs/superpowers/open-items.md` (the owed real-window smoke, until it is done)

**Interfaces:** none new.

- [ ] **Step 1: Run the scenarios**

For each of `HARNESS_SCENARIO=long`, `filters`, `empty`, `offline`, run the harness and walk all six tabs. Open one frame panel and the member panel at a 1100 px window width.
Expected:
- long names truncate (tables, title) or break (panel title), and no column moves;
- the Members table scrolls horizontally with 12 filters;
- every empty card or table shows its empty line and no NaN;
- the offline pill keeps its size;
- at 1100 px with the panel open, the table scrolls horizontally and nothing wraps.

Fix any defect in the task that owns it and re-run that task's tests.

- [ ] **Step 2: Final side-by-side sweep**

Run `measure.js` on all six tabs plus the frame panel in both pages and diff the JSON. Put the table of residual differences in the ledger. Everything > 1 px must be explained by D1–D7, or fixed.

- [ ] **Step 3: Docs**

- `CLAUDE.md`, under Frontend Conventions, add: "Shared compact UI primitives live in `src/components/ui/` (they mirror the collab mockup's CSS: `Button`, `Chip`, `Card`, `KV`, `DialogShell`, `SidePanel`, …); every modal uses `DialogShell`. `npm run ui:harness` renders the project page on the mockup's data for side-by-side checks."
- In the spec, apply the §16 amendment and the §21 notes.
- In `open-items.md`, add: "Wave 5.5 — real-window (Tauri/WebKit) check of the project page: owed until the terminal has Screen Recording permission." Delete that line once Step 4 is done.

- [ ] **Step 4: Real-window check (needs the owner's one-time permission)**

If the terminal can capture the screen (`screencapture -x /tmp/x.png` succeeds), run `npm run tauri dev` and screenshot the project page on the owner's data: all six tabs, the frame panel, one dialog. Compare them with the harness screenshots for font rendering and layout. If the capture fails, report "real-window check not done — needs Screen Recording permission for the terminal" and leave the open item in place. Never report this step as passed without the screenshots.

- [ ] **Step 5: Full verification and commit**

Run: `npx vitest run && npx tsc --noEmit -p .` — Expected: PASS. Rust is untouched; the standing pre-push rule still applies: `cargo test -p athenaeum-core --all-targets` before any push.

```bash
git add -A docs CLAUDE.md
git commit -m "docs(collab): wave 5.5 notes — ui primitives, harness, rate-history amendment, owed real-window check"
```
