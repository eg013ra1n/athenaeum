import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { NotificationProvider } from '../../../contexts/NotificationContext';
import { ToastStack } from '../../Toast';
import { api } from '../../../api';
import type { BlinkViewerProps } from '../../blink/types';
import type { CollabBlinkEntry, FileWithFrame } from '../../../types/models';
import { fromLibrary, fromOwn, type FrameVM } from './frames';
import ProjectBlink from './ProjectBlink';
import { ownFrameRow as own, projectFrameView as lib } from './testFixtures';

vi.mock('../../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

let blink: BlinkViewerProps | null = null;
vi.mock('../../BlinkViewer', () => ({
  default: (p: BlinkViewerProps) => { blink = p; return <div data-testid="blink" />; },
}));

afterEach(cleanup);
beforeEach(() => {
  blink = null;
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.listen).mockReset();
  vi.mocked(api.listen).mockResolvedValue((() => {}) as never);
});

function entry(key: string, source: CollabBlinkEntry['source']): CollabBlinkEntry {
  return {
    key, source, entry: { file: { id: 1, path: '/x.fits' }, frame: null } as unknown as FileWithFrame,
    frameUuid: null, sourceFrameId: null, publisherName: null,
  };
}

function tree(props: Partial<React.ComponentProps<typeof ProjectBlink>> & Pick<React.ComponentProps<typeof ProjectBlink>, 'table' | 'vms'>) {
  const all = new Map<string, FrameVM>(props.vms.map((v) => [v.key, v]));
  return (
    <MemoryRouter><NotificationProvider>
      <ProjectBlink
        projectId="p1"
        canModerate={false}
        lookup={(k) => all.get(k)}
        onClose={vi.fn()}
        onChanged={vi.fn()}
        {...props}
      />
      <ToastStack />
    </NotificationProvider></MemoryRouter>
  );
}

