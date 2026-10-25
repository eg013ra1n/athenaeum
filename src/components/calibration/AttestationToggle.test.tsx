import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { AttestationToggle } from './AttestationToggle';
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

describe('AttestationToggle', () => {
  it('renders unchecked for calibratedExternally=false', () => {
    render(
      <AttestationToggle framesSetId={5} calibratedExternally={false} linkedCalibrationSets={2} onChanged={vi.fn()} />
    );
    expect(screen.getByRole('switch')).not.toBeChecked();
    expect(screen.getByText('Calibrated externally')).toBeInTheDocument();
  });

  it('opens the confirm dialog when turning on with linked calibration sets', () => {
    render(
      <AttestationToggle framesSetId={5} calibratedExternally={false} linkedCalibrationSets={2} onChanged={vi.fn()} />
    );
    fireEvent.click(screen.getByRole('switch'));
    expect(screen.getByText('Attest external calibration?')).toBeInTheDocument();
    expect(screen.getByText(/This set has 2 linked calibration sets/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Attest' })).toBeInTheDocument();
  });

  it('Cancel closes the confirm and never invokes set_frame_set_attestation or onChanged', async () => {
    const onChanged = vi.fn();
    render(
      <AttestationToggle framesSetId={5} calibratedExternally={false} linkedCalibrationSets={2} onChanged={onChanged} />
    );
    fireEvent.click(screen.getByRole('switch'));
    expect(screen.getByText('Attest external calibration?')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByText('Attest external calibration?')).not.toBeInTheDocument();
    expect(api.invoke).not.toHaveBeenCalled();
    expect(onChanged).not.toHaveBeenCalled();
    // The switch itself is unaffected — still unchecked, still enabled.
    expect(screen.getByRole('switch')).not.toBeChecked();
    expect(screen.getByRole('switch')).not.toBeDisabled();
  });

  it('invokes set_frame_set_attestation with attested: true on confirm', async () => {
    vi.mocked(api.invoke).mockResolvedValue(undefined);
    const onChanged = vi.fn();
    render(
      <AttestationToggle framesSetId={5} calibratedExternally={false} linkedCalibrationSets={2} onChanged={onChanged} />
    );
    fireEvent.click(screen.getByRole('switch'));
    fireEvent.click(screen.getByRole('button', { name: 'Attest' }));
    await vi.waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('set_frame_set_attestation', { framesSetId: 5, attested: true })
    );
    await vi.waitFor(() => expect(onChanged).toHaveBeenCalled());
  });

  it('turning off invokes with attested: false and shows no confirm', async () => {
    vi.mocked(api.invoke).mockResolvedValue(undefined);
    const onChanged = vi.fn();
    render(
      <AttestationToggle framesSetId={5} calibratedExternally={true} linkedCalibrationSets={2} onChanged={onChanged} />
    );
    fireEvent.click(screen.getByRole('switch'));
    expect(screen.queryByText('Attest external calibration?')).not.toBeInTheDocument();
    await vi.waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('set_frame_set_attestation', { framesSetId: 5, attested: false })
    );
    await vi.waitFor(() => expect(onChanged).toHaveBeenCalled());
  });

  it('does not open the confirm dialog when turning on with no linked calibration sets', async () => {
    vi.mocked(api.invoke).mockResolvedValue(undefined);
    const onChanged = vi.fn();
    render(
      <AttestationToggle framesSetId={5} calibratedExternally={false} linkedCalibrationSets={0} onChanged={onChanged} />
    );
    fireEvent.click(screen.getByRole('switch'));
    expect(screen.queryByText('Attest external calibration?')).not.toBeInTheDocument();
    await vi.waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('set_frame_set_attestation', { framesSetId: 5, attested: true })
    );
  });

  it('renders ON as a checked switch, and a disabled switch cannot be flipped', () => {
    render(
      <AttestationToggle framesSetId={5} calibratedExternally={true} linkedCalibrationSets={0} onChanged={vi.fn()} disabled />
    );
    expect(screen.getByRole('switch')).toBeChecked();
    fireEvent.click(screen.getByRole('switch'));
    expect(api.invoke).not.toHaveBeenCalled();
  });

  it('logs and notifies on failure', async () => {
    const consoleErrorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValue(new Error('boom'));
    render(
      <AttestationToggle framesSetId={5} calibratedExternally={true} linkedCalibrationSets={0} onChanged={vi.fn()} />
    );
    fireEvent.click(screen.getByRole('switch'));
    await vi.waitFor(() => expect(notifyMock).toHaveBeenCalledWith(expect.objectContaining({ title: 'Could not change the attestation' })));
    expect(consoleErrorSpy).toHaveBeenCalled();
    consoleErrorSpy.mockRestore();
  });
});
