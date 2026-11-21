// Test fixtures for the plan window: a Black Hole plan with two groups, built like the planner builds one.
import type { McpPlanView, PlanGroup, PlanItem, PlanState, SummaryCount } from '../../types/mcp';

export const count = (n: number, labelOne: string, label: string): SummaryCount => ({ count: n, labelOne, label });

export function item(n: number, over: Partial<PlanItem> = {}): PlanItem {
  return {
    id: `item:${n}`,
    fileName: `copy-${n}.fits`,
    folder: `/data/night-${n}`,
    values: {
      IMAGETYP: 'LIGHT',
      OBJECT: 'M31',
      FILTER: 'L',
      EXPTIME: '300',
      'DATE-OBS': '2026-10-02 21:14:05',
      INSTRUME: 'ASI2600MM',
      size: '1000 B',
      modified: '2026-10-03 08:00:00',
    },
    action: 'moves to the Black Hole',
    evidence: 'identical bytes to the copy that stays',
    selected: false,
    locked: null,
    candidate: true,
    sizeBytes: 1000,
    ...over,
  };
}

// The columns a Black Hole plan names: header keywords the window must never show.
export const BLACK_HOLE_COLUMNS = [
  { key: 'IMAGETYP', label: 'IMAGETYP' },
  { key: 'OBJECT', label: 'OBJECT' },
  { key: 'FILTER', label: 'FILTER' },
  { key: 'EXPTIME', label: 'EXPTIME' },
  { key: 'DATE-OBS', label: 'DATE-OBS' },
  { key: 'INSTRUME', label: 'INSTRUME' },
  { key: 'size', label: 'Size' },
  { key: 'modified', label: 'Modified' },
];

export function plan(over: Partial<McpPlanView> = {}): McpPlanView {
  return {
    planId: 'plan-1',
    kind: 'black_hole',
    title: 'Move duplicate copies to the Black Hole',
    source: { build: 'athenaeum-dev', version: '0.7.1', catalog: '/home/me/catalog.db' },
    state: { kind: 'waiting' } as PlanState,
    replacedBy: null,
    expiresAt: '2026-11-20 15:00:00',
    columns: BLACK_HOLE_COLUMNS,
    groups: [
      {
        id: 'group:1',
        title: 'M31 Light 300 s (3 copies)',
        note: null,
        chooseOne: 'Copy that stays',
        chosen: 'item:1',
        items: [
          item(1),
          item(2, { selected: true, sizeBytes: 1000 }),
          item(3, { selected: true, sizeBytes: 2000 }),
        ],
      },
      {
        id: 'group:2',
        title: 'M42 Flat (2 copies)',
        note: 'checked byte for byte',
        chooseOne: 'Copy that stays',
        chosen: 'item:4',
        items: [
          item(4),
          item(5, { selected: true, sizeBytes: 4000 }),
          item(6, { locked: 'calibration links point at it; it stays in v1', sizeBytes: 500 }),
        ],
      },
    ],
    // The backend still sends its notes for the assistant's text answer; the window prints none of them.
    notes: ['2 groups of byte-identical copies; the copy in the first folder of each stays'],
    summary: {
      counts: [count(2, 'group', 'groups')],
      leftOut: [],
      warnings: [],
      facts: [],
      explain: 'Only the marked copies move. The copy that stays is never touched. Pick another copy with Keep this one.',
    },
    report: null,
    ...over,
  };
}

/** A metadata plan as the planner builds one: one group per proposed value, items are (frame, field). */
export function metadataPlan(over: Partial<McpPlanView> = {}): McpPlanView {
  const change = (n: number, field: string, proposed: string, over: Partial<PlanItem> = {}): PlanItem => ({
    id: `item:${n}:${field}`,
    fileName: `frame-${n}.fits`,
    folder: `/data/night-${n}`,
    values: { field, current: '(empty)', proposed },
    action: `Set ${field} to '${proposed}'`,
    evidence: 'the folder name says M31',
    selected: true,
    locked: null,
    candidate: false,
    sizeBytes: null,
    ...over,
  });
  return plan({
    kind: 'metadata',
    title: 'Fill in missing metadata',
    columns: [
      { key: 'field', label: 'Field' },
      { key: 'current', label: 'Now' },
      { key: 'proposed', label: 'Proposed' },
    ],
    groups: [
      { id: 'group:1', title: 'Object: M31', note: null, chooseOne: null, chosen: null, items: [change(1, 'OBJECT', 'M31'), change(2, 'OBJECT', 'M31', { locked: 'the file is on a drive that is not connected', selected: false })] },
      { id: 'group:2', title: 'Camera: ASI2600MM', note: null, chooseOne: null, chosen: null, items: [change(3, 'INSTRUME', 'ASI2600MM')] },
    ],
    notes: ['a metadata note the window does not print'],
    summary: { counts: [], leftOut: [], warnings: [], facts: [], explain: 'Only the catalog changes. Files on disk are not rewritten, and a later scan keeps these values.' },
    ...over,
  });
}

