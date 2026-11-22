// Settings → General → FITS keywords: one field's ordered keyword chips. A default keyword is a plain chip, a
// keyword the user added carries a ×. Chips reorder by native drag and drop and by keyboard (focus a chip, the arrow
// keys move it and focus follows it; Delete or Backspace removes a user keyword). No dependency.

import { useEffect, useRef, useState, type DragEvent, type KeyboardEvent } from 'react';
import { GripVertical, X } from 'lucide-react';
import { Chip } from '../../ui';
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

export function KeywordChain({ field, order, defaults, onChange }: KeywordChainProps) {
  const chips = useRef(new Map<string, HTMLElement>());
  const focusAfter = useRef<string | null>(null);
  const dragFrom = useRef<number | null>(null);
  const [overIndex, setOverIndex] = useState<number | null>(null);

  // Focus follows a moved chip: React moves its node, which can drop focus, so put it back after the commit.
  useEffect(() => {
    const kw = focusAfter.current;
    if (kw === null) return;
    focusAfter.current = null;
    chips.current.get(kw)?.focus();
  }, [order]);

  const move = (from: number, to: number) => {
    const next = moveKeyword(order, from, to);
    if (next === order) return;
    focusAfter.current = order[from] ?? null;
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

  const onDragStart = (e: DragEvent<HTMLElement>, index: number) => {
    dragFrom.current = index;
    if (e.dataTransfer) {
      e.dataTransfer.effectAllowed = 'move';
      e.dataTransfer.setData('text/plain', order[index] ?? '');
    }
  };

  const onDragOver = (e: DragEvent<HTMLElement>, index: number) => {
    if (dragFrom.current === null) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move';
    setOverIndex(index);
  };

  const endDrag = () => {
    dragFrom.current = null;
    setOverIndex(null);
  };

  const onDrop = (e: DragEvent<HTMLElement>, index: number) => {
    const from = dragFrom.current;
    if (from === null) return;
    e.preventDefault();
    e.stopPropagation();
    endDrag();
    move(from, index);
  };

  return (
    <ul
      role="list"
      aria-label={`${field} keywords`}
      data-field={field}
      className="flex flex-wrap items-center gap-1.5 min-w-0 list-none m-0 p-0"
      // A drop past the last chip puts the dragged keyword last.
      onDragOver={(e) => {
        if (dragFrom.current !== null) e.preventDefault();
      }}
      onDrop={(e) => onDrop(e, order.length - 1)}
    >
      {order.map((kw, index) => {
        const isDefault = defaults.includes(kw);
        return (
          <li
            key={kw}
            ref={(el) => {
              if (el) chips.current.set(kw, el);
              else chips.current.delete(kw);
            }}
            tabIndex={0}
            draggable
            data-keyword={kw}
            title="Drag, or press the arrow keys, to change the order"
            onKeyDown={(e) => onKeyDown(e, index)}
            onDragStart={(e) => onDragStart(e, index)}
            onDragOver={(e) => onDragOver(e, index)}
            onDragEnd={endDrag}
            onDrop={(e) => onDrop(e, index)}
            className={`cursor-grab rounded-[3px] focus:outline-none focus-visible:ring-1 focus-visible:ring-accent ${
              overIndex === index ? 'ring-1 ring-accent' : ''
            }`}
          >
            <Chip tone={isDefault ? 'mute' : 'info'}>
              <GripVertical size={11} aria-hidden="true" className="opacity-60" />
              <span>{kw}</span>
              {!isDefault && (
                <button
                  type="button"
                  tabIndex={-1}
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
  );
}
