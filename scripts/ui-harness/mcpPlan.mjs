// Dev-only `mcp-plan` scenario: the assistant's plan window beside the Duplicates page, built from the same files.
//   HARNESS_SCENARIO=mcp-plan HARNESS_PLAN=black_hole|metadata|plate_solve npm run ui:harness
// The web build learns of a waiting plan by asking `get_mcp_plan({ planId: null })` on load, so the plan opens by
// itself over any page (http://localhost:1430/files; the Duplicates tab shows the groups the Black Hole plan names).
// Shapes: `src/types/mcp.ts` (`McpPlanView`) and `src/types/models.ts` (`DuplicateGroup`). Reload the page after an
// answer to get the plan back.

const GB = 1024 ** 3;
const MB = 1024 ** 2;
const ROOT = { id: 1, path: '/Volumes/Astro' };

// The files of the scenario, the mockup's three groups of byte-identical copies: a light with two extra copies; a master
// flat whose first copy is linked by calibration links (so the plan locks it and it stays); a dark whose copy that would
// stay is on a drive that cannot be reached (no copy is picked, the user must pick one).
const LIGHT = '2025-09-14_01-25-40__-9.90_180.00s_0006.fits';
const DARK = '2025-09-12_22-41-07__-10.00_300.00s_0001.fits';
const GROUPS = [
  {
    id: 1,
    name: LIGHT,
    type: 'LIGHT',
    object: 'M 78',
    filter: 'L',
    exptime: '180',
    date: '2025-09-14T01:25:40Z',
    size: Math.round(42.3 * MB),
    copies: [
      { fileId: 101, folder: '/Volumes/Universe/Astrophotography/MBPMAX/Astro/Unsorted/2025-09-13/LIGHT', modified: '2025-09-14 01:28:47' },
      { fileId: 102, folder: '/Volumes/BigMac/Pictures/Import/2025-09-13', modified: '2025-09-14 09:02:11' },
      { fileId: 103, folder: '/Volumes/Universe/Astrophotography/Backup/2025-09-13/LIGHT', modified: '2025-09-15 18:44:30' },
    ],
  },
  {
    id: 2,
    name: 'masterFlat_L_2025-09-14.xisf',
    type: 'FLAT',
    object: null,
    filter: 'L',
    exptime: '2',
    date: '2025-09-14T20:15:03Z',
    size: Math.round(118.4 * MB),
    copies: [
      { fileId: 201, folder: '/Volumes/Universe/Astrophotography/Masters/Flat', modified: '2025-09-14 20:15:03', lock: '3 calibration links point at this copy, so it stays' },
      { fileId: 202, folder: '/Volumes/BigMac/Pictures/Import', modified: '2025-09-14 20:15:03' },
    ],
  },
  {
    id: 3,
    name: DARK,
    type: 'DARK',
    object: null,
    filter: null,
    exptime: '300',
    date: '2025-09-12T22:41:07Z',
    size: Math.round(54.9 * MB),
    noKeeper: 'No copy is picked here: the copy that would stay, in /Volumes/Archive, cannot be reached.',
    copies: [
      { fileId: 301, folder: '/Volumes/Universe/Astrophotography/MBPMAX/Astro/Unsorted/2025-09-12/DARK', modified: '2025-09-12 22:46:12' },
      { fileId: 302, folder: '/Volumes/BigMac/Pictures/Import/2025-09-12', modified: '2025-09-13 07:30:55' },
    ],
  },
];

const pad = (n) => String(n).padStart(2, '0');
const stamp = (d) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
const shown = (iso) => stamp(new Date(iso));

function duplicateGroups() {
  return GROUPS.map((g) => ({
    id: g.id,
    size: g.size,
    content_hash: `${g.id}`.repeat(16),
    file_count: g.copies.length,
    file_paths: g.copies.map((c) => `${c.folder}/${g.name}`),
    file_ids: g.copies.map((c) => c.fileId),
    key: 'content',
    files: g.copies.map((c) => ({
      fileId: c.fileId,
      path: `${c.folder}/${g.name}`,
      filename: g.name,
      modifiedAt: new Date(c.modified.replace(' ', 'T')).toISOString(),
      scanRootId: ROOT.id,
      scanRootPath: ROOT.path,
      imagetyp: g.type,
      dateObs: g.date,
    })),
  }));
}

const count = (n, labelOne, label) => ({ count: n, labelOne, label });
const EXPLAIN_BH = 'Only the marked copies move. The copy that stays is never touched. Pick another copy with Keep this one.';

