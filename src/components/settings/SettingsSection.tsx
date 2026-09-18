// Settings redesign (spec 2026-09-18 §4/§6/§7): the card every registered
// settings section renders inside — title/description come from the
// registry (`sectionById`), never restated by the caller, so `?section=`
// scrolling, search and the section-level "Reset all" all key off the same
// `id`. Registers itself in `renderedSections` for the registry-coverage
// test (Task C1) for as long as it is mounted.

import { useEffect, useState, type ReactNode } from 'react';
import { RotateCcw } from 'lucide-react';
import { sectionById } from '../../settings/registry';
import { ConfirmDialog } from '../ConfirmDialog';
import { registerRenderedSection, unregisterRenderedSection } from './renderedSections';
import { useResetAllRegistry, ResetAllProvider } from '../../settings/ResetAllContext';
import { useSearchHighlight, isSectionHighlighted } from './SearchHighlightContext';

export interface SettingsSectionProps {
  /** A registry section id — `sectionById(id)` supplies the title and
   *  description. Throws (dev-time crash, per the registry's own contract)
   *  if `id` isn't registered. */
  id: string;
  children: ReactNode;
  /** Resets every field in the section back to its default. A KV section
   *  writes each field's own default through its normal commit path; a
   *  typed-config section calls the document's `resetAll()` (spec §6). When
   *  omitted, "Reset all" still appears once at least one `useSettingField`
   *  rendered underneath has registered itself (see `ResetAllContext`) — a
   *  KV section needs no explicit wiring at all. */
  onResetAll?: () => Promise<void>;
  /** Extra header controls, left of "Reset all" (e.g. a preset picker). */
  actions?: ReactNode;
  /** Overrides the confirm dialog's message for a reset that is bigger than
   *  "this card" — a whole typed document (Analysis, Plate Solving,
   *  Calibration Matching, Stacking, Logging), or one that carries a caveat
   *  the generic "every setting in this section" text would miss. Ignored
   *  when `onResetAll` isn't given. */
  resetScopeLabel?: string;
}

export function SettingsSection({ id, children, onResetAll, actions, resetScopeLabel }: SettingsSectionProps) {
  const meta = sectionById(id);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [resetting, setResetting] = useState(false);
  const { registry, registeredCount } = useResetAllRegistry();
  const highlighted = isSectionHighlighted(useSearchHighlight(), id);

  useEffect(() => {
    registerRenderedSection(id);
    return () => unregisterRenderedSection(id);
  }, [id]);

  // An explicit `onResetAll` (a typed document's `reset_*` command) always
  // wins; otherwise fall back to resetting every field a KV section's own
  // `useSettingField` calls have registered underneath.
  const effectiveOnResetAll = onResetAll ?? (registeredCount > 0 ? registry.resetAll : undefined);

  const handleConfirmed = async () => {
    if (!effectiveOnResetAll) return;
    setResetting(true);
    try {
      await effectiveOnResetAll();
    } finally {
      setResetting(false);
      setConfirmOpen(false);
    }
  };

  const confirmMessage = resetScopeLabel ?? `This resets every setting in "${meta.title}" back to its default.`;

  return (
    <section
      id={`settings-${id}`}
      data-settings-section={id}
      className={`bg-surface-elevated rounded-lg p-6 ${highlighted ? 'ring-1 ring-accent/60' : ''}`}
    >
      <div className="flex items-start justify-between gap-4 mb-4">
        <div className="min-w-0">
          <h3 className="text-lg font-semibold text-content">{meta.title}</h3>
          {meta.description && <p className="text-sm text-content-muted mt-1">{meta.description}</p>}
        </div>
        {(actions || effectiveOnResetAll) && (
          <div className="flex items-center gap-2 shrink-0">
            {actions}
            {effectiveOnResetAll && (
              <button
                type="button"
                onClick={() => setConfirmOpen(true)}
                disabled={resetting}
                title="Reset every setting in this section to its default"
                className="flex items-center gap-1.5 rounded-md border border-border px-2.5 py-1.5 text-xs text-content-secondary hover:bg-surface-hover disabled:opacity-50 transition-colors"
              >
                <RotateCcw size={13} />
                Reset all
              </button>
            )}
          </div>
        )}
      </div>

      <ResetAllProvider registry={registry}>{children}</ResetAllProvider>

      {effectiveOnResetAll && (
        <ConfirmDialog
          isOpen={confirmOpen}
          title={`Reset ${meta.title}?`}
          message={confirmMessage}
          confirmText="Reset"
          confirmDanger
          onConfirm={() => void handleConfirmed()}
          onCancel={() => setConfirmOpen(false)}
        />
      )}
    </section>
  );
}
