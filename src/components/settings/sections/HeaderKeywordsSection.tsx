// Settings → General → FITS keywords (plan Task 7, spec §5.6): the user orders the header keywords each of twelve
// catalog fields is read from. One typed document (`HeaderFallbackConfig`) autosaved through
// `useAutosaveDocument`; no Save button, no banner. Below the rows, a preview of how many frames already in the
// catalog would read differently, and an explicit Apply (confirmed, with progress) that re-reads them.

import { useEffect, useRef, useState } from 'react';
import { api } from '../../../api';
import { useNotifications } from '../../../contexts/NotificationContext';
import { useAutosaveDocument } from '../../../hooks/useAutosaveDocument';
import { useHeaderFallbackPreview } from '../../../hooks/useHeaderFallbackPreview';
import { useSettingsDefaults } from '../../../settings/SettingsDefaultsContext';
import {
  DEFAULT_HEADER_CHAINS,
  HEADER_FIELDS,
  HEADER_FIELD_LABELS,
} from '../../../settings/headerFields';
import { fieldMeta } from '../../../settings/registry';
import type {
  HeaderField,
  HeaderFallbackConfig,
  HeaderFallbackCounts,
  HeaderFallbackImportPlan,
  HeaderFallbackProgress,
} from '../../../types/models';
import { ConfirmDialog } from '../../ConfirmDialog';
import { Button } from '../../ui';
import { ResetButton } from '../ResetButton';
import { SettingsSection } from '../SettingsSection';
import { HeaderKeywordsTransfer } from './HeaderKeywordsTransfer';
import { KeywordChain } from './KeywordChain';

export { HEADER_FIELD_LABELS };

const RESET_ALL_SCOPE =
  "This puts every field's keywords back in the default order. Catalog values change only when you apply.";

function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function plural(n: number, one: string, many: string): string {
  return n === 1 ? one : many;
}

/** The inline refusal for a keyword typed into a field, or `null` when it can be added. The server's `validate`
 *  stays the authority; this only keeps an obviously unusable keyword from being sent. */
export function keywordProblem(raw: string, order: string[]): { keyword: string; error: string | null } {
  const keyword = raw.trim().toUpperCase();
  if (keyword === '' || keyword.includes('=')) {
    return { keyword, error: 'Keywords can\'t be empty or contain "=".' };
  }
  if (order.includes(keyword)) return { keyword, error: `${keyword} is already in this list.` };
  return { keyword, error: null };
}

interface FieldRowProps {
  field: HeaderField;
  order: string[];
  defaultChain: string[];
  showReset: boolean;
  onChange(order: string[]): void;
  onReset(): void;
}

function FieldRow({ field, order, defaultChain, showReset, onChange, onReset }: FieldRowProps) {
  const meta = fieldMeta('general.headerKeywords', field);
  const [draft, setDraft] = useState('');
  const [problem, setProblem] = useState<string | null>(null);

  // Enter or blur. An untouched input is no event; anything typed is checked, and while it is refused nothing is saved.
  const commit = () => {
    if (draft === '') {
      setProblem(null);
      return;
    }
    const { keyword, error } = keywordProblem(draft, order);
    if (error) {
      setProblem(error);
      return;
    }
    setDraft('');
    setProblem(null);
    onChange([...order, keyword]);
  };

  return (
    <div data-field-row={field}>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
        <span className="w-36 shrink-0 text-sm text-content-secondary" title={meta.help}>
          {meta.label}
        </span>
        <KeywordChain field={field} order={order} defaults={defaultChain} onChange={onChange} />
        <input
          type="text"
          value={draft}
          placeholder="+ keyword"
          aria-label={`Add a keyword to ${meta.label}`}
          aria-invalid={problem !== null}
          onChange={(e) => {
            setDraft(e.target.value);
            setProblem(null);
          }}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              commit();
            } else if (e.key === 'Escape') {
              setDraft('');
              setProblem(null);
            }
          }}
          className="w-28 bg-surface-hover border border-border rounded px-2 py-0.5 text-xs text-content placeholder:text-content-muted focus:outline-none focus:border-accent"
        />
        <ResetButton visible={showReset} defaultLabel={defaultChain.join(', ')} onReset={onReset} />
      </div>
      {problem && (
        <p role="alert" className="text-xs text-error mt-1 ml-0 sm:ml-[9.75rem]">
          {problem}
        </p>
      )}
    </div>
  );
}

