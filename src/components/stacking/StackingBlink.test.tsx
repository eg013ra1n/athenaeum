// Blink over the Frames table's rows (spec §6.7): the originals of a frame set or a project, with the stacking actions
// in place of the Black Hole. `BlinkViewer` is replaced by a stub that runs the real project-mode hook over a selection
// the test picks, so the actions it lists are the ones the viewer would offer.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { useState } from 'react';
import { api } from '../../api';
import { routeInvoke, unexpectedCommand } from '../../test/apiMock';
import type { BlinkViewerProps } from '../blink/types';
import type { CollabBlinkEntry, FileWithFrame } from '../../types/models';
import type { AlignedFrameStatus, PlanGroup, StackingRunDetail } from '../../types/stacking';
import type { FrameRow } from './frameRows';
import { StackingBlink } from './StackingBlink';
import type { StackSubject } from './subject';

vi.mock('../../api');

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

let blink: BlinkViewerProps | null = null;
vi.mock('../BlinkViewer', async () => {
  const { useBlinkProjectMode } = await import('../blink/useBlinkProjectMode');
  return {
    default: (p: BlinkViewerProps) => {
      blink = p;
      const [selected, setSelected] = useState<ReadonlySet<number>>(new Set());
      const mode = useBlinkProjectMode(p.frames, p.actions, selected as Set<number>);
      const toggle = (i: number) =>
        setSelected((prev) => {
          const next = new Set(prev);
          if (next.has(i)) next.delete(i);
          else next.add(i);
          return next;
        });
      return (
        <div data-testid="blink">
          {mode.listFrames.map((f, i) => (
            <div key={f.key} data-testid="entry">
              <button type="button" onClick={() => toggle(i)}>{`pick ${f.file.filename}`}</button>
              <span>{`${f.file.filename}|${f.badge ?? ''}`}</span>
            </div>
          ))}
          {(mode.projectActions ?? []).map((a) => (
            <button key={a.id} type="button" onClick={a.onClick}>{a.label}</button>
          ))}
        </div>
      );
    },
  };
});

afterEach(cleanup);
beforeEach(() => {
  blink = null;
  notifyMock.mockClear();
  vi.mocked(api.invoke).mockReset();
});

const SET: StackSubject = { kind: 'frameSet', framesSetId: 7, lightFrames: [] };
const PROJECT: StackSubject = { kind: 'project', projectId: 'p1', title: 'Project' };

const row = (key: string, over: Partial<FrameRow> = {}): FrameRow => ({
  key, frameId: Number(key) || null, filename: `${key}.fit`, publisher: null, groupKey: 'L-mono', night: null,
  exptimeSec: 300, focalLenMm: null, weight: null, fwhmPx: null, eccentricity: null, stars: null, regRmsPx: null, lnScale: null,
  regInliers: null, regModel: null,
  outcome: { kind: 'included', label: 'Included', tone: 'success', reason: null },
  outOfNextRun: false, editable: true, unavailable: null, pinnable: true, pinned: false, pinNote: null,
  ...over,
});

/** A catalog file + frame as `get_files_with_frames_by_ids` returns it. */
const fileFrame = (frameId: number): FileWithFrame =>
  ({
    file: { id: frameId * 10, path: `/d/${frameId}.fit`, filename: `${frameId}.fit`, format: 'FITS' },
    frame: { id: frameId, imagetyp: 'Light' },
  }) as unknown as FileWithFrame;

const collabEntry = (key: string, source: CollabBlinkEntry['source'] = 'replica'): CollabBlinkEntry => ({
  key, source,
  entry: { file: { id: null, path: `/c/${key}.fit`, filename: `${key}.fit`, format: 'FITS' }, frame: null } as unknown as FileWithFrame,
  frameUuid: key, sourceFrameId: null, publisherName: null,
});

const PLAN_GROUPS = [
  { key: 'L-mono', filter: 'L', exposureClass: '300s', colorMode: 'mono', binning: 1 },
] as unknown as PlanGroup[];

