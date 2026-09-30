import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { SessionStateProvider } from '../../../contexts/SessionStateContext';
import { api } from '../../../api';
import { formatRelative } from '../format';
import { formatTimestamp } from '../../../utils/dateFormatting';
import MembersTab from './MembersTab';
import type { MemberSummary } from '../../../types/models';

vi.mock('../../../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

afterEach(() => {
  cleanup();
  // Safety net: a test that enables fake timers and then times out before
  // reaching its own `vi.useRealTimers()` would otherwise leave every
  // following test's `findByText`/`waitFor` (which poll on real timers)
  // hanging too.
  vi.useRealTimers();
});

function member(overrides: Partial<MemberSummary> = {}): MemberSummary {
  return {
    accountId: 'acc-1',
    displayName: 'Alice',
    dataRole: 'send_receive',
    coordinator: false,
    devices: [],
    online: false,
    lastSeenAt: null,
    publishedFrames: 0,
    secondsByFilter: {},
    qualityByCamera: [],
    holdsFrames: 0,
    holdsBytes: 0,
    holdsShare: 0,
    ...overrides,
  };
}

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.listen).mockReset();
  vi.mocked(api.listen).mockImplementation((() => Promise.resolve(() => {})) as never);
});

function mockMembers(list: MemberSummary[] | Error) {
  vi.mocked(api.invoke).mockImplementation(((command: string) => {
    if (command === 'get_collab_member_summary') {
      return list instanceof Error ? Promise.reject(list) : Promise.resolve(list);
    }
    return Promise.resolve(null);
  }) as never);
}

function renderTab(onMembers?: (m: MemberSummary[]) => void) {
  return render(
    <SessionStateProvider>
      <MembersTab projectId="proj-1" onMembers={onMembers} />
    </SessionStateProvider>,
  );
}

function nameOrder(): string[] {
  return screen.getAllByTestId('member-name').map((el) => el.textContent ?? '');
}

describe('MembersTab — load', () => {
  it('calls get_collab_member_summary with the project id on mount', async () => {
    mockMembers([member({ accountId: 'a1', displayName: 'Alice' })]);
    renderTab();
    await screen.findByText('Alice');
    expect(api.invoke).toHaveBeenCalledWith('get_collab_member_summary', { projectId: 'proj-1' });
  });

  it('logs and shows inline text on a failed load', async () => {
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
    mockMembers(new Error('boom'));
    renderTab();
    expect(await screen.findByText('Could not load members — see console.')).toBeInTheDocument();
    expect(spy).toHaveBeenCalledWith('[members] get_collab_member_summary failed:', expect.any(Error));
    spy.mockRestore();
  });

  it('calls onMembers with the loaded list', async () => {
    const onMembers = vi.fn();
    const list = [member({ accountId: 'a1', displayName: 'Alice' })];
    mockMembers(list);
    renderTab(onMembers);
    await screen.findByText('Alice');
    expect(onMembers).toHaveBeenCalledWith(list);
  });
});

describe('MembersTab — Role', () => {
  it('maps send_receive to Processor and send to Contributor, else shows the raw value', async () => {
    mockMembers([
      member({ accountId: 'a1', displayName: 'Alice', dataRole: 'send_receive' }),
      member({ accountId: 'a2', displayName: 'Bob', dataRole: 'send' }),
      member({ accountId: 'a3', displayName: 'Carol', dataRole: 'receive' }),
    ]);
    renderTab();
    await screen.findByText('Alice');
    expect(screen.getByText('Processor')).toBeInTheDocument();
    expect(screen.getByText('Contributor')).toBeInTheDocument();
    expect(screen.getByText('receive')).toBeInTheDocument();
  });
});

describe('MembersTab — Member column', () => {
  it('shows a coordinator chip only for a coordinator', async () => {
    mockMembers([
      member({ accountId: 'a1', displayName: 'Alice', coordinator: true }),
      member({ accountId: 'a2', displayName: 'Bob', coordinator: false }),
    ]);
    renderTab();
    await screen.findByText('Alice');
    const aliceRow = screen.getByText('Alice').closest('tr') as HTMLElement;
    const bobRow = screen.getByText('Bob').closest('tr') as HTMLElement;
    expect(within(aliceRow).getByText('Coordinator')).toBeInTheDocument();
    expect(within(bobRow).queryByText('Coordinator')).toBeNull();
  });
});

describe('MembersTab — Last seen', () => {
  it('an online member reads "online now"', async () => {
    mockMembers([member({ accountId: 'a1', displayName: 'Alice', online: true, lastSeenAt: '2026-09-29T10:00:00Z' })]);
    renderTab();
    const el = await screen.findByText('online now');
    expect(el).toHaveClass('text-success');
  });

  it('an offline member with lastSeenAt shows its timestamp and a relative time', async () => {
    // Fake only `Date` — RTL's `findByText`/`waitFor` poll on real timers,
    // and faking those too would hang every following test as well.
    vi.useFakeTimers({ toFake: ['Date'] });
    vi.setSystemTime(new Date('2026-09-29T12:00:00Z'));
    const iso = '2026-09-29T11:00:00Z';
    mockMembers([member({ accountId: 'a1', displayName: 'Alice', online: false, lastSeenAt: iso })]);
    renderTab();
    await screen.findByText('Alice');
    expect(screen.getByText(formatTimestamp(iso))).toBeInTheDocument();
    expect(screen.getByText(formatRelative(iso, Date.now()))).toBeInTheDocument();
    vi.useRealTimers();
  });

  it('a member with no lastSeenAt reads "never"', async () => {
    mockMembers([member({ accountId: 'a1', displayName: 'Alice', online: false, lastSeenAt: null })]);
    renderTab();
    await screen.findByText('Alice');
    expect(screen.getByText('never')).toBeInTheDocument();
  });
});

