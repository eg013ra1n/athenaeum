import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import FilterMappingDialog from './FilterMappingDialog';
import { api } from '../../api';
import type { FilterMappingSheet, GateReport } from '../../types/models';

vi.mock('../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));
vi.mock('../../contexts/NotificationContext', () => ({ useNotifications: () => ({ notify: vi.fn() }) }));
afterEach(cleanup);

const sheet: FilterMappingSheet = {
  projectId: 'p',
  dictionary: [
    { canonical: 'L', aliases: ['lum'], kind: 'luminance' },
    { canonical: 'Ha', aliases: [], kind: 'narrowband' },
    { canonical: 'None', aliases: ['none'], kind: 'unfiltered' },
  ],
  rows: [
    { instrume: 'ATR2600M', filterRaw: '', frames: 891, resolution: 'unmapped', canonical: null, proposal: 'None' },
    { instrume: 'QHY268M', filterRaw: 'Slot 0', frames: 53, resolution: 'unmapped', canonical: null, proposal: null },
    { instrume: 'ASI294MM Pro', filterRaw: 'H', frames: 1538, resolution: 'mappedToMissing', canonical: 'Hb', proposal: 'Ha' },
    { instrume: 'QHY268M', filterRaw: 'L', frames: 1093, resolution: 'matched', canonical: 'L', proposal: null },
  ],
};
const report: GateReport = { projectId: 'p', total: 4, publishable: 4, rows: [], blockers: [] };

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((command: string) => {
    if (command === 'get_collab_filter_mapping_sheet') return Promise.resolve(sheet);
    if (command === 'set_collab_filter_mappings') return Promise.resolve(report);
    return Promise.reject(new Error(`unexpected ${command}`));
  }) as typeof api.invoke);
});

describe('FilterMappingDialog', () => {
  it('lists unresolved rows first with proposals preselected, and sends only the changed rows', async () => {
    const onSaved = vi.fn();
    render(<FilterMappingDialog projectId="p" onClose={vi.fn()} onSaved={onSaved} />);
    await waitFor(() => expect(screen.getByText('(no FILTER) · ATR2600M — 891 frames')).toBeInTheDocument());
    const selects = screen.getAllByRole('combobox');
    expect(selects[0]).toHaveValue('None'); // proposal preselected
    expect(selects[1]).toHaveValue(''); // slot name: no proposal
    expect(selects[2]).toHaveValue('Ha'); // mappedToMissing: proposal, with the stale name shown
    expect(screen.getByText(/mapped to "Hb", not in this project/)).toBeInTheDocument();
    expect(selects[3]).toHaveValue('L'); // resolved row below the divider
    // A preselected proposal is the publisher's pending choice (F3): Save is
    // enabled and confirming writes it.
    const save = screen.getByRole('button', { name: 'Save' });
    expect(save).not.toBeDisabled();
    fireEvent.change(selects[1], { target: { value: 'L' } });
    fireEvent.click(save);
    await waitFor(() => expect(onSaved).toHaveBeenCalledWith(report));
    expect(api.invoke).toHaveBeenCalledWith('set_collab_filter_mappings', {
      projectId: 'p',
      mappings: [
        { instrume: 'ATR2600M', filterRaw: '', canonical: 'None' },
        { instrume: 'QHY268M', filterRaw: 'Slot 0', canonical: 'L' },
        { instrume: 'ASI294MM Pro', filterRaw: 'H', canonical: 'Ha' },
      ],
    });
  });

  it('disables Save when nothing changed, including AUTO on an already-matched row (a no-op)', async () => {
    render(<FilterMappingDialog projectId="p" onClose={vi.fn()} onSaved={vi.fn()} />);
    await waitFor(() => expect(screen.getAllByRole('combobox')).toHaveLength(4));
    const selects = screen.getAllByRole('combobox');
    // Undo every proposal: the two proposed rows back to "no choice".
    fireEvent.change(selects[0], { target: { value: '' } });
    fireEvent.change(selects[2], { target: { value: '' } });
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();
    // The `matched` row already resolves through the dictionary alone —
    // nothing is stored, so "— automatic —" on it is a no-op and must not
    // enable Save.
    fireEvent.change(selects[3], { target: { value: '__auto__' } });
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();
  });

  it('AUTO on a mappedToMissing row deletes its stale mapping', async () => {
    render(<FilterMappingDialog projectId="p" onClose={vi.fn()} onSaved={vi.fn()} />);
    await waitFor(() => expect(screen.getAllByRole('combobox')).toHaveLength(4));
    const selects = screen.getAllByRole('combobox');
    // Undo the two proposed unresolved rows: no pending change from them.
    fireEvent.change(selects[0], { target: { value: '' } });
    // The stale `mappedToMissing` mapping ("Hb", no longer in this project's
    // dictionary) is cleared back to automatic instead of forcing a pick.
    fireEvent.change(selects[2], { target: { value: '__auto__' } });
    expect(screen.getByRole('button', { name: 'Save' })).not.toBeDisabled();
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('set_collab_filter_mappings', {
      projectId: 'p',
      mappings: [{ instrume: 'ASI294MM Pro', filterRaw: 'H', canonical: null }],
    }));
  });

  it('shows the sheet refusal inline', async () => {
    vi.mocked(api.invoke).mockImplementation(() => Promise.reject(new Error("the project's filter dictionary has not been fetched yet")));
    render(<FilterMappingDialog projectId="p" onClose={vi.fn()} onSaved={vi.fn()} />);
    await waitFor(() => expect(screen.getByText(/has not been fetched yet/)).toBeInTheDocument());
  });

  it('is an accessible dialog, Escape closes it, and a resolved row has no blank option', async () => {
    const onClose = vi.fn();
    render(<FilterMappingDialog projectId="p" onClose={onClose} onSaved={vi.fn()} />);
    await waitFor(() => expect(screen.getAllByRole('combobox')).toHaveLength(4));
    const dialog = screen.getByRole('dialog', { name: 'Filter mapping' });
    expect(dialog).toHaveAttribute('aria-modal', 'true');

    // Row 3 is `matched` (resolved): only "— automatic —" plus the
    // dictionary entries, never a blank placeholder option.
    const resolvedSelect = screen.getAllByRole('combobox')[3];
    const optionTexts = Array.from(resolvedSelect.querySelectorAll('option')).map((o) => o.value);
    expect(optionTexts).not.toContain('');

    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onClose).toHaveBeenCalled();
  });
});
