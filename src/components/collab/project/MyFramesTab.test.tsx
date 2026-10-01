import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { NotificationProvider } from '../../../contexts/NotificationContext';
import { SessionStateProvider } from '../../../contexts/SessionStateContext';
import { ToastStack } from '../../Toast';
import { api } from '../../../api';
import type { OwnFrameRow } from '../../../types/models';
import MyFramesTab, { type MyFramesTabProps } from './MyFramesTab';

vi.mock('../../../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

afterEach(cleanup);

function own(o: Partial<OwnFrameRow> = {}): OwnFrameRow {
  return {
    frameId: 1,
    frameUuid: null,
    fileName: 'f.fits',
    setId: null,
    setName: null,
    night: '2026-09-29',
    filter: 'Ha',
    filterMapped: true,
    camera: 'ASI2600MM Pro',
    exptimeSec: 300,
    byteSize: 42_000_000,
    fwhmArcsec: 2.4,
    eccentricity: 0.4,
    starsDetected: 1200,
    medianSnr: 18,
    segment: 'ready',
    contributorState: 'published',
    contributorReason: null,
    failures: [],
    contentVersion: null,
    pubState: null,
    acceptedReason: null,
    holdersOnline: null,
    holdersTotal: null,
    localState: null,
    publishedAt: null,
    lastError: null,
    rules: [],
    path: '/data/f.fits',
    accepted: null,
    calibratedPath: null,
    calibratedBytes: null,
    preparedAt: null,
    withheld: false,
    ...o,
  };
}

/** Every `api.listen` registration this render made, by event name — lets a
 *  test fire `analysis-complete`/`plate-solve-complete` without a dedicated
 *  capture variable per event (matches `ProjectDetail.test.tsx`). */
const listeners: Record<string, ((payload: unknown) => void) | undefined> = {};

beforeEach(() => {
  for (const k of Object.keys(listeners)) delete listeners[k];
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation((() => Promise.resolve(null)) as never);
  vi.mocked(api.listen).mockImplementation((<T,>(event: string, cb: (p: T) => void) => {
    listeners[event] = cb as unknown as (payload: unknown) => void;
    return Promise.resolve(() => {});
  }) as never);
});

function defaultProps(overrides: Partial<MyFramesTabProps> = {}): MyFramesTabProps {
  return {
    projectId: 'proj-1',
    rows: [],
    error: false,
    links: [],
    segment: 'ready',
    onSegment: vi.fn(),
    onReload: vi.fn(),
    onDetailReload: vi.fn(),
    onRequestPublish: vi.fn(),
    publishBusy: false,
    onRequestRepublish: vi.fn(),
    republishBusy: false,
    canRepublish: false,
    canModerate: false,
    republishError: null,
    refusal: null,
    onOpen: vi.fn(),
    ...overrides,
  };
}

/** Shared element tree — also used directly by the rerender test below, so a
 *  rerender updates the SAME mounted `MyFramesTab` instance (same position,
 *  same component type at every level) instead of remounting it. */
function tree(props: MyFramesTabProps) {
  return (
    <MemoryRouter>
      <SessionStateProvider>
        <NotificationProvider>
          <MyFramesTab {...props} />
          <ToastStack />
        </NotificationProvider>
      </SessionStateProvider>
    </MemoryRouter>
  );
}

function renderTab(overrides: Partial<MyFramesTabProps> = {}) {
  const props = defaultProps(overrides);
  const utils = render(tree(props));
  return { ...utils, props };
}

const fourRows: OwnFrameRow[] = [
  own({ frameId: 1, fileName: 'r1.fits', segment: 'ready' }),
  own({ frameId: 2, fileName: 'r2.fits', segment: 'ready' }),
  own({ frameId: 3, fileName: 'p1.fits', frameUuid: 'u3', segment: 'published', pubState: 'published' }),
  own({
    frameId: 4,
    fileName: 'h1.fits',
    segment: 'held',
    setId: 10,
    setName: 'M31',
    failures: [{ kind: 'solve', text: 'no coordinates or pixel scale' }],
  }),
];

describe('MyFramesTab — segments', () => {
  it('segment tiles and the two buttons sit on one row like the mockup', () => {
    renderTab({ rows: fourRows.slice(0, 1).concat(fourRows.slice(2)) });
    expect(screen.getByRole('button', { name: /1 Ready to publish/ })).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByRole('button', { name: /1 Published/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /1 Held back/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: '+ Link an object' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Recalibrate and republish all' })).toBeInTheDocument();
    expect(screen.queryByText('Linked objects')).toBeNull();
    expect(screen.queryByText('Auto-publish my frames')).toBeNull();
  });

  it('Ready shows "Publish all N" as the primary action', () => {
    renderTab({ rows: fourRows });
    expect(screen.getByRole('button', { name: 'Publish all 2' }).className).toContain('bg-accent');
  });

  it('Recalibrate and republish all is disabled without published frames, spins while busy', () => {
    const { unmount } = renderTab({ canRepublish: false });
    expect(screen.getByRole('button', { name: 'Recalibrate and republish all' })).toBeDisabled();
    unmount();
    renderTab({ canRepublish: true, republishBusy: true });
    const b = screen.getByRole('button', { name: 'Recalibrate and republish all' });
    expect(b).toBeDisabled();
    expect(b.querySelector('.animate-spin')).not.toBeNull();
  });

  it('while loading, no tiles but the Link button stays; both buttons use the 33 px box', () => {
    renderTab({ rows: null });
    expect(screen.queryByRole('button', { name: /Ready to publish/ })).toBeNull();
    expect(screen.getByText('Loading…')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: '+ Link an object' }).className).toContain('h-[33px]');
  });

  it('both row buttons are the tile-size Button, with no !important overrides', () => {
    renderTab({ rows: [] });
    for (const name of ['+ Link an object', 'Recalibrate and republish all']) {
      const cls = screen.getByRole('button', { name }).className.split(/\s+/);
      expect(cls).toEqual(expect.arrayContaining(['h-[33px]', 'min-w-[150px]', 'rounded-md', 'px-3.5', 'py-[7px]', 'text-[12px]']));
      expect(cls.filter((c) => c.startsWith('!'))).toEqual([]);
      expect(cls).not.toContain('justify-center');
    }
  });

  it('the link dialog lists a linked set once, and Unlink sends linked:false', async () => {
    vi.mocked(api.invoke).mockImplementation(((cmd: string) =>
      Promise.resolve(
        cmd === 'list_collab_link_suggestions'
          ? [{ framesSetId: 1, name: 'M31', lightCount: 40, withinRadius: true, distanceDeg: 0, alreadyLinked: true }]
          : null,
      )) as never);
    renderTab({ links: [{ framesSetId: 1, name: 'M31', lightCount: 40, withinRadius: true }] as never });
    fireEvent.click(screen.getByRole('button', { name: '+ Link an object' }));
    await screen.findByRole('button', { name: 'Unlink' });
    expect(screen.getAllByText('M31')).toHaveLength(1);
    fireEvent.click(screen.getByRole('button', { name: 'Unlink' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('set_collab_link', { projectId: 'proj-1', framesSetId: 1, linked: false }),
    );
  });

  it('the link dialog lists the linked objects with their target state', async () => {
    vi.mocked(api.invoke).mockImplementation(((cmd: string) =>
      Promise.resolve(cmd === 'list_collab_link_suggestions' ? [] : null)) as never);
    renderTab({
      links: [
        { framesSetId: 1, name: 'M31', lightCount: 40, withinRadius: true },
        { framesSetId: 2, name: 'M33', lightCount: 12, withinRadius: false },
      ] as never,
    });
    fireEvent.click(screen.getByRole('button', { name: '+ Link an object' }));
    expect(await screen.findByText('M31')).toBeInTheDocument();
    expect(screen.getByText('on target')).toBeInTheDocument();
    expect(screen.getByText('outside the target')).toBeInTheDocument();
    expect(screen.getByText('· 40 lights')).toBeInTheDocument();
  });

  it('1. the segment buttons read the count of each segment', () => {
    renderTab({ rows: fourRows, segment: 'ready' });
    expect(screen.getByRole('button', { name: /2 Ready to publish/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /1 Published/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /1 Held back/ })).toBeInTheDocument();
  });

  it('2. with no selection, "Publish all 2" calls onRequestPublish with the two ready ids', () => {
    const onRequestPublish = vi.fn();
    renderTab({ rows: fourRows, segment: 'ready', onRequestPublish });
    fireEvent.click(screen.getByRole('button', { name: 'Publish all 2' }));
    expect(onRequestPublish).toHaveBeenCalledWith([1, 2]);
  });

  it('3. an empty Ready segment with no links shows the link prompt and no Publish button', () => {
    renderTab({
      rows: [own({ frameId: 9, fileName: 'h.fits', segment: 'held' })],
      segment: 'ready',
      links: [],
    });
    expect(screen.getByText('Link an object to start.')).toBeInTheDocument();
    // `/^Publish /` (not `/Publish/`) — the segment switch's own "Published 0"
    // button legitimately matches a loose "Publish" search.
    expect(screen.queryByRole('button', { name: /^Publish / })).toBeNull();
  });
});

describe('MyFramesTab — Held back solve', () => {
  const heldSolveRow = (frameId: number, fileName: string): OwnFrameRow =>
    own({
      frameId,
      fileName,
      segment: 'held',
      setId: 10,
      setName: 'M31',
      failures: [{ kind: 'solve', text: 'no coordinates or pixel scale' }],
    });

  it('4a. the Reason group renders "Solve 1"; clicking starts a batch solve; completion here reloads once', async () => {
    const onReload = vi.fn();
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      if (command === 'plate_solve_batch') return Promise.resolve(undefined);
      return Promise.resolve(null);
    }) as never);
    renderTab({ rows: [heldSolveRow(4, 'h1.fits')], segment: 'held', onReload });

    const btn = await screen.findByRole('button', { name: 'Solve 1' });
    fireEvent.click(btn);
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('plate_solve_batch', { frameIds: [4] }));

    act(() => {
      listeners['plate-solve-complete']?.({});
    });
    await waitFor(() => expect(onReload).toHaveBeenCalledTimes(1));
  });

  it('4b. a plate-solve-complete this tab never started calls nothing', async () => {
    const onReload = vi.fn();
    renderTab({ rows: [heldSolveRow(4, 'h1.fits')], segment: 'held', onReload });
    await screen.findByRole('button', { name: 'Solve 1' });

    act(() => {
      listeners['plate-solve-complete']?.({});
    });
    await new Promise((r) => setTimeout(r, 0));
    expect(onReload).not.toHaveBeenCalled();
  });
});

