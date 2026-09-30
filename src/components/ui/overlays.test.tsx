import { describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { useState } from 'react';
import { DialogShell, PanelLayout, Popover, SidePanel } from '.';

describe('DialogShell', () => {
  it('Esc and scrim close; clicks inside do not', () => {
    const onClose = vi.fn();
    render(<DialogShell title="Export" onClose={onClose}><p>body</p></DialogShell>);
    fireEvent.click(screen.getByText('body'));
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.keyDown(document, { key: 'Escape' });
    fireEvent.mouseDown(screen.getByTestId('dialog-scrim'));
    fireEvent.click(screen.getByTestId('dialog-scrim'));
    expect(onClose).toHaveBeenCalledTimes(2);
  });
  it('does not close while busy', () => {
    const onClose = vi.fn();
    render(<DialogShell title="Export" onClose={onClose} busy><p>body</p></DialogShell>);
    fireEvent.keyDown(document, { key: 'Escape' });
    fireEvent.mouseDown(screen.getByTestId('dialog-scrim'));
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

describe('overlay ownership and fixes', () => {
  it('Esc closes the side panel even with a hidden closed role=dialog aside present', () => {
    const onClose = vi.fn();
    render(<div><aside role="dialog" aria-hidden="true" /><SidePanel title="t" label="P" onClose={onClose}>x</SidePanel></div>);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(1);
  });
  it('a dialog over a side panel takes Esc alone', () => {
    const panel = vi.fn(); const dlg = vi.fn();
    const ui = (open: boolean) => <SidePanel title="t" label="P" onClose={panel}>{open && <DialogShell title="D" onClose={dlg}>b</DialogShell>}</SidePanel>;
    const { rerender } = render(ui(false));
    rerender(ui(true));
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(dlg).toHaveBeenCalledTimes(1);
    expect(panel).not.toHaveBeenCalled();
  });
  it('a popover inside a side panel takes Esc alone', () => {
    const panel = vi.fn(); const pop = vi.fn();
    const ui = (open: boolean) => <SidePanel title="t" label="P" onClose={panel}><div className="relative"><Popover open={open} onClose={pop}>c</Popover></div></SidePanel>;
    const { rerender } = render(ui(false));
    rerender(ui(true));
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(pop).toHaveBeenCalledTimes(1);
    expect(panel).not.toHaveBeenCalled();
  });
  it('two dialogs: only the top one gets Esc and Tab', () => {
    const a = vi.fn(); const b = vi.fn();
    render(<><DialogShell title="A" onClose={a} footer={<button>A-last</button>}><button>A-first</button></DialogShell>
      <DialogShell title="B" onClose={b} footer={<button>B-last</button>}><button>B-first</button></DialogShell></>);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(a).not.toHaveBeenCalled();
    expect(b).toHaveBeenCalledTimes(1);
    screen.getByText('B-last').focus();
    fireEvent.keyDown(document, { key: 'Tab' });
    const dlgB = screen.getByRole('dialog', { name: 'B' });
    expect(dlgB.contains(document.activeElement)).toBe(true);
    expect(document.activeElement).toBe(within(dlgB).getByRole('button', { name: 'Close' }));
  });
  it('focuses data-autofocus', () => {
    render(<DialogShell title="T" onClose={() => {}}><input data-autofocus aria-label="a" /></DialogShell>);
    expect(document.activeElement).toBe(screen.getByLabelText('a'));
  });
  it('restores focus to the opener', () => {
    const btn = document.createElement('button');
    document.body.appendChild(btn); btn.focus();
    const { unmount } = render(<DialogShell title="T" onClose={() => {}}>b</DialogShell>);
    expect(document.activeElement).not.toBe(btn);
    unmount();
    expect(document.activeElement).toBe(btn);
    btn.remove();
  });
  it('portals the scrim to body', () => {
    render(<div><DialogShell title="T" onClose={() => {}}>b</DialogShell></div>);
    expect(screen.getByTestId('dialog-scrim').parentElement).toBe(document.body);
  });
  it('scrim closes only when the press started on it', () => {
    const onClose = vi.fn();
    render(<DialogShell title="T" onClose={onClose}><p>body</p></DialogShell>);
    const scrim = screen.getByTestId('dialog-scrim');
    fireEvent.mouseDown(screen.getByText('body'));
    fireEvent.click(scrim);
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.mouseDown(scrim);
    fireEvent.click(scrim);
    expect(onClose).toHaveBeenCalledTimes(1);
  });
  it('side panel refits on scroll', () => {
    vi.stubGlobal('requestAnimationFrame', (cb: FrameRequestCallback) => { cb(0); return 1; });
    let top = 100;
    const spy = vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(() => ({ top } as DOMRect));
    try {
      render(<SidePanel title="t" label="P" onClose={() => {}}>x</SidePanel>);
      const aside = screen.getByRole('complementary', { name: 'P' });
      const before = aside.style.height;
      top = 40;
      act(() => { fireEvent.scroll(window); });
      expect(aside.style.height).not.toBe(before);
    } finally {
      spy.mockRestore();
      vi.unstubAllGlobals();
    }
  });
  it('a popover inside a dialog keeps the Tab trap; Esc closes only the popover', () => {
    const dlg = vi.fn(); const pop = vi.fn();
    const ui = (open: boolean) => (
      <DialogShell title="D" onClose={dlg} footer={<button>Last</button>}>
        <div className="relative"><Popover open={open} onClose={pop}><button>in-pop</button></Popover></div>
      </DialogShell>
    );
    const { rerender } = render(ui(false));
    rerender(ui(true));
    screen.getByText('Last').focus();
    fireEvent.keyDown(document, { key: 'Tab' });
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Close' }));
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(pop).toHaveBeenCalledTimes(1);
    expect(dlg).not.toHaveBeenCalled();
  });
  it('a side panel mounted after a dialog does not steal Esc', () => {
    const panel = vi.fn(); const dlg = vi.fn();
    const ui = (withPanel: boolean) => (
      <>
        <DialogShell title="D" onClose={dlg}>b</DialogShell>
        {withPanel && <SidePanel title="t" label="P" onClose={panel}>x</SidePanel>}
      </>
    );
    const { rerender } = render(ui(false));
    rerender(ui(true));
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(dlg).toHaveBeenCalledTimes(1);
    expect(panel).not.toHaveBeenCalled();
  });
  it('mousedown in a dialog opened from an open popover does not close the popover', () => {
    const pop = vi.fn();
    const ui = (dialog: boolean) => (
      <div className="relative">
        <Popover open onClose={pop}>
          {dialog && <DialogShell title="D" onClose={() => {}}><p>inside</p></DialogShell>}
        </Popover>
      </div>
    );
    const { rerender } = render(ui(false));
    rerender(ui(true));
    fireEvent.mouseDown(screen.getByText('inside'));
    expect(pop).not.toHaveBeenCalled();
  });
  it('an Escape already handled elsewhere does not close the side panel', () => {
    const onClose = vi.fn();
    render(<SidePanel title="t" label="P" onClose={onClose}>x</SidePanel>);
    const pre = (e: Event) => e.preventDefault();
    document.addEventListener('keydown', pre, true);
    try {
      document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    } finally {
      document.removeEventListener('keydown', pre, true);
    }
    expect(onClose).not.toHaveBeenCalled();
  });
});
