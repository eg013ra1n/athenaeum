import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { api } from '../../../api';
import FilterRemapSection from './FilterRemapSection';
import type { CollabRemapFinished, CollabRemapProgress, FilterBucket, FilterBucketSheet } from '../../../types/models';

vi.mock('../../../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

afterEach(cleanup);

function bucket(overrides: Partial<FilterBucket> = {}): FilterBucket {
  return {
    publisherAccountId: 'acc-1',
    publisher: 'Alice',
    instrume: 'ASI2600MM Pro',
    filterRaw: 'Red',
    canonical: 'L',
    frameUuids: ['u-1', 'u-2', 'u-3'],
    ...overrides,
  };
}

function sheet(buckets: FilterBucket[], running: CollabRemapProgress | null = null): FilterBucketSheet {
  return {
    projectId: 'proj-1',
    dictionary: [
      { canonical: 'L', aliases: [], kind: 'mono' },
      { canonical: 'R', aliases: [], kind: 'mono' },
    ],
    buckets,
    running,
  };
}

let current: FilterBucketSheet;
let listeners: Record<string, (p: unknown) => void> = {};

function emit<T>(event: string, payload: T) {
  const h = listeners[event];
  if (!h) throw new Error(`no listener for ${event}`);
  act(() => h(payload));
}

function bucketCalls() {
  return vi.mocked(api.invoke).mock.calls.filter(([cmd]) => cmd === 'get_collab_filter_buckets');
}

beforeEach(() => {
  listeners = {};
  current = sheet([bucket()]);
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((command: string) => {
    if (command === 'get_collab_filter_buckets') return Promise.resolve(current);
    return Promise.resolve(null);
  }) as never);
  vi.mocked(api.listen).mockImplementation(((event: string, cb: (p: unknown) => void) => {
    listeners[event] = cb;
    return Promise.resolve(() => {});
  }) as never);
});

async function renderSection() {
  const utils = render(<FilterRemapSection projectId="proj-1" />);
  await screen.findByText('Alice');
  return utils;
}

describe('FilterRemapSection', () => {
  it('renders one row per bucket with raw filter, canonical and count', async () => {
    current = sheet([bucket(), bucket({ publisherAccountId: 'acc-2', publisher: 'Bob', filterRaw: '', canonical: null, frameUuids: ['x'] })]);
    await renderSection();
    expect(screen.getByText('Red')).toBeInTheDocument();
    expect(screen.getByText('(no FILTER)')).toBeInTheDocument();
    expect(screen.getByText('mixed')).toBeInTheDocument();
    expect(screen.getByText('3')).toBeInTheDocument();
    expect(screen.getAllByText('ASI2600MM Pro')).toHaveLength(2);
  });

  it('Apply is disabled until the choice differs; confirming remaps the bucket frames', async () => {
    await renderSection();
    const apply = screen.getByRole('button', { name: 'Apply' });
    expect(apply).toBeDisabled();
    fireEvent.change(screen.getByLabelText('Canonical filter for Red'), { target: { value: 'R' } });
    expect(apply).toBeEnabled();
    fireEvent.click(apply);
    expect(await screen.findByText('Remap filter')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Remap' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('remap_collab_filter', {
        projectId: 'proj-1',
        frameUuids: ['u-1', 'u-2', 'u-3'],
        canonical: 'R',
      }),
    );
  });

  it('shows a run already in progress on mount, without any event', async () => {
    current = sheet([bucket()], { projectId: 'proj-1', canonical: 'R', done: 2, failed: 1, total: 10 });
    await renderSection();
    expect(await screen.findByText(/Remapping… 3 of 10/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Apply' })).toBeDisabled();
  });

  it('shows progress with a Cancel button that cancels the run', async () => {
    await renderSection();
    emit<CollabRemapProgress>('collab-remap-progress', { projectId: 'proj-1', canonical: 'R', done: 3, failed: 0, total: 12 });
    expect(await screen.findByText(/Remapping… 3 of 12/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('cancel_collab_remap', { projectId: 'proj-1' }));
  });

  it('finished shows the result and reloads; other projects are ignored', async () => {
    await renderSection();
    const finished: CollabRemapFinished = {
      projectId: 'proj-1', canonical: 'R', done: 12, failed: 0, total: 12, cancelled: false, error: null,
    };
    emit('collab-remap-finished', { ...finished, projectId: 'other' });
    expect(bucketCalls()).toHaveLength(1);
    emit('collab-remap-finished', finished);
    expect(await screen.findByText('12 of 12 frames remapped to R.')).toBeInTheDocument();
    await waitFor(() => expect(bucketCalls()).toHaveLength(2));
  });

  it('a failed load shows the error and logs it', async () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockImplementation((() => Promise.reject(new Error('offline'))) as never);
    render(<FilterRemapSection projectId="proj-1" />);
    const el = await screen.findByText(/offline/);
    expect(el.className).toContain('text-error');
    expect(err).toHaveBeenCalled();
    err.mockRestore();
  });

  it('an older load resolving after a newer one is ignored', async () => {
    const running: CollabRemapProgress = { projectId: 'proj-1', canonical: 'R', done: 1, failed: 0, total: 5 };
    const pending: Array<(s: FilterBucketSheet) => void> = [];
    const { rerender } = await renderSection();
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      if (command === 'get_collab_filter_buckets') return new Promise<FilterBucketSheet>((res) => pending.push(res));
      return Promise.resolve(null);
    }) as never);
    rerender(<FilterRemapSection projectId="proj-1" syncToken={1} />);
    rerender(<FilterRemapSection projectId="proj-1" syncToken={2} />);
    await waitFor(() => expect(pending).toHaveLength(2));
    // The newer request (no run) resolves first, the older one (run still set) resolves last.
    await act(async () => pending[1](sheet([bucket()], null)));
    await act(async () => pending[0](sheet([bucket()], running)));
    expect(screen.queryByText(/Remapping…/)).not.toBeInTheDocument();
  });
});
