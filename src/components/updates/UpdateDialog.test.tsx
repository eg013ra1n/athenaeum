import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { SidePanel } from '../ui';
import { UpdateDialog } from './UpdateDialog';

const { close, state } = vi.hoisted(() => ({
  close: vi.fn(),
  state: { dialog: 'whatsNew' as 'whatsNew' | 'available', check: null as null | { latestVersion: string } },
}));

vi.mock('../../api', () => ({
  api: { invoke: vi.fn(() => Promise.resolve([])), listen: vi.fn(() => Promise.resolve(() => {})) },
}));
vi.mock('../../api/desktop', () => ({ openUrl: vi.fn() }));
vi.mock('../../contexts/TransfersContext', () => ({ useTransfers: () => ({ active: [] }) }));
vi.mock('../../contexts/UpdatesContext', () => ({
  useUpdates: () => ({
    check: state.check,
    whatsNew: { version: '0.7.0', notes: 'Notes.', blogUrl: '' },
    dialog: state.dialog,
    phase: { kind: 'idle' },
    close,
    install: vi.fn(),
    restart: vi.fn(),
  }),
}));

describe('UpdateDialog on the overlay stack', () => {
  it('opened over a docked side panel, Escape closes the dialog alone', () => {
    const panel = vi.fn();
    render(
      <>
        <SidePanel title="t" label="Frame details" onClose={panel}>kv</SidePanel>
        <UpdateDialog />
      </>,
    );
    expect(screen.getByRole('dialog', { name: "What's new in v0.7.0" })).toBeInTheDocument();
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(close).toHaveBeenCalledTimes(1);
    expect(panel).not.toHaveBeenCalled();
  });

  it('open with nothing to show (no check yet) holds no stack slot: Escape reaches the panel', () => {
    state.dialog = 'available';
    state.check = null;
    close.mockClear();
    const panel = vi.fn();
    render(
      <>
        <SidePanel title="t" label="Frame details" onClose={panel}>kv</SidePanel>
        <UpdateDialog />
      </>,
    );
    expect(screen.queryByRole('dialog')).toBeNull();
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(close).not.toHaveBeenCalled();
    expect(panel).toHaveBeenCalledTimes(1);
    state.dialog = 'whatsNew';
  });
});