describe('MyFramesTab — Held back analyze', () => {
  it('5. Analyze on a two-set group opens a menu naming both sets; picking one analyzes it; completion reloads', async () => {
    const onReload = vi.fn();
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      if (command === 'analyze_frame_set') return Promise.resolve(undefined);
      return Promise.resolve(null);
    }) as never);
    renderTab({
      rows: [
        own({
          frameId: 5,
          fileName: 'a1.fits',
          segment: 'held',
          setId: 10,
          setName: 'M31',
          failures: [{ kind: 'analyze', text: 'no analysis' }],
        }),
        own({
          frameId: 6,
          fileName: 'a2.fits',
          segment: 'held',
          setId: 11,
          setName: 'M42',
          failures: [{ kind: 'analyze', text: 'no analysis' }],
        }),
      ],
      segment: 'held',
      onReload,
    });

    fireEvent.click(await screen.findByRole('button', { name: 'Analyze' }));
    expect(screen.getByRole('menuitem', { name: 'M31' })).toBeInTheDocument();
    expect(screen.getByRole('menuitem', { name: 'M42' })).toBeInTheDocument();

    fireEvent.click(screen.getByRole('menuitem', { name: 'M31' }));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('analyze_frame_set', { frameSetId: 10 }));

    act(() => {
      listeners['analysis-complete']?.({
        frame_set_id: 10,
        analyzed: 2,
        skipped: 0,
        failed: 0,
        errors: [],
        cancelled: false,
      });
    });
    await waitFor(() => expect(onReload).toHaveBeenCalledTimes(1));
  });
});

