import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { SidePanel } from '../ui';
import { EMPTY_EXCHANGE } from '../collab/exchange/state';
import { TransfersPanel } from './TransfersPanel';

const { closePanel } = vi.hoisted(() => ({ closePanel: vi.fn() }));

vi.mock('../../api', () => ({
  api: {
    invoke: vi.fn((cmd: string) => Promise.resolve(cmd === 'get_sync_device_names' ? {} : [])),
    listen: vi.fn(() => Promise.resolve(() => {})),
  },
}));
vi.mock('../../contexts/TransfersContext', () => ({
  useTransfers: () => ({ open: true, closePanel, status: null, active: [], refresh: vi.fn() }),
}));
vi.mock('../../contexts/CollabExchangeContext', () => ({
  useCollabExchange: () => ({ state: EMPTY_EXCHANGE, refreshProject: vi.fn() }),
}));

describe('TransfersPanel on the overlay stack', () => {
  it('open over a docked side panel, Escape closes the transfers panel alone', () => {
    const panel = vi.fn();
    render(
      <MemoryRouter>
        <SidePanel title="t" label="Frame details" onClose={panel}>kv</SidePanel>
        <TransfersPanel />
      </MemoryRouter>,
    );
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(closePanel).toHaveBeenCalledTimes(1);
    expect(panel).not.toHaveBeenCalled();
  });
});
