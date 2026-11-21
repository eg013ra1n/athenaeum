// Test fixtures for the plan window: a Black Hole plan with two groups, built like the planner builds one.
import type { McpPlanView, PlanItem, PlanState } from '../../types/mcp';

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
    notes: [],
    summary: { counts: [], leftOut: [], warnings: [], facts: [], explain: null },
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
    ...over,
  });
}
