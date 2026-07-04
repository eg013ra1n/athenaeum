import React, { useCallback, useMemo, useState } from 'react';
import { ChevronDown, ChevronRight, Plus, RotateCcw, Settings } from 'lucide-react';
import type { Rule, RuleChain, RuleKind, ScanRootWithAvailability } from '../../types/helpers';
import { RULE_KIND_META, defaultRuleChain } from './keepRules';
import { RuleRow } from './rules/RuleRow';

interface Props {
  chain: RuleChain;
  scanRoots: ScanRootWithAvailability[];
  /** Per-rule-kind count of groups the rule currently picks. Surfaced inline
   *  so the user can see what each rule actually does in their data. */
  ruleCounts?: Partial<Record<RuleKind, number>>;
  /** Total number of duplicate groups currently displayed. Used in the header
   *  to summarise coverage. */
  totalGroups?: number;
  onChange: (next: RuleChain) => void;
  /** Called when the user clicks "Reset to defaults". */
  onResetToDefaults: () => void;
}

const ALL_KINDS: RuleKind[] = [
  'master_root',
  'path_contains',
  'oldest_mtime',
  'shortest_path',
];

function newRuleOfKind(kind: RuleKind): Rule {
  switch (kind) {
    case 'master_root':
      return { kind: 'master_root', enabled: true, config: { rootId: null } };
    case 'path_contains':
      return {
        kind: 'path_contains',
        enabled: true,
        config: { patterns: [], caseSensitive: false },
      };
    case 'oldest_mtime':
      return { kind: 'oldest_mtime', enabled: true, config: {} };
    case 'shortest_path':
      return { kind: 'shortest_path', enabled: true, config: {} };
  }
}

