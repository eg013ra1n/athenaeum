// Settings → General → FITS keywords: one field's ordered keyword chips. A default keyword is a plain chip, a
// keyword the user added carries a ×. Chips reorder by pointer drag (pointer events, not HTML5 drag and drop: the
// desktop webview on Windows swallows HTML5 drags unless file drops are turned off app-wide) and by keyboard (focus a
// chip, the arrow keys move it and focus follows it; Delete or Backspace removes a user keyword). No dependency.
//
// A drag shows its order live and patches once, on release; Escape or a cancelled pointer drops it. The keyboard
// patches once per key.

import { useEffect, useId, useRef, useState, type KeyboardEvent, type PointerEvent } from 'react';
import { GripVertical, X } from 'lucide-react';
import { Chip } from '../../ui';
import { HEADER_FIELD_LABELS } from '../../../settings/headerFields';
import type { HeaderField } from '../../../types/models';

export interface KeywordChainProps {
  field: HeaderField;
  order: string[];
  /** The field's default chain: these keywords have no ×. */
  defaults: string[];
  onChange(order: string[]): void;
}

/** `order` with the keyword at `from` taken out and put back at index `to`. */
export function moveKeyword(order: string[], from: number, to: number): string[] {
  if (from === to || from < 0 || to < 0 || from >= order.length || to >= order.length) return order;
  const next = [...order];
  const [kw] = next.splice(from, 1);
  next.splice(to, 0, kw as string);
  return next;
}

interface DragState {
  keyword: string;
  pointerId: number;
}