function confirmMessage(counts: HeaderFallbackCounts): string {
  const lines = counts.byField
    .filter((c) => c.frames > 0)
    .map((c) => `${HEADER_FIELD_LABELS[c.field]}: ${c.frames} ${plural(c.frames, 'frame', 'frames')}`);
  if (counts.valuesKept > 0) lines.push(`Values you set yourself stay as they are (${counts.valuesKept}).`);
  if (counts.setsRegrouped > 0) {
    let line = `Darks, flats and bias in ${counts.setsRegrouped} ${plural(counts.setsRegrouped, 'calibration set', 'calibration sets')} will be grouped again.`;
    // Nothing re-matches the lights afterwards: say how many links go, and what to do about it.
    if (counts.lightLinksDropped > 0) {
      const by = counts.manualLinksDropped > 0 ? ` (${counts.manualLinksDropped} made by hand)` : '';
      line += ` ${counts.lightLinksDropped} ${plural(counts.lightLinksDropped, 'light link', 'light links')} to those sets will be removed${by}. Run Find calibration on their frame sets afterwards.`;
    }
    lines.push(line);
  }
  if (counts.framesWithoutHeader > 0) {
    lines.push(
      counts.framesWithoutHeader === 1
        ? '1 frame has no stored header and is not changed.'
        : `${counts.framesWithoutHeader} frames have no stored header and are not changed.`,
    );
  }
  return lines.join('\n');
}

