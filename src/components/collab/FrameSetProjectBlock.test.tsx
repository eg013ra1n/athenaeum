import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import FrameSetProjectBlock from './FrameSetProjectBlock';
import { api } from '../../api';
import type { FrameSetProjectStatus } from '../../types/models';

vi.mock('../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));
const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));
afterEach(() => {
  cleanup();
  notifyMock.mockClear();
});

const counts = { notPublished: 0, failsGate: 79, pendingApproval: 0, published: 812, updatePending: 3, rejected: 0, publishedNotOnDisk: 0, publishedNowFailsGate: 0 };

describe('FrameSetProjectBlock', () => {
  it('shows the linked project with its counts and Open project', () => {
    const status: FrameSetProjectStatus = { links: [{ projectId: 'p1', slug: 'm101', title: 'M 101', publishingHere: true, autoPublish: true, counts, frames: [] }], candidates: [] };
    render(<MemoryRouter><FrameSetProjectBlock framesSetId={5} status={status} onChanged={vi.fn()} /></MemoryRouter>);
    expect(screen.getByText(/Project M 101/)).toBeInTheDocument();
    expect(screen.getByText(/812 published · 79 fail gate · 3 update pending/)).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Open project' })).toHaveAttribute('href', '/projects/p1?tab=mine');
  });

  it('offers Link to project for a candidate and links on click', async () => {
    vi.mocked(api.invoke).mockResolvedValue(undefined);
    const onChanged = vi.fn();
    const status: FrameSetProjectStatus = { links: [], candidates: [{ projectId: 'p1', slug: 'm101', title: 'M 101', distanceDeg: 0.42 }] };
    render(<MemoryRouter><FrameSetProjectBlock framesSetId={5} status={status} onChanged={onChanged} /></MemoryRouter>);
    expect(screen.getByText('Matches project M 101 (0.4° away)')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Link to project' }));
    await vi.waitFor(() => expect(api.invoke).toHaveBeenCalledWith('set_collab_link', { projectId: 'p1', framesSetId: 5, linked: true }));
    await vi.waitFor(() => expect(onChanged).toHaveBeenCalled());
  });

  it('a failed link logs and notify()s, and never calls onChanged', async () => {
    const consoleErrorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValue(new Error('boom'));
    const onChanged = vi.fn();
    const status: FrameSetProjectStatus = { links: [], candidates: [{ projectId: 'p1', slug: 'm101', title: 'M 101', distanceDeg: 0.42 }] };
    render(<MemoryRouter><FrameSetProjectBlock framesSetId={5} status={status} onChanged={onChanged} /></MemoryRouter>);
    fireEvent.click(screen.getByRole('button', { name: 'Link to project' }));
    await vi.waitFor(() =>
      expect(notifyMock).toHaveBeenCalledWith(expect.objectContaining({ title: 'Could not link to the project' })),
    );
    expect(consoleErrorSpy).toHaveBeenCalled();
    expect(onChanged).not.toHaveBeenCalled();
    consoleErrorSpy.mockRestore();
  });

  it('renders nothing with no links and no candidates', () => {
    const { container } = render(<MemoryRouter><FrameSetProjectBlock framesSetId={5} status={{ links: [], candidates: [] }} onChanged={vi.fn()} /></MemoryRouter>);
    expect(container).toBeEmptyDOMElement();
  });
});
