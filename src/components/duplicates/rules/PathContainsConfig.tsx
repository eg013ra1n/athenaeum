import React, { useState, useCallback } from 'react';
import { Plus, X } from 'lucide-react';

interface Props {
  patterns: string[];
  caseSensitive: boolean;
  onChange: (next: { patterns: string[]; caseSensitive: boolean }) => void;
  disabled?: boolean;
}

export const PathContainsConfig: React.FC<Props> = ({
  patterns,
  caseSensitive,
  onChange,
  disabled,
}) => {
  const [draft, setDraft] = useState('');

  const commitDraft = useCallback(() => {
    const trimmed = draft.trim();
    if (!trimmed) return;
    if (patterns.includes(trimmed)) {
      setDraft('');
      return;
    }
    onChange({ patterns: [...patterns, trimmed], caseSensitive });
    setDraft('');
  }, [draft, patterns, caseSensitive, onChange]);

  const removePattern = useCallback(
    (pattern: string) => {
      onChange({
        patterns: patterns.filter((p) => p !== pattern),
        caseSensitive,
      });
    },
    [patterns, caseSensitive, onChange],
  );

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        commitDraft();
      } else if (e.key === ',') {
        // Comma also commits — convenient for typing multiple at once.
        e.preventDefault();
        commitDraft();
      } else if (
        e.key === 'Backspace' &&
        draft === '' &&
        patterns.length > 0
      ) {
        // Backspace on empty input pops the last chip.
        e.preventDefault();
        removePattern(patterns[patterns.length - 1]);
      }
    },
    [commitDraft, draft, patterns, removePattern],
  );

  return (
    <div className="flex items-start gap-2 max-w-[640px]">
      <div className="flex-1 min-w-0 flex flex-wrap items-center gap-1.5 px-2 py-1.5 bg-surface-hover border border-border rounded-lg min-h-[28px]">
        {patterns.map((p) => (
          <span
            key={p}
            className="inline-flex items-center gap-1 h-5 px-1.5 rounded bg-accent/15 text-accent border border-accent/40 text-[11px] font-mono"
          >
            {p}
            <button
              type="button"
              onClick={() => removePattern(p)}
              disabled={disabled}
              className="hover:text-content transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              aria-label={`Remove pattern ${p}`}
            >
              <X size={10} />
            </button>
          </span>
        ))}
        <input
          type="text"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={handleKeyDown}
          disabled={disabled}
          placeholder={
            patterns.length === 0
              ? 'e.g. Backup, _copy, (1) — Enter to add'
              : 'Add… (Enter)'
          }
          className="flex-1 min-w-[120px] bg-transparent text-xs focus:outline-none placeholder:text-content-muted disabled:opacity-50 disabled:cursor-not-allowed"
        />
        {draft.trim().length > 0 && (
          <button
            type="button"
            onClick={commitDraft}
            disabled={disabled}
            title="Add pattern"
            aria-label="Add pattern"
            className="inline-flex items-center justify-center h-5 w-5 rounded text-accent hover:bg-accent/15 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Plus size={11} />
          </button>
        )}
      </div>
      <label
        title={
          caseSensitive
            ? 'Case-sensitive matching is on — patterns must match exact case'
            : 'Case-insensitive matching — click to require exact case'
        }
        className={`inline-flex items-center gap-1.5 h-7 flex-shrink-0 select-none cursor-pointer transition-colors ${
          caseSensitive ? 'text-accent' : 'text-content-muted hover:text-content'
        } ${disabled ? 'opacity-50 cursor-not-allowed' : ''}`}
      >
        <input
          type="checkbox"
          checked={caseSensitive}
          onChange={(e) =>
            onChange({ patterns, caseSensitive: e.target.checked })
          }
          disabled={disabled}
          className="rounded border-border focus:ring-accent"
        />
        <span className="font-semibold text-xs">Aa</span>
      </label>
    </div>
  );
};
