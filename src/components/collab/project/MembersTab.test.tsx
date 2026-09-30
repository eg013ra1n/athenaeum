import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { formatRelative } from '../format';
import MembersTab, { filterColumns } from './MembersTab';
import type { MemberSummary } from '../../../types/models';

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

let seq = 0;
/** Builds a `MemberSummary` from a name, seconds per filter and a patch. */
function m(name: string, secondsByFilter: Record<string, number> = {}, patch: Partial<MemberSummary> = {}): MemberSummary {
  seq += 1;
  return {
    accountId: `acc-${name}-${seq}`,
    displayName: name,
    dataRole: 'send_receive',
    coordinator: false,
    devices: [],
    online: false,
    lastSeenAt: null,
    publishedFrames: 0,
    secondsByFilter,
    qualityByCamera: [],
    holdsFrames: 0,
    holdsBytes: 0,
    holdsShare: 0,
    ...patch,
  };
}

function names(): string[] {
  return screen.getAllByTestId('member-name').map((el) => el.textContent ?? '');
}

describe('MembersTab — mockup table', () => {
  it('columns in the mockup order, numeric headers right-aligned', () => {
    render(<MembersTab projectId="p" members={[m('Kostya', { L: 7200 })]} error={false} />);
    const heads = screen.getAllByRole('columnheader').map((h) => h.textContent?.trim());
    expect(heads).toEqual(['Member', 'Role', 'Devices', 'Published ↓', 'L', 'Σ', 'FWHM x̃', 'Holds', 'Last seen']);
    expect(screen.getByRole('columnheader', { name: 'Σ' }).className).toContain('text-right');
  });

  it('filter cells read "2h 00m" or a ghost dash; None becomes "No filter" last', () => {
    render(<MembersTab projectId="p" members={[m('A', { L: 7200, None: 600 }), m('B', {})]} error={false} />);
    expect(screen.getByRole('columnheader', { name: 'No filter' })).toBeInTheDocument();
    expect(screen.getAllByText('2h 00m')[0]).toBeInTheDocument();
    expect(screen.getAllByText('—')[0].className).toContain('text-content-ghost');
    const heads = screen.getAllByRole('columnheader').map((h) => h.textContent?.trim());
    expect(heads.indexOf('No filter')).toBe(heads.indexOf('Σ') - 1);
  });

  it('12 filters scroll horizontally inside the table box (review focus 2)', () => {
    const secs = Object.fromEntries(
      ['L', 'R', 'G', 'B', 'Ha', 'OIII', 'SII', 'OSC', 'CLS', 'S2 6nm', 'O3 3nm', 'L-eXtreme'].map((f) => [f, 600]),
    );
    const { container } = render(<MembersTab projectId="p" members={[m('A', secs)]} error={false} />);
    expect(container.querySelector('[data-testid="members-scroll"]')!.className).toContain('overflow-x-auto');
    expect(screen.getAllByRole('columnheader')).toHaveLength(9 + 11);
  });

  it('a row click opens the member panel with cameras and devices', () => {
    render(
      <MembersTab
        projectId="p"
        members={[
          m('Kostya', { L: 7200 }, {
            devices: [{ device: 'deviceabcdef12', name: 'Mac mini', online: true }],
            qualityByCamera: [{ camera: 'ASI6200MM Pro', filter: 'L', frames: 60, medianFwhm: 2.3, medianEcc: 0.4 }],
          }),
        ]}
        error={false}
      />,
    );
    fireEvent.click(screen.getByText('Kostya'));
    const panel = screen.getByRole('complementary', { name: 'Member details' });
    expect(within(panel).getByText('ASI6200MM Pro')).toBeInTheDocument();
    expect(within(panel).getByText(/60 fr · x̃ FWHM 2\.30″ · x̃ ecc 0\.40/)).toBeInTheDocument();
    expect(within(panel).getByText('Mac mini')).toBeInTheDocument();
    fireEvent.click(within(panel).getByRole('button', { name: 'Close' }));
    expect(screen.queryByRole('complementary')).not.toBeInTheDocument();
  });

  it('no members → an empty state (review focus 4)', () => {
    render(<MembersTab projectId="p" members={[]} error={false} />);
    expect(screen.getByText('No members yet.')).toBeInTheDocument();
  });

  it('members === null shows Loading…, an error shows the inline message and no table', () => {
    const { rerender } = render(<MembersTab projectId="p" members={null} error={false} />);
    expect(screen.getByText('Loading…')).toBeInTheDocument();
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
    rerender(<MembersTab projectId="p" members={null} error />);
    expect(screen.getByText('Could not load members — see console.')).toBeInTheDocument();
    expect(screen.queryByText('Loading…')).not.toBeInTheDocument();
    expect(screen.queryByRole('table')).not.toBeInTheDocument();
    spy.mockRestore();
  });

  it('filterColumns: filters with seconds, filterOrder, None last', () => {
    expect(filterColumns([m('A', { Ha: 1, None: 5, L: 2, B: 0 }), m('B', { R: 3 })])).toEqual(['L', 'R', 'Ha', 'None']);
  });

  it('Role: Processor/Contributor/raw; a coordinator carries the data role faintly', () => {
    render(
      <MembersTab
        projectId="p"
        members={[
          m('Alice', {}, { dataRole: 'send' }),
          m('Bob', {}, { dataRole: 'receive' }),
          m('Carol', {}, { coordinator: true, dataRole: 'send_receive' }),
        ]}
        error={false}
      />,
    );
    expect(screen.getByText('Contributor')).toBeInTheDocument();
    expect(screen.getByText('receive')).toBeInTheDocument();
    expect(screen.getByText('(Processor data)').className).toContain('text-content-faint');
  });

  it('device dots carry the device name as a title', () => {
    const { container } = render(
      <MembersTab projectId="p" members={[m('A', {}, { devices: [{ device: 'deviceabcdef12', name: null, online: false }] })]} error={false} />,
    );
    expect(container.querySelector('[title="deviceab"]')).not.toBeNull();
  });

  it('FWHM x̃ is the frame-weighted median over cameras; a ghost dash without data', () => {
    render(
      <MembersTab
        projectId="p"
        members={[
          m('A', {}, {
            qualityByCamera: [
              { camera: 'c1', filter: 'L', frames: 10, medianFwhm: 2.0, medianEcc: null },
              { camera: 'c2', filter: 'L', frames: 1, medianFwhm: 4.0, medianEcc: null },
              { camera: 'c3', filter: 'R', frames: 10, medianFwhm: 3.0, medianEcc: null },
            ],
          }),
          m('B'),
        ]}
        error={false}
      />,
    );
    expect(screen.getByText('3.00″')).toBeInTheDocument();
  });

  it('Holds and Last seen', () => {
    vi.useFakeTimers({ toFake: ['Date'] });
    vi.setSystemTime(new Date('2026-09-29T12:00:00Z'));
    const iso = '2026-09-29T11:00:00Z';
    render(
      <MembersTab
        projectId="p"
        members={[
          m('Alice', {}, { holdsFrames: 42, holdsBytes: 1024 * 1024, holdsShare: 0.256, online: true }),
          m('Bob', {}, { lastSeenAt: iso }),
        ]}
        error={false}
      />,
    );
    const row = screen.getByText('Alice').closest('tr') as HTMLElement;
    expect(within(row).getByText(/42 fr · 1 MB/)).toBeInTheDocument();
    expect(within(row).getByText('26%').className).toContain('text-content-faint');
    expect(within(row).getByText('now').className).toContain('text-success');
    expect(screen.getByText(formatRelative(iso, Date.now()))).toBeInTheDocument();
  });

  it('sorting: Published desc by default, a header click flips, Last seen puts online first', () => {
    render(
      <MembersTab
        projectId="p"
        members={[
          m('Alice', {}, { publishedFrames: 5, lastSeenAt: '2026-09-20T10:00:00Z' }),
          m('Bob', {}, { publishedFrames: 50, online: true, lastSeenAt: '2026-09-01T10:00:00Z' }),
          m('Carol', {}, { publishedFrames: 20 }),
          m('Dana', {}, { publishedFrames: 1, lastSeenAt: '2026-09-25T10:00:00Z' }),
        ]}
        error={false}
      />,
    );
    expect(names()).toEqual(['Bob', 'Carol', 'Alice', 'Dana']);
    fireEvent.click(screen.getByText(/^Published/));
    expect(names()).toEqual(['Dana', 'Alice', 'Carol', 'Bob']);
    fireEvent.click(screen.getByText('Last seen'));
    expect(names()).toEqual(['Bob', 'Dana', 'Alice', 'Carol']);
  });

  it('labels an empty camera "Unknown camera" in the panel', () => {
    render(
      <MembersTab
        projectId="p"
        members={[m('A', {}, { qualityByCamera: [{ camera: '', filter: 'L', frames: 4, medianFwhm: 2.1, medianEcc: 0.2 }] })]}
        error={false}
      />,
    );
    fireEvent.click(screen.getByText('A'));
    expect(screen.getByText('Unknown camera')).toBeInTheDocument();
  });

  it('a panel without published frames says so', () => {
    render(<MembersTab projectId="p" members={[m('A')]} error={false} />);
    fireEvent.click(screen.getByText('A'));
    expect(screen.getByText('Nothing published yet.')).toBeInTheDocument();
  });
});
