import { describe, expect, it, vi, beforeEach } from 'vitest';
import type { ReactElement } from 'react';
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { NotificationProvider } from '../../../contexts/NotificationContext';
import { ToastStack } from '../../Toast';
import MetaLine from './MetaLine';
import { api } from '../../../api';
import type { ProjectCard } from '../../../types/models';

vi.mock('../../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

function card(patch: Partial<ProjectCard>): ProjectCard {
  return {
    projectId: 'p',
    slug: 'm31',
    title: 'M31 Deep Field 2026',
    dataRole: 'send_receive',
    coordinator: false,
    canModerate: false,
    requireApproval: false,
    pendingFrames: 0,
    projectStatus: 'open',
    targetName: 'M31',
    targetRaDeg: 10.68,
    targetDecDeg: 41.27,
    targetRadiusDeg: 1.5,
    membershipVersion: 1,
    linkedSets: 1,
    candidates: 0,
    publishable: 0,
    autoReplicate: true,
    publishMode: 'manual',
    syncedAt: null,
    fetchedAt: '2026-09-29T10:00:00Z',
    publishingDevice: null,
    publishingHere: false,
    ...patch,
  };
}

const onChanged = vi.fn();
const onSwitch = vi.fn();

beforeEach(() => {
  localStorage.clear();
  onChanged.mockReset();
  onSwitch.mockReset();
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation((() => Promise.resolve(null)) as never);
  vi.mocked(api.listen).mockImplementation((() => Promise.resolve(() => {})) as never);
});

/** `NotificationProvider` + `ToastStack`, as `CollabAttention.test.tsx` does. */
const renderWithNotifications = (ui: ReactElement) =>
  render(
    <MemoryRouter>
      <NotificationProvider>
        {ui}
        <ToastStack />
      </NotificationProvider>
    </MemoryRouter>,
  );

describe('MetaLine', () => {
  it('reads like the mockup', () => {
    renderWithNotifications(<MetaLine card={card({ publishingHere: true, autoReplicate: true })} canReceive onChanged={onChanged} onSwitchHere={() => {}} switchBusy={false} />);
    expect(screen.getByText('Publishing from this device')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Auto-replicate on' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Publish from here' })).toBeNull();
    expect(screen.queryByRole('button', { name: /Auto-publish/ })).toBeNull();
  });

  it('names the other publishing device and offers to switch', () => {
    renderWithNotifications(<MetaLine card={card({ publishingHere: false, publishingDevice: { deviceId: 'd', name: 'kostya-obs' } })} canReceive onChanged={() => {}} onSwitchHere={onSwitch} switchBusy={false} />);
    expect(screen.getByText('Publishing from kostya-obs')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Publish from here' }));
    expect(onSwitch).toHaveBeenCalled();
  });

  it('the switch link is disabled while a switch runs', () => {
    renderWithNotifications(<MetaLine card={card({ publishingDevice: { deviceId: 'd', name: 'kostya-obs' } })} canReceive onChanged={() => {}} onSwitchHere={onSwitch} switchBusy />);
    expect(screen.getByRole('button', { name: 'Publish from here' })).toBeDisabled();
  });

  it('an unbound project says nobody is publishing yet, with no switch', () => {
    renderWithNotifications(<MetaLine card={card({ publishingDevice: null, publishingHere: false })} canReceive onChanged={() => {}} onSwitchHere={onSwitch} switchBusy={false} />);
    expect(screen.getByText('Nobody is publishing to this project yet')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Publish from here' })).toBeNull();
  });

  it('hides auto-replicate for a send-only member', () => {
    renderWithNotifications(<MetaLine card={card({})} canReceive={false} onChanged={() => {}} onSwitchHere={() => {}} switchBusy={false} />);
    expect(screen.queryByRole('button', { name: /Auto-replicate/ })).toBeNull();
  });

  // Moved from AutoReplicateBar.test.tsx ("writes the local preference and
  // re-reads it"): the write goes out, then the parent re-reads the card.
  it('writes the auto-replicate preference and asks the parent to re-read it', async () => {
    renderWithNotifications(<MetaLine card={card({ autoReplicate: true })} canReceive onChanged={onChanged} onSwitchHere={() => {}} switchBusy={false} />);
    fireEvent.click(screen.getByRole('button', { name: 'Auto-replicate on' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('set_project_auto_replicate', { projectId: 'p', enabled: false }),
    );
    expect(onChanged).toHaveBeenCalledTimes(1);
  });

  it('an off toggle reads off and turns on', async () => {
    renderWithNotifications(<MetaLine card={card({ autoReplicate: false })} canReceive onChanged={onChanged} onSwitchHere={() => {}} switchBusy={false} />);
    fireEvent.click(screen.getByRole('button', { name: 'Auto-replicate off' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('set_project_auto_replicate', { projectId: 'p', enabled: true }),
    );
  });

  it('the toggle is disabled while a write runs', async () => {
    let finish: (() => void) | undefined;
    vi.mocked(api.invoke).mockImplementation((() => new Promise<void>((r) => { finish = r; })) as never);
    renderWithNotifications(<MetaLine card={card({})} canReceive onChanged={onChanged} onSwitchHere={() => {}} switchBusy={false} />);
    fireEvent.click(screen.getByRole('button', { name: 'Auto-replicate on' }));
    expect(screen.getByRole('button', { name: 'Auto-replicate on' })).toBeDisabled();
    await act(async () => finish?.());
    expect(screen.getByRole('button', { name: 'Auto-replicate on' })).toBeEnabled();
  });

  it('explains the toggle in its tooltip', () => {
    renderWithNotifications(<MetaLine card={card({})} canReceive onChanged={() => {}} onSwitchHere={() => {}} switchBusy={false} />);
    expect(screen.getByRole('button', { name: 'Auto-replicate on' }).getAttribute('title')).toMatch(/download automatically/);
  });

  // Moved from AutoReplicateBar.test.tsx: the one Sync is the header's pill.
  it('has no Sync of its own — the one Sync is the header pill', () => {
    renderWithNotifications(<MetaLine card={card({})} canReceive onChanged={() => {}} onSwitchHere={() => {}} switchBusy={false} />);
    expect(screen.queryByRole('button', { name: /Sync/ })).toBeNull();
  });

  it('a failed toggle notifies and never flips the label', async () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValueOnce(new Error('db locked'));
    renderWithNotifications(<MetaLine card={card({ autoReplicate: true })} canReceive onChanged={onChanged} onSwitchHere={() => {}} switchBusy={false} />);
    fireEvent.click(screen.getByRole('button', { name: 'Auto-replicate on' }));
    expect(await screen.findByText('Could not change auto-replicate')).toBeInTheDocument();
    expect(onChanged).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: 'Auto-replicate on' })).toBeEnabled();
    expect(error).toHaveBeenCalled();
    error.mockRestore();
  });
});
