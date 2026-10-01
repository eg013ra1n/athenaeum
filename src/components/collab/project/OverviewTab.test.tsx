import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { CollabExchangeProvider } from '../../../contexts/CollabExchangeContext';
import { api } from '../../../api';
import OverviewTab, { type OverviewTabProps } from './OverviewTab';
import type { ExchangeSnapshot, FlowView, MemberSummary, OwnFrameRow, ProjectFlows, ThresholdRuleView } from '../../../types/models';

vi.mock('../../../contexts/NotificationContext', () => ({ useNotifications: () => ({ notify: vi.fn() }) }));
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
    calibratedPath: null,
    calibratedBytes: null,
    preparedAt: null,
    withheld: false,
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
  library: null,
  pending: 0,
  canModerate: false,
  thresholds: [],
  thresholdsVersion: null,
  onOpenSegment: vi.fn(),
  onOpenTab: vi.fn(),
  canReceive: true,
  liveRunning: true,
  onReloadOwn: vi.fn(),
  onAttention: vi.fn(),
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

    expect(await screen.findByText('7h 00m to go')).toBeInTheDocument();
    expect(screen.getByTitle('Alice · 2h 00m')).toBeInTheDocument();
    expect(screen.getByTitle('Bob · 1h 00m')).toBeInTheDocument();
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
    expect(screen.getByText('1h 00m')).toBeInTheDocument();
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

describe('OverviewTab — my contribution', () => {
  it('clicking the ready tile calls onOpenSegment("ready")', async () => {
    const onOpenSegment = vi.fn();
    renderTab({
      own: [ownRow({ frameId: 1, segment: 'ready' }), ownRow({ frameId: 2, segment: 'ready' })],
      onOpenSegment,
    });

    fireEvent.click(await screen.findByRole('button', { name: /2 ready to calibrate/ }));
    expect(onOpenSegment).toHaveBeenCalledWith('ready');
  });

  it('renders all four tiles from the segment counts', async () => {
    renderTab({
      own: [
        ownRow({ frameId: 1, segment: 'review' }),
        ownRow({ frameId: 2, segment: 'published' }),
        ownRow({ frameId: 3, segment: 'held', failures: [{ kind: 'solve', text: 'No coordinates or pixel scale' }] }),
      ],
    });

    expect(await screen.findByRole('button', { name: /0 ready to calibrate/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /1 to review/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /1 published/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /1 held back/ })).toBeInTheDocument();
  });

  it('renders My contribution above Integration in the left column and the settings slot first on the right', () => {
    renderTab({ settings: <section aria-label="Project settings">S</section> });
    const left = screen.getByRole('heading', { name: 'My contribution' }).closest('[data-col="left"]');
    expect(left).not.toBeNull();
    const headings = within(left as HTMLElement).getAllByRole('heading').map((h) => h.textContent);
    expect(headings.slice(0, 2)).toEqual(['My contribution', 'Integration toward goal']);
    const right = screen.getByLabelText('Project settings').closest('[data-col="right"]');
    expect(right?.firstElementChild?.getAttribute('aria-label')).toBe('Project settings');
  });

  it('each contribution tile shows hours, nights, size and per-filter hours', () => {
    renderTab({ own: [ownRow({ segment: 'ready', exptimeSec: 3600, filter: 'L' })] });
    const tile = screen.getByRole('button', { name: /ready to calibrate/i });
    expect(tile).toHaveTextContent('1h 00m');
    expect(tile).toHaveTextContent('1 night');
    expect(tile).toHaveTextContent('L');
  });

  it('the To review tile opens the review segment', () => {
    const onOpenSegment = vi.fn();
    renderTab({ onOpenSegment, own: [ownRow({ segment: 'review' })] });
    fireEvent.click(screen.getByRole('button', { name: /to review/i }));
    expect(onOpenSegment).toHaveBeenCalledWith('review');
  });
});

