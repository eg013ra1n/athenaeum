import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Loader2 } from 'lucide-react';
import { api } from '../../api';
import { Button, DialogShell, Select } from '../ui';
import { useNotifications } from '../../contexts/NotificationContext';
import type { FilterMappingEdit, FilterMappingRowView, FilterMappingSheet, GateReport } from '../../types/models';

const AUTO = '__auto__';

function label(r: FilterMappingRowView): string {
  return `${r.filterRaw === '' ? '(no FILTER)' : r.filterRaw} · ${r.instrume || '(no INSTRUME)'} — ${r.frames} frame${r.frames === 1 ? '' : 's'}`;
}

/** Spec 2026-09-28 §5.3 — the publish-flow modal of §6.2. Unresolved rows
 * first (a proposal preselected — F3: it is a pending choice until Save),
 * resolved rows below a divider with their current value. Save sends only
 * the changed rows; `__auto__` sends `null` (harmless even on a `matched`
 * row with nothing stored — the backend delete is idempotent). */
export default function FilterMappingDialog({ projectId, onClose, onSaved }: { projectId: string; onClose: () => void; onSaved: (report: GateReport) => void }) {
  const { notify } = useNotifications();
  const [sheet, setSheet] = useState<FilterMappingSheet | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [choice, setChoice] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);

  const key = (r: FilterMappingRowView) => `${r.instrume}\u0000${r.filterRaw}`;
  const initial = (r: FilterMappingRowView) => (r.resolution === 'mapped' || r.resolution === 'matched' ? (r.canonical ?? '') : (r.proposal ?? ''));

  useEffect(() => {
    let cancelled = false;
    api.invoke<FilterMappingSheet>('get_collab_filter_mapping_sheet', { projectId })
      .then((s) => {
        if (cancelled) return;
        setSheet(s);
        setChoice(Object.fromEntries(s.rows.map((r) => [key(r), initial(r)])));
      })
      .catch((err) => {
        console.error('[projects] filter mapping sheet failed:', err);
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      });
    return () => { cancelled = true; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId]);

  // The shell focuses `[data-autofocus]` on mount, when the sheet has not
  // loaded yet — focus the first select once its rows exist.
  const bodyRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (sheet) bodyRef.current?.querySelector<HTMLElement>('[data-autofocus]')?.focus();
  }, [sheet]);

  const edits = useMemo<FilterMappingEdit[]>(() => {
    if (!sheet) return [];
    const out: FilterMappingEdit[] = [];
    for (const r of sheet.rows) {
      const c = choice[key(r)] ?? '';
      // The user explicitly picked "— automatic —": a genuine delete for a
      // `mapped`/`mappedToMissing` row (there is a real mapping to clear),
      // but a `matched` row already resolves through the dictionary alone
      // with nothing stored — sending it anyway is harmless (an idempotent
      // no-op delete) but must not count as a pending change, or Save would
      // light up for a no-op edit.
      if (c === AUTO) {
        if (r.resolution !== 'matched') {
          out.push({ instrume: r.instrume, filterRaw: r.filterRaw, canonical: null });
        }
        continue;
      }
      if (c === '') continue; // no choice on an unresolved row: not sent
      // The row's current effective value with nothing chosen: `null` only
      // when nothing is resolved yet (`unmapped`).
      const stored = r.resolution === 'unmapped' ? null : r.canonical;
      if (c !== stored) out.push({ instrume: r.instrume, filterRaw: r.filterRaw, canonical: c });
    }
    return out;
  }, [sheet, choice]);

  const save = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const report = await api.invoke<GateReport>('set_collab_filter_mappings', { projectId, mappings: edits });
      // `GateReport` carries only the absolute `publishable` count, not a
      // "newly passing" delta — word it as a plain count, not as a change,
      // and pluralize correctly (it was always "frames", even for 1).
      const n = report.publishable;
      notify({ title: 'Filter mappings saved', detail: `${n} frame${n === 1 ? '' : 's'} pass the gate`, kind: 'project', tone: 'success' });
      onSaved(report);
    } catch (err) {
      console.error('[projects] set_collab_filter_mappings failed:', err);
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, [projectId, edits, notify, onSaved]);

  const unresolved = sheet?.rows.filter((r) => r.resolution === 'unmapped' || r.resolution === 'mappedToMissing') ?? [];
  const resolved = sheet?.rows.filter((r) => r.resolution === 'mapped' || r.resolution === 'matched') ?? [];

  const firstKey = [...unresolved, ...resolved][0] ? key([...unresolved, ...resolved][0]) : null;

  const row = (r: FilterMappingRowView) => (
    <li key={key(r)} className="flex flex-wrap items-center gap-2 py-1">
      <span className="min-w-[18rem] text-content">{label(r)}</span>
      {r.resolution === 'mappedToMissing' && <span className="text-[11.5px] text-warning">mapped to &quot;{r.canonical}&quot;, not in this project</span>}
      <Select
        aria-label={`Canonical for ${label(r)}`}
        data-autofocus={key(r) === firstKey ? true : undefined}
        value={choice[key(r)] ?? ''}
        onChange={(e) => setChoice({ ...choice, [key(r)]: e.target.value })}
      >
        {r.resolution !== 'mapped' && r.resolution !== 'matched' && <option value="">— choose —</option>}
        {/* `mappedToMissing`'s stored canonical is no longer in this
            project's dictionary — "— automatic —" is how the user deletes
            that stale mapping instead of having to pick a replacement. */}
        {r.resolution !== 'unmapped' && <option value={AUTO}>— automatic —</option>}
        {sheet!.dictionary.map((d) => (
          <option key={d.canonical} value={d.canonical}>{d.canonical} · {d.kind}</option>
        ))}
      </Select>
    </li>
  );

  return (
    <DialogShell
      title="Filter mapping"
      size="md"
      onClose={onClose}
      busy={busy}
      footer={sheet ? (
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" onClick={() => void save()} disabled={busy || edits.length === 0}>
            {busy && <Loader2 size={12} className="animate-spin" />}Save
          </Button>
        </>
      ) : undefined}
    >
      <div ref={bodyRef} className="max-h-[60vh] overflow-auto">
        <p className="mb-3 text-[11.5px] text-content-faint">Pick the project&apos;s canonical filter for each raw name. Remembered for your account and asked once.</p>
        {error && <p className="mb-2 text-[12.5px] text-error">{error}</p>}
        {!sheet && !error && <Loader2 size={16} className="animate-spin text-content-muted" />}
        {sheet && (
          <>
            <ul>{unresolved.map(row)}</ul>
            {resolved.length > 0 && (
              <>
                <p className="mt-3 border-t border-border pt-2 text-[11.5px] text-content-faint">Already resolved</p>
                <ul>{resolved.map(row)}</ul>
              </>
            )}
          </>
        )}
      </div>
    </DialogShell>
  );
}
