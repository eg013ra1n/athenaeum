import type { DuplicateFile, DuplicateGroup } from '../../types/models';

/**
 * Per-group deletion status for the header badge.
 *  - `none`    — nothing in the group is marked for deletion.
 *  - `partial` — some files marked, at least one will survive.
 *  - `all`     — every copy in the group is marked for deletion (warn).
 */
export type DeletionStatus = 'none' | 'partial' | 'all';

/**
 * Master-root rule, expressed as "what to delete":
 *  - `masterRootId == null` → no auto-deletes; user marks everything manually.
 *  - At least one file lives in the master root → auto-check every file
 *    **not** in the master root.
 *  - No file lives in the master root → no auto-deletes (user decides).
 *
 * This is intentionally conservative: we never auto-schedule every copy
 * for deletion. If the rule can't find a keeper, it leaves the group alone.
 */
export function autoDeletesForGroup(
  files: DuplicateFile[],
  masterRootId: number | null,
): Set<number> {
  if (masterRootId == null) return new Set();
  const hasMasterFile = files.some((f) => f.scanRootId === masterRootId);
  if (!hasMasterFile) return new Set();
  return new Set(
    files.filter((f) => f.scanRootId !== masterRootId).map((f) => f.fileId),
  );
}

export function groupDeletionStatus(
  group: DuplicateGroup,
  deleteSet: Set<number>,
): DeletionStatus {
  if (deleteSet.size === 0) return 'none';
  if (deleteSet.size >= group.files.length) return 'all';
  return 'partial';
}

/** Stable identifier for a duplicate group — prefers DB id, falls back to hash. */
export function groupKey(group: DuplicateGroup): string {
  if (group.id != null) return `id:${group.id}`;
  return `hash:${group.content_hash}:${group.size}`;
}

export interface DeletionPlan {
  deleteIds: number[];
  groupsWithDeletions: number;
  /** Groups where every copy is marked for deletion. Surfaced to the user
   *  as a warning — usually unintended. */
  groupsWithAllDeleted: number;
  bytesToFree: number;
}

export function computePlan(
  groups: DuplicateGroup[],
  deletesByGroup: Map<string, Set<number>>,
): DeletionPlan {
  const deleteIds: number[] = [];
  let groupsWithDeletions = 0;
  let groupsWithAllDeleted = 0;
  let bytesToFree = 0;
  for (const group of groups) {
    const set = deletesByGroup.get(groupKey(group));
    if (!set || set.size === 0) continue;
    groupsWithDeletions += 1;
    if (set.size >= group.files.length) groupsWithAllDeleted += 1;
    for (const file of group.files) {
      if (set.has(file.fileId)) {
        deleteIds.push(file.fileId);
        bytesToFree += group.size;
      }
    }
  }
  return { deleteIds, groupsWithDeletions, groupsWithAllDeleted, bytesToFree };
}

/** Rebuild the auto-deletes map for every group from the current rule. */
export function buildAutoDeletes(
  groups: DuplicateGroup[],
  masterRootId: number | null,
): Map<string, Set<number>> {
  const out = new Map<string, Set<number>>();
  for (const group of groups) {
    out.set(groupKey(group), autoDeletesForGroup(group.files, masterRootId));
  }
  return out;
}