function blackHolePlan() {
  const values = (g, c) => {
    const v = { IMAGETYP: g.type, EXPTIME: g.exptime, 'DATE-OBS': shown(g.date), INSTRUME: 'ASI2600MM Pro', modified: c.modified };
    if (g.object) v.OBJECT = g.object;
    if (g.filter) v.FILTER = g.filter;
    return v;
  };
  return {
    columns: ['IMAGETYP', 'OBJECT', 'FILTER', 'EXPTIME', 'DATE-OBS', 'INSTRUME', 'size', 'modified'].map((k) => ({ key: k, label: k === 'size' ? 'Size' : k === 'modified' ? 'Modified' : k })),
    title: 'Move extra copies to the Black Hole',
    summary: {
      counts: [count(GROUPS.length, 'group', 'groups')],
      leftOut: [],
      warnings: [count(1, 'group needs a copy that stays', 'groups need a copy that stays')],
      facts: [],
      explain: EXPLAIN_BH,
    },
    // The assistant's text answer keeps these; the window prints none of them.
    notes: ['3 groups of byte-identical copies; the copy in the first folder of each stays'],
    groups: GROUPS.map((g, i) => ({
      id: `group:${i + 1}`,
      title: g.name,
      note: g.noKeeper ?? null,
      chooseOne: 'Copy that stays',
      chosen: g.noKeeper ? null : `item:${g.copies[0].fileId}`,
      items: g.copies.map((c, k) => ({
        id: `item:${c.fileId}`,
        fileName: g.name,
        folder: c.folder,
        values: { ...values(g, c), size: `${g.size} B` },
        action: 'Move to the Black Hole',
        evidence: k === 0 && !g.noKeeper ? null : 'Same content; bytes are compared with the copy that stays.',
        selected: k > 0 && !c.lock && !g.noKeeper,
        locked: c.lock ?? null,
        candidate: true,
        sizeBytes: g.size,
      })),
    })),
  };
}

const ROOT_DIR = '/Volumes/Universe/Astrophotography/MBPMAX/Astro/Unsorted/';
const frameName = (start, i, exp, temp) => {
  const d = new Date(start + i * (exp + 7) * 1000);
  const p2 = (n) => String(n).padStart(2, '0');
  return `${d.getUTCFullYear()}-${p2(d.getUTCMonth() + 1)}-${p2(d.getUTCDate())}_${p2(d.getUTCHours())}-${p2(d.getUTCMinutes())}-${p2(d.getUTCSeconds())}__${temp}_${exp.toFixed(2)}s_${String(i + 1).padStart(4, '0')}.fits`;
};

// One value proposed (1 frame) and a locked group of 1,829 frames with no value found: the window collapses the second to one row.
function metadataPlan() {
  const change = (n, name, folder, proposed, evidence, over = {}) => ({
    id: `item:${n}:OBJECT`,
    fileName: name,
    folder,
    values: { field: 'OBJECT', current: '(empty)', proposed },
    action: `Set OBJECT to '${proposed}'`,
    evidence,
    selected: true,
    locked: null,
    candidate: false,
    sizeBytes: null,
    ...over,
  });
  const start = Date.UTC(2025, 8, 20, 20, 31, 12);
  return {
    columns: [{ key: 'field', label: 'Field' }, { key: 'current', label: 'Now' }, { key: 'proposed', label: 'Proposed' }],
    title: 'Fill missing metadata',
    summary: {
      counts: [],
      leftOut: [count(1829, 'frame with no value found', 'frames with no value found')],
      warnings: [count(1, 'frame leaves its session', 'frames leave their sessions'), count(2, 'calibration link removed', 'calibration links removed')],
      facts: [],
      explain: 'Only the catalog changes. Files on disk are not rewritten, and a later scan keeps these values.',
    },
    notes: ['Only the catalog changes: the files on disk are not rewritten, and a frame keeps the value when Athenaeum scans its file again.'],
    groups: [
      {
        id: 'group:1',
        title: 'Object: Filter test',
        note: null,
        chooseOne: null,
        chosen: null,
        items: [change(1, frameName(Date.UTC(2025, 8, 13, 21, 40, 12), 5, 30, '-9.90'), `${ROOT_DIR}2025-09-13/LIGHT`, 'Filter test', 'the other frames of that night carry the same object name')],
      },
      {
        id: 'group:2',
        title: 'Object: nothing proposed',
        note: 'no value could be found for these frames',
        chooseOne: null,
        chosen: null,
        items: Array.from({ length: 1829 }, (_, i) =>
          change(i + 2, frameName(start, i, 300, '-10.00'), `${ROOT_DIR}2025-09-20/LIGHT`, '(none)', null, {
            selected: false,
            locked: 'No value found',
          }),
        ),
      },
    ],
  };
}

