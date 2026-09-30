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
    autoPublish: true,
    segment: 'ready',
    onSegment: vi.fn(),
    onReload: vi.fn(),
    onDetailReload: vi.fn(),
    onRequestPublish: vi.fn(),
    publishBusy: false,
    onRequestRepublish: vi.fn(),
    republishBusy: false,
    canRepublish: false,
    coordinator: false,
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
  it('1. the segment buttons read the count of each segment', () => {
    renderTab({ rows: fourRows, segment: 'ready' });
    expect(screen.getByRole('button', { name: 'Ready to publish 2' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Published 1' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Held back 1' })).toBeInTheDocument();
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

  it('8a. Exclude is not offered to a non-coordinator', () => {
    renderTab({ rows: exclusionFixture, segment: 'published', coordinator: false });
    expect(screen.queryByRole('button', { name: /Exclude/ })).toBeNull();
  });

  it('8b. a coordinator sees "Exclude 2 of 3" (one of the three selected is already excluded) and it opens the dialog with just those two', () => {
    renderTab({ rows: exclusionFixture, segment: 'published', coordinator: true });
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
