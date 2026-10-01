import { describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { useState } from 'react';
import { DialogShell, PanelLayout, Popover, SidePanel, useOverlayEscape } from '.';

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
  it('busy with every control disabled, Tab and Shift+Tab stay on the dialog', () => {
    const outside = document.createElement('button');
    document.body.appendChild(outside);
    try {
      render(<DialogShell title="T" onClose={() => {}} busy footer={<button disabled>Working…</button>}><p>b</p></DialogShell>);
      const dlg = screen.getByRole('dialog', { name: 'T' });
      expect(dlg).toHaveAttribute('tabindex', '-1');
      outside.focus();
      const tab = new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true });
      document.dispatchEvent(tab);
      expect(tab.defaultPrevented).toBe(true);
      expect(document.activeElement).toBe(dlg);
      fireEvent.keyDown(document, { key: 'Tab', shiftKey: true });
      expect(document.activeElement).toBe(dlg);
    } finally {
      outside.remove();
    }
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
  it('an Escape typed into a field outside the side panel belongs to the field', () => {
    const onClose = vi.fn();
    render(
      <div>
        <input aria-label="Search" />
        <select aria-label="Group by"><option>none</option></select>
        <div contentEditable aria-label="note" />
        <SidePanel title="t" label="P" onClose={onClose}><input aria-label="inside" /></SidePanel>
      </div>,
    );
    fireEvent.keyDown(screen.getByLabelText('Search'), { key: 'Escape' });
    fireEvent.keyDown(screen.getByLabelText('Group by'), { key: 'Escape' });
    fireEvent.keyDown(screen.getByLabelText('note'), { key: 'Escape' });
    expect(onClose).not.toHaveBeenCalled();
    // A field inside the panel is the panel's own: Escape closes it.
    fireEvent.keyDown(screen.getByLabelText('inside'), { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(1);
  });
  it('an Escape on a row checkbox or the page body still closes the side panel', () => {
    const onClose = vi.fn();
    render(<div><input type="checkbox" aria-label="Select row" /><SidePanel title="t" label="P" onClose={onClose}>x</SidePanel></div>);
    fireEvent.keyDown(screen.getByLabelText('Select row'), { key: 'Escape' });
    fireEvent.keyDown(document.body, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(2);
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

describe('useOverlayEscape — overlays that draw their own markup join the stack', () => {
  /** Stand-in for the always-mounted slide-overs (notifications, transfers)
   *  and the update dialog: mounted for good, registered only while open. */
  function AlwaysMounted({ open, kind = 'dialog', onEscape }: { open: boolean; kind?: 'dialog' | 'popover'; onEscape: () => void }) {
    useOverlayEscape(open, kind, onEscape);
    return <aside role="dialog" aria-hidden={!open} />;
  }

  it('opened over a docked side panel, one Escape closes it alone and the next closes the panel', () => {
    function Harness() {
      const [docked, setDocked] = useState(true);
      const [open, setOpen] = useState(false);
      return (
        <>
          <button onClick={() => setOpen(true)}>bell</button>
          {docked && <SidePanel title="t" label="P" onClose={() => setDocked(false)}>x</SidePanel>}
          <AlwaysMounted open={open} onEscape={() => setOpen(false)} />
          <span data-testid="state">{open ? 'open' : 'closed'}</span>
        </>
      );
    }
    render(<Harness />);
    fireEvent.click(screen.getByText('bell'));
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.getByTestId('state')).toHaveTextContent('closed');
    expect(screen.getByRole('complementary', { name: 'P' })).toBeInTheDocument();
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('complementary', { name: 'P' })).toBeNull();
  });

  it('marks the Escape handled, so a window listener below sees defaultPrevented', () => {
    const onEscape = vi.fn();
    render(<AlwaysMounted open onEscape={onEscape} />);
    let prevented: boolean | null = null;
    const spy = (e: KeyboardEvent) => { prevented = e.defaultPrevented; };
    window.addEventListener('keydown', spy);
    try {
      fireEvent.keyDown(document, { key: 'Escape' });
    } finally {
      window.removeEventListener('keydown', spy);
    }
    expect(onEscape).toHaveBeenCalledTimes(1);
    expect(prevented).toBe(true);
  });

  it('closed or unmounted, it holds no slot: the side panel gets Escape', () => {
    const panel = vi.fn(); const esc = vi.fn();
    const ui = (open: boolean, mounted: boolean) => (
      <><SidePanel title="t" label="P" onClose={panel}>x</SidePanel>{mounted && <AlwaysMounted open={open} onEscape={esc} />}</>
    );
    const { rerender } = render(ui(false, true));
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(panel).toHaveBeenCalledTimes(1);
    rerender(ui(true, true));
    rerender(ui(true, false));
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(panel).toHaveBeenCalledTimes(2);
    expect(esc).not.toHaveBeenCalled();
  });

  it('a registered dialog on top takes the Tab trap from a DialogShell below', () => {
    const ui = (open: boolean) => (
      <>
        <DialogShell title="D" onClose={() => {}} footer={<button>Last</button>}><button>First</button></DialogShell>
        <AlwaysMounted open={open} onEscape={() => {}} />
      </>
    );
    const { rerender } = render(ui(false));
    rerender(ui(true));
    const last = screen.getByText('Last');
    last.focus();
    fireEvent.keyDown(document, { key: 'Tab' });
    expect(document.activeElement).toBe(last);
  });
});
