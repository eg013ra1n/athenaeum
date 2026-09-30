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