/** A plate-solve plan as the planner builds one: one group per folder, no sizes. */
export function plateSolvePlan(over: Partial<McpPlanView> = {}): McpPlanView {
  const frame = (n: number, object: string, over: Partial<PlanItem> = {}): PlanItem => ({
    id: `item:${n}`,
    fileName: `light-${n}.fits`,
    folder: '/data/night-1',
    values: { object, filter: 'Ha', exposure: '300 s', coordinates: n % 2 ? 'RA 10.6847, Dec 41.2690' : 'none' },
    action: 'Solve for its coordinates',
    evidence: 'no plate solve is stored for this frame',
    selected: true,
    locked: null,
    candidate: false,
    sizeBytes: null,
    ...over,
  });
  return plan({
    kind: 'plate_solve',
    title: 'Plate solve',
    columns: [
      { key: 'object', label: 'Object' },
      { key: 'filter', label: 'Filter' },
      { key: 'exposure', label: 'Exposure' },
      { key: 'coordinates', label: 'Coordinates' },
    ],
    groups: [
      {
        id: 'group:1',
        title: '/data/night-1',
        note: null,
        chooseOne: null,
        chosen: null,
        items: [frame(1, 'M31'), frame(2, '(none)'), frame(3, 'M31', { locked: 'the star catalog is not installed', selected: false })],
      },
    ],
    notes: ['3 of 3 frames can be solved'],
    summary: {
      counts: [count(1, 'folder', 'folders')],
      leftOut: [],
      warnings: [],
      facts: [{ label: 'Star catalog', value: 'installed', tone: 'ok' }],
      explain: "A solve writes the frame's coordinates.",
    },
    ...over,
  });
}

const pad = (n: number, w = 2) => String(n).padStart(w, '0');

/** A Black Hole plan as the mockup shows it: 3 groups, one needs a copy that stays, one master stays (its calibration links). */
export function blackHoleMockupPlan(over: Partial<McpPlanView> = {}): McpPlanView {
  const MB = 1024 * 1024;
  const copy = (id: number, name: string, folder: string, size: number, over: Partial<PlanItem> = {}): PlanItem =>
    item(id, { fileName: name, folder, sizeBytes: Math.round(size * MB), values: { ...item(id).values, size: `${size} MB` }, ...over });
  const light = '2025-09-14_01-25-40__-9.90_180.00s_0006.fits';
  const dark = '2025-09-12_22-41-07__-10.00_300.00s_0001.fits';
  const group = (g: Partial<PlanGroup> & Pick<PlanGroup, 'id' | 'title' | 'items'>): PlanGroup => ({ note: null, chooseOne: 'Copy that stays', chosen: null, ...g });
  return plan({
    groups: [
      group({
        id: 'group:1',
        title: light,
        chosen: 'item:11',
        items: [
          copy(11, light, '/Volumes/Universe/Astrophotography/MBPMAX/Astro/Unsorted/2025-09-13/LIGHT', 42.3, { evidence: null }),
          copy(12, light, '/Volumes/BigMac/Pictures/Import/2025-09-13', 42.3, { selected: true }),
          copy(13, light, '/Volumes/Universe/Astrophotography/Backup/2025-09-13/LIGHT', 42.3, { selected: true }),
        ],
      }),
      group({
        id: 'group:2',
        title: 'masterFlat_L_2025-09-14.xisf',
        chosen: 'item:21',
        items: [
          copy(21, 'masterFlat_L_2025-09-14.xisf', '/Volumes/Universe/Astrophotography/Masters/Flat', 118.4, {
            locked: '3 calibration links point at this copy, so it stays',
            evidence: null,
            values: { ...item(21).values, IMAGETYP: 'FLAT' },
          }),
          copy(22, 'masterFlat_L_2025-09-14.xisf', '/Volumes/BigMac/Pictures/Import', 118.4, { selected: true, values: { ...item(22).values, IMAGETYP: 'FLAT' } }),
        ],
      }),
      group({
        id: 'group:3',
        title: dark,
        chosen: null,
        note: 'No copy is picked here: the copy that would stay, in /Volumes/Archive, cannot be reached.',
        items: [
          copy(31, dark, '/Volumes/Universe/Astrophotography/MBPMAX/Astro/Unsorted/2025-09-12/DARK', 54.9, { values: { ...item(31).values, IMAGETYP: 'DARK' } }),
          copy(32, dark, '/Volumes/BigMac/Pictures/Import/2025-09-12', 54.9, { values: { ...item(32).values, IMAGETYP: 'DARK' } }),
        ],
      }),
    ],
    summary: {
      counts: [count(3, 'group', 'groups')],
      leftOut: [],
      warnings: [count(1, 'group needs a copy that stays', 'groups need a copy that stays')],
      facts: [],
      explain: 'Only the marked copies move. The copy that stays is never touched. Pick another copy with Keep this one.',
    },
    ...over,
  });
}