/** The backend a frame set's Blink reads: the frames asked for, in date order (here: the order the test gives). */
function setBackend(order: number[]) {
  routeInvoke({
    get_files_with_frames_by_ids: (args?: { frameIds?: number[] }) => {
      const asked = new Set(args?.frameIds ?? []);
      return order.filter((id) => asked.has(id)).map(fileFrame);
    },
  }, unexpectedCommand);
}

function props(over: Partial<React.ComponentProps<typeof StackingBlink>> = {}): React.ComponentProps<typeof StackingBlink> {
  return {
    subject: SET, rows: [row('1'), row('2')], planGroups: PLAN_GROUPS, runDetail: null, excludedKeys: [],
    pinnedKey: null, disabled: false, onSetExcluded: vi.fn(), onPin: vi.fn(), onClose: vi.fn(), ...over,
  };
}

const pick = (file: string) => fireEvent.click(screen.getByRole('button', { name: `pick ${file}` }));
const opened = () => waitFor(() => expect(blink).not.toBeNull());

describe('StackingBlink', () => {
  it('blink_skips_rows_without_uuid', async () => {
    vi.mocked(api.invoke).mockResolvedValueOnce([collabEntry('u1')] as never);
    const rows = [row('u1'), row('u2'), row('h:9', { editable: false })];
    render(<StackingBlink {...props({ subject: PROJECT, rows })} />);
    await opened();

    expect(api.invoke).toHaveBeenCalledTimes(1);
    expect(api.invoke).toHaveBeenCalledWith('get_collab_blink_frames', {
      projectId: 'p1',
      refs: [{ frameId: null, frameUuid: 'u1' }, { frameId: null, frameUuid: 'u2' }],
    });
    expect(notifyMock).toHaveBeenCalledWith({
      title: 'Some frames are not on this device',
      detail: '2 of 3 frames are not on this device.',
      kind: 'stacking',
      tone: 'warning',
    });
    expect(blink!.frames.map((f) => f.key)).toEqual(['u1']);
  });

  it('a project entry that is not a raw file loads through its frame ref', async () => {
    vi.mocked(api.invoke).mockResolvedValueOnce([collabEntry('u1', 'replica'), collabEntry('u2', 'raw')] as never);
    render(<StackingBlink {...props({ subject: PROJECT, rows: [row('u1'), row('u2')] })} />);
    await opened();

    expect(notifyMock).not.toHaveBeenCalled();
    expect(blink!.frames[0]!.imageRef).toEqual({ projectId: 'p1', frame: { frameId: null, frameUuid: 'u1' } });
    expect(blink!.frames[1]!.imageRef).toBeUndefined();
    expect(blink!.frameSetId).toBeUndefined();
  });

  it('every project entry, a raw one too, carries the ref its stars load by', async () => {
    vi.mocked(api.invoke).mockResolvedValueOnce([collabEntry('u1', 'replica'), collabEntry('u2', 'raw')] as never);
    render(<StackingBlink {...props({ subject: PROJECT, rows: [row('u1'), row('u2')] })} />);
    await opened();

    expect(blink!.frames.map((f) => f.starsRef)).toEqual([
      { projectId: 'p1', frame: { frameId: null, frameUuid: 'u1' } },
      { projectId: 'p1', frame: { frameId: null, frameUuid: 'u2' } },
    ]);
  });

  it('nothing on this device closes Blink after the notice, and opens nothing', async () => {
    vi.mocked(api.invoke).mockResolvedValueOnce([] as never);
    const onClose = vi.fn();
    render(<StackingBlink {...props({ subject: PROJECT, rows: [row('u1'), row('u2')], onClose })} />);

    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    expect(notifyMock).toHaveBeenCalledWith(expect.objectContaining({
      title: 'Some frames are not on this device', detail: '2 of 2 frames are not on this device.',
    }));
    expect(blink).toBeNull();
  });

  it('rows that cannot be asked for at all close Blink with the notice, without a backend call', async () => {
    const onClose = vi.fn();
    render(<StackingBlink {...props({ subject: PROJECT, rows: [row('h:9', { editable: false })], onClose })} />);

    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    expect(api.invoke).not.toHaveBeenCalled();
    expect(notifyMock).toHaveBeenCalledWith(expect.objectContaining({ detail: '1 of 1 frames are not on this device.' }));
  });

  it('a set loads its frames by id, opens with the set for the frame list, and counts the missing ones', async () => {
    setBackend([1, 3]);
    render(<StackingBlink {...props({ rows: [row('1'), row('2'), row('3')] })} />);
    await opened();

    expect(api.invoke).toHaveBeenCalledWith('get_files_with_frames_by_ids', { frameIds: [1, 2, 3] });
    expect(blink!.frameSetId).toBe(7);
    expect(blink!.frames.map((f) => f.key)).toEqual(['1', '3']);
    expect(blink!.frames.every((f) => f.imageRef === undefined)).toBe(true);
    expect(notifyMock).toHaveBeenCalledWith(expect.objectContaining({ detail: '1 of 3 frames are not on this device.' }));
  });

  it('blink_keeps_table_order', async () => {
    setBackend([3, 1, 2]); // date order, not the table's
    render(<StackingBlink {...props({ rows: [row('1'), row('2'), row('3')] })} />);
    await opened();

    expect(blink!.frames.map((f) => f.key)).toEqual(['1', '2', '3']);
    expect(blink!.frames.map((f) => f.file.filename)).toEqual(['1.fit', '2.fit', '3.fit']);
    expect(notifyMock).not.toHaveBeenCalled();
  });

  it('a failed load is logged and notified, and closes Blink', async () => {
    vi.mocked(api.invoke).mockRejectedValueOnce(new Error('db locked'));
    const logged = vi.spyOn(console, 'error').mockImplementation(() => {});
    const onClose = vi.fn();
    render(<StackingBlink {...props({ onClose })} />);

    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    expect(logged).toHaveBeenCalledWith('[StackingBlink] loading the frames failed:', expect.any(Error));
    expect(notifyMock).toHaveBeenCalledWith({
      title: 'Could not open Blink', detail: 'db locked', kind: 'stacking', tone: 'warning', hasErrors: true,
    });
    logged.mockRestore();
  });

  it('blink_actions_follow_eligibility', async () => {
    setBackend([1, 2]);
    render(<StackingBlink {...props({ excludedKeys: ['2'] })} />);
    await opened();

    pick('1.fit');
    pick('2.fit');
    expect(screen.getByRole('button', { name: 'Exclude from next run (1)' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Include (1)' })).toBeInTheDocument();
    // Two pinnable entries are selected: the single-target star waits for one.
    expect(screen.queryByRole('button', { name: 'Use as reference' })).toBeNull();

    pick('2.fit'); // back to 1.fit alone
    expect(screen.getByRole('button', { name: 'Exclude from next run (1)' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Include/ })).toBeNull();
    expect(screen.getByRole('button', { name: 'Use as reference' })).toBeInTheDocument();
  });

  it('a row the table cannot write is offered no action; an unpinnable one no star', async () => {
    setBackend([1, 2]);
    const rows = [row('1', { editable: false, pinnable: false }), row('2', { pinnable: false })];
    render(<StackingBlink {...props({ rows })} />);
    await opened();

    pick('1.fit');
    // Nothing but the stub's pick buttons: no Exclude (not editable), no Include, no star.
    expect(screen.getAllByRole('button').map((b) => b.textContent)).toEqual(['pick 1.fit', 'pick 2.fit']);
    pick('1.fit');
    pick('2.fit');
    expect(screen.getByRole('button', { name: 'Exclude from next run (1)' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Use as reference' })).toBeNull();
  });

  it('the pinned frame is not offered as the reference again', async () => {
    setBackend([1, 2]);
    render(<StackingBlink {...props({ pinnedKey: '1' })} />);
    await opened();

    pick('1.fit');
    expect(screen.queryByRole('button', { name: 'Use as reference' })).toBeNull();
    pick('1.fit');
    pick('2.fit');
    expect(screen.getByRole('button', { name: 'Use as reference' })).toBeInTheDocument();
  });

  it('hides every action while a run is active', async () => {
    setBackend([1, 2]);
    render(<StackingBlink {...props({ disabled: true })} />);
    await opened();

    pick('1.fit');
    pick('2.fit');
    expect(blink!.actions).toEqual([]);
    expect(screen.queryByRole('button', { name: /Exclude|Include|reference/ })).toBeNull();
  });

  it('Exclude, Include and Use as reference hand the eligible keys to the table’s writers', async () => {
    setBackend([1, 2, 3]);
    const onSetExcluded = vi.fn();
    const onPin = vi.fn();
    render(<StackingBlink {...props({ rows: [row('1'), row('2'), row('3')], excludedKeys: ['3'], onSetExcluded, onPin })} />);
    await opened();

    pick('1.fit');
    pick('2.fit');
    pick('3.fit');
    fireEvent.click(screen.getByRole('button', { name: 'Exclude from next run (2)' }));
    await waitFor(() => expect(onSetExcluded).toHaveBeenCalledWith(['1', '2'], true));
    fireEvent.click(screen.getByRole('button', { name: 'Include (1)' }));
    await waitFor(() => expect(onSetExcluded).toHaveBeenCalledWith(['3'], false));

    pick('1.fit');
    pick('2.fit'); // 3.fit alone
    fireEvent.click(screen.getByRole('button', { name: 'Use as reference' }));
    await waitFor(() => expect(onPin).toHaveBeenCalledWith('3'));
  });

  it('badge_flips_after_exclude', async () => {
    setBackend([1, 2]);
    const onSetExcluded = vi.fn();
    const view = (excludedKeys: string[]) => <StackingBlink {...props({ excludedKeys, onSetExcluded })} />;
    const { rerender } = render(view([]));
    await opened();
    expect(screen.getByText('1.fit|Included')).toBeInTheDocument();

    pick('1.fit');
    fireEvent.click(screen.getByRole('button', { name: 'Exclude from next run (1)' }));
    await waitFor(() => expect(onSetExcluded).toHaveBeenCalledWith(['1'], true));
    // The parent writes the list and renders the key listed; the snapshot of entries stays, the badge follows.
    rerender(view(['1']));
    expect(screen.getByText('1.fit|Included · out of next run')).toBeInTheDocument();
    expect(screen.getByText('2.fit|Included')).toBeInTheDocument();
    // The action followed the entry: it can be included now, not excluded again.
    expect(screen.queryByRole('button', { name: /Exclude from next run/ })).toBeNull();
    expect(screen.getByRole('button', { name: 'Include (1)' })).toBeInTheDocument();
  });

  it('a listed frame with no outcome yet shows only that it is out of the next run, and an unlisted one no badge', async () => {
    setBackend([1, 2]);
    render(<StackingBlink {...props({ rows: [row('1', { outcome: null }), row('2', { outcome: null })], excludedKeys: ['1'] })} />);
    await opened();

    expect(blink!.frames.map((f) => f.badge)).toEqual(['out of next run', undefined]);
  });

  it('the strip names the group and the outcome’s reason', async () => {
    setBackend([1, 2, 3]);
    const failed = { kind: 'regFailed', label: 'Reg failed', tone: 'error', reason: '0 quad matches' } as const;
    const rows = [row('1', { outcome: failed }), row('2'), row('3', { groupKey: null })];
    render(<StackingBlink {...props({ rows })} />);
    await opened();

    expect(blink!.frames.map((f) => blink!.contextLabel!(f))).toEqual([
      'L · 300s · mono · 0 quad matches',
      'L · 300s · mono',
      'Not grouped yet',
    ]);
  });

  describe('over a run', () => {
    const RUN = { run: { id: 8 }, groups: [], frames: [], summary: null } as unknown as StackingRunDetail;

    /** The frame set's frames and the run's verdict on them (or a failure of that one call). */
    function setRunBackend(order: number[], aligned: AlignedFrameStatus[] | Error) {
      routeInvoke({
        get_stacking_aligned_frames: () => {
          if (aligned instanceof Error) throw aligned;
          return aligned;
        },
        get_files_with_frames_by_ids: (args?: { frameIds?: number[] }) => {
          const asked = new Set(args?.frameIds ?? []);
          return order.filter((id) => asked.has(id)).map(fileFrame);
        },
      }, unexpectedCommand);
    }
    const alignedCalls = () => vi.mocked(api.invoke).mock.calls.filter(([c]) => c === 'get_stacking_aligned_frames');

    it('maps_aligned_states_to_reasons', async () => {
      setRunBackend([1, 2, 3, 4, 5], [
        { frameId: 1, state: 'available' },
        { frameId: 2, state: 'registrationFailed' },
        { frameId: 3, state: 'notRegistered' },
        { frameId: 4, state: 'missing' },
        { frameId: 5, state: 'replaced' },
      ]);
      render(<StackingBlink {...props({ runDetail: RUN, rows: ['1', '2', '3', '4', '5'].map((k) => row(k)) })} />);
      await opened();

      // One call, at open, with each row's run frame id.
      expect(alignedCalls()).toEqual([['get_stacking_aligned_frames', { runId: 8, frameIds: [1, 2, 3, 4, 5] }]]);
      expect(blink!.frames.map((f) => f.aligned)).toEqual([
        { runId: 8, frameId: 1 },
        { runId: 8, reason: 'registration failed in run #8' },
        { runId: 8, reason: 'not registered in run #8' },
        { runId: 8, reason: 'not in the working folder any more' },
        { runId: 8, reason: 'a later run replaced it' },
      ]);
      expect(notifyMock).not.toHaveBeenCalled();
    });

    it('a project row is asked for by its run frame id, not its uuid', async () => {
      routeInvoke({
        get_stacking_aligned_frames: () => [{ frameId: 31, state: 'available' }],
        get_collab_blink_frames: () => [collabEntry('u1', 'raw')],
      }, unexpectedCommand);
      render(<StackingBlink {...props({ subject: PROJECT, runDetail: RUN, rows: [row('u1', { frameId: 31 })] })} />);
      await opened();

      expect(alignedCalls()).toEqual([['get_stacking_aligned_frames', { runId: 8, frameIds: [31] }]]);
      expect(blink!.frames[0]!.aligned).toEqual({ runId: 8, frameId: 31 });
    });

    it('no run: no aligned call, and the entries carry no verdict', async () => {
      setBackend([1, 2]);
      render(<StackingBlink {...props()} />);
      await opened();

      expect(alignedCalls()).toHaveLength(0);
      expect(blink!.frames.every((f) => f.aligned === undefined)).toBe(true);
    });

    it('aligned_call_failure_opens_originals', async () => {
      setRunBackend([1, 2], new Error('stat failed'));
      const logged = vi.spyOn(console, 'error').mockImplementation(() => {});
      render(<StackingBlink {...props({ runDetail: RUN })} />);
      await opened();

      expect(logged).toHaveBeenCalledWith('[StackingBlink] aligned frames unavailable:', expect.any(Error));
      expect(notifyMock).toHaveBeenCalledWith(expect.objectContaining({
        title: 'Aligned frames are unavailable', kind: 'stacking', tone: 'warning', hasErrors: true,
      }));
      expect(blink!.frames.map((f) => f.key)).toEqual(['1', '2']);
      expect(blink!.frames.every((f) => f.aligned === undefined)).toBe(true);
      logged.mockRestore();
    });

    it('a frame the run does not answer for opens as an original, and is logged', async () => {
      setRunBackend([1, 2], [{ frameId: 1, state: 'available' }]);
      const logged = vi.spyOn(console, 'error').mockImplementation(() => {});
      render(<StackingBlink {...props({ runDetail: RUN })} />);
      await opened();

      expect(blink!.frames.map((f) => f.aligned)).toEqual([{ runId: 8, frameId: 1 }, undefined]);
      expect(logged).toHaveBeenCalledWith('[StackingBlink] no aligned state answered for frames', [2]);
      logged.mockRestore();
    });
  });
});