export function KeywordChain({ field, order, defaults, onChange }: KeywordChainProps) {
  const hintId = useId();
  const chips = useRef(new Map<string, HTMLElement>());
  const focusAfter = useRef<string | null>(null);
  // The order shown while a pointer drag runs; `null` when none does. `liveRef` is the same value for the document
  // listeners, which must not close over a stale render.
  const [live, setLive] = useState<string[] | null>(null);
  const liveRef = useRef<string[] | null>(null);
  const dragRef = useRef<DragState | null>(null);
  const orderRef = useRef(order);
  orderRef.current = order;
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;
  const [announcement, setAnnouncement] = useState('');

  const shown = live ?? order;
  const dragging = dragRef.current?.keyword ?? null;

  // Focus follows a moved chip: React moves its node, which can drop focus, so put it back after the commit.
  useEffect(() => {
    const kw = focusAfter.current;
    if (kw === null) return;
    focusAfter.current = null;
    chips.current.get(kw)?.focus();
  }, [order]);

  const announce = (kw: string, next: string[]) => {
    setAnnouncement(`Moved ${kw} to position ${next.indexOf(kw) + 1} of ${next.length}.`);
  };

  const move = (from: number, to: number) => {
    const next = moveKeyword(order, from, to);
    if (next === order) return;
    const kw = order[from] as string;
    focusAfter.current = kw;
    announce(kw, next);
    onChange(next);
  };

  const remove = (index: number, byKeyboard: boolean) => {
    const kw = order[index];
    if (kw === undefined || defaults.includes(kw)) return;
    // From the keyboard, focus lands on the neighbour that takes its place, so the user keeps their position.
    if (byKeyboard) focusAfter.current = order[index + 1] ?? order[index - 1] ?? null;
    onChange(order.filter((_, i) => i !== index));
  };

  const onKeyDown = (e: KeyboardEvent<HTMLElement>, index: number) => {
    if (e.target !== e.currentTarget) return;
    switch (e.key) {
      case 'ArrowRight':
      case 'ArrowDown':
        e.preventDefault();
        move(index, index + 1);
        break;
      case 'ArrowLeft':
      case 'ArrowUp':
        e.preventDefault();
        move(index, index - 1);
        break;
      case 'Home':
        e.preventDefault();
        move(index, 0);
        break;
      case 'End':
        e.preventDefault();
        move(index, order.length - 1);
        break;
      case 'Delete':
      case 'Backspace':
        e.preventDefault();
        remove(index, true);
        break;
    }
  };

  // ---- pointer drag -------------------------------------------------------------------------------------------

  const startDrag = (e: PointerEvent<HTMLElement>, keyword: string) => {
    if (e.button !== 0) return;
    // The × is a button of its own.
    if ((e.target as HTMLElement).closest('button')) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    dragRef.current = { keyword, pointerId: e.pointerId };
    liveRef.current = order;
    setLive(order);
  };

  const endDrag = () => {
    dragRef.current = null;
    liveRef.current = null;
    setLive(null);
  };

  // While a drag runs, the document sees every move, release and Escape, so the drag survives the chip's node being
  // moved by a reorder (which can release a pointer capture).
  const isDragging = dragging !== null;
  useEffect(() => {
    if (!isDragging) return;

    const onMove = (e: globalThis.PointerEvent) => {
      const drag = dragRef.current;
      const current = liveRef.current;
      if (!drag || !current || e.pointerId !== drag.pointerId) return;
      const from = current.indexOf(drag.keyword);
      if (from < 0) return;
      let to = from;
      // The chip under the pointer; the dragged chip moves to its place once the pointer passes its midpoint.
      for (let j = 0; j < current.length; j++) {
        if (j === from) continue;
        const r = chips.current.get(current[j] as string)?.getBoundingClientRect();
        if (!r) continue;
        const inside = e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom;
        if (!inside) continue;
        const mid = r.left + r.width / 2;
        if ((j > from && e.clientX > mid) || (j < from && e.clientX < mid)) to = j;
        break;
      }
      // Past the last chip, on its row: the keyword goes last.
      if (to === from) {
        const lastKw = current[current.length - 1] as string;
        const r = lastKw === drag.keyword ? undefined : chips.current.get(lastKw)?.getBoundingClientRect();
        if (r && e.clientY >= r.top && e.clientY <= r.bottom && e.clientX > r.right) to = current.length - 1;
      }
      const next = moveKeyword(current, from, to);
      if (next !== current) {
        liveRef.current = next;
        setLive(next);
      }
    };

    const onUp = (e: globalThis.PointerEvent) => {
      const drag = dragRef.current;
      const result = liveRef.current;
      if (!drag || e.pointerId !== drag.pointerId) return;
      endDrag();
      if (result && result.some((kw, i) => kw !== orderRef.current[i])) {
        focusAfter.current = drag.keyword;
        announce(drag.keyword, result);
        onChangeRef.current(result);
      }
    };

    const onCancel = (e: globalThis.PointerEvent) => {
      if (dragRef.current && e.pointerId === dragRef.current.pointerId) endDrag();
    };

    const onEscape = (e: globalThis.KeyboardEvent) => {
      if (e.key === 'Escape') endDrag();
    };

    document.addEventListener('pointermove', onMove);
    document.addEventListener('pointerup', onUp);
    document.addEventListener('pointercancel', onCancel);
    document.addEventListener('keydown', onEscape);
    return () => {
      document.removeEventListener('pointermove', onMove);
      document.removeEventListener('pointerup', onUp);
      document.removeEventListener('pointercancel', onCancel);
      document.removeEventListener('keydown', onEscape);
    };
  }, [isDragging]);

  return (
    <>
      <ul
        role="list"
        aria-label={`${HEADER_FIELD_LABELS[field]} keywords`}
        data-field={field}
        className="flex flex-wrap items-center gap-1.5 min-w-0 list-none m-0 p-0"
      >
        {shown.map((kw, index) => {
          const isDefault = defaults.includes(kw);
          return (
            <li
              key={kw}
              ref={(el) => {
                if (el) chips.current.set(kw, el);
                else chips.current.delete(kw);
              }}
              tabIndex={0}
              data-keyword={kw}
              aria-describedby={hintId}
              title="Drag, or press the arrow keys, to change the order"
              onKeyDown={(e) => onKeyDown(e, index)}
              onPointerDown={(e) => startDrag(e, kw)}
              className={`touch-none select-none cursor-grab rounded-[3px] focus:outline-none focus-visible:ring-1 focus-visible:ring-accent ${
                dragging === kw ? 'ring-1 ring-accent opacity-80 cursor-grabbing' : ''
              }`}
            >
              <Chip tone={isDefault ? 'mute' : 'info'}>
                <GripVertical size={11} aria-hidden="true" className="opacity-60" />
                <span>{kw}</span>
                {!isDefault && (
                  <button
                    type="button"
                    aria-label={`Remove ${kw}`}
                    onClick={() => remove(index, false)}
                    className="-mr-0.5 rounded hover:text-content"
                  >
                    <X size={11} aria-hidden="true" />
                  </button>
                )}
              </Chip>
            </li>
          );
        })}
      </ul>
      <span id={hintId} className="sr-only">
        Drag a keyword or use the arrow keys to move it; Delete removes a keyword you added.
      </span>
      <span role="status" aria-live="polite" className="sr-only">
        {announcement}
      </span>
    </>
  );
}
