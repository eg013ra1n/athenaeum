// Light-group selection in the Calibration coverage table: plain click picks
// one group (clicking the only selected group clears it), Cmd/Ctrl-click
// toggles one, Shift-click adds or removes the range from the anchor — the
// same rule as the Re-assign panel's frame list.

export interface SelectionModifiers {
  shift: boolean;
  /** Cmd on macOS, Ctrl elsewhere. */
  toggle: boolean;
}

/** The selection after a click on `clicked`. `orderedKeys` is the table's
 *  displayed order; `anchor` is the last group clicked without Shift. */
export function nextLightSelection(
  prev: ReadonlySet<string>,
  orderedKeys: readonly string[],
  clicked: string,
  anchor: string | null,
  mods: SelectionModifiers,
): Set<string> {
  if (mods.shift && anchor !== null) {
    const from = orderedKeys.indexOf(anchor);
    const to = orderedKeys.indexOf(clicked);
    if (from !== -1 && to !== -1) {
      const next = new Set(prev);
      const adding = !prev.has(clicked);
      for (let i = Math.min(from, to); i <= Math.max(from, to); i++) {
        if (adding) next.add(orderedKeys[i]);
        else next.delete(orderedKeys[i]);
      }
      return next;
    }
  }
  if (mods.toggle || mods.shift) {
    const next = new Set(prev);
    if (next.has(clicked)) next.delete(clicked);
    else next.add(clicked);
    return next;
  }
  if (prev.size === 1 && prev.has(clicked)) return new Set();
  return new Set([clicked]);
}
