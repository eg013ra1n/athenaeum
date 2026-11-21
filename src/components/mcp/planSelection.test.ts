import { describe, expect, it } from 'vitest';
import {
  chooseOne,
  initialSelection,
  selectionProblems,
  toggleAll,
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
    // A group with only some copies ticked is unticked by one click (it shows half-ticked).
    const partly = toggleItem(p, initialSelection(p), 'group:1', 'item:3');
    expect(included(partly, 'group:1')).toEqual(['item:2']);
    expect(included(toggleGroup(p, partly, 'group:1'), 'group:1')).toEqual([]);
  });

  it('locked_item_cannot_be_ticked', () => {
    const p = plan();
    const sel = initialSelection(p);
    expect(toggleItem(p, sel, 'group:2', 'item:6')).toEqual(sel);
    // Not even once it is the copy that stays: a lock forbids moving a copy, and the kept copy is never moved.
    const kept = chooseOne(p, sel, 'group:2', 'item:6');
    expect(toggleItem(p, kept, 'group:2', 'item:6')).toEqual(kept);
    expect(toggleGroup(p, kept, 'group:2').groups[1].included).toEqual([]);
    expect(toggleGroup(p, toggleGroup(p, kept, 'group:2'), 'group:2').groups[1].included).toEqual(['item:4', 'item:5']);
  });

  it('a_locked_copy_can_be_the_one_that_stays', () => {
    const p = plan();
    const sel = initialSelection(p);
    const kept = chooseOne(p, sel, 'group:2', 'item:6');
    expect(kept.groups[1].chosen).toBe('item:6');
    expect(kept.groups[1].included).toEqual(['item:5']);
    // A misclick back to the planned (locked or not) keeper works too.
    const back = chooseOne(p, kept, 'group:2', 'item:4');
    expect(back.groups[1].chosen).toBe('item:4');
    // A copy that is not a candidate can never be the one that stays.
    const base = plan();
    const q = plan({
      groups: [base.groups[0], { ...base.groups[1], items: base.groups[1].items.map((i) => (i.id === 'item:5' ? { ...i, candidate: false } : i)) }],
    });
    expect(chooseOne(q, sel, 'group:2', 'item:5')).toEqual(sel);
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

  it('problems_for_a_metadata_or_plate_solve_plan_name_frames', () => {
    for (const kind of ['metadata', 'plate_solve'] as const) {
      const p = plan({ kind });
      const sel = toggleGroup(p, toggleGroup(p, initialSelection(p), 'group:1'), 'group:2');
      expect(selectionProblems(p, sel)).toEqual(['tick at least one frame']);
    }
  });

  it('select_all_ticks_every_tickable_item_or_clears_them_all', () => {
    const p = plan();
    const sel = initialSelection(p);
    // Some are ticked, not all: one click ticks every item that may be ticked (never the kept copy or a locked one).
    const partly = toggleItem(p, sel, 'group:1', 'item:3');
    const full = toggleAll(p, partly);
    expect(included(full, 'group:1')).toEqual(['item:2', 'item:3']);
    expect(included(full, 'group:2')).toEqual(['item:5']);
    // Everything ticked: one click clears.
    const none = toggleAll(p, full);
    expect(included(none, 'group:1')).toEqual([]);
    expect(included(none, 'group:2')).toEqual([]);
    expect(none.groups.map((g) => g.chosen)).toEqual(['item:1', 'item:4']);
  });
});
