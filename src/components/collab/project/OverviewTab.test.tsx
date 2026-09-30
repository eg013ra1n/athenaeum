import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { CollabExchangeProvider } from '../../../contexts/CollabExchangeContext';
import { api } from '../../../api';
import OverviewTab, { type OverviewTabProps } from './OverviewTab';
import type { ExchangeSnapshot, FlowView, MemberSummary, OwnFrameRow, ProjectFlows, ThresholdRuleView } from '../../../types/models';

vi.mock('../../../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

afterEach(cleanup);

function ownRow(o: Partial<OwnFrameRow> = {}): OwnFrameRow {
  return {
    frameId: 1,
    frameUuid: 'u-own-1',
    fileName: 'Light_Ha_300s_0001.fits',
    setId: 1,
    setName: 'M31',
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
    contentVersion: 1,
    pubState: null,
    acceptedReason: null,
    holdersOnline: null,
    holdersTotal: null,
    localState: null,
    publishedAt: null,
    lastError: null,
    rules: [],
    path: '/data/m31/Light_Ha_300s_0001.fits',
    accepted: null,
    ...o,
  };
}

function member(o: Partial<MemberSummary> = {}): MemberSummary {
  return {
    accountId: 'acc-1',
    displayName: 'Alice',
    dataRole: 'send_receive',
    coordinator: false,
    devices: [],
    online: true,
    lastSeenAt: null,
    publishedFrames: 0,
    secondsByFilter: {},
    qualityByCamera: [],
    holdsFrames: 0,
    holdsBytes: 0,
    holdsShare: 0,
    ...o,
  };
}

function flow(o: Partial<FlowView> = {}): FlowView {
  return {
    projectId: 'proj-1',
    device: 'devA',
    direction: 'recv',
    bytesSession: 0,
    rateBps: 0,
    etaSecs: null,
    moving: true,
    completed: 0,
    inFlight: [],
    ...o,
  };
}

function projectFlows(o: Partial<ProjectFlows> = {}): ProjectFlows {
  return { projectId: 'proj-1', recv: [], send: [], toGo: 0, waitingForPublisher: null, ...o };
}

const EMPTY_SNAPSHOT: ExchangeSnapshot = { projects: [], names: [] };
let exchangeSnapshot: ExchangeSnapshot = EMPTY_SNAPSHOT;

beforeEach(() => {
  exchangeSnapshot = EMPTY_SNAPSHOT;
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((command: string) => {
    if (command === 'get_collab_exchange') return Promise.resolve(exchangeSnapshot);
    return Promise.resolve(null);
  }) as never);
  vi.mocked(api.listen).mockReset();
  vi.mocked(api.listen).mockImplementation((() => Promise.resolve(() => {})) as never);
});

const BASE_PROPS: OverviewTabProps = {
  projectId: 'proj-1',
  goals: null,
  members: null,
  own: null,
  libraryToCome: 0,
  pending: 0,
  canModerate: false,
  thresholds: [],
  thresholdsVersion: null,
  onOpenSegment: vi.fn(),
  onOpenTab: vi.fn(),
};

function renderTab(overrides: Partial<OverviewTabProps> = {}) {
  return render(
    <CollabExchangeProvider>
      <OverviewTab {...BASE_PROPS} {...overrides} />
    </CollabExchangeProvider>,
  );
}

