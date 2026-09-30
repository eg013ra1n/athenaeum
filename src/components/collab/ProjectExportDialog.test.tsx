import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import ProjectExportDialog from './ProjectExportDialog';
import { api } from '../../api';

vi.mock('../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));
vi.mock('../../api/desktop', () => ({ pickDirectory: vi.fn().mockResolvedValue('/out') }));
vi.mock('../../utils/platform', () => ({ isTauri: true }));
vi.mock('../FolderBrowserModal', () => ({ FolderBrowserModal: () => null }));
afterEach(cleanup);

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.listen).mockImplementation((() => Promise.resolve(() => {})) as never);
});

describe('ProjectExportDialog', () => {
  it('renders through the shared dialog shell', () => {
    render(<ProjectExportDialog projectId="p" projectTitle="M31" onClose={vi.fn()} />);
    const d = screen.getByRole('dialog', { name: 'Export “M31” for WBPP' });
    expect(d.className).toContain('rounded-lg');
    expect(d.className).toMatch(/w-\[560px\]/);
  });

  it('puts initial focus on Browse (Export starts disabled)', () => {
    render(<ProjectExportDialog projectId="p" onClose={vi.fn()} />);
    expect(screen.getByRole('button', { name: /Browse/ })).toHaveFocus();
    expect(screen.getByRole('button', { name: /Export$/ })).toBeDisabled();
  });

  it('while exporting, Cancel calls cancel_export and Escape is locked', async () => {
    vi.mocked(api.invoke).mockImplementation((() => new Promise(() => {})) as never);
    const onClose = vi.fn();
    render(<ProjectExportDialog projectId="p" onClose={onClose} />);
    fireEvent.click(screen.getByRole('button', { name: /Browse/ }));
    await waitFor(() => expect(screen.getByRole('button', { name: /Export$/ })).not.toBeDisabled());
    fireEvent.click(screen.getByRole('button', { name: /Export$/ }));
    const cancel = await screen.findByRole('button', { name: 'Cancel' });
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.click(cancel);
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('cancel_export', { frameSetId: -1 }));
  });
});
