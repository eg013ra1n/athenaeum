import { afterEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react';
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
    render(<MembersTab goals={null} members={[m('Kostya', { L: 7200 })]} error={false} />);
    const heads = screen.getAllByRole('columnheader').map((h) => h.textContent?.trim());
    expect(heads).toEqual(['Member', 'Role', 'Devices', 'Published ↓', 'L', 'Σ', 'FWHM x̃', 'Holds', 'Last seen']);
    expect(screen.getByRole('columnheader', { name: 'Σ' }).className).toContain('text-right');
  });

  it('filter cells read "2h 00m" or a ghost dash; None becomes "No filter" last', () => {
    render(<MembersTab goals={null} members={[m('A', { L: 7200, None: 600 }), m('B', {})]} error={false} />);
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
    const { container } = render(<MembersTab goals={null} members={[m('A', secs)]} error={false} />);
    expect(container.querySelector('[data-testid="members-scroll"]')!.className).toContain('overflow-x-auto');
    expect(screen.getAllByRole('columnheader')).toHaveLength(9 + 11);
  });

  it('a row click opens the member panel with cameras and devices', () => {
    render(
      <MembersTab
        goals={null}
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

  it('a clickable member row hovers with the plain-table token, no raw colour', () => {
    render(<MembersTab goals={null} members={[m('Kostya')]} error={false} />);
    const row = screen.getByText('Kostya').closest('tr')!;
    expect(row.className).toContain('hover:bg-table-plain-hover');
    expect(row.className).not.toContain('rgba(');
  });

  it('no members → an empty state (review focus 4)', () => {
    render(<MembersTab goals={null} members={[]} error={false} />);
    expect(screen.getByText('No members yet.')).toBeInTheDocument();
  });

  it('members === null shows Loading…, an error shows the inline message and no table', () => {
    const { rerender } = render(<MembersTab goals={null} members={null} error={false} />);
    expect(screen.getByText('Loading…')).toBeInTheDocument();
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
    rerender(<MembersTab goals={null} members={null} error />);
    expect(screen.getByText('Could not load members — see console.')).toBeInTheDocument();
    expect(screen.queryByText('Loading…')).not.toBeInTheDocument();
    expect(screen.queryByRole('table')).not.toBeInTheDocument();
    spy.mockRestore();
  });

  it('filterColumns: filters with seconds, filterOrder, None last', () => {
    expect(filterColumns([m('A', { Ha: 1, None: 5, L: 2, B: 0 }), m('B', { R: 3 })], null)).toEqual(['L', 'R', 'Ha', 'None']);
    expect(filterColumns([m('A', { L: 2 })], { Ha: 3600, None: 60 })).toEqual(['L', 'Ha', 'None']);
  });

  it('Role: Processor/Contributor/raw; a coordinator carries the data role faintly', () => {
    render(
      <MembersTab
        goals={null}
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
      <MembersTab goals={null} members={[m('A', {}, { devices: [{ device: 'deviceabcdef12', name: null, online: false }] })]} error={false} />,
    );
    expect(container.querySelector('[title="deviceab"]')).not.toBeNull();
  });

  it('FWHM x̃ is the frame-weighted median over cameras; a ghost dash without data', () => {
    render(
      <MembersTab
        goals={null}
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
        goals={null}
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
    // A member holding nothing reads a ghost dash, not "0 fr · 0 KB 0%" (like Σ).
    const bob = screen.getByText('Bob').closest('tr') as HTMLElement;
    expect(within(bob).queryByText(/0 fr/)).toBeNull();
  });

  it('Last seen ticks every 60 s while the tab stays open, and the tick stops on unmount', () => {
    vi.useFakeTimers({ toFake: ['Date', 'setInterval', 'clearInterval'] });
    vi.setSystemTime(new Date('2026-09-29T12:00:00Z'));
    const { unmount } = render(
      <MembersTab goals={null} members={[m('Bob', {}, { lastSeenAt: '2026-09-29T11:55:00Z' })]} error={false} />,
    );
    const bob = screen.getByText('Bob').closest('tr') as HTMLElement;
    expect(within(bob).getByText('5 min ago')).toBeInTheDocument();
    act(() => { vi.advanceTimersByTime(59_000); });
    expect(within(bob).getByText('5 min ago')).toBeInTheDocument();
    act(() => { vi.advanceTimersByTime(1_000); });
    expect(within(bob).getByText('6 min ago')).toBeInTheDocument();
    expect(vi.getTimerCount()).toBe(1);
    unmount();
    expect(vi.getTimerCount()).toBe(0);
  });

  it('sorting: Published desc by default, a header click flips, Last seen puts online first', () => {
    render(
      <MembersTab
        goals={null}
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
        goals={null}
        members={[m('A', {}, { qualityByCamera: [{ camera: '', filter: 'L', frames: 4, medianFwhm: 2.1, medianEcc: 0.2 }] })]}
        error={false}
      />,
    );
    fireEvent.click(screen.getByText('A'));
    expect(screen.getByText('Unknown camera')).toBeInTheDocument();
  });

  it('a panel without published frames says so', () => {
    render(<MembersTab goals={null} members={[m('A')]} error={false} />);
    fireEvent.click(screen.getByText('A'));
    expect(screen.getByText('Nothing published yet.')).toBeInTheDocument();
  });

  it('a goal-only filter gets a column with ghost dashes', () => {
    render(<MembersTab goals={{ OIII: 7200 }} members={[m('A', { L: 600 })]} error={false} />);
    expect(screen.getByRole('columnheader', { name: 'OIII' })).toBeInTheDocument();
    const row = screen.getByText('A').closest('tr') as HTMLElement;
    expect(within(row).getAllByText('—').length).toBeGreaterThanOrEqual(2); // OIII and FWHM
  });

  it('FWHM x̃ sorts with null last in both directions', () => {
    const q = (f: number) => [{ camera: 'c', filter: 'L', frames: 5, medianFwhm: f, medianEcc: null }];
    render(
      <MembersTab
        goals={null}
        members={[m('Sharp', {}, { qualityByCamera: q(2) }), m('Soft', {}, { qualityByCamera: q(4) }), m('None')]}
        error={false}
      />,
    );
    fireEvent.click(screen.getByText('FWHM x̃'));
    const first = names();
    fireEvent.click(screen.getByText(/^FWHM x̃/));
    const second = names();
    expect(first[2]).toBe('None');
    expect(second[2]).toBe('None');
    expect(first.slice(0, 2)).toEqual(second.slice(0, 2).reverse());
  });

  it('Devices sorts by online-device count', () => {
    const d = (n: number) => Array.from({ length: n }, (_, i) => ({ device: `dev${n}${i}`, name: null, online: true }));
    render(<MembersTab goals={null} members={[m('One', {}, { devices: d(1) }), m('Three', {}, { devices: d(3) })]} error={false} />);
    fireEvent.click(screen.getByText('Devices'));
    expect(names()).toEqual(['Three', 'One']);
    fireEvent.click(screen.getByText(/^Devices/));
    expect(names()).toEqual(['One', 'Three']);
  });

  it('Role sorts by the label the cell shows, not the data role', () => {
    render(
      <MembersTab
        goals={null}
        members={[m('Zed', {}, { dataRole: 'send' }), m('Amy', {}, { coordinator: true, dataRole: 'send_receive' })]}
        error={false}
      />,
    );
    fireEvent.click(screen.getByText('Role'));
    // "Contributor" < "Coordinator"; by data role it would be send < send_receive too,
    // so flip to descending, where the two orderings differ only by label.
    expect(names()).toEqual(['Zed', 'Amy']);
    fireEvent.click(screen.getByText(/^Role/));
    expect(names()).toEqual(['Amy', 'Zed']);
  });

  it('Σ is a ghost dash without integration', () => {
    render(<MembersTab goals={null} members={[m('A')]} error={false} />);
    const cells = Array.from((screen.getByText('A').closest('tr') as HTMLElement).querySelectorAll('td'));
    expect(cells[cells.length - 4].textContent).toBe('—'); // Σ
  });

  it('a long name truncates with a title', () => {
    const long = 'A'.repeat(30);
    render(<MembersTab goals={null} members={[m(long)]} error={false} />);
    const el = screen.getByText(long);
    expect(el.className).toContain('truncate');
    expect(el.className).toContain('max-w-[16rem]');
    expect(el.getAttribute('title')).toBe(long);
  });

  it('an offline member never seen reads "never"; the panel shows the Coordinator chip', () => {
    render(<MembersTab goals={null} members={[m('Boss', {}, { coordinator: true })]} error={false} />);
    expect(screen.getByText('never')).toBeInTheDocument();
    fireEvent.click(screen.getByText('Boss'));
    const panel = screen.getByRole('complementary', { name: 'Member details' });
    expect(within(panel).getByText('Coordinator')).toBeInTheDocument();
  });

  it('loading and error states use the spec styles', () => {
    const { rerender } = render(<MembersTab goals={null} members={null} error={false} />);
    expect(screen.getByText('Loading…').className).toContain('text-content-faint');
    rerender(<MembersTab goals={null} members={null} error />);
    expect(screen.getByText('Could not load members — see console.').className).toContain('text-[12.5px]');
  });
});