describe('OverviewTab — integration', () => {
  it('members === null shows Loading…', async () => {
    renderTab({ members: null, goals: { Ha: 36000 }, own: [] });
    expect(await screen.findByText('Loading…')).toBeInTheDocument();
  });

  it('goal Ha 36000s with two members contributing 7200s and 3600s reads "7h to go" and renders two segments', async () => {
    renderTab({
      goals: { Ha: 36000 },
      members: [
        member({ accountId: 'a1', displayName: 'Alice', secondsByFilter: { Ha: 7200 } }),
        member({ accountId: 'a2', displayName: 'Bob', secondsByFilter: { Ha: 3600 } }),
      ],
    });

    expect(await screen.findByText('7h to go')).toBeInTheDocument();
    expect(screen.getByTitle('Alice · 2h')).toBeInTheDocument();
    expect(screen.getByTitle('Bob · 1h')).toBeInTheDocument();
  });

  it('a goal for SII with no frames still renders an SII row', async () => {
    renderTab({
      goals: { SII: 36000 },
      members: [member({ accountId: 'a1', displayName: 'Alice', secondsByFilter: {} })],
    });

    expect(await screen.findByText('SII')).toBeInTheDocument();
  });

  it('a filter without a goal shows only its total, no "to go" / "goal met"', async () => {
    renderTab({
      goals: null,
      members: [member({ accountId: 'a1', displayName: 'Alice', secondsByFilter: { L: 3600 } })],
    });

    expect(await screen.findByText('L')).toBeInTheDocument();
    expect(screen.getByText('1h')).toBeInTheDocument();
    expect(screen.queryByText(/to go/)).not.toBeInTheDocument();
    expect(screen.queryByText('goal met')).not.toBeInTheDocument();
  });

  it('a goal already met shows "goal met" in success tone', async () => {
    renderTab({
      goals: { Ha: 3600 },
      members: [member({ accountId: 'a1', displayName: 'Alice', secondsByFilter: { Ha: 7200 } })],
    });

    expect(await screen.findByText('goal met')).toBeInTheDocument();
  });

  it('no filters at all reads "No integration yet."', async () => {
    renderTab({ goals: null, members: [] });
    expect(await screen.findByText('No integration yet.')).toBeInTheDocument();
  });
});

describe('OverviewTab — my frames', () => {
  it('clicking "Ready 2" calls onOpenSegment("ready")', async () => {
    const onOpenSegment = vi.fn();
    renderTab({
      own: [ownRow({ frameId: 1, segment: 'ready' }), ownRow({ frameId: 2, segment: 'ready' })],
      onOpenSegment,
    });

    fireEvent.click(await screen.findByText('Ready 2'));
    expect(onOpenSegment).toHaveBeenCalledWith('ready');
  });

  it('renders Published N and Held back N from the segment counts', async () => {
    renderTab({
      own: [
        ownRow({ frameId: 1, segment: 'published' }),
        ownRow({ frameId: 2, segment: 'held', failures: [{ kind: 'solve', text: 'No coordinates or pixel scale' }] }),
      ],
    });

    expect(await screen.findByText('Published 1')).toBeInTheDocument();
    expect(screen.getByText('Held back 1')).toBeInTheDocument();
  });
});

