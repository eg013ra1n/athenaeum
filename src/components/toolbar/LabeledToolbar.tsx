import type { ReactNode } from 'react';
import { ArrowLeftRight, Play } from 'lucide-react';

/**
 * The labelled toolbar pieces shared by the Analysis tab and the Stacking tab's Frames toolbar: every control
 * is a 28 px-high button with a 10 px label under it. Class strings are the Analysis toolbar's own, so the two
 * tabs read as one family.
 */

const LABEL_CLASS = 'text-[10px] text-content-muted leading-tight';
const CELL_CLASS = 'flex flex-col items-center gap-0.5 flex-shrink-0';
/** Added only while a control is disabled, so an enabled control keeps its exact class string. */
const DISABLED_CLASS = 'opacity-30 cursor-default';

/** One toolbar action: a 40×28 button with its label under it, the shape of every button in this toolbar. */
export function ToolButton({ label, title, onClick, disabled, className = 'text-content-secondary', children }: {
  label: string;
  title: string;
  onClick: () => void;
  disabled?: boolean;
  className?: string;
  children: ReactNode;
}) {
  return (
    <div className={CELL_CLASS}>
      <button
        type="button"
        onClick={onClick}
        disabled={disabled}
        aria-label={label}
        title={title}
        className={`min-w-10 h-7 px-2 inline-flex items-center justify-center gap-1 text-xs font-medium bg-surface-hover hover:bg-surface-elevated rounded-lg border border-border transition-colors disabled:opacity-30 disabled:cursor-default ${className}`}
      >
        {children}
      </button>
      <span className={LABEL_CLASS}>{label}</span>
    </div>
  );
}

/** A segmented button: one segment per option, the active one lit, the label under the whole group. */
export function ToolSplit<T extends string>({ label, options, value, onChange, disabled }: {
  label: string;
  options: { value: T; title: string; content: ReactNode }[];
  /** The active option, or null when none is. */
  value: T | null;
  onChange: (v: T) => void;
  disabled?: boolean;
}) {
  return (
    <div className={CELL_CLASS}>
      <div className="flex h-7">
        {options.map((option, i) => {
          const first = i === 0;
          const last = i === options.length - 1;
          const shape = first && last
            ? 'rounded-lg border'
            : first
              ? 'rounded-l-lg border border-r-0'
              : last
                ? 'rounded-r-lg border'
                : 'border border-r-0';
          const active = option.value === value;
          return (
            <button
              key={option.value}
              type="button"
              onClick={() => onChange(option.value)}
              disabled={disabled}
              aria-pressed={active}
              title={option.title}
              className={`min-w-10 px-2 h-7 inline-flex items-center justify-center text-xs font-medium ${shape} transition-colors ${
                active
                  ? 'bg-accent/20 text-accent border-accent/40'
                  : 'bg-surface-hover hover:bg-surface-elevated text-content-secondary border-border'
              } ${disabled ? DISABLED_CLASS : ''}`.trimEnd()}
            >
              {option.content}
            </button>
          );
        })}
      </div>
      <span className={LABEL_CLASS}>{label}</span>
    </div>
  );
}

/** One 40×28 on/off toggle with its label under it (the Analysis Units button). */
export function ToolToggle({ label, title, on, onClick, disabled, children }: {
  label: string;
  title: string;
  on: boolean;
  onClick: () => void;
  disabled?: boolean;
  children: ReactNode;
}) {
  return (
    <div className={CELL_CLASS}>
      <button
        type="button"
        onClick={onClick}
        disabled={disabled}
        aria-pressed={on}
        title={title}
        className={`w-10 h-7 text-xs font-medium rounded-lg border transition-colors ${
          on
            ? 'bg-accent/20 border-accent text-accent'
            : 'bg-surface-hover border-border text-content-secondary hover:text-content'
        } ${disabled ? DISABLED_CLASS : ''}`.trimEnd()}
      >
        {children}
      </button>
      <span className={LABEL_CLASS}>{label}</span>
    </div>
  );
}

/** The Selection split button: the count clears the selection, the arrows invert it. */
export function ToolSelection({ count, onClear, onInvert }: {
  count: number;
  onClear: () => void;
  onInvert: () => void;
}) {
  return (
    <div className={CELL_CLASS}>
      <div className="flex h-7">
        <button
          type="button"
          onClick={onClear}
          disabled={count <= 0}
          className="w-10 h-7 inline-flex items-center justify-center text-xs font-medium bg-surface-hover hover:bg-surface-elevated text-content-secondary rounded-l-lg border border-r-0 border-border transition-colors disabled:opacity-30 disabled:cursor-default"
          title="Clear selection"
        >
          {count}
        </button>
        <button
          type="button"
          onClick={onInvert}
          className="w-7 h-7 inline-flex items-center justify-center text-xs font-medium bg-surface-hover hover:bg-surface-elevated text-content-secondary rounded-r-lg border border-border transition-colors"
          title="Invert selection"
        >
          <ArrowLeftRight size={10} />
        </button>
      </div>
      <span className={LABEL_CLASS}>Selection</span>
    </div>
  );
}

/** Opens the Blink viewer on the selected frames, or on every visible frame when none is selected. */
export function BlinkToolButton({ selectedCount, visibleCount, onClick }: {
  selectedCount: number;
  visibleCount: number;
  onClick: () => void;
}) {
  const hasSelection = selectedCount > 0;
  return (
    <div className={CELL_CLASS}>
      <button
        type="button"
        onClick={onClick}
        disabled={visibleCount === 0}
        className="w-10 h-7 inline-flex items-center justify-center text-xs font-medium bg-blink hover:bg-blink-hover text-white rounded-lg transition-colors disabled:opacity-30 disabled:cursor-default"
        title={
          hasSelection
            ? `Blink ${selectedCount} selected frame${selectedCount === 1 ? '' : 's'}`
            : `Blink all ${visibleCount} frame${visibleCount === 1 ? '' : 's'}`
        }
      >
        <Play size={12} />
      </button>
      <span className={LABEL_CLASS}>{hasSelection ? 'Blink' : 'Blink all'}</span>
    </div>
  );
}

/** The `|` between toolbar groups. */
export function ToolDivider() {
  return <span className="text-border h-7 flex items-center">|</span>;
}
