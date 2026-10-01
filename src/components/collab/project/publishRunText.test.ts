import { describe, expect, it } from 'vitest';
import type { CollabPublishFinished } from '../../../types/models';
import { describeLastRun } from './publishRunText';

const fin = (o: Partial<CollabPublishFinished>): CollabPublishFinished => ({
  projectId: 'p1', publishRunId: 'r', kind: 'publish', trigger: 'manual', outcome: 'done', calibrated: 0,
  announced: 0, updated: 0, stale: 0, heldBack: 0, error: null,
  startedAt: '2026-10-02T10:00:00Z', finishedAt: '2026-10-02T10:03:22Z', ...o,
});

describe('describeLastRun', () => {
  it.each([
    [fin({ kind: 'calibrate', calibrated: 46, heldBack: 2 }), 'Calibrated 46 · 2 held back', 'review', 'ok'],
    [fin({ kind: 'calibrate' }), 'Nothing to calibrate', null, 'ok'],
    [fin({ kind: 'auto', calibrated: 5 }), 'Calibrated 5', 'review', 'ok'],
    [fin({ announced: 3, updated: 1, stale: 2 }), 'Published 4 · 2 back to Ready', 'published', 'ok'],
    [fin({ heldBack: 2 }), 'Nothing new to publish · 2 held back', 'held', 'warn'],
    [fin({ outcome: 'cancelled' }), 'Stopped', null, 'warn'],
    [fin({ outcome: 'refused', error: 'publication of this project is already running' }),
      'Not run — publication of this project is already running', null, 'warn'],
    [fin({ outcome: 'refused', trigger: 'auto', error: 'collab_publishing_device:Obs PC' }),
      'Not run — Obs PC publishes this project', null, 'warn'],
    [fin({ outcome: 'failed', error: 'disk full' }), 'Failed — disk full', null, 'error'],
    // A line, not a sentence: the card puts " · {time}" after it.
    [fin({ outcome: 'refused', error: 'collab_api_outdated: the hub requires collab API 4' }),
      'This hub needs a newer Athenaeum — update to publish', null, 'warn'],
    // Only frames sent back to Ready: what the stale-only toast says.
    [fin({ stale: 2 }), '2 frames back to Ready — changed since calibration', 'ready', 'warn'],
    [fin({ kind: 'calibrate', stale: 1 }), '1 frame back to Ready — changed since calibration', 'ready', 'warn'],
    // The free-space refusal names both sizes, never the raw code.
    [fin({ outcome: 'refused', trigger: 'auto', kind: 'auto', error: 'collab_no_space:12500000000:3200000000' }),
      'Not run — not enough free space (12.5 GB needed, 3.2 GB free)', null, 'warn'],
  ])('%#', (last, text, segment, tone) => {
    expect(describeLastRun(last)).toEqual({ text, segment, tone });
  });
});
