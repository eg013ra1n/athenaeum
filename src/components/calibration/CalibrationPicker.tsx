import { useCallback, useEffect, useMemo, useState } from 'react';
import { Loader2, RotateCcw, X } from 'lucide-react';
import { api } from '../../api';
import { useNotifications } from '../../contexts/NotificationContext';
import { CandidateCard } from './CandidateCard';
import { SubjectSummary } from './SubjectSummary';
import {
  applyFilter,
  camerasOf,
  EMPTY_FILTER,
  exposuresOf,
  filterIsActive,
  slotsFor,
  SLOT_LABEL,
  type CandidateFilter,
  type PickerSubject,
  type SlotKind,
} from './pickerModel';
import { saveSubCalibration, useCalibrationPicker } from './useCalibrationPicker';

/**
 * What the user did to one slot before pressing Apply.
 *
 * - `number` — a set was picked (or swapped) → assign it.
 * - `null`   — untouched → the caller must do nothing.
 * - `'clear'` — explicitly deselected → the caller must clear that link.
 *   Without this third state a deselect is indistinguishable from "untouched"
 *   and silently does nothing.
 */
export type ManualPick = number | null | 'clear';

const pick = (selected: number | null, current: number | null): ManualPick => {
  if (selected === current) return null;
  if (selected === null) return 'clear';
  return selected;
};

/**
 * Choosing calibration, for light frames and for a calibration set's own
 * sub-calibration alike — one interface for what used to be two modals that
 * asked the same question in different words.
 *
 * `onApplyLights` is required for a `lights` subject: the calibration
 * hierarchy owns that write and its refresh. A `set` subject saves itself.
 */