describe('ProjectBlink', () => {
  it('resolves the refs once and maps entries: imageRef only for calibrated and replica', async () => {
    const vms = [
      fromOwn(own({ frameId: 1, segment: 'review', calibratedPath: '/c/c_a.fits' })),
      fromOwn(own({ frameId: 2, segment: 'review' })),
    ];
    vi.mocked(api.invoke).mockResolvedValueOnce([entry('f1', 'calibrated'), entry('f2', 'raw')] as never);
    render(tree({ table: 'review', vms }));
    await waitFor(() => expect(blink).not.toBeNull());
    expect(api.invoke).toHaveBeenCalledTimes(1);
    expect(api.invoke).toHaveBeenCalledWith('get_collab_blink_frames', {
      projectId: 'p1', refs: [{ frameId: 1, frameUuid: null }, { frameId: 2, frameUuid: null }],
    });
    expect(blink!.frames.map((f) => f.key)).toEqual(['f1', 'f2']);
    expect(blink!.frames[0].imageRef).toEqual({ projectId: 'p1', frame: { frameId: 1, frameUuid: null } });
    expect(blink!.frames[1].imageRef).toBeUndefined();
  });

  it("own To review: Don't publish confirms, writes withheld, and the reload turns the badge to withheld under the same key", async () => {
    const vm = fromOwn(own({ frameId: 1, segment: 'review', calibratedPath: '/c/c_a.fits' }));
    const onChanged = vi.fn();
    vi.mocked(api.invoke).mockImplementation((async (cmd: string) => {
      if (cmd === 'get_collab_blink_frames') return [entry('f1', 'calibrated')];
      return 1;
    }) as never);
    const { rerender } = render(tree({ table: 'review', vms: [vm], onChanged }));
    await waitFor(() => expect(blink).not.toBeNull());

    let done!: Promise<void>;
    act(() => { done = Promise.resolve(blink!.actions!.find((a) => a.id === 'withhold')!.run([blink!.frames[0]])); });
    fireEvent.click(await screen.findByRole('button', { name: "Don't publish" }));
    await act(async () => { await done; });
    expect(api.invoke).toHaveBeenCalledWith('set_collab_frames_withheld', { projectId: 'p1', frameIds: [1], withheld: true });
    expect(onChanged).toHaveBeenCalled();

    const after = fromOwn(own({ frameId: 1, segment: 'held', withheld: true, calibratedPath: null }));
    rerender(tree({ table: 'review', vms: [vm], lookup: () => after, onChanged }));
    expect(blink!.frames[0].badge).toBe('withheld');
    expect(blink!.frames[0].key).toBe('f1');
  });

  it('the strip hint only says what the chip and the file name do not: To review is the file Publish sends', async () => {
    const published = fromOwn(own({ frameId: 1, segment: 'published', pubState: 'published', localState: 'own_held', frameUuid: 'u1' }));
    vi.mocked(api.invoke).mockResolvedValueOnce([entry('f1', 'calibrated')] as never); // own refs go by id
    const first = render(tree({ table: 'published', vms: [published], canModerate: true }));
    await waitFor(() => expect(blink).not.toBeNull());
    expect(blink!.contextLabel!(blink!.frames[0])).toBe('');
    first.unmount();

    blink = null;
    const vms = [
      fromOwn(own({ frameId: 2, segment: 'review', calibratedPath: '/c/c_b.fits' })),
      fromOwn(own({ frameId: 3, segment: 'review' })), // attested: core serves the original as raw
    ];
    vi.mocked(api.invoke).mockResolvedValueOnce([entry('f2', 'calibrated'), entry('f3', 'raw')] as never);
    render(tree({ table: 'review', vms }));
    await waitFor(() => expect(blink).not.toBeNull());
    expect(blink!.frames.map((f) => blink!.contextLabel!(f))).toEqual([
      'exactly the file that will be published',
      'exactly the file that will be published',
    ]);
  });

  it('a member in Library is view only, with no actions and a moderator hint', async () => {
    const vm = fromLibrary(lib({ receivedFromMember: 'Anna' }), new Map());
    vi.mocked(api.invoke).mockResolvedValueOnce([entry('u1', 'replica')] as never);
    render(tree({ table: 'library', vms: [vm], canModerate: false }));
    await waitFor(() => expect(blink).not.toBeNull());
    // A replica loads through get_collab_frame_image, by uuid.
    expect(blink!.frames[0].source).toBe('replica');
    expect(blink!.frames[0].imageRef).toEqual({ projectId: 'p1', frame: { frameId: null, frameUuid: 'u1' } });
    expect(blink!.viewOnly).toBe(true);
    expect(blink!.actions).toEqual([]);
    const label = blink!.contextLabel!(blink!.frames[0]);
    expect(label).toContain('received from Anna');
    expect(label).toContain('Only a moderator can exclude');
  });

  it('a moderator in Library gets Exclude from project; it opens ExcludeDialog over Blink with the matching rows', async () => {
    const vm = fromLibrary(lib(), new Map());
    const onChanged = vi.fn();
    vi.mocked(api.invoke).mockImplementation((async (cmd: string) => {
      if (cmd === 'get_collab_blink_frames') return [entry('u1', 'replica')];
      return null;
    }) as never);
    render(tree({ table: 'library', vms: [vm], canModerate: true, onChanged }));
    await waitFor(() => expect(blink).not.toBeNull());
    const exclude = blink!.actions!.find((a) => a.id === 'exclude')!;
    expect(exclude).toBeDefined();
    act(() => { void exclude.run([blink!.frames[0]]); });
    expect(await screen.findByText(/Exclude 1 frame from the project/)).toBeInTheDocument();
    fireEvent.change(screen.getByRole('textbox'), { target: { value: 'bad tracking' } });
    fireEvent.click(screen.getByRole('button', { name: 'Exclude' }));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('exclude_collab_frame', {
      projectId: 'p1', frameUuid: 'u1', reason: 'bad tracking',
    }));
    await waitFor(() => expect(onChanged).toHaveBeenCalled());
  });

  it('a finished Exclude reports through onExcluded when given, not onChanged', async () => {
    const vm = fromOwn(own({ frameId: 1, frameUuid: 'u1', segment: 'published', pubState: 'published', localState: 'own_held' }));
    const onChanged = vi.fn();
    const onExcluded = vi.fn();
    vi.mocked(api.invoke).mockImplementation((async (cmd: string) => {
      if (cmd === 'get_collab_blink_frames') return [entry('f1', 'calibrated')];
      return null;
    }) as never);
    render(tree({ table: 'published', vms: [vm], canModerate: true, onChanged, onExcluded }));
    await waitFor(() => expect(blink).not.toBeNull());
    act(() => { void blink!.actions!.find((a) => a.id === 'exclude')!.run([blink!.frames[0]]); });
    fireEvent.change(await screen.findByRole('textbox'), { target: { value: 'bad tracking' } });
    fireEvent.click(screen.getByRole('button', { name: 'Exclude' }));
    await waitFor(() => expect(onExcluded).toHaveBeenCalledTimes(1));
    expect(onChanged).not.toHaveBeenCalled();
  });

  it('an entry whose key matches no requested frame is dropped and logged as a contract break', async () => {
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockResolvedValueOnce([entry('f1', 'calibrated'), entry('f99', 'raw')] as never);
    render(tree({ table: 'ready', vms: [fromOwn(own({ frameId: 1 }))] }));
    await waitFor(() => expect(blink).not.toBeNull());
    expect(blink!.frames.map((f) => f.key)).toEqual(['f1']);
    expect(spy).toHaveBeenCalledWith(
      expect.stringContaining('match no requested frame'),
      expect.objectContaining({ projectId: 'p1', keys: ['f99'] }),
    );
    spy.mockRestore();
  });

  it('a failed resolve logs, notifies "Could not open Blink" and closes', async () => {
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
    const onClose = vi.fn();
    vi.mocked(api.invoke).mockRejectedValueOnce(new Error('boom') as never);
    render(tree({ table: 'ready', vms: [fromOwn(own())], onClose }));
    expect(await screen.findByText('Could not open Blink')).toBeInTheDocument();
    expect(spy).toHaveBeenCalled();
    expect(onClose).toHaveBeenCalled();
    expect(blink).toBeNull();
    spy.mockRestore();
  });

  it('every ref dropped by the backend: notifies "Nothing to blink" and closes', async () => {
    const spy = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const onClose = vi.fn();
    vi.mocked(api.invoke).mockResolvedValueOnce([] as never);
    render(tree({ table: 'ready', vms: [fromOwn(own())], onClose }));
    expect(await screen.findByText('Nothing to blink')).toBeInTheDocument();
    expect(onClose).toHaveBeenCalled();
    expect(blink).toBeNull();
    spy.mockRestore();
  });
});
