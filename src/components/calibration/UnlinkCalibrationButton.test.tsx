import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { UnlinkCalibrationButton } from './UnlinkCalibrationButton';
import { api } from '../../api';

vi.mock('../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('UnlinkCalibrationButton', () => {
  it('with nothing selected, unlinks every light of the frame set, all three types', async () => {
    vi.mocked(api.invoke).mockResolvedValue(6);
    const onUnlinked = vi.fn();
    render(<UnlinkCalibrationButton selectedFrameIds={[]} selectedGroupCount={0} allFrameIds={[1, 2, 3]} onUnlinked={onUnlinked} />);
    fireEvent.click(screen.getByRole('button', { name: /Unlink all/ }));
    expect(screen.getByText(/every light of this frame set \(3 frames\)/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Unlink' }));
    await vi.waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('unlink_calibration_for_frames', {
        frameIds: [1, 2, 3],
        calibrationTypes: ['Flat', 'Dark', 'Bias'],
      })
    );
    await vi.waitFor(() => expect(onUnlinked).toHaveBeenCalled());
    expect(notifyMock).toHaveBeenCalledWith(expect.objectContaining({ title: '6 calibration links removed' }));
  });

  it('with groups selected, unlinks only their frames and only the ticked types', async () => {
    vi.mocked(api.invoke).mockResolvedValue(2);
    render(<UnlinkCalibrationButton selectedFrameIds={[7, 8]} selectedGroupCount={2} allFrameIds={[1, 2, 7, 8]} onUnlinked={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: /Unlink selected \(2\)/ }));
    fireEvent.click(screen.getByRole('checkbox', { name: 'Bias' }));
    fireEvent.click(screen.getByRole('button', { name: 'Unlink' }));
    await vi.waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('unlink_calibration_for_frames', {
        frameIds: [7, 8],
        calibrationTypes: ['Flat', 'Dark'],
      })
    );
  });

  it('cannot run with no type ticked, and stays off when disabled', () => {
    const { rerender } = render(
      <UnlinkCalibrationButton selectedFrameIds={[]} selectedGroupCount={0} allFrameIds={[1]} onUnlinked={vi.fn()} />
    );
    fireEvent.click(screen.getByRole('button', { name: /Unlink all/ }));
    for (const t of ['Flat', 'Dark', 'Bias']) fireEvent.click(screen.getByRole('checkbox', { name: t }));
    expect(screen.getByRole('button', { name: 'Unlink' })).toBeDisabled();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));

    rerender(<UnlinkCalibrationButton selectedFrameIds={[]} selectedGroupCount={0} allFrameIds={[1]} onUnlinked={vi.fn()} disabled />);
    expect(screen.getByRole('button', { name: /Unlink all/ })).toBeDisabled();
  });

  it('logs and notifies on failure', async () => {
    const consoleErrorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValue(new Error('boom'));
    render(<UnlinkCalibrationButton selectedFrameIds={[]} selectedGroupCount={0} allFrameIds={[1]} onUnlinked={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: /Unlink all/ }));
    fireEvent.click(screen.getByRole('button', { name: 'Unlink' }));
    await vi.waitFor(() =>
      expect(notifyMock).toHaveBeenCalledWith(expect.objectContaining({ title: 'Could not remove the calibration links' }))
    );
    expect(consoleErrorSpy).toHaveBeenCalled();
    consoleErrorSpy.mockRestore();
  });
});
