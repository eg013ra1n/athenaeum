// The plan window's selection rules (docs/mcp/README.md, "Plans"). Pure functions over the plan and the user's
// ticks; the window keeps the `PlanSelection` and sends it back as the `apply` answer.
import type { McpPlanView, PlanItem, PlanSelection } from '../../types/mcp';

/** An item the user may tick: not locked and not the copy that stays (the one now chosen, not the plan's). */
function ticks(chosen: string | null, item: PlanItem): boolean {
  return item.locked === null && item.id !== chosen;
}

const chosenOf = (sel: PlanSelection, groupId: string): string | null =>
  sel.groups.find((g) => g.groupId === groupId)?.chosen ?? null;

function mapGroup(sel: PlanSelection, groupId: string, f: (included: string[], chosen: string | null) => { included: string[]; chosen: string | null }): PlanSelection {
  return {
    groups: sel.groups.map((g) => (g.groupId === groupId ? { groupId, ...f(g.included, g.chosen) } : g)),
  };
}

/** What the plan proposes: its `selected` items (never a locked one or the kept copy) and its `chosen` copies. */
export function initialSelection(plan: McpPlanView): PlanSelection {
  return {
    groups: plan.groups.map((g) => ({
      groupId: g.id,
      chosen: g.chosen,
      included: g.items.filter((i) => i.selected && ticks(g.chosen, i)).map((i) => i.id),
    })),
  };
}

export function toggleItem(plan: McpPlanView, sel: PlanSelection, groupId: string, itemId: string): PlanSelection {
  const group = plan.groups.find((g) => g.id === groupId);
  const item = group?.items.find((i) => i.id === itemId);
  if (!group || !item || !ticks(chosenOf(sel, groupId), item)) return sel;
  return mapGroup(sel, groupId, (included, chosen) => ({
    chosen,
    included: included.includes(itemId) ? included.filter((id) => id !== itemId) : [...included, itemId],
  }));
}

/** A group with any copy ticked is unticked; a group with none ticked gets every copy that may be ticked. */
export function toggleGroup(plan: McpPlanView, sel: PlanSelection, groupId: string): PlanSelection {
  const group = plan.groups.find((g) => g.id === groupId);
  if (!group) return sel;
  const ids = group.items.filter((i) => ticks(chosenOf(sel, groupId), i)).map((i) => i.id);
  return mapGroup(sel, groupId, (included, chosen) => {
    return { chosen, included: included.some((id) => ids.includes(id)) ? [] : ids };
  });
}

/** The user picks the copy that stays; a copy that was ticked is unticked. A lock forbids moving a copy, not keeping it; only a non-candidate cannot be picked. */
export function chooseOne(plan: McpPlanView, sel: PlanSelection, groupId: string, itemId: string): PlanSelection {
  const group = plan.groups.find((g) => g.id === groupId);
  const item = group?.items.find((i) => i.id === itemId);
  if (!group || !item || group.chooseOne === null || !item.candidate) return sel;
  return mapGroup(sel, groupId, (included) => ({ chosen: itemId, included: included.filter((id) => id !== itemId) }));
}

export function totals(plan: McpPlanView, sel: PlanSelection): { items: number; bytes: number } {
  let items = 0;
  let bytes = 0;
  for (const g of sel.groups) {
    const group = plan.groups.find((x) => x.id === g.groupId);
    if (!group) continue;
    for (const item of group.items) {
      if (!g.included.includes(item.id)) continue;
      items += 1;
      bytes += item.sizeBytes ?? 0;
    }
  }
  return { items, bytes };
}

/** Why Apply is not possible yet, in words; empty when it is. */
export function selectionProblems(plan: McpPlanView, sel: PlanSelection): string[] {
  const problems: string[] = [];
  let any = false;
  for (const g of sel.groups) {
    const group = plan.groups.find((x) => x.id === g.groupId);
    if (!group || g.included.length === 0) continue;
    any = true;
    if (group.chooseOne !== null && g.chosen === null) problems.push(`pick the copy that stays in ${group.title}`);
  }
  if (!any) problems.push(plan.kind === 'black_hole' ? 'tick at least one file' : 'tick at least one frame');
  return problems;
}

/** The items the user may tick now: not locked and not the copy that stays. The footer's "of M". */
export function offered(plan: McpPlanView, sel: PlanSelection): number {
  return plan.groups.reduce((n, g) => n + g.items.filter((i) => ticks(chosenOf(sel, g.id), i)).length, 0);
}

/** The table's "select all": every tickable item ticked → none; otherwise every tickable item (a half-ticked plan fills up). */
export function toggleAll(plan: McpPlanView, sel: PlanSelection): PlanSelection {
  const ids = (g: McpPlanView['groups'][number]) => g.items.filter((i) => ticks(chosenOf(sel, g.id), i)).map((i) => i.id);
  const everyTicked = sel.groups.every((s) => {
    const group = plan.groups.find((g) => g.id === s.groupId);
    return !group || ids(group).every((id) => s.included.includes(id));
  });
  return {
    groups: sel.groups.map((s) => {
      const group = plan.groups.find((g) => g.id === s.groupId);
      return group ? { ...s, included: everyTicked ? [] : ids(group) } : s;
    }),
  };
}