// 552 frames in 6 folders, with 650 not light frames and 339 already solved left out; folder 2 has hints on 9 frames.
function plateSolvePlan() {
  const spec = [
    ['2025-09-13', 118, 'M 78', 'L', '180 s', []],
    ['2025-09-14', 143, 'M 78', 'L', '180 s', [2, 6, 9, 31, 48, 77, 90, 101, 120]],
    ['2025-09-20', 96, 'NGC 2024', 'Ha', '300 s', []],
    ['2025-10-02', 87, 'NGC 2024', 'Ha', '300 s', []],
    ['2025-10-11', 74, 'IC 434', 'Ha', '300 s', []],
    ['2025-10-18', 34, '(none)', 'OIII', '300 s', []],
  ];
  const hints = ['05h 46m 45s  +00° 03′', '05h 46m 44s  +00° 04′', '05h 46m 46s  +00° 03′'];
  let n = 0;
  return {
    columns: [{ key: 'object', label: 'Object' }, { key: 'filter', label: 'Filter' }, { key: 'exposure', label: 'Exposure' }, { key: 'coordinates', label: 'Coordinates' }],
    title: 'Plate solve frames',
    summary: {
      counts: [count(6, 'folder', 'folders')],
      leftOut: [count(650, 'not a light frame', 'not light frames'), count(339, 'already solved', 'already solved')],
      warnings: [],
      facts: [
        { label: 'Star catalog', value: 'installed', tone: 'ok' },
        { label: 'Index packs', value: '0', tone: 'neutral' },
        { label: 'At a time', value: '3', tone: 'neutral' },
      ],
      explain: "A solve writes the frame's coordinates and, when the frame has no object name, the name of the object found in its field.",
    },
    notes: ['552 of 552 frames can be solved', 'A solved frame gets its coordinates; a frame with no object name also gets the name of the catalogued object the solve finds in its field.'],
    groups: spec.map(([day, size, object, filter, exposure, hinted], g) => {
      const [y, m, d] = day.split('-').map(Number);
      const start = Date.UTC(y, m - 1, d, 20, 31, 12);
      const exp = Number.parseInt(exposure, 10);
      const folder = `${ROOT_DIR}${day}/LIGHT`;
      return {
        id: `group:${g + 1}`,
        title: folder,
        note: null,
        chooseOne: null,
        chosen: null,
        items: Array.from({ length: size }, (_, i) => {
          n += 1;
          return {
            id: `item:${n}`,
            fileName: frameName(start, i, exp, exp === 180 ? '-9.90' : '-10.00'),
            folder,
            values: { object, filter, exposure, coordinates: hinted.includes(i) ? hints[hinted.indexOf(i) % 3] : 'none' },
            action: 'Solve for its coordinates',
            evidence: 'no plate solve is stored for this frame',
            selected: true,
            locked: null,
            candidate: false,
            sizeBytes: null,
          };
        }),
      };
    }),
  };
}

const BUILDERS = { black_hole: blackHolePlan, metadata: metadataPlan, plate_solve: plateSolvePlan };

/** The commands the scenario answers. `kind` is `HARNESS_PLAN`; the plan waits until it is answered. */
export function mcpPlanHandlers(kind = 'black_hole') {
  if (!BUILDERS[kind]) throw new Error(`HARNESS_PLAN must be black_hole, metadata or plate_solve, not ${kind}`);
  let state = { kind: 'waiting' };
  let report = null;
  let planId = `harness-${kind}-1`;
  const view = () => ({
    planId,
    kind,
    ...BUILDERS[kind](),
    source: { build: 'athenaeum-dev', version: '0.7.2', catalog: '/Volumes/Astro/Catalog/athenaeum.db' },
    state,
    replacedBy: null,
    expiresAt: state.kind === 'waiting' ? stamp(new Date(Date.now() + 30 * 60 * 1000)) : null,
    report,
  });
  const lines = (reason) => BUILDERS[kind]().groups.flatMap((g) => g.items.filter((i) => i.selected).map((i) => ({ itemId: i.id, fileName: i.fileName, folder: i.folder, reason })));
  return {
    // The Files page the plan opens over: its folder reads answer empty, never null (a null count map blanks the page).
    get_missing_files_counts: () => ({}),
    get_folder_overview: () => ({ scan_roots: [], archive_roots: [] }),
    // A page that opens finds the plan; once answered, a reload brings a new waiting plan back.
    get_mcp_plan: (args) => {
      if (args?.planId == null && state.kind !== 'waiting' && state.kind !== 'applying') {
        state = { kind: 'waiting' };
        report = null;
        planId = `harness-${kind}-${Number(planId.split('-').pop()) + 1}`;
      }
      if (state.kind === 'applying') {
        state = { kind: 'applied' };
        report = { applied: lines('done in the harness; nothing was changed'), skipped: [], failed: [], runId: kind === 'plate_solve' ? 'solve-harness' : null };
      }
      return view();
    },
    answer_mcp_plan: (args) => {
      state = args?.answer?.action === 'cancel' ? { kind: 'cancelled' } : { kind: 'applying' };
      return view();
    },
    get_duplicates: () => duplicateGroups(),
    get_scan_roots: () => [{ id: ROOT.id, path: ROOT.path, enabled: true, find_duplicates: true, unique_camera: false, last_scan: null, last_scan_errors: null, monitor_enabled: false }],
    check_all_scan_roots_availability: () => [[ROOT.id, true]],
  };
}
