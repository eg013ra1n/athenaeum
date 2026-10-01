import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { api } from '../../../api';
import { NotificationProvider } from '../../../contexts/NotificationContext';
import ProjectSettingsCard from './ProjectSettingsCard';
import { projectCard } from './testFixtures';
import { formatTimestamp } from '../../../utils/dateFormatting';
import type { ProjectCard } from '../../../types/models';
import type { PublishRunState } from './useCollabPublishRun';

vi.mock('../../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn(() => Promise.resolve(() => {})) } }));

const card = (o: Partial<ProjectCard> = {}): ProjectCard =>
  projectCard({
    projectId: 'p1', publishMode: 'manual', publishingHere: true, publishingDevice: { deviceId: 'd1', name: 'Mac Studio' },
    autoReplicate: true, ...o,
  });
const idle: PublishRunState = { running: null, last: null, reached: -1, cancel: vi.fn(), cancelBusy: false };
const renderCard = (o: Partial<Parameters<typeof ProjectSettingsCard>[0]> = {}) =>
  render(
    <NotificationProvider>
      <ProjectSettingsCard card={card()} canReceive run={idle} liveState="live" onChanged={vi.fn()}
        onSwitchHere={vi.fn()} switchBusy={false} onOpenMyFrames={vi.fn()} {...o} />
    </NotificationProvider>,
  );

describe('ProjectSettingsCard', () => {
  beforeEach(() => vi.mocked(api.invoke).mockReset());

  it('shows the device, the mode with its help line and auto-replicate', () => {
    renderCard();
    expect(screen.getByText(/This device · Mac Studio/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Manual', pressed: true })).toBeInTheDocument();
    expect(screen.getByText(/Nothing runs on its own/)).toBeInTheDocument();
    expect(screen.getByRole('switch', { name: 'On' })).toBeChecked();
  });

  it('a mode click commits set_project_publish_mode then re-reads the card', async () => {
    const onChanged = vi.fn();
    vi.mocked(api.invoke).mockResolvedValueOnce(undefined);
    renderCard({ onChanged });
    fireEvent.click(screen.getByRole('button', { name: 'Auto-calibrate' }));
    await waitFor(() => expect(onChanged).toHaveBeenCalled());
    expect(api.invoke).toHaveBeenCalledWith('set_project_publish_mode', { projectId: 'p1', mode: 'autoCalibrate' });
  });

  it('a failed mode write logs, notifies and keeps the stored mode', async () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValueOnce(new Error('nope'));
    renderCard();
    fireEvent.click(screen.getByRole('button', { name: 'Fully automatic' }));
    await waitFor(() => expect(err).toHaveBeenCalled());
    expect(screen.getByRole('button', { name: 'Manual', pressed: true })).toBeInTheDocument();
    err.mockRestore();
  });

  it('another device publishing shows Publish from this device', async () => {
    const onSwitchHere = vi.fn();
    renderCard({ card: card({ publishingHere: false, publishingDevice: { deviceId: 'd2', name: 'Observatory' } }), onSwitchHere });
    expect(screen.getByText(/Observatory/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Publish from this device' }));
    expect(onSwitchHere).toHaveBeenCalled();
  });

  it('a running run shows its stage and progress; idle shows the last run line', () => {
    const running = { ...idle, running: { projectId: 'p1', publishRunId: 'r', kind: 'auto', trigger: 'auto', mode: 'autoCalibrate',
      stage: 'calibrating', current: 12, total: 48, currentFile: 'c_x.fits', startedAt: '2026-10-02T10:00:00Z' } } as PublishRunState;
    const { unmount } = renderCard({ run: running });
    expect(screen.getByText(/Calibrating 12 \/ 48/)).toBeInTheDocument();
    unmount();
    renderCard({ run: { ...idle, last: { projectId: 'p1', publishRunId: 'r', kind: 'calibrate', trigger: 'manual', outcome: 'done',
      calibrated: 46, announced: 0, updated: 0, stale: 0, heldBack: 2, error: null, startedAt: '2026-10-02T10:00:00Z',
      finishedAt: '2026-10-02T10:03:22Z' } } });
    expect(screen.getByText(/Calibrated 46/)).toBeInTheDocument();
  });

  it('the outdated-hub last-run line has no stray period before the time', () => {
    const finishedAt = '2026-10-02T10:03:22Z';
    renderCard({ run: { ...idle, last: { projectId: 'p1', publishRunId: 'r', kind: 'publish', trigger: 'manual', outcome: 'refused',
      calibrated: 0, announced: 0, updated: 0, stale: 0, heldBack: 0, error: 'collab_api_outdated: the hub requires collab API 4',
      startedAt: '2026-10-02T10:03:20Z', finishedAt } } });
    const line = screen.getByText(/^Last run/);
    expect(line.textContent).toBe(
      `Last run · This hub needs a newer Athenaeum — update to publish · ${formatTimestamp(finishedAt, { seconds: true })} · manual`,
    );
    expect(line.textContent).not.toContain('. ·');
  });

  it('a queued run reads "Waiting for a compute slot" with no counts', () => {
    const queued = { ...idle, running: { projectId: 'p1', publishRunId: 'r', kind: 'calibrate', trigger: 'manual', mode: null,
      stage: 'queued', current: 0, total: 48, currentFile: null, startedAt: '2026-10-02T10:00:00Z' } } as PublishRunState;
    renderCard({ run: queued });
    expect(screen.getByText('Waiting for a compute slot')).toBeInTheDocument();
    expect(screen.queryByText(/0 \/ 48/)).toBeNull();
  });

  it.each(['off', 'signedOut'])('an auto mode with live state %s reads Paused', (liveState) => {
    renderCard({ card: card({ publishMode: 'automatic' }), liveState });
    expect(screen.getByText(/Paused — collaboration is off/)).toBeInTheDocument();
  });

  it('a nameless publishing device here reads just "This device"', () => {
    renderCard({ card: card({ publishingDevice: { deviceId: 'd1', name: null } }) });
    expect(screen.getByText(/This device/)).toBeInTheDocument();
    expect(screen.queryByText(/another device/)).toBeNull();
  });

  it('auto-replicate is hidden for a member who cannot receive', () => {
    renderCard({ canReceive: false });
    expect(screen.queryByRole('switch')).toBeNull();
  });
});