describe('OverviewTab — needs attention', () => {
  it('an attention row calls onAttention with its target', () => {
    const onAttention = vi.fn();
    renderTab({ onAttention, own: [ownRow({ segment: 'held', failures: [{ kind: 'solve', text: 'No coordinates or pixel scale' }] })] });
    fireEvent.click(screen.getByRole('button', { name: 'Review' }));
    expect(onAttention).toHaveBeenCalledWith({ kind: 'segment', segment: 'held', state: 'solve' });
  });

  it('one row per held-back cause instead of a single "mostly" line', () => {
    renderTab({
      own: [
        ownRow({ frameId: 1, segment: 'held', failures: [{ kind: 'solve', text: 's' }] }),
        ownRow({ frameId: 2, segment: 'held', failures: [{ kind: 'analyze', text: 'a' }] }),
      ],
    });
    expect(screen.getByText(/1 frame from 2026-09-29 is not plate-solved/)).toBeInTheDocument();
    expect(screen.getByText(/1 frame is not analyzed/)).toBeInTheDocument();
  });

  it('own published frames in one copy only open the published segment with state single', () => {
    const onAttention = vi.fn();
    renderTab({
      own: [ownRow({ segment: 'published', pubState: 'published', localState: 'own_held', holdersTotal: 0 })],
      onAttention,
    });
    expect(screen.getByText(/1 published frame exists in one copy only/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Show' }));
    expect(onAttention).toHaveBeenCalledWith({ kind: 'segment', segment: 'published', state: 'single' });
  });

  it('the approval row routes to moderation for a moderator', () => {
    const onAttention = vi.fn();
    renderTab({ own: [], pending: 2, canModerate: true, onAttention });
    expect(screen.getByText(/2 frames wait for your approval/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Moderate' }));
    expect(onAttention).toHaveBeenCalledWith({ kind: 'tab', tab: 'moderation' });
  });

  it('pending approval is hidden when canModerate is false', async () => {
    renderTab({ own: [], pending: 2, canModerate: false });
    expect(screen.queryByText(/wait for your approval/)).not.toBeInTheDocument();
  });

  it('while own frames are still loading, My contribution and Needs attention read Loading…, never zero counts or "Nothing needs your attention."', async () => {
    renderTab({ own: null, members: [], library: [], pending: 0, canModerate: false });
    const myFrames = (await screen.findByRole('heading', { name: 'My contribution' })).closest('section')!;
    expect(within(myFrames).getByText('Loading…')).toBeInTheDocument();
    expect(within(myFrames).queryByRole('button')).not.toBeInTheDocument();
    const attention = screen.getByRole('heading', { name: 'Needs attention' }).closest('section')!;
    expect(within(attention).getByText('Loading…')).toBeInTheDocument();
    expect(screen.queryByText('Nothing needs your attention.')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /ready to calibrate/ })).not.toBeInTheDocument();
  });

  it('own frames that failed to load (ownError) show neither Loading… nor zero counts in My contribution and Needs attention', async () => {
    renderTab({ own: null, ownError: true, members: [], library: [], pending: 0, canModerate: false });
    const myFrames = (await screen.findByRole('heading', { name: 'My contribution' })).closest('section')!;
    expect(within(myFrames).queryByText('Loading…')).not.toBeInTheDocument();
    expect(within(myFrames).getByText('Not available.')).toBeInTheDocument();
    expect(within(myFrames).queryByRole('button')).not.toBeInTheDocument();
    const attention = screen.getByRole('heading', { name: 'Needs attention' }).closest('section')!;
    expect(within(attention).queryByText('Loading…')).not.toBeInTheDocument();
    expect(within(attention).getByText('Not available.')).toBeInTheDocument();
    expect(screen.queryByText('Nothing needs your attention.')).not.toBeInTheDocument();
  });

  it('reads "Nothing needs your attention." when nothing qualifies', async () => {
    renderTab({ own: [], library: [], pending: 0, canModerate: false });
    expect(await screen.findByText('Nothing needs your attention.')).toBeInTheDocument();
  });
});

describe('OverviewTab — exchange now', () => {
  it('reads "Nothing is moving." when nothing is moving', async () => {
    renderTab();
    expect(await screen.findByText('Nothing is moving.')).toBeInTheDocument();
  });

  it('shows the recv rate and member name, and Open Exchange routes through onOpenTab', async () => {
    exchangeSnapshot = {
      projects: [projectFlows({ recv: [flow({ device: 'devA', rateBps: 1024 })] })],
      names: [{ projectId: 'proj-1', device: 'devA', memberName: 'Alice', deviceName: 'alice-pc' }],
    };
    const onOpenTab = vi.fn();
    renderTab({ onOpenTab });

    await screen.findByText('1 KB/s');
    expect(document.body.textContent).toContain('↓ 1 KB/s from Alice');
    fireEvent.click(screen.getByText('Open Exchange →'));
    expect(onOpenTab).toHaveBeenCalledWith('exchange');
  });
});

describe('OverviewTab — quality thresholds', () => {
  it('renders thresholds with the mockup wording and version', async () => {
    const thresholds: ThresholdRuleView[] = [
      { metricKey: 'fwhm_arcsec', op: 'lte', value: 3 },
      { metricKey: 'not_trailed', op: 'reject_if', value: true },
    ];
    renderTab({ thresholds, thresholdsVersion: 3 });

    expect(await screen.findByText('FWHM ≤ 3.00″')).toBeInTheDocument();
    expect(screen.getByText('Reject trailed frames')).toBeInTheDocument();
    expect(screen.getByText('v3 · set by the coordinator')).toBeInTheDocument();
  });

  it('shows "No quality rules set." when empty', async () => {
    renderTab({ thresholds: [], thresholdsVersion: null });
    expect(await screen.findByText('No quality rules set.')).toBeInTheDocument();
    expect(screen.getByText('Quality thresholds')).toBeInTheDocument();
  });
});

describe('OverviewTab — actions', () => {
  it('Map opens the filter-mapping dialog instead of navigating', async () => {
    const onAttention = vi.fn();
    renderTab({ onAttention, own: [ownRow({ segment: 'held', filter: 'S2', failures: [{ kind: 'mapFilter', text: 'm' }] })] });
    fireEvent.click(screen.getByRole('button', { name: 'Map' }));
    expect(onAttention).not.toHaveBeenCalled();
    expect(await screen.findByRole('dialog')).toBeInTheDocument();
  });
  it('attention rows carry no stray top border on the first row', () => {
    renderTab({ own: [ownRow({ segment: 'held', failures: [{ kind: 'solve', text: 's' }] })] });
    expect(screen.getByRole('button', { name: 'Review' }).parentElement!.className).toContain('first-of-type:border-t-0');
  });
});

describe('OverviewTab — mockup cards', () => {
  it('renders the four cards like the mockup', async () => {
    renderTab({
      goals: { Ha: 144000 },
      members: [member({ secondsByFilter: { Ha: 29_760 } })],
      own: Array.from({ length: 136 }, (_, i) => ownRow({ frameId: i + 1, segment: 'ready' })),
      thresholds: [
        { metricKey: 'fwhm_arcsec', op: 'lte', value: 3 },
        { metricKey: 'not_trailed', op: 'reject_if', value: true },
      ],
    });
    // The heading is named by its title alone; the subtitle sits beside it.
    const integration = screen.getByRole('heading', { name: 'Integration toward goal' });
    expect(integration.parentElement).toHaveTextContent('published, accepted frames · by member');
    expect(screen.getByRole('heading', { name: 'My contribution' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /136 ready to calibrate/ })).toBeInTheDocument();
    expect(screen.getByText(/8h 16m/)).toBeInTheDocument();
    expect(screen.getByText('FWHM ≤ 3.00″')).toBeInTheDocument();
    expect(screen.getByText('Reject trailed frames')).toBeInTheDocument();
  });

  it('an empty project shows empty states, never NaN', () => {
    renderTab({ own: [], members: [], library: [], goals: null });
    expect(screen.getByText('No integration yet.')).toBeInTheDocument();
    expect(screen.getByText('Nothing needs your attention.')).toBeInTheDocument();
    expect(document.body.textContent).not.toMatch(/NaN/);
  });
});
