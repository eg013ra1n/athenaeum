import { describe, expect, it } from 'vitest';
import {
  chooseOne,
  initialSelection,
  selectionProblems,
  toggleGroup,
  toggleItem,
  totals,
} from './planSelection';
import { plan } from './planFixtures';

const included = (sel: ReturnType<typeof initialSelection>, g: string) =>
  sel.groups.find((x) => x.groupId === g)!.included;

describe('planSelection', () => {
  it('initial_selection_follows_the_plan', () => {
    const p = plan();
    const sel = initialSelection(p);
    expect(sel.groups.map((g) => g.groupId)).toEqual(['group:1', 'group:2']);
    expect(sel.groups[0].chosen).toBe('item:1');
    expect(included(sel, 'group:1')).toEqual(['item:2', 'item:3']);
    expect(included(sel, 'group:2')).toEqual(['item:5']);
    expect(totals(p, sel)).toEqual({ items: 3, bytes: 7000 });
    expect(selectionProblems(p, sel)).toEqual([]);
  });

  it('unticking_a_group_updates_the_totals', () => {
    const p = plan();
    const sel = toggleGroup(p, initialSelection(p), 'group:2');
    expect(included(sel, 'group:2')).toEqual([]);
    expect(totals(p, sel)).toEqual({ items: 2, bytes: 3000 });
    // Ticking the group again takes every item that may be ticked, never the locked one or the kept copy.
    const back = toggleGroup(p, sel, 'group:2');
    expect(included(back, 'group:2')).toEqual(['item:5']);
  });

  it('locked_item_cannot_be_ticked', () => {
    const p = plan();
    const sel = initialSelection(p);
    expect(toggleItem(p, sel, 'group:2', 'item:6')).toEqual(sel);
    expect(chooseOne(p, sel, 'group:2', 'item:6')).toEqual(sel);
  });

  it('chosen_copy_is_never_included', () => {
    const p = plan();
    const sel = initialSelection(p);
    // The kept copy cannot be ticked.
    expect(toggleItem(p, sel, 'group:1', 'item:1')).toEqual(sel);
    // Picking a ticked copy as the one that stays unticks it; the old keeper is an ordinary copy.
    const moved = chooseOne(p, sel, 'group:1', 'item:2');
    expect(moved.groups[0].chosen).toBe('item:2');
    expect(included(moved, 'group:1')).toEqual(['item:3']);
    expect(toggleItem(p, moved, 'group:1', 'item:2')).toEqual(moved);
    const ticked = toggleItem(p, moved, 'group:1', 'item:1');
    expect(included(ticked, 'group:1')).toEqual(['item:3', 'item:1']);
  });

  it('problems_when_no_copy_stays', () => {
    const base = plan();
    const p = plan({
      groups: [{ ...base.groups[0], chosen: null, items: base.groups[0].items.map((i) => ({ ...i, selected: false })) }, base.groups[1]],
    });
    let sel = initialSelection(p);
    expect(selectionProblems(p, sel)).toEqual([]);
    sel = toggleItem(p, sel, 'group:1', 'item:2');
    expect(selectionProblems(p, sel)).toEqual(['pick the copy that stays in M31 Light 300 s (3 copies)']);
    sel = chooseOne(p, sel, 'group:1', 'item:1');
    expect(selectionProblems(p, sel)).toEqual([]);
  });

  it('problems_when_nothing_is_ticked', () => {
    const p = plan();
    let sel = initialSelection(p);
    sel = toggleGroup(p, toggleGroup(p, sel, 'group:1'), 'group:2');
    expect(selectionProblems(p, sel)).toEqual(['tick at least one file']);
  });
});