export const RuleChainPanel: React.FC<Props> = ({
  chain,
  scanRoots,
  ruleCounts,
  totalGroups,
  onChange,
  onResetToDefaults,
}) => {
  const [expanded, setExpanded] = useState(false);
  const [addMenuOpen, setAddMenuOpen] = useState(false);

  const presentKinds = useMemo(
    () => new Set(chain.map((r) => r.kind)),
    [chain],
  );

  const missingKinds = ALL_KINDS.filter((k) => !presentKinds.has(k));

  const enabledCount = chain.filter((r) => r.enabled).length;
  const summary = useMemo(() => {
    const parts: string[] = [];
    for (const r of chain) {
      if (!r.enabled) continue;
      let label = RULE_KIND_META[r.kind].name;
      if (
        r.kind === 'path_contains' &&
        r.config.patterns.length > 0
      ) {
        label += ` (${r.config.patterns.length})`;
      }
      parts.push(label);
    }
    return parts.length > 0 ? parts.join(' → ') : 'No rules enabled';
  }, [chain]);

  /** Total groups currently picked by any rule in the chain. */
  const pickedCount = useMemo(
    () =>
      Object.values(ruleCounts ?? {}).reduce<number>(
        (acc, n) => acc + (n ?? 0),
        0,
      ),
    [ruleCounts],
  );

  const updateAt = useCallback(
    (index: number, next: Rule) => {
      const arr = chain.slice();
      arr[index] = next;
      onChange(arr);
    },
    [chain, onChange],
  );

  const removeAt = useCallback(
    (index: number) => {
      const arr = chain.slice();
      arr.splice(index, 1);
      onChange(arr);
    },
    [chain, onChange],
  );

  const moveUp = useCallback(
    (index: number) => {
      if (index === 0) return;
      const arr = chain.slice();
      [arr[index - 1], arr[index]] = [arr[index], arr[index - 1]];
      onChange(arr);
    },
    [chain, onChange],
  );

  const moveDown = useCallback(
    (index: number) => {
      if (index === chain.length - 1) return;
      const arr = chain.slice();
      [arr[index], arr[index + 1]] = [arr[index + 1], arr[index]];
      onChange(arr);
    },
    [chain, onChange],
  );

  const addRule = useCallback(
    (kind: RuleKind) => {
      onChange([...chain, newRuleOfKind(kind)]);
      setAddMenuOpen(false);
    },
    [chain, onChange],
  );

  return (
    <div className="rounded-lg border border-l-2 border-accent/30 border-l-accent bg-accent/5">
      {/* Header — always visible, toggles expansion */}
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        className="w-full flex items-center gap-2 px-3 py-2 text-left hover:bg-accent/10 transition-colors rounded-lg"
      >
        {expanded ? (
          <ChevronDown size={16} className="flex-shrink-0 text-accent" />
        ) : (
          <ChevronRight size={16} className="flex-shrink-0 text-accent" />
        )}
        <Settings size={14} className="flex-shrink-0 text-accent" />
        <span className="text-xs font-semibold text-accent flex-shrink-0 uppercase tracking-wide">
          Picking rules
        </span>
        <span className="text-[11px] text-content-muted flex-shrink-0">
          ·
        </span>
        <span className="text-[11px] text-content-muted truncate min-w-0">
          {summary}
        </span>
        <span className="text-[10px] text-content-muted ml-auto flex-shrink-0 inline-flex items-center gap-2">
          {totalGroups != null && totalGroups > 0 && (
            <span>
              <span className="text-content">{pickedCount}</span>
              <span> / {totalGroups} groups picked</span>
            </span>
          )}
          <span>·</span>
          <span>
            {enabledCount} of {chain.length} rules
          </span>
        </span>
      </button>

      {expanded && (
        <div className="px-3 pb-3 pt-1 flex flex-col gap-2">
          <p className="text-[11px] text-content-muted">
            Rules run in order, top to bottom. The first rule that picks a
            keeper for a group wins. A rule that would mark every file in a
            group is automatically skipped.
          </p>

          {chain.length === 0 && (
            <p className="text-xs text-warning bg-warning-muted border border-warning/40 rounded p-2">
              No rules — every group will require manual marking.
            </p>
          )}

          <div className="flex flex-col gap-1.5">
            {chain.map((rule, i) => (
              <RuleRow
                key={rule.kind}
                rule={rule}
                index={i}
                total={chain.length}
                scanRoots={scanRoots}
                pickedGroups={ruleCounts?.[rule.kind] ?? 0}
                onUpdate={(next) => updateAt(i, next)}
                onRemove={() => removeAt(i)}
                onMoveUp={() => moveUp(i)}
                onMoveDown={() => moveDown(i)}
              />
            ))}
          </div>

          {/* Footer controls */}
          <div className="flex items-center gap-2 flex-wrap">
            <div className="relative">
              <button
                type="button"
                onClick={() => setAddMenuOpen((v) => !v)}
                disabled={missingKinds.length === 0}
                className="h-7 px-2.5 inline-flex items-center gap-1 text-xs font-medium rounded-lg border border-border bg-surface-hover hover:bg-surface-elevated text-content-secondary hover:text-content transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <Plus size={12} />
                Add rule
              </button>
              {addMenuOpen && missingKinds.length > 0 && (
                <div className="absolute z-30 mt-1 left-0 min-w-[260px] rounded-lg border border-border bg-surface-elevated shadow-lg p-1">
                  {missingKinds.map((k) => (
                    <button
                      key={k}
                      type="button"
                      onClick={() => addRule(k)}
                      className="w-full text-left px-2 py-1.5 rounded hover:bg-surface-hover transition-colors"
                    >
                      <div className="text-xs font-semibold text-content">
                        {RULE_KIND_META[k].name}
                      </div>
                      <div className="text-[11px] text-content-muted">
                        {RULE_KIND_META[k].description}
                      </div>
                    </button>
                  ))}
                </div>
              )}
            </div>

            <button
              type="button"
              onClick={onResetToDefaults}
              title="Replace the chain with the four built-in rules in their default order"
              className="h-7 px-2.5 inline-flex items-center gap-1 text-xs font-medium rounded-lg border border-border bg-surface-hover hover:bg-surface-elevated text-content-secondary hover:text-content transition-colors"
            >
              <RotateCcw size={12} />
              Reset chain to defaults
            </button>
          </div>
        </div>
      )}
    </div>
  );
};

// Re-export for callers that want to construct a fresh default chain.
export { defaultRuleChain };
