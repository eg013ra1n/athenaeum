// Settings redesign (spec 2026-09-18 §3/§10): a module-level record of which
// registry section ids are currently mounted as a real `SettingsSection`.
// The registry-coverage test (Task C1) renders every tab and asserts this
// set equals every id in `SETTINGS_SECTIONS` exactly — a section that exists
// in the registry but is never actually rendered (or vice versa) fails it.
//
// A `Map<id, count>` rather than a bare `Set` so React 18 StrictMode's
// double-mount (mount → cleanup → mount, in dev only) can never leave a
// section incorrectly absent: if the second mount's `register` ran before
// the first mount's cleanup `unregister` (or the reverse), a plain
// add/delete pair could drop the count to zero while the section is still
// on screen. Refcounting keeps the final state correct regardless of that
// interleaving — a section is "rendered" for as long as at least one
// mounted `SettingsSection` holds its id.
const counts = new Map<string, number>();

export function registerRenderedSection(id: string): void {
  counts.set(id, (counts.get(id) ?? 0) + 1);
}

export function unregisterRenderedSection(id: string): void {
  const next = (counts.get(id) ?? 0) - 1;
  if (next <= 0) {
    counts.delete(id);
  } else {
    counts.set(id, next);
  }
}

/** A fresh snapshot — the registry-coverage test reads this after rendering. */
export function renderedSectionIds(): Set<string> {
  return new Set(counts.keys());
}
