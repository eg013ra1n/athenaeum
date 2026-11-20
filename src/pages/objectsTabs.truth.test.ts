// Truth-table parity for the Objects page's Stage / WIP / Archive rule, the same rows the Rust core reads.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { frameSetTab } from './objectsTabs';
import type { FramesSetWithCount } from '../types/helpers';

interface Row {
  isCustom: boolean;
  isArchived: boolean;
  tab: 'stage' | 'wip' | 'archive';
}

const FIXTURE = resolve(__dirname, '../../crates/athenaeum-core/tests/fixtures/frontend_truth_tables.json');
const rows = (JSON.parse(readFileSync(FIXTURE, 'utf8')) as { frameSetTab: Row[] }).frameSetTab;

describe('frameSetTab truth table', () => {
  it('reads the fixture rows', () => {
    expect(rows.length).toBe(4);
  });

  it.each(rows.map((r) => [`custom=${r.isCustom} archived=${r.isArchived}`, r] as const))(
    'frame_set_tab_matches_the_truth_table: %s',
    (_name, row) => {
      const fs = {
        frames_set: { is_custom: row.isCustom, is_archived: row.isArchived },
        member_count: 0,
        zip_member_count: 0,
        duplicate_count: 0,
      } as unknown as FramesSetWithCount;
      expect(frameSetTab(fs)).toBe(row.tab);
    },
  );
});
