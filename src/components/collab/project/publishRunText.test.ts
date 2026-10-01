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
  ])('%#', (last, text, segment, tone) => {
    expect(describeLastRun(last)).toEqual({ text, segment, tone });
  });
});
