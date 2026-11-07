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

// `stacking`: the project's Stacking tab. Every value is shaped from `src/types/stacking.ts` (`StackingPlan`,
// `StackingConfig`, `WorkUsage`, `StackingPaths`) and `src/types/models.ts` (`CanonicalGrid`).
const RAD = Math.PI / 180;

// A tangent-plane point (offsets in degrees: xi east, eta north) about (ra0, dec0), back to RA/Dec in degrees.
function tanToSky(xiDeg, etaDeg, ra0, dec0) {
  const xi = xiDeg * RAD;
  const eta = etaDeg * RAD;
  const rho = Math.hypot(xi, eta);
  if (rho === 0) return [ra0, dec0];
  const c = Math.atan(rho);
  const d0 = dec0 * RAD;
  const dec = Math.asin(Math.cos(c) * Math.sin(d0) + (eta * Math.sin(c) * Math.cos(d0)) / rho);
  const ra = ra0 * RAD + Math.atan2(xi * Math.sin(c), rho * Math.cos(d0) * Math.cos(c) - eta * Math.sin(d0) * Math.sin(c));
  return [(((ra / RAD) % 360) + 360) % 360, dec / RAD];
}

// One frame's outline: a w × h degree sensor centred at (cx, cy) on the tangent plane, turned by `rot` degrees.
function sensorOutline(cx, cy, rot, ra0, dec0, w = 1.1, h = 0.74) {
  const c = Math.cos(rot * RAD);
  const s = Math.sin(rot * RAD);
  return [[-w / 2, h / 2], [w / 2, h / 2], [w / 2, -h / 2], [-w / 2, -h / 2]].map(([x, y]) => tanToSky(cx + x * c - y * s, cy + x * s + y * c, ra0, dec0));
}

// The project seed document (`project_seed_config`): the global default with `exposureMode = stops` and the coverage
// map on. The three presets are the built-ins; the seed differs from all of them, so the tab reads "Custom".
const STACKING_DEFAULT = {
  version: 1,
  grouping: { exposureToleranceSec: 2, exposureMode: 'seconds', stopsWidth: 1 },
  calibration: {
    flatNorm: true, flatNormMode: 'centralThird',
    params: { trimFraction: 0.05, pedestalDn: 0, biasFallback: 'subtractBias', cfaFlatScaling: true },
    hotPixelCorrection: true, debayerOsc: true, format: 'fits',
  },
  measurement: {
    weightMode: 'psfSignalWeight', psfModel: 'auto', maxStars: 24576,
    formula: { fwhm: 15, eccentricity: 15, snr: 20, stars: 0, pedestal: 50 }, keyword: 'SSWEIGHT',
    detectionSigma: 20, seedPrefilter: 'none', seedDetector: 'peak', seedSource: 'matchedFilter', seedThreshold: 6,
  },
  selection: { minWeightFraction: 0.05, maxFwhmPx: null, maxEccentricity: null, minStars: null, excludeOnRegistrationFailure: true },
  reference: { mode: 'auto', twoPass: true },
  registration: {
    geometry: 'coRegistered', model: 'auto', distortion: 'off', tpsSmoothing: 0.5, localDistortion: false,
    interpolation: 'bicubicBSpline', clampingThreshold: 0.3, maxStars: 2000, ransacTolerancePx: 1.9,
    ransacMaxIterations: 2000, maxRmsPx: 2, failOnMaxRms: false, detection: { minSnr: 10, maxEccentricity: 0.8 },
    writeRegisteredFrames: false,
  },
  normalization: {
    output: 'additiveWithScaling', rejection: 'local', scaleEstimator: 'bwmv',
    local: {
      enabled: false, scale: 1024, scaleMode: 'auto', gridSize: 4, referenceFrames: 20, psfModel: 'auto', localScale: false,
      starSelection: { minPeakSnr: 40, maxStars: 500, saturation: 0.75 }, fluxEstimator: 'commonAperture', aperture: 'growthCurve',
    },
  },
  integration: {
    combination: 'average', rejection: { method: 'auto' }, minWeight: 0.005, rangeLow: 0, rangeHigh: null,
    writeRejectionMaps: false, largeScale: { enabled: false, protectedLayers: 2, growth: 2 },
  },
  drizzle: {
    enabled: false, scale: 2, dropShrink: 0.9, kernel: 'square', useRejection: true, useWeights: true,
    useLocalNormalization: true, writeWeightMap: false, bayer: false,
  },
  output: { format: 'fits', cleanup: 'keepAll', coverageMap: false, cropToCoverage: 0 },
  paths: { workingDir: null, outputDir: null },
};