/** A plate-solve plan at the mockup's volume: 552 frames in 6 folders, 650 not light frames and 339 already solved left out. */
export function plateSolveVolumePlan(over: Partial<McpPlanView> = {}): McpPlanView {
  const spec: Array<[string, number, string, string, string, number[]]> = [
    ['2025-09-13', 118, 'M 78', 'L', '180 s', []],
    ['2025-09-14', 143, 'M 78', 'L', '180 s', [2, 6, 9, 31, 48, 77, 90, 101, 120]],
    ['2025-09-20', 96, 'NGC 2024', 'Ha', '300 s', []],
    ['2025-10-02', 87, 'NGC 2024', 'Ha', '300 s', []],
    ['2025-10-11', 74, 'IC 434', 'Ha', '300 s', []],
    ['2025-10-18', 34, '(none)', 'OIII', '300 s', []],
  ];
  const hints = ['05h 46m 45s  +00° 03′', '05h 46m 44s  +00° 04′', '05h 46m 46s  +00° 03′'];
  let n = 0;
  const groups = spec.map(([day, size, object, filter, exposure, hinted], g): PlanGroup => {
    const folder = `/Volumes/Universe/Astrophotography/MBPMAX/Astro/Unsorted/${day}/LIGHT`;
    return {
      id: `group:${g + 1}`,
      title: folder,
      note: null,
      chooseOne: null,
      chosen: null,
      items: Array.from({ length: size }, (_, i): PlanItem => {
        n += 1;
        return {
          id: `item:${n}`,
          fileName: `${day}_20-31-12__light_${pad(i + 1, 4)}.fits`,
          folder,
          values: { object, filter, exposure, coordinates: hinted.includes(i) ? (hints[hinted.indexOf(i) % 3] ?? 'none') : 'none' },
          action: 'Solve for its coordinates',
          evidence: 'no plate solve is stored for this frame',
          selected: true,
          locked: null,
          candidate: false,
          sizeBytes: null,
        };
      }),
    };
  });
  return plateSolvePlan({
    groups,
    notes: ['552 of 552 frames can be solved'],
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
    ...over,
  });
}

/** A metadata plan at the mockup's volume: one value proposed, and a locked group of 1,829 frames with no value found. */
export function metadataVolumePlan(over: Partial<McpPlanView> = {}): McpPlanView {
  const row = (n: number, locked: string | null): PlanItem => ({
    id: `item:${n}:OBJECT`,
    fileName: `2025-09-20_20-31-12__frame_${pad(n, 4)}.fits`,
    folder: '/Volumes/Universe/Astrophotography/MBPMAX/Astro/Unsorted/2025-09-20/LIGHT',
    values: { field: 'OBJECT', current: '(empty)', proposed: locked ? '(none)' : 'Filter test' },
    action: 'Set OBJECT',
    evidence: locked ? null : 'the other frames of that night carry the same object name',
    selected: locked === null,
    locked,
    candidate: false,
    sizeBytes: null,
  });
  return metadataPlan({
    groups: [
      { id: 'group:1', title: 'Object: Filter test', note: null, chooseOne: null, chosen: null, items: [row(1, null)] },
      {
        id: 'group:2',
        title: 'Object: nothing proposed',
        note: 'no value could be found for these frames',
        chooseOne: null,
        chosen: null,
        items: Array.from({ length: 1829 }, (_, i) => row(i + 2, 'No value found')),
      },
    ],
    notes: ['1 value can be written'],
    summary: {
      counts: [],
      leftOut: [count(1829, 'frame with no value found', 'frames with no value found')],
      warnings: [count(1, 'frame leaves its session', 'frames leave their sessions'), count(2, 'calibration link removed', 'calibration links removed')],
      facts: [],
      explain: 'Only the catalog changes. Files on disk are not rewritten, and a later scan keeps these values.',
    },
    ...over,
  });
}
