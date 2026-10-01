import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { api } from '../../../api';
import { MemoryRouter } from 'react-router-dom';
import { NotificationProvider } from '../../../contexts/NotificationContext';
import { ToastStack } from '../../Toast';
import { useWithhold } from './useWithhold';

vi.mock('../../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

afterEach(cleanup);
beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.listen).mockReset();
  vi.mocked(api.listen).mockResolvedValue((() => {}) as never);
});

type Hook = ReturnType<typeof useWithhold>;

/** A tiny harness: calls the hook, exposes it through `ref`, renders its dialog. */
function renderWithhold(onChanged: () => void) {
  const ref: { current: Hook | null } = { current: null };
  function Harness() {
    const h = useWithhold('p1', onChanged);
    ref.current = h;
    return <>{h.dialog}</>;
  }
  render(<MemoryRouter><NotificationProvider><Harness /><ToastStack /></NotificationProvider></MemoryRouter>);
  return { result: ref as { current: Hook } };
}

describe('useWithhold', () => {
  it("Don't publish with prepared frames confirms, names the files, then writes withheld=true", async () => {
    vi.mocked(api.invoke).mockResolvedValueOnce(2 as never);
    const onChanged = vi.fn();
    const { result } = renderWithhold(onChanged);
    let done!: Promise<boolean>;
    act(() => { done = result.current.dontPublish([{ frameId: 1, prepared: true }, { frameId: 2, prepared: false }]); });
    expect(await screen.findByText(/1 calibrated file will be deleted/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: "Don't publish" }));
    await expect(done).resolves.toBe(true);
    expect(api.invoke).toHaveBeenCalledWith('set_collab_frames_withheld', { projectId: 'p1', frameIds: [1, 2], withheld: true });
    expect(onChanged).toHaveBeenCalled();
  });

  it("Don't publish on Ready frames only acts at once with no dialog", async () => {
    vi.mocked(api.invoke).mockResolvedValueOnce(1 as never);
    const { result } = renderWithhold(vi.fn());
    await act(async () => { await result.current.dontPublish([{ frameId: 5, prepared: false }]); });
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(api.invoke).toHaveBeenCalledWith('set_collab_frames_withheld', { projectId: 'p1', frameIds: [5], withheld: true });
  });

  it('cancelling the confirm writes nothing and resolves false', async () => {
    const { result } = renderWithhold(vi.fn());
    let done!: Promise<boolean>;
    act(() => { done = result.current.dontPublish([{ frameId: 1, prepared: true }]); });
    fireEvent.click(await screen.findByRole('button', { name: 'Cancel' }));
    await expect(done).resolves.toBe(false);
    expect(api.invoke).not.toHaveBeenCalled();
  });

  it('a failed write logs, notifies and resolves false', async () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValueOnce(new Error('withholding a published frame is refused'));
    const onChanged = vi.fn();
    const { result } = renderWithhold(onChanged);
    await act(async () => { await expect(result.current.release([7])).resolves.toBe(false); });
    expect(err).toHaveBeenCalled();
    expect(onChanged).not.toHaveBeenCalled();
    expect(screen.getByRole('status')).toHaveTextContent('Could not release the frames');
    err.mockRestore();
  });

  it("a failed Don't publish notifies with its own title", async () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValueOnce(new Error('db locked'));
    const { result } = renderWithhold(vi.fn());
    await act(async () => { await expect(result.current.dontPublish([{ frameId: 5, prepared: false }])).resolves.toBe(false); });
    expect(screen.getByRole('status')).toHaveTextContent('Could not withhold the frames');
    err.mockRestore();
  });

  it('Release writes withheld=false', async () => {
    vi.mocked(api.invoke).mockResolvedValueOnce(1 as never);
    const onChanged = vi.fn();
    const { result } = renderWithhold(onChanged);
    await act(async () => { await expect(result.current.release([7])).resolves.toBe(true); });
    expect(api.invoke).toHaveBeenCalledWith('set_collab_frames_withheld', { projectId: 'p1', frameIds: [7], withheld: false });
    expect(onChanged).toHaveBeenCalled();
  });
});