function stackingHandlers({ projectId, card, nameOf, acc }) {
  const seed = { ...STACKING_DEFAULT, grouping: { ...STACKING_DEFAULT.grouping, exposureMode: 'stops' }, output: { ...STACKING_DEFAULT.output, coverageMap: true } };
  const presets = {
    default: STACKING_DEFAULT,
    fastPreview: { ...STACKING_DEFAULT, reference: { mode: 'auto', twoPass: false }, normalization: { ...STACKING_DEFAULT.normalization, rejection: 'scaleZeroOffset' } },
    maximumQuality: { ...STACKING_DEFAULT, registration: { ...STACKING_DEFAULT.registration, distortion: 'auto' }, drizzle: { ...STACKING_DEFAULT.drizzle, enabled: true } },
  };
  const { targetRaDeg: ra0, targetDecDeg: dec0, targetRadiusDeg } = card;
  // The proposal: 4800 × 3600 px at 1.5″/px = 2.0° × 1.5° about the target, north up.
  const proposal = { scaleArcsec: 1.5, widthPx: 4800, heightPx: 3600, crval1: ra0, crval2: dec0, rotationDeg: 0 };

  // Twelve accepted frames of three publishers. Rows 0-9 carry a WCS in the manifest (their outlines are drawn; rows 4
  // and 7 reach past the proposal); rows 10-11 do not (`framesNotDrawn: 2`). Row 6 is an unsupported file, row 8 is
  // not on this device. The usable ten form the three groups: L 5, Ha 3, OSC 2.
  const FILTER = { L: ['mono', 120], Ha: ['mono', 300], None: ['osc', 180] };
  const ROWS = [
    // [publisher, filter, rotation, centre on the tangent plane (deg) | null without a WCS, flags]
    ['you', 'L', 0, [0, 0]], ['you', 'L', 0, [0.12, -0.08]], ['you', 'L', 0, [-0.10, 0.10]], ['you', 'Ha', 0, [0.05, 0.15]],
    ['kostya', 'L', 3.5, [-0.05, -0.52]], ['kostya', 'Ha', 3.5, [0.18, 0.12]], ['kostya', 'Ha', 3.5, [-0.20, 0.05], 'unsupported'],
    ['masha', 'None', -2, [0.62, 0.10]], ['masha', 'L', -2, [-0.15, 0.20], 'offDevice'], ['masha', 'None', -2, [0.25, -0.20]],
    ['you', 'L', 0, null], ['kostya', 'Ha', 3.5, null],
  ];
  const frames = ROWS.map(([publisherId, filter, rot, centre, flag], i) => {
    const [colorMode, exp] = FILTER[filter];
    const n = String(i + 1).padStart(4, '0');
    return {
      i, filter, colorMode, exp, rot, centre, flag, uuid: `s-${n}`, publisher: nameOf(publisherId), accountId: acc(publisherId),
      fileName: `Light_M31_${filter === 'None' ? 'OSC' : filter}_${exp}s_${n}.fits`,
    };
  });
  const usable = frames.filter((f) => !f.flag);
  const footprints = frames.filter((f) => f.centre).map((f) => ({
    frameUuid: f.uuid, publisherAccountId: f.accountId, publisher: f.publisher, filterCanonical: f.filter,
    outline: sensorOutline(f.centre[0], f.centre[1], f.rot, ra0, dec0),
  }));
  const projectFrames = frames.map((f) => ({
    handle: f.flag === 'offDevice' ? null : 100 + f.i, frameUuid: f.uuid, fileName: f.fileName, publisher: f.publisher, filterCanonical: f.filter,
    // Two rigs: a 530 mm refractor on the mono camera, a 400 mm one on the OSC; the frames without a WCS declare none.
    exptimeSec: f.exp, colorMode: f.colorMode, focalLenMm: f.centre === null ? null : f.colorMode === 'osc' ? 400 : 530,
    hasWcs: f.centre !== null, onDisk: f.flag !== 'offDevice',
    reason: f.flag === 'offDevice' ? 'not on this device' : f.flag === 'unsupported' ? 'unsupported file: tile-compressed FITS' : null,
  }));
  const group = (filter, cameras, anchor, scale, source) => {
    const [colorMode, exp] = FILTER[filter];
    const members = usable.filter((f) => f.filter === filter);
    return {
      key: `${colorMode}__${filter}__st${exp}`, instrume: null, colorMode, filter, binning: 1, cameras,
      exposureS: exp, exposureClass: `st${exp}`, frameCount: members.length, memberFrameIds: members.map((f) => 100 + f.i),
      includedCount: members.length,
      totalExposureS: members.length * exp, calibratedCached: 0, metricsCached: 0, fitsCached: 0, lnCached: 0,
      lnScalePx: null, lnScaleEstimated: false, lnWidth: null, lnHeight: null, anchorWidth: anchor[0], anchorHeight: anchor[1],
      pixelScaleArcsec: scale, scaleRatioToReference: null, scaleSource: source,
    };
  };
  const groups = [
    group('L', ['ASI2600MM Pro'], [6248, 4176], 1.52, 'solve'),
    group('Ha', ['ASI2600MM Pro'], [6248, 4176], 1.52, 'solve'),
    group('None', ['ASI294MC Pro'], [4144, 2822], 1.51, 'header'),
  ];
  const plan = {
    setId: null, projectId, setName: card.title, config: seed, configHash: 'harness-stacking', groups,
    blockers: [{ code: 'grid', message: "Confirm the project's grid in the Stacking tab" }],
    warnings: ['1 frame of the project is not on this device and is left out (L: 1)'],
    readiness: {
      total: 12, unlinkedLights: 0, rawSetsWithoutMaster: 0, rawSetIdsWithoutMaster: [], missingMasterFiles: 0,
      missingRawCalibrationFiles: 0, blackHoleZipMasters: 0,
      fileCounts: { lightsOnly: 12, rawWithCalibrationSets: 0, rawWithMasters: 0, calibratedLights: 12 },
      rawSetsBuildable: [], rawSetsUnbuildable: [], mastersRebuildable: [], mastersUnrebuildable: [], attested: false,
      calibrationWarnings: [], geometryMismatches: [], geometryWarnings: [],
    },
    mastersToBuild: [], reference: { mode: 'auto', frameId: null, filename: null, onDisk: false },
    frameCount: frames.length, includedCount: usable.length, excludedFrameIds: [], estimateBytes: 38e9, freeBytes: 412e9,
    workingDir: '/Volumes/Astro/Stacking/work', outputDir: '/Volumes/Astro/Stacking/out',
    staleStages: ['calibrate', 'measure', 'register'], activeRunId: null,
    grid: {
      state: 'proposed', current: null, proposal, framesInside: footprints.length, canEdit: true,
      proposalRotation: { source: 'bestWeighted', fileName: frames[0].fileName },
      target: { raDeg: ra0, decDeg: dec0, radiusDeg: targetRadiusDeg }, footprints, framesNotDrawn: frames.length - footprints.length,
    },
    projectFrames, prepare: { inPlace: 9, debayered: 0, converted: 1, unsupported: 1, atRun: 0 },
  };
  const projectConfig = { config: seed, excludedFrameUuids: [], referenceFrameUuid: null, isDefault: true, updatedAt: null };
  // One finished run, #9, for the reference strip and the gold outline: its auto pick was row 0 (the best weight), and
  // the second pass moved it to row 1. Its groups and frames are the usable ten; it wrote no master.
  const runId = 9;
  const runGroups = groups.map((g, i) => ({
    id: i + 1, runId, groupKey: g.key, instrume: null, colorMode: g.colorMode, filter: g.filter, binning: 1,
    width: g.anchorWidth, height: g.anchorHeight, exposure: g.exposureS, frameCount: g.frameCount, includedCount: g.includedCount,
  }));
  const runFrames = usable.map((f, i) => ({
    id: i + 1, runId, groupId: runGroups.find((g) => g.groupKey === groups.find((pg) => pg.filter === f.filter).key).id,
    frameId: 100 + f.i, included: true, exclusionReason: null, weight: 0.9, weightChannelsJson: null, metricsJson: null,
    regStatus: f.i === 1 ? 'reference' : 'ok', regModel: 'homography', regRmsPx: 0.21, regInliers: 400, regInlierRatio: 0.9,
    regFlipped: false, rejectedFraction: 0.02, frameUuid: f.uuid,
  }));
  const runRow = {
    id: runId, framesSetId: null, projectId, status: 'done', startedAt: '2026-11-06T09:03:59', finishedAt: '2026-11-06T09:41:12',
    configJson: JSON.stringify(seed), configHash: 'harness-stacking', referenceFrameId: 101, referenceMode: 'auto',
    workingDir: '/Volumes/Astro/Stacking/work', outputDir: '/Volumes/Astro/Stacking/out', summaryJson: null, error: null,
  };
  const runDetail = {
    run: runRow, groups: runGroups, frames: runFrames,
    summary: {
      config: seed, grid: proposal, groups: [], stages: [],
      reference: { frameId: 101, filename: frames[1].fileName, mode: 'auto', weight: 0.932, switchedFrom: 100 },
    },
  };
  const path = (dir) => ({ configured: null, effective: dir, default: dir, restartRequired: false });
  return {
    get_project_stacking_plan: () => plan,
    get_project_stacking_config: () => projectConfig,
    get_project_stacking_runs: () => [{ run: runRow, groupCount: runGroups.length, masterPaths: [] }],
    get_stacking_run: () => runDetail,
    get_project_stacking_work_usage: () => ({
      calibratedBytes: 0, registeredBytes: 0, lnBytes: 0, fitsBytes: 0, measuredBytes: 0, runsBytes: 0, rejBytes: 0, previewsBytes: 0, totalBytes: 0,
    }),
    get_stacking_presets: () => presets,
    list_stacking_presets: () => [],
    get_stacking_paths: () => ({ working: path('/Volumes/Astro/Stacking/work'), output: path('/Volumes/Astro/Stacking/out') }),
    // The writes a click on the tab can reach: answered with what was sent, so nothing logs an unknown command.
    set_project_stacking_config: (args) => ({ ...projectConfig, config: args.config, excludedFrameUuids: args.excludedFrameUuids, referenceFrameUuid: args.referenceFrameUuid, isDefault: false }),
    set_collab_project_grid: (args) => args.grid,
  };
}

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
    autoReplicate: true, publishMode: 'manual', syncedAt: new Date(Date.now() - 8000).toISOString(), fetchedAt: NOW,
    publishingDevice: { deviceId: devId('you', 0), name: 'This Mac' }, publishingHere: true,
  };
  const detail = {
    card,
    members: MEMBERS.map((m) => ({ displayName: m.name, dataRole: (m.data || m.role) === 'Contributor' ? 'send' : 'send_receive', coordinator: m.role === 'Coordinator' })),
    thresholdsVersion: 3,
    thresholds: [
      { metricKey: 'fwhm_arcsec', op: 'lte', value: 3.0 },
      { metricKey: 'eccentricity', op: 'lte', value: 0.55 },
      { metricKey: 'stars_detected', op: 'gte', value: 150 },
      { metricKey: 'not_trailed', op: 'reject_if', value: true },
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
    { metricKey: 'fwhm_arcsec', label: 'FWHM', value: `${f.fwhm.toFixed(2)}″`, needs: '≤ 3.00″', pass: f.fwhm <= 3 },
    { metricKey: 'eccentricity', label: 'eccentricity', value: f.ecc.toFixed(2), needs: '≤ 0.55', pass: f.ecc <= 0.55 },
    { metricKey: 'stars_detected', label: 'stars', value: String(f.stars), needs: '≥ 150', pass: f.stars >= 150 },
    { metricKey: 'not_trailed', label: 'trailed', value: 'no', needs: 'not trailed', pass: true },
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
    calibratedPath: null, calibratedBytes: null, preparedAt: null, withheld: false,
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
  // `review`: 48 calibrated frames waiting for review, three the member withheld.
  const reviewRows = () => {
    const base = FRAMES.filter((f) => f.own).slice(0, 51).map(ownRow);
    const prepared = Date.now() - 600000;
    return base.map((r, i) => {
      const fl = i % 2 ? 'OIII' : 'Hα';
      const common = { ...r, filter: fl, exptimeSec: 300, pubState: null, publishedAt: null, contentVersion: null, accepted: null };
      if (i < 48) {
        return { ...common, segment: 'review', contributorState: 'notPublished', contributorReason: null, failures: [],
          calibratedPath: `/collab/own/c_${r.fileName}`, calibratedBytes: 44 * 1024 * 1024, preparedAt: new Date(prepared + i * 1000).toISOString(), withheld: false };
      }
      return { ...common, segment: 'held', contributorState: 'notPublished', contributorReason: 'Withheld by you',
        failures: [{ kind: 'withheld', text: 'Withheld by you' }], withheld: true };
    });
  };
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
  if (scenario === 'stacking') card.canEditGrid = true;
  const zeroPublishResult = { announced: 0, updated: 0, state: null, heldBack: [], unchanged: 0, calibrated: 0, stale: 0 };
  const handlers = {
    list_collab_projects: () => [card],
    refresh_collab_projects: () => [card],
    get_collab_project_detail: () => detail,
    list_collab_frames: () => inProject.map(frameView),
    list_project_own_frames: () => (scenario === 'review' ? reviewRows() : FRAMES.filter((f) => f.own).map(ownRow)),
    get_collab_member_summary: () => memberSummary,
    get_collab_exchange: () => exchange,
    list_collab_moderation: () => moderation,
    list_collab_attention: () => ({ changed: [], awaitingChoice: [], notKept: [], otherFiles: [] }),
    get_collaboration_dir: () => '/Volumes/Astro/Collaboration',
    get_collab_live_status: () => live,
    get_collab_storage_status: () => ({ state: 'available', reason: null, root: '/Volumes/Astro/Collaboration', watcherDegraded: false, networkVolume: false, replace: null, unknownDevice: null }),
    list_collab_receive_sessions: () => sessions,
    list_collab_link_suggestions: () => [],
    // An object, not a list: the server's `list_*` → [] default would crash the Transfers page.
    list_terminal_transfers: () => ({ sent: [], received: [] }),
    get_export_dir: () => '/Volumes/Astro/Exports',
    get_collab_frame_holders: (args) => holdersFor(args.frameUuid),
    account_status: () => ({ signedIn: true, email: 'you@example.org', deviceId: devId('you', 0), capability: 'full', hubUrl: 'https://hub.example' }),
    get_compute_queue: () => [],
    get_scan_roots: () => [],
    initialize_database: () => null,
    get_collab_publish_run: () => (scenario === 'review'
      ? { running: { kind: 'calibrate', trigger: 'manual', mode: null, stage: 'calibrating', current: 12, total: 48, currentFile: 'M42_Ha_300s_0012.fits', publishRunId: 'h1', projectId: PID, startedAt: new Date(Date.now() - 134000).toISOString() }, last: null }
      : { running: null, last: null }),
    cancel_collab_publish: () => null,
    calibrate_collab_frames: () => zeroPublishResult,
    publish_collab_frames: () => zeroPublishResult,
    set_collab_frames_withheld: (args) => args.frameIds.length,
    set_project_publish_mode: () => null,
    get_collab_blink_frames: () => [],
    get_collab_frame_image: () => null,
    get_setting: (args) => args.defaultValue ?? null,
  };
  if (scenario === 'stacking') Object.assign(handlers, stackingHandlers({ projectId: PID, card, nameOf: (id) => MBY[id].name, acc }));
  return { handlers, counts: { frames: FRAMES.length, members: MEMBERS.length } };
}

if (process.argv[2] === '--check') {
  const { counts, handlers } = buildFixtures('default');
  if (counts.frames !== 3719 || counts.members !== 8) throw new Error(`unexpected fixture counts ${JSON.stringify(counts)}`);
  console.log('fixtures ok', counts, 'library rows', handlers.list_collab_frames().length);
}