describe('MembersTab — sorting', () => {
  it('defaults to Published desc', async () => {
    mockMembers([
      member({ accountId: 'a1', displayName: 'Alice', publishedFrames: 5 }),
      member({ accountId: 'a2', displayName: 'Bob', publishedFrames: 50 }),
      member({ accountId: 'a3', displayName: 'Carol', publishedFrames: 20 }),
    ]);
    renderTab();
    await screen.findByText('Alice');
    expect(nameOrder()).toEqual(['Bob', 'Carol', 'Alice']);
  });

  it('sorting by Last seen puts online first, then by lastSeenAt desc, then never last', async () => {
    mockMembers([
      member({ accountId: 'a1', displayName: 'Alice', online: false, lastSeenAt: '2026-09-20T10:00:00Z' }),
      member({ accountId: 'a2', displayName: 'Bob', online: true, lastSeenAt: '2026-09-01T10:00:00Z' }),
      member({ accountId: 'a3', displayName: 'Carol', online: false, lastSeenAt: null }),
      member({ accountId: 'a4', displayName: 'Dana', online: false, lastSeenAt: '2026-09-25T10:00:00Z' }),
    ]);
    renderTab();
    await screen.findByText('Alice');
    fireEvent.click(screen.getByText('Last seen'));
    expect(nameOrder()).toEqual(['Bob', 'Dana', 'Alice', 'Carol']);
  });

  it('clicking the same header again flips direction', async () => {
    mockMembers([
      member({ accountId: 'a1', displayName: 'Alice', publishedFrames: 5 }),
      member({ accountId: 'a2', displayName: 'Bob', publishedFrames: 50 }),
    ]);
    renderTab();
    await screen.findByText('Alice');
    expect(nameOrder()).toEqual(['Bob', 'Alice']); // desc default
    fireEvent.click(screen.getByText(/^Published/));
    expect(nameOrder()).toEqual(['Alice', 'Bob']); // flipped to asc
  });
});

describe('MembersTab — filter columns', () => {
  it('shows one column per filter present in any member, ordered by filterOrder, with formatted hours', async () => {
    mockMembers([
      member({ accountId: 'a1', displayName: 'Alice', secondsByFilter: { Ha: 7200, B: 3600 } }),
      member({ accountId: 'a2', displayName: 'Bob', secondsByFilter: { L: 1800 } }),
    ]);
    renderTab();
    await screen.findByText('Alice');
    const headers = screen.getAllByRole('columnheader').map((h) => h.textContent);
    const lIdx = headers.findIndex((h) => h?.startsWith('L'));
    const bIdx = headers.findIndex((h) => h?.startsWith('B'));
    const haIdx = headers.findIndex((h) => h?.startsWith('Ha'));
    expect(lIdx).toBeGreaterThanOrEqual(0);
    expect(lIdx).toBeLessThan(bIdx);
    expect(bIdx).toBeLessThan(haIdx);

    const aliceRow = screen.getByText('Alice').closest('tr') as HTMLElement;
    expect(within(aliceRow).getByText('2h')).toBeInTheDocument(); // Ha 7200s
    expect(within(aliceRow).getByText('1h')).toBeInTheDocument(); // B 3600s
  });
});

describe('MembersTab — Holds', () => {
  it('renders frames, bytes and share percent', async () => {
    mockMembers([
      member({ accountId: 'a1', displayName: 'Alice', holdsFrames: 42, holdsBytes: 1024 * 1024, holdsShare: 0.256 }),
    ]);
    renderTab();
    const row = (await screen.findByText('Alice')).closest('tr') as HTMLElement;
    expect(within(row).getByText(/42 fr/)).toBeInTheDocument();
    expect(within(row).getByText(/1\.0 MB/)).toBeInTheDocument();
    expect(within(row).getByText(/26%/)).toBeInTheDocument();
  });
});

describe('MembersTab — expanded row', () => {
  it('is collapsed by default and expands on row click', async () => {
    mockMembers([
      member({
        accountId: 'a1',
        displayName: 'Alice',
        devices: [{ device: 'deviceabcdef12', name: 'Mac mini', online: true }],
        qualityByCamera: [{ camera: 'ZWO ASI2600MM', filter: 'L', frames: 10, medianFwhm: 2.5, medianEcc: 0.3 }],
      }),
    ]);
    renderTab();
    const row = (await screen.findByText('Alice')).closest('tr') as HTMLElement;
    expect(screen.queryByText('Mac mini')).not.toBeInTheDocument();
    fireEvent.click(row);
    expect(await screen.findByText('Mac mini')).toBeInTheDocument();
    expect(screen.getByText('ZWO ASI2600MM')).toBeInTheDocument();
  });

  it('lists a device by its id prefix when it has no name', async () => {
    mockMembers([
      member({
        accountId: 'a1',
        displayName: 'Alice',
        devices: [{ device: 'deviceabcdef12', name: null, online: false }],
      }),
    ]);
    renderTab();
    const row = (await screen.findByText('Alice')).closest('tr') as HTMLElement;
    fireEvent.click(row);
    expect(await screen.findByText('deviceab')).toBeInTheDocument();
  });

  it('labels an empty camera as "Unknown camera"', async () => {
    mockMembers([
      member({
        accountId: 'a1',
        displayName: 'Alice',
        qualityByCamera: [{ camera: '', filter: 'L', frames: 4, medianFwhm: 2.1, medianEcc: 0.2 }],
      }),
    ]);
    renderTab();
    const row = (await screen.findByText('Alice')).closest('tr') as HTMLElement;
    fireEvent.click(row);
    expect(await screen.findByText('Unknown camera')).toBeInTheDocument();
  });
});
