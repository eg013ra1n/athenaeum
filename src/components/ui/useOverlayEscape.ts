import { useLayoutEffect, useRef } from 'react';
import { isTopOverlay, popOverlay, pushOverlay, type OverlayKind } from './overlayStack';

/**
 * Puts an overlay that draws its own markup (an always-mounted slide-over, a
 * menu, a legacy modal) on the one overlay stack while `open`, and runs
 * `onEscape` only while it is the top overlay — the `Popover` contract, so
 * one Escape closes exactly one layer. Registered in a layout effect like the
 * primitives; unregistered on close and on unmount.
 */
export function useOverlayEscape(open: boolean, kind: OverlayKind, onEscape: () => void): void {
  const escapeRef = useRef(onEscape);
  escapeRef.current = onEscape;
  useLayoutEffect(() => {
    if (!open) return undefined;
    const overlayId = pushOverlay(kind);
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || e.defaultPrevented || !isTopOverlay(overlayId)) return;
      e.preventDefault();
      escapeRef.current();
    };
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('keydown', onKey);
      popOverlay(overlayId);
    };
  }, [open, kind]);
}
