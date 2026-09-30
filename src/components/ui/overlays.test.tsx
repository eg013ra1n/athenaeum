import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { useState } from 'react';
import { DialogShell, PanelLayout, Popover, SidePanel } from '.';

describe('DialogShell', () => {
  it('Esc and scrim close; clicks inside do not', () => {
    const onClose = vi.fn();
    render(<DialogShell title="Export" onClose={onClose}><p>body</p></DialogShell>);
    fireEvent.click(screen.getByText('body'));
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.keyDown(document, { key: 'Escape' });
    fireEvent.click(screen.getByTestId('dialog-scrim'));
    expect(onClose).toHaveBeenCalledTimes(2);
  });
  it('does not close while busy', () => {
    const onClose = vi.fn();
    render(<DialogShell title="Export" onClose={onClose} busy><p>body</p></DialogShell>);
    fireEvent.keyDown(document, { key: 'Escape' });
    fireEvent.click(screen.getByTestId('dialog-scrim'));
    expect(onClose).not.toHaveBeenCalled();
  });
  it('is a labelled modal dialog and focuses inside', () => {
    render(<DialogShell title="Publish to M31" onClose={() => {}} footer={<button>OK</button>}><input aria-label="x" /></DialogShell>);
    const d = screen.getByRole('dialog', { name: 'Publish to M31' });
    expect(d).toHaveAttribute('aria-modal', 'true');
    expect(d.contains(document.activeElement)).toBe(true);
  });
  it('traps Tab at the last focusable element', () => {
    render(<DialogShell title="T" onClose={() => {}} footer={<button>Last</button>}><button>First</button></DialogShell>);
    const last = screen.getByText('Last');
    last.focus();
    fireEvent.keyDown(document, { key: 'Tab' });
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Close' }));
  });
});

describe('Popover', () => {
  it('closes on Esc and on an outside mousedown', () => {
    const onClose = vi.fn();
    render(<div><span>outside</span><div className="relative"><Popover open onClose={onClose}><label>col</label></Popover></div></div>);
    fireEvent.keyDown(document, { key: 'Escape' });
    fireEvent.mouseDown(screen.getByText('outside'));
    expect(onClose).toHaveBeenCalledTimes(2);
  });
});

describe('PanelLayout + SidePanel', () => {
  function Harness() {
    const [open, setOpen] = useState(true);
    return (
      <PanelLayout panel={open ? <SidePanel title="Light_0003.fits" label="Frame details" onClose={() => setOpen(false)}>kv</SidePanel> : null}>
        <table><tbody><tr><td>row</td></tr></tbody></table>
      </PanelLayout>
    );
  }
  it('splits into a 400px panel column while open and closes on Esc (review focus 3)', () => {
    const { container } = render(<Harness />);
    expect((container.firstChild as HTMLElement).className).toContain('grid-cols-[minmax(0,1fr)_400px]');
    expect(screen.getByRole('complementary', { name: 'Frame details' })).toBeInTheDocument();
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('complementary')).toBeNull();
    expect((container.firstChild as HTMLElement).className).not.toContain('grid-cols');
  });
});
