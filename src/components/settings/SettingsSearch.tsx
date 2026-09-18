// Settings redesign (spec 2026-09-18 §7): the search input above the tab
// bar. This task (C1) renders the input only and holds no search state of
// its own — `Settings.tsx` owns `query`, and Task E1 wires
// `useSettingsSearch(query)` + `SearchResults` in to actually filter the
// page. `/` focuses it when no other input is focused; `Escape` clears it.
import { useEffect, useRef } from 'react';
import { Search, X } from 'lucide-react';

export interface SettingsSearchProps {
  value: string;
  onChange: (value: string) => void;
}

function isTypingTarget(el: Element | null): boolean {
  if (!el) return false;
  const tag = el.tagName;
  return tag === 'INPUT' || tag === 'TEXTAREA' || (el as HTMLElement).isContentEditable;
}

export function SettingsSearch({ value, onChange }: SettingsSearchProps) {
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === '/' && !isTypingTarget(document.activeElement)) {
        e.preventDefault();
        inputRef.current?.focus();
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, []);

  return (
    <div className="relative max-w-md">
      <Search size={16} className="absolute left-3 top-1/2 -translate-y-1/2 text-content-muted pointer-events-none" />
      <input
        ref={inputRef}
        type="text"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Escape' && value) {
            e.preventDefault();
            onChange('');
          }
        }}
        placeholder="Search settings…"
        aria-label="Search settings"
        className="w-full bg-surface-hover border border-border rounded-lg pl-9 pr-9 py-2 text-sm text-content focus:outline-none focus:border-accent"
      />
      {value && (
        <button
          type="button"
          onClick={() => onChange('')}
          aria-label="Clear search"
          className="absolute right-2.5 top-1/2 -translate-y-1/2 text-content-muted hover:text-content"
        >
          <X size={14} />
        </button>
      )}
    </div>
  );
}
