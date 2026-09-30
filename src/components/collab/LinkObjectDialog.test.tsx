import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import LinkObjectDialog from './LinkObjectDialog';
import { api } from '../../api';
import type { LinkSuggestion, LinkedSetView } from '../../types/models';

vi.mock('../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));
const notify = vi.fn();
vi.mock('../../contexts/NotificationContext', () => ({ useNotifications: () => ({ notify }) }));
afterEach(cleanup);

const suggestion = (o: Partial<LinkSuggestion> = {}): LinkSuggestion => ({
  framesSetId: 7, name: 'M31 set', lightCount: 12, withinRadius: true, distanceDeg: 0.2, alreadyLinked: false, ...o,
}) as LinkSuggestion;

function mock(list: unknown, link?: () => Promise<unknown>) {
  vi.mocked(api.invoke).mockImplementation(((command: string) => {
    if (command === 'list_collab_link_suggestions') return Promise.resolve(list);
    if (command === 'set_collab_link') return link ? link() : Promise.resolve(null);
    return Promise.reject(new Error(`unexpected ${command}`));
  }) as typeof api.invoke);
}

const renderDialog = (links: LinkedSetView[] = [], onClose = vi.fn()) =>
  render(<LinkObjectDialog projectId="p" links={links} onClose={onClose} onChanged={vi.fn()} />);

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  notify.mockReset();
  vi.spyOn(console, 'error').mockImplementation(() => {});
});

describe('LinkObjectDialog', () => {
  it('renders through the shared dialog shell', async () => {
    mock([suggestion()]);
    renderDialog();
    const d = screen.getByRole('dialog', { name: /Link an object/ });
    expect(d.className).toContain('rounded-lg');
    expect(d.className).toMatch(/w-\[560px\]/);
    await screen.findByText('M31 set');
  });

  it('puts initial focus on Close (no field, no primary)', async () => {
    mock([suggestion()]);
    renderDialog();
    await screen.findByText('M31 set');
    expect(document.activeElement?.textContent).toBe('Close');
  });

  it('survives a null suggestion list', async () => {
    mock(null);
    renderDialog();
    expect(await screen.findByText('No frame sets to link yet.')).toBeInTheDocument();
  });

  it('a failed Link logs and notifies', async () => {
    mock([suggestion()], () => Promise.reject(new Error('hub down')));
    renderDialog();
    fireEvent.click(await screen.findByRole('button', { name: 'Link' }));
    await waitFor(() => expect(notify).toHaveBeenCalledWith(expect.objectContaining({ detail: 'hub down', hasErrors: true })));
    expect(console.error).toHaveBeenCalled();
  });

  it('a failed Unlink logs and notifies', async () => {
    mock([], () => Promise.reject(new Error('nope')));
    renderDialog([{ framesSetId: 3, name: 'Old', lightCount: 2, withinRadius: true } as LinkedSetView]);
    fireEvent.click(await screen.findByRole('button', { name: 'Unlink' }));
    await waitFor(() => expect(notify).toHaveBeenCalledWith(expect.objectContaining({ detail: 'nope', hasErrors: true })));
  });

  it('Escape closes', async () => {
    mock([]);
    const onClose = vi.fn();
    renderDialog([], onClose);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onClose).toHaveBeenCalled();
    await waitFor(() => expect(api.invoke).toHaveBeenCalled());
  });
});
