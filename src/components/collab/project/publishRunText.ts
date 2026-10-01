import type { CollabPublishFinished, PublishMode, PublishStage } from '../../../types/models';
import type { Segment } from './MyFramesTab';
import { HUB_OUTDATED_TEXT, isOutdated, publishingDeviceRefusal } from './usePublishing';

/** The one stage wording of a publish run (settings card status, My frames run panel). */
export const STAGE_TITLE: Record<PublishStage, string> = {
  queued: 'Waiting for a compute slot',
  calibrating: 'Calibrating',
  seeding: 'Seeding',
  announcing: 'Announcing',
  versions: 'Posting new versions',
};

/** The one wording of a project's publish mode. */
export const MODE_LABEL: Record<PublishMode, string> = {
  manual: 'Manual',
  autoCalibrate: 'Auto-calibrate',
  automatic: 'Fully automatic',
};

/** One line for a finished run, and the My frames segment it points at (spec §7.3, §8.1). */
export function describeLastRun(last: CollabPublishFinished): {
  text: string;
  segment: Segment | null;
  tone: 'ok' | 'warn' | 'error';
} {
  const sent = last.announced + last.updated;
  const tail =
    (last.stale > 0 ? ` · ${last.stale} back to Ready` : '') +
    (last.heldBack > 0 ? ` · ${last.heldBack} held back` : '');
  switch (last.outcome) {
    case 'cancelled':
      return { text: 'Stopped', segment: null, tone: 'warn' };
    case 'refused': {
      // An outdated build: the update sentence, never the raw code — without
      // its full stop, as the card goes on with " · {time}".
      if (last.error && isOutdated(last.error)) {
        return { text: HUB_OUTDATED_TEXT.replace(/\.$/, ''), segment: null, tone: 'warn' };
      }
      const device = last.error ? publishingDeviceRefusal(last.error) : null;
      return {
        text: `Not run — ${device ? `${device} publishes this project` : (last.error ?? 'refused')}`,
        segment: null,
        tone: 'warn',
      };
    }
    case 'failed':
      return { text: `Failed — ${last.error ?? 'unknown error'}`, segment: null, tone: 'error' };
    case 'done':
      if (sent > 0) return { text: `Published ${sent}${tail}`, segment: 'published', tone: 'ok' };
      if (last.calibrated > 0) return { text: `Calibrated ${last.calibrated}${tail}`, segment: 'review', tone: 'ok' };
      if (last.stale > 0 && last.heldBack === 0) {
        // Only frames whose source changed since calibration went back to
        // Ready: what the stale-only notification says (§16.1 N15).
        return {
          text: `${last.stale} ${last.stale === 1 ? 'frame' : 'frames'} back to Ready — changed since calibration`,
          segment: 'ready',
          tone: 'warn',
        };
      }
      if (last.heldBack > 0) {
        return {
          text: `${last.kind === 'calibrate' ? 'Nothing calibrated' : 'Nothing new to publish'}${tail}`,
          segment: 'held',
          tone: 'warn',
        };
      }
      return {
        text: last.kind === 'calibrate' ? 'Nothing to calibrate' : 'Nothing to publish',
        segment: null,
        tone: 'ok',
      };
  }
}
