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

export interface SettingsSectionProps {
  /** A registry section id — `sectionById(id)` supplies the title and
   *  description. Throws (dev-time crash, per the registry's own contract)
   *  if `id` isn't registered. */
  id: string;
  children: ReactNode;
  /** Resets every field in the section back to its default. A KV section
   *  writes each field's own default through its normal commit path; a
   *  typed-config section calls the document's `resetAll()` (spec §6). The
   *  "Reset all" button — and its confirm dialog — render only when this is
   *  given. */
  onResetAll?: () => Promise<void>;
  /** Extra header controls, left of "Reset all" (e.g. a preset picker). */
  actions?: ReactNode;
}

export function SettingsSection({ id, children, onResetAll, actions }: SettingsSectionProps) {
  const meta = sectionById(id);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [resetting, setResetting] = useState(false);

  useEffect(() => {
    registerRenderedSection(id);
    return () => unregisterRenderedSection(id);
  }, [id]);

  const handleConfirmed = async () => {
    if (!onResetAll) return;
    setResetting(true);
    try {
      await onResetAll();
    } finally {
      setResetting(false);
      setConfirmOpen(false);
    }
  };

  return (
    <section id={`settings-${id}`} data-settings-section={id} className="bg-surface-elevated rounded-lg p-6">
      <div className="flex items-start justify-between gap-4 mb-4">
        <div className="min-w-0">
          <h3 className="text-lg font-semibold text-content">{meta.title}</h3>
          {meta.description && <p className="text-sm text-content-muted mt-1">{meta.description}</p>}
        </div>
        {(actions || onResetAll) && (
          <div className="flex items-center gap-2 shrink-0">
            {actions}
            {onResetAll && (
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

      {children}

      {onResetAll && (
        <ConfirmDialog
          isOpen={confirmOpen}
          title={`Reset ${meta.title}?`}
          message={`This resets every setting in "${meta.title}" back to its default.`}
          confirmText="Reset"
          confirmDanger
          onConfirm={() => void handleConfirmed()}
          onCancel={() => setConfirmOpen(false)}
        />
      )}
    </section>
  );
}