export function CalibrationPicker({
  subject,
  onApplyLights,
  onApplied,
  onClose,
}: {
  subject: PickerSubject;
  onApplyLights?: (flat: ManualPick, dark: ManualPick, bias: ManualPick) => void;
  /** Called after a successful save so the opener can refresh. */
  onApplied?: () => void;
  onClose: () => void;
}) {
  const { notify } = useNotifications();
  const slots = useMemo(() => slotsFor(subject), [subject]);
  const [slot, setSlot] = useState<SlotKind>(slots[0]);
  const [onlyCompatible, setOnlyCompatible] = useState(true);
  const [filter, setFilter] = useState<CandidateFilter>(EMPTY_FILTER);
  const [selection, setSelection] = useState<Partial<Record<SlotKind, number | null>>>({});
  const [saving, setSaving] = useState(false);
  const [resetting, setResetting] = useState(false);

  const { requirement, candidates, current, loading, error, reload } =
    useCalibrationPicker(subject);

  // The backend's answer is the starting point; the user's picks layer on top.
  useEffect(() => {
    setSelection({});
  }, [current]);

  const selectedIn = (k: SlotKind): number | null =>
    selection[k] !== undefined ? (selection[k] as number | null) : current[k];

  // "Only sets that fit" narrows on the client: the compatibility answer
  // travels with every candidate, and the currently linked set stays visible
  // either way so its badge always has somewhere to render.
  const visible = useMemo(() => {
    const kept = onlyCompatible
      ? candidates[slot].filter(c => c.compatible || c.set.id === current[slot])
      : candidates[slot];
    return applyFilter(kept, filter);
  }, [candidates, slot, filter, onlyCompatible, current]);
  const cameras = useMemo(() => camerasOf(candidates[slot]), [candidates, slot]);
  const exposures = useMemo(() => exposuresOf(candidates[slot]), [candidates, slot]);

  const patchFilter = useCallback(
    (patch: Partial<CandidateFilter>) => setFilter(f => ({ ...f, ...patch })),
    [],
  );

  const hasChanges = slots.some(k => selectedIn(k) !== current[k]);

  const handleApply = async () => {
    if (subject.kind === 'lights') {
      onApplyLights?.(
        pick(selectedIn('flat'), current.flat),
        pick(selectedIn('dark'), current.dark),
        pick(selectedIn('bias'), current.bias),
      );
      return;
    }
    setSaving(true);
    try {
      await saveSubCalibration(subject.setId, {
        darkflat: selectedIn('darkflat'),
        dark: selectedIn('dark'),
        bias: selectedIn('bias'),
      });
      onApplied?.();
      onClose();
    } catch (e) {
      console.error('[CalibrationPicker] save failed:', e);
      notify({
        title: 'Could not save the calibration',
        detail: String(e),
        kind: 'merge',
        tone: 'warning',
        hasErrors: true,
      });
    } finally {
      setSaving(false);
    }
  };

  const handleResetToAutomatic = async () => {
    setResetting(true);
    try {
      if (subject.kind === 'lights') {
        const cleared = await api.invoke<number>('clear_manual_calibration_override', {
          frameIds: subject.frameIds,
          calibrationType: null,
        });
        notify({
          title: cleared > 0 ? 'Manual choices cleared' : 'Nothing to clear',
          detail:
            cleared > 0
              ? `${cleared} link${cleared === 1 ? '' : 's'} can be reassigned by Find Calibration.`
              : 'These frames had no manual choices.',
          kind: 'merge',
          tone: 'info',
        });
      } else {
        await api.invoke('clear_subcalibration_override', {
          sourceSetId: subject.setId,
          calibrationType: null,
        });
        notify({
          title: 'Manual choices cleared',
          detail: 'Find Calibration can assign this set again.',
          kind: 'merge',
          tone: 'info',
        });
      }
      setSelection({});
      reload();
      onApplied?.();
    } catch (e) {
      console.error('[CalibrationPicker] reset failed:', e);
      notify({
        title: 'Could not clear the manual choices',
        detail: String(e),
        kind: 'merge',
        tone: 'warning',
        hasErrors: true,
      });
    } finally {
      setResetting(false);
    }
  };

  // The caller's label is a filter/exposure caption ("No Filter (180s)",
  // "Selected frames"), so it goes on its own line — glued into the title it
  // read "92 Selected frames frames".
  const title =
    subject.kind === 'lights'
      ? `Choose calibration for ${subject.frameIds.length} light frame${subject.frameIds.length === 1 ? '' : 's'}`
      : `Choose calibration for this ${subject.sourceType === 'flat' ? 'flat' : 'dark'} set`;
  const subtitle =
    subject.kind === 'lights' && subject.label ? subject.label : null;

  const totalHere = candidates[slot].length;
  const compatibleHere = candidates[slot].filter(c => c.compatible).length;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4">
      <div className="flex h-[85vh] w-full max-w-6xl flex-col overflow-hidden rounded-xl border border-border bg-surface-elevated shadow-2xl">
        {/* Header */}
        <div className="flex items-start justify-between border-b border-border px-5 py-3">
          <div>
            <h2 className="text-lg font-semibold text-content">
              {title}
              {subtitle && (
                <span className="ml-2 text-sm font-normal text-content-muted">{subtitle}</span>
              )}
            </h2>
            <p className="mt-0.5 text-xs text-content-muted">
              Your matching rules decide what fits; the order shows how near a miss the rest are.
            </p>
          </div>
          <button
            onClick={onClose}
            aria-label="Close"
            className="rounded-lg p-2 transition-colors hover:bg-surface-hover"
          >
            <X className="h-5 w-5 text-content-muted" />
          </button>
        </div>

        <div className="flex min-h-0 flex-1">
          {/* Left: what has to be fitted */}
          <aside className="w-64 flex-shrink-0 overflow-y-auto border-r border-border bg-surface p-4">
            {requirement ? (
              <SubjectSummary requirement={requirement} onFilterChange={patchFilter} />
            ) : (
              <div className="text-sm text-content-muted">
                {loading ? 'Loading…' : error ?? 'No parameters'}
              </div>
            )}
          </aside>

          {/* Right: the slots and their candidates */}
          <section className="flex min-w-0 flex-1 flex-col">
            {/* Slots */}
            <div className="flex items-center gap-1 border-b border-border px-4 pt-2">
              {slots.map(k => {
                const chosen = selectedIn(k);
                return (
                  <button
                    key={k}
                    onClick={() => setSlot(k)}
                    className={`-mb-px border-b-2 px-3 py-2 text-sm transition-colors ${
                      slot === k
                        ? 'border-accent font-medium text-accent'
                        : 'border-transparent text-content-muted hover:text-content'
                    }`}
                  >
                    {SLOT_LABEL[k]}
                    <span className="ml-2 text-xs text-content-muted">
                      {chosen ? `#${chosen}` : 'none'}
                    </span>
                  </button>
                );
              })}
            </div>

            {/* Filters — always visible, so narrowing never needs a mode change */}
            <div className="flex flex-wrap items-center gap-2 border-b border-border px-4 py-2 text-xs">
              <select
                value={filter.camera}
                onChange={e => patchFilter({ camera: e.target.value })}
                className="rounded border border-border bg-surface px-2 py-1 text-content focus:border-accent focus:outline-none"
              >
                <option value="">Any camera</option>
                {cameras.map(c => (
                  <option key={c} value={c}>
                    {c}
                  </option>
                ))}
              </select>
              <select
                value={filter.exposure}
                onChange={e => patchFilter({ exposure: e.target.value })}
                className="rounded border border-border bg-surface px-2 py-1 text-content focus:border-accent focus:outline-none"
              >
                <option value="">Any exposure</option>
                {exposures.map(e => (
                  <option key={e} value={String(e)}>
                    {e} s
                  </option>
                ))}
              </select>
              <input
                type="date"
                aria-label="Shot from"
                value={filter.from}
                onChange={e => patchFilter({ from: e.target.value })}
                className="rounded border border-border bg-surface px-2 py-1 text-content focus:border-accent focus:outline-none"
              />
              <span className="text-content-muted">→</span>
              <input
                type="date"
                aria-label="Shot until"
                value={filter.to}
                onChange={e => patchFilter({ to: e.target.value })}
                className="rounded border border-border bg-surface px-2 py-1 text-content focus:border-accent focus:outline-none"
              />
              {filterIsActive(filter) && (
                <button
                  onClick={() => setFilter(EMPTY_FILTER)}
                  className="text-content-muted underline underline-offset-2 hover:text-content"
                >
                  Clear
                </button>
              )}

              <label className="ml-auto flex cursor-pointer items-center gap-2 text-content-muted">
                <input
                  type="checkbox"
                  checked={onlyCompatible}
                  onChange={e => setOnlyCompatible(e.target.checked)}
                  className="accent-accent"
                />
                Only sets that fit
                <span className="tabular-nums">({compatibleHere} of {totalHere})</span>
              </label>
            </div>

            {/* Candidates */}
            <div className="min-h-0 flex-1 overflow-y-auto p-4">
              {loading ? (
                <div className="flex items-center gap-2 py-10 text-sm text-content-muted">
                  <Loader2 className="h-4 w-4 animate-spin" /> Loading candidates…
                </div>
              ) : error ? (
                <div className="rounded-lg border border-error/30 bg-error/10 p-3">
                  <p className="text-sm font-medium text-error">Could not load candidates</p>
                  <p className="mt-1 text-xs text-content-muted">{error}</p>
                </div>
              ) : visible.length === 0 ? (
                <div className="py-10 text-center text-sm text-content-muted">
                  {filterIsActive(filter)
                    ? 'No sets match this filter. Clear it to see the rest.'
                    : onlyCompatible
                      ? 'No set here satisfies your matching rules. Turn off “Only sets that fit” to choose one by hand.'
                      : 'No calibration sets of this kind in the catalog yet.'}
                </div>
              ) : (
                <div className="space-y-2">
                  {visible.map(c => (
                    <CandidateCard
                      key={c.set.id ?? `${slot}-${c.set.date_start}`}
                      candidate={c}
                      selected={selectedIn(slot) === c.set.id}
                      isCurrent={current[slot] === c.set.id}
                      onSelect={() =>
                        setSelection(s => ({
                          ...s,
                          [slot]: selectedIn(slot) === c.set.id ? null : c.set.id,
                        }))
                      }
                    />
                  ))}
                </div>
              )}
            </div>
          </section>
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between border-t border-border bg-surface px-5 py-3">
          <div className="flex items-center gap-4">
            <button
              onClick={handleResetToAutomatic}
              disabled={resetting}
              title="Clear manual choices so Find Calibration can assign these again"
              className="flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-sm text-error transition-colors hover:bg-error-muted disabled:cursor-not-allowed disabled:opacity-50"
            >
              {resetting ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <RotateCcw className="h-3.5 w-3.5" />
              )}
              Reset to automatic
            </button>
            <span className="text-sm text-content-muted">
              {hasChanges ? (
                <span className="text-warning">Unsaved changes</span>
              ) : (
                'Pick a set for each kind you want to change'
              )}
            </span>
          </div>
          <div className="flex gap-2">
            <button
              onClick={onClose}
              className="rounded-lg bg-surface-hover px-4 py-2 text-sm text-content transition-colors hover:brightness-110"
            >
              Cancel
            </button>
            <button
              onClick={handleApply}
              disabled={!hasChanges || saving}
              className="flex items-center gap-2 rounded-lg bg-accent px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
            >
              {saving && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
              Apply
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