export function HeaderKeywordsSection() {
  const { defaults } = useSettingsDefaults();
  const { notify } = useNotifications();

  const { doc, patch, error, saving, dirty, savedAt, isDefault, resetField, resetAll } =
    useAutosaveDocument<HeaderFallbackConfig>({
      load: () => api.invoke<HeaderFallbackConfig>('get_header_fallbacks'),
      save: (config) => api.invoke('set_header_fallbacks', { config }),
      resetAll: () => api.invoke('reset_header_fallbacks').then(() => {}),
      defaults: defaults?.headerFallbacks ?? null,
      label: 'FITS keywords',
    });

  const { counts, current, refresh } = useHeaderFallbackPreview({ enabled: doc !== null, savedAt });

  // Apply re-reads the frames under the rules the server has stored. It is offered, and the confirm stands, only while
  // the screen's rules are those stored ones (nothing waiting to be saved, no save running, none refused) and `counts`
  // is the preview of them. The refs let a click or confirm that follows a state change within one event re-check.
  const ready = counts !== null && current && !dirty && !saving && error === null;
  const readyRef = useRef(ready);
  readyRef.current = ready;
  const applyingRef = useRef(false);

  const [confirmOpen, setConfirmOpen] = useState(false);
  const [applying, setApplying] = useState(false);
  const [progress, setProgress] = useState<HeaderFallbackProgress | null>(null);
  // What the user confirmed: the line keeps showing it while Apply runs, whatever a preview says meanwhile.
  const [confirmed, setConfirmed] = useState<HeaderFallbackCounts | null>(null);

  // `resetAll` reloads the document without a save, so the preview is asked for here.
  const onResetAll = async () => {
    await resetAll();
    refresh();
  };

  // Progress of an Apply (StrictMode-safe listener pattern).
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<HeaderFallbackProgress>('header-fallbacks-progress', (p) => {
        if (cancelled) return;
        setProgress(p);
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[HeaderKeywords] listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // A confirm the rules outran closes; it does not come back on its own with the new counts.
  useEffect(() => {
    if (!ready) setConfirmOpen(false);
  }, [ready]);

  const openConfirm = () => {
    if (!readyRef.current || applyingRef.current) return;
    setConfirmOpen(true);
  };

  const apply = async () => {
    if (!readyRef.current || applyingRef.current) {
      setConfirmOpen(false);
      return;
    }
    applyingRef.current = true;
    setConfirmOpen(false);
    setProgress(null);
    setConfirmed(counts);
    setApplying(true);
    try {
      const done = await api.invoke<HeaderFallbackCounts>('apply_header_fallbacks');
      notify({
        kind: 'generic',
        tone: 'success',
        title: 'FITS keywords applied',
        detail: `${done.framesChanged} ${plural(done.framesChanged, 'frame', 'frames')} changed, ${done.setsRegrouped} ${plural(done.setsRegrouped, 'calibration set', 'calibration sets')} grouped again.`,
      });
    } catch (err) {
      console.error('[HeaderKeywords] apply failed:', err);
      notify({
        kind: 'generic',
        tone: 'warning',
        hasErrors: true,
        title: 'FITS keywords not applied',
        detail: errMsg(err),
      });
    } finally {
      applyingRef.current = false;
      setApplying(false);
      // A failed Apply can have written the frames and the sets before it failed: the line and
      // the title must follow what the catalog holds now, as after a success.
      refresh();
    }
  };

  if (doc === null) {
    return (
      <SettingsSection id="general.headerKeywords">
        {error ? (
          <div className="p-4 bg-error-muted border border-error/50 rounded-lg">
            <p className="text-sm text-error">Failed to load FITS keywords: {error}</p>
          </div>
        ) : (
          <div className="text-sm text-content-muted">Loading FITS keywords…</div>
        )}
      </SettingsSection>
    );
  }

  const defaultsLoaded = defaults?.headerFallbacks != null;
  const chainOf = (f: HeaderField): string[] =>
    defaults?.headerFallbacks?.fields?.[f]?.order ?? DEFAULT_HEADER_CHAINS[f];

  const setOrder = (f: HeaderField, order: string[]) =>
    patch((prev) => ({
      ...prev,
      fields: { ...prev.fields, [f]: { order, retired: prev.fields[f]?.retired ?? [] } },
    }));

  // The plan was computed from the stored rules some time ago: only the fields it changes are taken from the file,
  // every other field stays as the screen has it now (an edit made while the plan was computed is kept).
  const importFields = (plan: HeaderFallbackImportPlan) =>
    patch((prev) => {
      const fields = { ...prev.fields };
      for (const c of plan.changes) {
        const rule = plan.config.fields[c.field];
        if (rule) fields[c.field] = rule;
      }
      return { ...prev, fields };
    });

  const line = applying ? confirmed : counts;
  const showLine = line !== null && line.framesChanged > 0;
  const progressLabel =
    progress?.phase === 'calibration'
      ? 'Grouping calibration frames…'
      : progress?.phase === 'frames'
        ? `Applying… ${progress.done} of ${progress.total} frames`
        : 'Applying…';

  return (
    <SettingsSection
      id="general.headerKeywords"
      onResetAll={onResetAll}
      resetScopeLabel={RESET_ALL_SCOPE}
      actions={<HeaderKeywordsTransfer onImport={importFields} />}
    >
      <div className="space-y-2.5">
        {HEADER_FIELDS.map((f) => (
          <FieldRow
            key={f}
            field={f}
            order={doc.fields[f]?.order ?? chainOf(f)}
            defaultChain={chainOf(f)}
            showReset={defaultsLoaded && !isDefault(`fields.${f}.order`)}
            onChange={(order) => setOrder(f, order)}
            onReset={() => resetField(`fields.${f}.order`)}
          />
        ))}
      </div>

      {showLine && line && (
        <div className="mt-5 pt-4 border-t border-border space-y-2" data-testid="header-keywords-preview">
          <p className="text-sm text-content">
            {line.framesChanged}{' '}
            {plural(line.framesChanged, 'frame reads', 'frames read')} differently under these rules:
          </p>
          <div className="flex flex-wrap items-center justify-between gap-3">
            <p className="text-sm text-content-muted min-w-0">
              {line.byField
                .filter((c) => c.frames > 0)
                .map((c) => `${HEADER_FIELD_LABELS[c.field]} ${c.frames}`)
                .join(' · ')}
            </p>
            <Button variant="primary" disabled={applying || !ready} onClick={openConfirm}>
              {applying ? progressLabel : 'Apply to catalog'}
            </Button>
          </div>
          {line.valuesKept > 0 && (
            <p className="text-xs text-content-muted">
              {line.valuesKept}{' '}
              {plural(line.valuesKept, 'value you set yourself is kept.', 'values you set yourself are kept.')}
            </p>
          )}
        </div>
      )}

      <ConfirmDialog
        isOpen={confirmOpen && ready}
        title="Apply FITS keywords to the catalog?"
        message={counts ? confirmMessage(counts) : ''}
        confirmText="Apply"
        onConfirm={() => void apply()}
        onCancel={() => setConfirmOpen(false)}
      />
    </SettingsSection>
  );
}