describe('MyFramesTab — solve failure notifies', () => {
  it('6. a failed plate_solve_batch raises a warning notification titled "Could not start the solve"', async () => {
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      if (command === 'plate_solve_batch') return Promise.reject(new Error('solver busy'));
      return Promise.resolve(null);
    }) as never);
    renderTab({
      rows: [
        own({
          frameId: 4,
          fileName: 'h1.fits',
          segment: 'held',
          setId: 10,
          setName: 'M31',
          failures: [{ kind: 'solve', text: 'no coordinates or pixel scale' }],
        }),
      ],
      segment: 'held',
    });

    fireEvent.click(await screen.findByRole('button', { name: 'Solve 1' }));
    const toast = await screen.findByRole('status');
    expect(toast).toHaveTextContent('Could not start the solve');
  });
});

describe('MyFramesTab — Published', () => {
  const publishedFixture: OwnFrameRow[] = [
    own({ frameId: 10, fileName: 'p1.fits', frameUuid: 'u10', segment: 'published', pubState: 'published' }),
    own({ frameId: 11, fileName: 'p2.fits', frameUuid: 'u11', segment: 'published', pubState: 'published' }),
  ];

  it('7. selecting two frames and clicking Republish 2 republishes the selection; Recalibrate acts on everything', () => {
    const onRequestRepublish = vi.fn();
    renderTab({
      rows: publishedFixture,
      segment: 'published',
      canRepublish: true,
      onRequestRepublish,
    });

    fireEvent.click(screen.getByRole('checkbox', { name: 'Select all shown' }));
    fireEvent.click(screen.getByRole('button', { name: 'Republish 2' }));
    expect(onRequestRepublish).toHaveBeenCalledWith([10, 11]);

    fireEvent.click(screen.getByRole('button', { name: 'Recalibrate and republish all' }));
    expect(onRequestRepublish).toHaveBeenCalledWith(null);
  });

  const exclusionFixture: OwnFrameRow[] = [
    own({ frameId: 20, fileName: 'e1.fits', frameUuid: 'u20', segment: 'published', pubState: 'published' }),
    own({ frameId: 21, fileName: 'e2.fits', frameUuid: 'u21', segment: 'published', pubState: 'published' }),
    own({
      frameId: 22,
      fileName: 'e3.fits',
      frameUuid: 'u22',
      segment: 'published',
      pubState: 'published',
      accepted: false,
      acceptedReason: 'trailed',
    }),
  ];

  it('8a. Exclude is not offered without canModerate', () => {
    renderTab({ rows: exclusionFixture, segment: 'published', canModerate: false });
    expect(screen.queryByRole('button', { name: /Exclude/ })).toBeNull();
  });

  it('8b. canModerate sees "Exclude 2 of 3" (one of the three selected is already excluded) and it opens the dialog with just those two', () => {
    renderTab({ rows: exclusionFixture, segment: 'published', canModerate: true });
    fireEvent.click(screen.getByRole('checkbox', { name: 'Select all shown' }));
    fireEvent.click(screen.getByRole('button', { name: 'Exclude 2 of 3' }));
    expect(screen.getByText('Exclude 2 frames from the project')).toBeInTheDocument();
  });
});