describe('OverviewTab — needs attention', () => {
  it('lists the held-back item naming the commonest first kind', async () => {
    const onOpenSegment = vi.fn();
    renderTab({
      own: [
        ownRow({ frameId: 1, segment: 'held', failures: [{ kind: 'solve', text: 'No coordinates or pixel scale' }] }),
        ownRow({ frameId: 2, segment: 'held', failures: [{ kind: 'solve', text: 'No coordinates or pixel scale' }] }),
        ownRow({ frameId: 3, segment: 'held', failures: [{ kind: 'analyze', text: 'No analysis' }] }),
      ],
      onOpenSegment,
    });

    const item = await screen.findByText(/held back — mostly No coordinates or pixel scale/);
    expect(item).toBeInTheDocument();
    fireEvent.click(item);
    expect(onOpenSegment).toHaveBeenCalledWith('held');
  });

  it('own published frames in one copy only surface as an attention item to the published segment', async () => {
    const onOpenSegment = vi.fn();
    renderTab({
      own: [
        ownRow({
          frameId: 1,
          segment: 'published',
          pubState: 'published',
          localState: 'own_held',
          holdersTotal: 0,
        }),
      ],
      onOpenSegment,
    });

    const item = await screen.findByText('1 of your frames exist in one copy only');
    fireEvent.click(item);
    expect(onOpenSegment).toHaveBeenCalledWith('published');
  });

  it('library-to-come and pending-review items route through onOpenTab', async () => {
    const onOpenTab = vi.fn();
    renderTab({ own: [], libraryToCome: 4, pending: 2, canModerate: true, onOpenTab });

    fireEvent.click(await screen.findByText('4 library frames still to come'));
    expect(onOpenTab).toHaveBeenCalledWith('library');

    fireEvent.click(screen.getByText('2 frames wait for your review'));
    expect(onOpenTab).toHaveBeenCalledWith('moderation');
  });

  it('pending review is hidden when canModerate is false', async () => {
    renderTab({ pending: 2, canModerate: false });
    expect(screen.queryByText(/frames wait for your review/)).not.toBeInTheDocument();
  });

  it('while own frames are still loading, My frames and Needs attention read Loading…, never zero counts or "Nothing needs attention."', async () => {
    renderTab({ own: null, members: [], libraryToCome: 0, pending: 0, canModerate: false });
    const myFrames = (await screen.findByRole('heading', { name: 'My frames' })).parentElement!;
    expect(within(myFrames).getByText('Loading…')).toBeInTheDocument();
    expect(within(myFrames).queryByRole('button')).not.toBeInTheDocument();
    const attention = screen.getByRole('heading', { name: 'Needs attention' }).parentElement!;
    expect(within(attention).getByText('Loading…')).toBeInTheDocument();
    expect(screen.queryByText('Nothing needs attention.')).not.toBeInTheDocument();
    expect(screen.queryByText(/^Ready \d/)).not.toBeInTheDocument();
  });

  it('reads "Nothing needs attention." when nothing qualifies', async () => {
    renderTab({ own: [], libraryToCome: 0, pending: 0, canModerate: false });
    expect(await screen.findByText('Nothing needs attention.')).toBeInTheDocument();
  });
});

describe('OverviewTab — exchange now', () => {
  it('reads "Quiet." when nothing is moving', async () => {
    renderTab();
    expect(await screen.findByText('Quiet.')).toBeInTheDocument();
  });

  it('shows the recv rate and member name, and Open Exchange routes through onOpenTab', async () => {
    exchangeSnapshot = {
      projects: [projectFlows({ recv: [flow({ device: 'devA', rateBps: 1024 })] })],
      names: [{ projectId: 'proj-1', device: 'devA', memberName: 'Alice', deviceName: 'alice-pc' }],
    };
    const onOpenTab = vi.fn();
    renderTab({ onOpenTab });

    expect(await screen.findByText('↓ 1 KB/s from Alice')).toBeInTheDocument();
    fireEvent.click(screen.getByText('Open Exchange →'));
    expect(onOpenTab).toHaveBeenCalledWith('exchange');
  });
});

describe('OverviewTab — quality thresholds', () => {
  it('renders thresholds verbatim with version and the portal note', async () => {
    const thresholds: ThresholdRuleView[] = [
      { metricKey: 'fwhm', op: 'lte', value: 3 },
      { metricKey: 'not_trailed', op: 'reject_if', value: true },
    ];
    renderTab({ thresholds, thresholdsVersion: 3 });

    expect(await screen.findByText('Quality thresholds (v3)')).toBeInTheDocument();
    expect(screen.getByText('fwhm ≤ 3')).toBeInTheDocument();
    expect(screen.getByText('Reject trailed frames')).toBeInTheDocument();
    expect(
      screen.getByText(
        'Thresholds are set by the coordinator on the portal. Changes are prospective — already-published frames stay published.',
      ),
    ).toBeInTheDocument();
  });

  it('shows "No thresholds set." when empty', async () => {
    renderTab({ thresholds: [], thresholdsVersion: null });
    expect(await screen.findByText('No thresholds set.')).toBeInTheDocument();
    expect(screen.getByText('Quality thresholds')).toBeInTheDocument();
  });
});
