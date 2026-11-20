// Truth-table parity: the same rows `catalog_query::frontend_truth` (Rust) checks against its own missing-metadata
// predicates. A row changes only when `computeMissingFlags` changes.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { computeMissingFlags, type MissingFlags } from './MissingMetadataTable';
import type { Frame } from '../../types/models';

interface TruthFrame {
  ra: number | null;
  dec: number | null;
  objctra: string | null;
  objctdec: string | null;
  object: string | null;
  date_obs: string | null;
  instrume: string | null;
  imagetyp: string | null;
}
interface Row {
  name: string;
  frame: TruthFrame;
  flags: MissingFlags;
}

const FIXTURE = resolve(__dirname, '../../../crates/athenaeum-core/tests/fixtures/frontend_truth_tables.json');
const rows = (JSON.parse(readFileSync(FIXTURE, 'utf8')) as { missingFlags: Row[] }).missingFlags;

describe('computeMissingFlags truth table', () => {
  it('reads the fixture rows', () => {
    expect(rows.length).toBeGreaterThanOrEqual(14);
  });

  it.each(rows.map((r) => [r.name, r] as const))('missing_flags_match_the_truth_table: %s', (_name, row) => {
    expect(computeMissingFlags(row.frame as unknown as Frame)).toEqual(row.flags);
  });
});