describe('MyFramesTab — fix round 1, finding 1: the toolbar Analyze excludes an already-running set', () => {
  it('after the group Analyze starts a set, the toolbar Analyze drops it (disabled, 0 eligible) and never re-invokes analyze_frame_set for it', async () => {
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      // Never resolves — simulates the backend's real run-for-the-whole-call
      // shape (`analyze_frame_set` is awaited for the whole analysis), so
      // the set stays "busy" for the rest of this test.
      if (command === 'analyze_frame_set') return new Promise(() => {});
      return Promise.resolve(null);
    }) as never);

    renderTab({
      rows: [
        own({
          frameId: 5,
          fileName: 'a1.fits',
          segment: 'held',
          setId: 10,
          setName: 'M31',
          failures: [{ kind: 'analyze', text: 'no analysis' }],
        }),
      ],
      segment: 'held',
    });

    // A single set: the group header's ReasonGroupAction is a plain button
    // (no menu), distinct from the toolbar's "Analyze all N".
    const groupBtn = await screen.findByRole('button', { name: 'Analyze' });
    fireEvent.click(groupBtn);
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('analyze_frame_set', { frameSetId: 10 }));

    // The toolbar action's `eligible` now excludes set 10 — 0 of the 1 held
    // frame is eligible, and the button disables at zero (never hidden).
    const toolbarBtn = screen.getByRole('button', { name: 'Analyze all 0' });
    expect(toolbarBtn).toBeDisabled();

    // A click on a disabled button fires no handler — belt-and-braces check
    // that it truly never re-invokes the command for the busy set.
    fireEvent.click(toolbarBtn);
    expect(
      vi.mocked(api.invoke).mock.calls.filter(
        ([c, a]) => c === 'analyze_frame_set' && (a as { frameSetId: number })?.frameSetId === 10,
      ),
    ).toHaveLength(1);
  });
});

describe('MyFramesTab — fix round 1, finding 2: listeners subscribe once, not per render', () => {
  it('a rerender with a new inline onReload does not re-subscribe api.listen, and the LATEST onReload runs on completion', async () => {
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      if (command === 'plate_solve_batch') return Promise.resolve(undefined);
      return Promise.resolve(null);
    }) as never);

    const onReload1 = vi.fn();
    const props = defaultProps({
      rows: [
        own({
          frameId: 4,
          fileName: 'h1.fits',
          segment: 'held',
          setId: 10,
          setName: 'M31',
          failures: [{ kind: 'solve', text: 'no coordinates or pixel scale' }],
        }),
      ],
      segment: 'held',
      onReload: onReload1,
    });
    const { rerender } = render(tree(props));

    fireEvent.click(await screen.findByRole('button', { name: 'Solve 1' }));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('plate_solve_batch', { frameIds: [4] }));

    const listenCallsBefore = vi
      .mocked(api.listen)
      .mock.calls.filter(([event]) => event === 'plate-solve-complete').length;
    expect(listenCallsBefore).toBe(1);

    // A brand-new inline `onReload` — exactly the shape a shell page that
    // does not memoize its callback would pass on every render.
    const onReload2 = vi.fn();
    rerender(tree({ ...props, onReload: onReload2 }));

    const listenCallsAfter = vi
      .mocked(api.listen)
      .mock.calls.filter(([event]) => event === 'plate-solve-complete').length;
    expect(listenCallsAfter).toBe(1); // still exactly one subscription — not re-subscribed

    act(() => {
      listeners['plate-solve-complete']?.({});
    });

    expect(onReload2).toHaveBeenCalledTimes(1);
    expect(onReload1).not.toHaveBeenCalled();
  });
});
