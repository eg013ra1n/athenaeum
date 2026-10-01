import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { useState } from 'react';
import { MemoryRouter } from 'react-router-dom';
import { NotificationProvider, useNotifications } from '../contexts/NotificationContext';
import { NotificationPanel } from './NotificationPanel';
import { SidePanel } from './ui';

vi.mock('../api', () => ({
  api: { invoke: vi.fn(() => Promise.resolve(null)), listen: vi.fn(() => Promise.resolve(() => {})) },
}));

/** A docked frame card plus the always-mounted notification panel, opened
 *  from a stand-in for the Layout header's bell. */
function Harness() {
  const [docked, setDocked] = useState(true);
  const { openPanel } = useNotifications();
  return (
    <>
      <button onClick={openPanel}>bell</button>
      {docked && <SidePanel title="Light_0003.fits" label="Frame details" onClose={() => setDocked(false)}>kv</SidePanel>}
      <NotificationPanel />
    </>
  );
}

function renderHarness() {
  return render(
    <MemoryRouter>
      <NotificationProvider>
        <Harness />
      </NotificationProvider>
    </MemoryRouter>,
  );
}

// A closed panel is aria-hidden, which also empties its accessible name.
const panelOpen = () => document.querySelector('aside[aria-label="Notifications"]')!.getAttribute('aria-hidden') === 'false';

describe('NotificationPanel on the overlay stack', () => {
  it('one Escape closes only the notification panel; the next closes the docked card', () => {
    renderHarness();
    fireEvent.click(screen.getByText('bell'));
    expect(panelOpen()).toBe(true);

    fireEvent.keyDown(document, { key: 'Escape' });
    expect(panelOpen()).toBe(false);
    expect(screen.getByRole('complementary', { name: 'Frame details' })).toBeInTheDocument();

    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('complementary', { name: 'Frame details' })).toBeNull();
  });

  it('while closed it holds no stack slot: Escape reaches the docked card', () => {
    renderHarness();
    fireEvent.click(screen.getByText('bell'));
    fireEvent.click(screen.getByRole('button', { name: 'Close notifications' }));
    expect(panelOpen()).toBe(false);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('complementary', { name: 'Frame details' })).toBeNull();
  });

  it('focuses its close button on open', () => {
    renderHarness();
    fireEvent.click(screen.getByText('bell'));
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Close notifications' }));
  });
});
