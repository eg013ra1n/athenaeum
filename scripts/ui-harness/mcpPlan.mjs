// Dev-only `mcp-plan` scenario: the assistant's plan window beside the Duplicates page, built from the same files.
//   HARNESS_SCENARIO=mcp-plan HARNESS_PLAN=black_hole|metadata|plate_solve npm run ui:harness
// The web build learns of a waiting plan by asking `get_mcp_plan({ planId: null })` on load, so the plan opens by
// itself over any page (http://localhost:1430/files; the Duplicates tab shows the groups the Black Hole plan names).
// Shapes: `src/types/mcp.ts` (`McpPlanView`) and `src/types/models.ts` (`DuplicateGroup`). Reload the page after an
// answer to get the plan back.

const GB = 1024 ** 3;
const MB = 1024 ** 2;
const ROOT = { id: 1, path: '/Volumes/Astro' };

// The files of the scenario: two groups of byte-identical copies. One copy of the master is linked by calibration
// links, so the plan locks it.
const GROUPS = [
  {
    id: 1,
    name: 'masterDark_300s_-10C.xisf',
    type: 'DARK',
    object: null,
    filter: null,
    exptime: '300',
    date: '2026-10-02T21:14:05Z',
    size: Math.round(1.08 * GB),
    copies: [
      { fileId: 101, folder: '/Volumes/Astro/Masters', modified: '2026-10-03 08:00:00' },
      { fileId: 102, folder: '/Volumes/Astro/Backup/Masters', modified: '2026-10-03 08:00:00' },
      { fileId: 103, folder: '/Volumes/Astro/Old/Masters', modified: '2026-10-03 08:00:00', lock: 'calibration links point at it; it stays in v1' },
    ],
  },
  {
    id: 2,
    name: 'M31_L_300s_0012.fits',
    type: 'LIGHT',
    object: 'M31',
    filter: 'L',
    exptime: '300',
    date: '2026-10-02T22:41:09Z',
    size: 62 * MB,
    copies: [
      { fileId: 201, folder: '/Volumes/Astro/M31/2026-10-02', modified: '2026-10-03 07:12:44' },
      { fileId: 202, folder: '/Volumes/Astro/Import/M31', modified: '2026-10-03 07:12:44' },
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

function blackHolePlan() {
  const values = (g, c) => {
    const v = { IMAGETYP: g.type, EXPTIME: g.exptime, 'DATE-OBS': shown(g.date), INSTRUME: 'ASI2600MM Pro', modified: c.modified };
    if (g.object) v.OBJECT = g.object;
    if (g.filter) v.FILTER = g.filter;
    return v;
  };
  return {
    columns: ['IMAGETYP', 'OBJECT', 'FILTER', 'EXPTIME', 'DATE-OBS', 'INSTRUME', 'size', 'modified'].map((k) => ({ key: k, label: k === 'size' ? 'Size' : k === 'modified' ? 'Modified' : k })),
    title: 'Move duplicate copies to the Black Hole',
    notes: ['2 groups of byte-identical copies; the copy in the first folder of each stays'],
    groups: GROUPS.map((g, i) => ({
      id: `group:${i + 1}`,
      title: `${g.name}: ${g.copies.length} copies`,
      note: null,
      chooseOne: 'Copy that stays',
      chosen: `item:${g.copies[0].fileId}`,
      items: g.copies.map((c, k) => ({
        id: `item:${c.fileId}`,
        fileName: g.name,
        folder: c.folder,
        values: { ...values(g, c), size: `${g.size} B` },
        action: 'Move to the Black Hole',
        evidence: k === 0 ? null : 'identical bytes to the copy that stays',
        selected: k > 0 && !c.lock,
        locked: c.lock ?? null,
        candidate: true,
        sizeBytes: g.size,
      })),
    })),
  };
}

function metadataPlan() {
  const change = (n, name, folder, field, proposed, evidence, over = {}) => ({
    id: `item:${n}:${field}`,
    fileName: name,
    folder,
    values: { field, current: '(empty)', proposed },
    action: `Set ${field} to '${proposed}'`,
    evidence,
    selected: true,
    locked: null,
    candidate: false,
    sizeBytes: null,
    ...over,
  });
  const f = (n) => `M31_L_300s_00${n}.fits`;
  return {
    columns: [{ key: 'field', label: 'Field' }, { key: 'current', label: 'Now' }, { key: 'proposed', label: 'Proposed' }],
    title: 'Fill in missing metadata',
    notes: ['Only the catalog changes: the files on disk are not rewritten, and a frame keeps the value when Athenaeum scans its file again.'],
    groups: [
      {
        id: 'group:1',
        title: 'Object: M31',
        note: null,
        chooseOne: null,
        chosen: null,
        items: [
          change(1, f(11), '/Volumes/Astro/M31/2026-10-02', 'OBJECT', 'M31', 'the other 41 frames in this folder are M31'),
          change(2, f(12), '/Volumes/Astro/M31/2026-10-02', 'OBJECT', 'M31', 'the other 41 frames in this folder are M31'),
          change(3, f(13), '/Volumes/Astro/Import/M31', 'OBJECT', 'M31', 'the file name starts with M31'),
        ],
      },
      {
        id: 'group:2',
        title: 'Camera: ASI2600MM Pro',
        note: null,
        chooseOne: null,
        chosen: null,
        items: [change(4, f(14), '/Volumes/Astro/M31/2026-10-02', 'INSTRUME', 'ASI2600MM Pro', 'every other frame of this night names this camera')],
      },
      {
        id: 'group:3',
        title: 'Object: nothing proposed',
        note: 'no value could be found for these frames',
        chooseOne: null,
        chosen: null,
        items: [
          {
            ...change(5, 'Light_0001.fits', '/Volumes/Astro/Unsorted', 'OBJECT', '(none)', null, {
              selected: false,
              locked: 'nothing in the file name, the folder or the other frames names an object',
            }),
          },
        ],
      },
    ],
  };
}

function plateSolvePlan() {
  const frame = (n, name, folder, object, coordinates, over = {}) => ({
    id: `item:${n}`,
    fileName: name,
    folder,
    values: { object, filter: 'Ha', exposure: '300 s', coordinates },
    action: 'Solve for its coordinates',
    evidence: 'no plate solve is stored for this frame',
    selected: true,
    locked: null,
    candidate: false,
    sizeBytes: null,
    ...over,
  });
  return {
    columns: [{ key: 'object', label: 'Object' }, { key: 'filter', label: 'Filter' }, { key: 'exposure', label: 'Exposure' }, { key: 'coordinates', label: 'Coordinates' }],
    title: 'Plate solve',
    notes: [
      '3 of 4 frames can be solved',
      '1 frame refused: the file is on a drive that is not connected',
      'A solved frame gets its coordinates; a frame with no object name also gets the name of the catalogued object the solve finds in its field, as a solve started in Athenaeum does.',
    ],
    groups: [
      {
        id: 'group:1',
        title: '/Volumes/Astro/M31/2026-10-02',
        note: null,
        chooseOne: null,
        chosen: null,
        items: [
          frame(1, 'M31_Ha_300s_0001.fits', '/Volumes/Astro/M31/2026-10-02', 'M31', 'RA 10.6847, Dec 41.2690'),
          frame(2, 'M31_Ha_300s_0002.fits', '/Volumes/Astro/M31/2026-10-02', 'M31', 'RA 10.6851, Dec 41.2688'),
        ],
      },
      {
        id: 'group:2',
        title: '/Volumes/Astro/Unsorted',
        note: null,
        chooseOne: null,
        chosen: null,
        items: [
          frame(3, 'Light_0003.fits', '/Volumes/Astro/Unsorted', '(none)', 'none'),
          frame(4, 'Light_0004.fits', '/Volumes/Astro/Unsorted', '(none)', 'none', { selected: false, locked: 'the file is on a drive that is not connected' }),
        ],
      },
    ],
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
