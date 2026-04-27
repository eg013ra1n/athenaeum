import React from 'react';
import { ArrowDown, ArrowUp, Trash2 } from 'lucide-react';
import type { Rule, ScanRootWithAvailability } from '../../../types/models';
import { RULE_KIND_META } from '../keepRules';
import { MasterRootConfig } from './MasterRootConfig';
import { PathContainsConfig } from './PathContainsConfig';

interface Props {
  rule: Rule;
  index: number;
  total: number;
  scanRoots: ScanRootWithAvailability[];
  /** Number of duplicate groups currently picked by this rule. */
  pickedGroups?: number;
  onUpdate: (next: Rule) => void;
  onRemove: () => void;
  onMoveUp: () => void;
  onMoveDown: () => void;
}

export const RuleRow: React.FC<Props> = ({
  rule,
  index,
  total,
  scanRoots,
  pickedGroups,
  onUpdate,
  onRemove,
  onMoveUp,
  onMoveDown,
}) => {
  const meta = RULE_KIND_META[rule.kind];
  const disabled = !rule.enabled;

  return (
    <div
      className={`flex items-start gap-2 p-2 rounded-lg border transition-colors ${
        disabled
          ? 'border-border bg-surface/40 opacity-60'
          : 'border-border bg-surface'
      }`}
    >
      {/* Position controls */}
      <div className="flex flex-col items-center -gap-px flex-shrink-0">
        <button
          type="button"
          onClick={onMoveUp}
          disabled={index === 0}
          title="Move up"
          aria-label="Move rule up"
          className="p-0.5 text-content-muted hover:text-content transition-colors disabled:opacity-30 disabled:cursor-not-allowed"
        >
          <ArrowUp size={12} />
        </button>
        <button
          type="button"
          onClick={onMoveDown}
          disabled={index === total - 1}
          title="Move down"
          aria-label="Move rule down"
          className="p-0.5 text-content-muted hover:text-content transition-colors disabled:opacity-30 disabled:cursor-not-allowed"
        >
          <ArrowDown size={12} />
        </button>
      </div>

      {/* Order badge */}
      <span className="flex-shrink-0 inline-flex items-center justify-center w-5 h-5 rounded-full bg-surface-hover border border-border text-[10px] font-semibold text-content-secondary mt-0.5">
        {index + 1}
      </span>

      {/* Body */}
      <div className="flex-1 min-w-0 flex flex-col gap-1">
        <div className="flex items-center gap-2 flex-wrap">
          <label className="inline-flex items-center gap-1.5 cursor-pointer select-none">
            <input
              type="checkbox"
              checked={rule.enabled}
              onChange={(e) =>
                onUpdate({ ...rule, enabled: e.target.checked } as Rule)
              }
              className="rounded border-border focus:ring-accent"
            />
            <span className="text-xs font-semibold text-content">
              {meta.name}
            </span>
          </label>
          {rule.enabled && pickedGroups != null && pickedGroups > 0 && (
            <span
              className="inline-flex items-center h-5 px-1.5 rounded text-[10px] font-semibold bg-accent/15 text-accent border border-accent/40"
              title="Number of duplicate groups currently picked by this rule"
            >
              {pickedGroups} group{pickedGroups === 1 ? '' : 's'}
            </span>
          )}
          <span className="text-[11px] text-content-muted">
            {meta.description}
          </span>
        </div>

        {/* Inline config — depends on rule kind */}
        {rule.kind === 'master_root' && (
          <MasterRootConfig
            rootId={rule.config.rootId}
            scanRoots={scanRoots}
            disabled={disabled}
            onChange={(rootId) =>
              onUpdate({ ...rule, config: { rootId } })
            }
          />
        )}
        {rule.kind === 'path_contains' && (
          <PathContainsConfig
            patterns={rule.config.patterns}
            caseSensitive={rule.config.caseSensitive}
            disabled={disabled}
            onChange={(cfg) => onUpdate({ ...rule, config: cfg })}
          />
        )}
        {/* oldest_mtime / shortest_path: no config UI — the description is enough */}
      </div>

      {/* Remove button */}
      <button
        type="button"
        onClick={onRemove}
        title="Remove rule"
        aria-label="Remove rule"
        className="flex-shrink-0 p-1 text-content-muted hover:text-error transition-colors"
      >
        <Trash2 size={12} />
      </button>
    </div>
  );
};
