import { useEffect } from 'react';

/** Tag names that own their own Backspace/arrow behaviour. */
const EDITABLE_TAGS = new Set(['INPUT', 'TEXTAREA', 'SELECT']);

function isEditableTarget(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el) return false;
  if (EDITABLE_TAGS.has(el.tagName)) return true;
  return el.isContentEditable === true;
}

/**
 * True while any modal dialog or slide-over is on screen.
 *
 * Every overlay in the app renders a `fixed inset-0` backdrop and is mounted
 * only while open, so probing the DOM covers all of them — dialogs, the Blink
 * viewer, the notification and transfer panels — without each one having to
 * register itself. In-page overlays use `absolute inset-0` and are deliberately
 * not matched: they don't take over the window and shouldn't block navigation.
 */
function isOverlayOpen(): boolean {
  return document.querySelector('.fixed.inset-0') !== null;
}

/**
 * App-wide back/forward keyboard shortcuts: Backspace, Alt+←/→ and, on macOS,
 * ⌘←/→ and ⌘[/].
 *
 * `back`/`forward` are expected to already refuse to leave the session (see
 * `NavHistoryContext`), so this hook only decides *whether the keystroke is
 * ours*, never where it goes.
 */
export function useGlobalNavKeys(back: () => void, forward: () => void) {
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      const alt = e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey;
      const meta = e.metaKey && !e.ctrlKey && !e.altKey && !e.shiftKey;
      const backChord =
        (alt && e.key === 'ArrowLeft') || (meta && (e.key === 'ArrowLeft' || e.key === '['));
      const forwardChord =
        (alt && e.key === 'ArrowRight') || (meta && (e.key === 'ArrowRight' || e.key === ']'));
      const backspace =
        e.key === 'Backspace' && !e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey;

      if (!backChord && !forwardChord && !backspace) return;

      // Alt+← and ⌘[ are the browser's OWN back/forward in the web build.
      // Cancel them before any guard below can return, so a chord we decline
      // to act on (focus in a text field, a dialog open) can't make the browser
      // navigate out from under it. Backspace gets no such treatment: browsers
      // stopped navigating on it years ago, and cancelling it unconditionally
      // would break typing in every text field.
      if (backChord || forwardChord) e.preventDefault();

      if (e.repeat) return; // a held key must not fly back through the stack
      if (isEditableTarget(e.target)) return;
      if (isOverlayOpen()) return;

      if (backspace) e.preventDefault();
      if (backChord || backspace) back();
      else forward();
    }

    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [back, forward]);
}
