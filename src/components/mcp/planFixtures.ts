// Test fixtures for the plan window: a Black Hole plan with two groups, built like the planner builds one.
import type { McpPlanView, PlanItem, PlanState } from '../../types/mcp';

export function item(n: number, over: Partial<PlanItem> = {}): PlanItem {
  return {
    id: `item:${n}`,
    fileName: `copy-${n}.fits`,
    folder: `/data/night-${n}`,
    values: { OBJECT: 'M31', size: '1.0 KB' },
    action: 'moves to the Black Hole',
    evidence: 'identical bytes to the copy that stays',
    selected: false,
    locked: null,
    candidate: true,
    sizeBytes: 1000,
    ...over,
  };
}

export function plan(over: Partial<McpPlanView> = {}): McpPlanView {
  return {
    planId: 'plan-1',
    kind: 'black_hole',
    title: 'Move duplicate copies to the Black Hole',
    source: { build: 'athenaeum-dev', version: '0.7.1', catalog: '/home/me/catalog.db' },
    state: { kind: 'waiting' } as PlanState,
    replacedBy: null,
    expiresAt: '2026-11-20 15:00:00',
    columns: [
      { key: 'OBJECT', label: 'Object' },
      { key: 'size', label: 'Size' },
    ],
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
    report: null,
    ...over,
  };
}
