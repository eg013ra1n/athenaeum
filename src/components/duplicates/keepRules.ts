import type { DuplicateFile, DuplicateGroup } from '../../types/models';
import type { Rule, RuleChain, RuleKind } from '../../types/helpers';

/**
 * Per-group deletion status for the header badge.
 *  - `none`    — nothing in the group is marked for deletion.
 *  - `partial` — some files marked, at least one will survive.
 *  - `all`     — every copy in the group is marked for deletion (warn).
 */
export type DeletionStatus = 'none' | 'partial' | 'all';

/** Static metadata for each known rule kind — drives display and the
 *  Add-rule menu. */
export interface RuleKindMeta {
  kind: RuleKind;
  name: string;
  description: string;
}

export const RULE_KIND_META: Record<RuleKind, RuleKindMeta> = {
  master_root: {
    kind: 'master_root',
    name: 'Master root',
    description:
      'Keep files inside the master scan root. Delete copies in any other root.',
  },
  path_contains: {
    kind: 'path_contains',
    name: 'Path contains',
    description:
      'Delete files whose path contains any of the listed substrings (e.g., "Backup", "_copy", "(1)").',
  },
  oldest_mtime: {
    kind: 'oldest_mtime',
    name: 'Oldest mtime wins',
    description:
      'Keep the file with the oldest modified time. Useful for catching re-imports.',
  },
  shortest_path: {
    kind: 'shortest_path',
    name: 'Shortest path wins',
    description:
      'Keep the file in the shallowest folder (fewest path segments); character length breaks ties. Final tiebreaker.',
  },
};

/** Build a fresh, default rule chain. The other rule kinds (oldest_mtime,
 *  shortest_path) can be added later from the panel's "Add rule" menu. */
export function defaultRuleChain(masterRootId: number | null = null): RuleChain {
  return [
    { kind: 'master_root', enabled: true, config: { rootId: masterRootId } },
    {
      kind: 'path_contains',
      enabled: true,
      config: { patterns: [], caseSensitive: false },
    },
  ];
}

/** Stable identifier for a duplicate group — prefers DB id, falls back to hash. */
export function groupKey(group: DuplicateGroup): string {
  if (group.id != null) return `id:${group.id}`;
  return `hash:${group.content_hash}:${group.size}`;
}

/** Fold both separator styles to '/'. User patterns like "Backup/2023" must
 *  match Windows catalog paths (`C:\...\Backup\2023\...`); an unmatched rule
 *  silently ABSTAINS and the chain falls through to a different rule — the
 *  user gets a different deletion set than configured. */
const normalizeSeparators = (s: string) => s.replace(/\\/g, '/');

/** Path depth in segments, separator-agnostic. */
const pathDepth = (p: string) => p.split(/[/\\]/).filter(Boolean).length;

// ── Per-rule evaluators ──────────────────────────────────────────────────────
//
// Each evaluator returns the set of file IDs the rule would mark for deletion
// in this group. An empty set means "abstain" (no opinion). The chain evaluator
// also treats a "delete every file" result as abstain for safety — see
// evaluateChainForGroup below.

function evalMasterRoot(
  files: DuplicateFile[],
  rule: Extract<Rule, { kind: 'master_root' }>,
): Set<number> {
  const rootId = rule.config.rootId;
  if (rootId == null) return new Set();
  const hasMasterFile = files.some((f) => f.scanRootId === rootId);
  if (!hasMasterFile) return new Set();
  return new Set(
    files.filter((f) => f.scanRootId !== rootId).map((f) => f.fileId),
  );
}

function evalPathContains(
  files: DuplicateFile[],
  rule: Extract<Rule, { kind: 'path_contains' }>,
): Set<number> {
  const cleaned = rule.config.patterns
    .map((p) => p.trim())
    .filter((p) => p.length > 0);
  if (cleaned.length === 0) return new Set();

  const caseSensitive = rule.config.caseSensitive;
  const normalizedPatterns = cleaned
    .map(normalizeSeparators)
    .map((p) => (caseSensitive ? p : p.toLowerCase()));

  const out = new Set<number>();
  for (const f of files) {
    const folded = normalizeSeparators(f.path);
    const haystack = caseSensitive ? folded : folded.toLowerCase();
    if (normalizedPatterns.some((p) => haystack.includes(p))) {
      out.add(f.fileId);
    }
  }
  return out;
}

function evalOldestMtime(files: DuplicateFile[]): Set<number> {
  // Compare via Date.parse so any ISO 8601 variation (with `Z`, `+00:00`, etc.)
  // sorts correctly. If parsing fails for every file, abstain.
  const times = files.map((f) => Date.parse(f.modifiedAt));
  const valid = times.filter((t) => Number.isFinite(t));
  if (valid.length === 0) return new Set();

  const min = Math.min(...valid);
  const out = new Set<number>();
  for (let i = 0; i < files.length; i++) {
    const t = times[i];
    if (Number.isFinite(t) && t > min) out.add(files[i].fileId);
  }
  return out;
}

function evalShortestPath(files: DuplicateFile[]): Set<number> {
  // Segment count first, character length only as a tie-break. Ranking by
  // character length alone made a UNC copy (`\\nas\astro\a.fits`) "longer" than
  // a drive-letter copy of equal depth, so the network copy was systematically
  // marked for deletion. Files tied with the best (depth, length) are all kept —
  // only strictly-worse ones enter the deletion set.
  let bestDepth = Number.POSITIVE_INFINITY;
  let bestLen = Number.POSITIVE_INFINITY;
  for (const f of files) {
    const d = pathDepth(f.path);
    if (d < bestDepth || (d === bestDepth && f.path.length < bestLen)) {
      bestDepth = d;
      bestLen = f.path.length;
    }
  }
  const out = new Set<number>();
  for (const f of files) {
    const d = pathDepth(f.path);
    if (d > bestDepth || (d === bestDepth && f.path.length > bestLen)) {
      out.add(f.fileId);
    }
  }
  return out;
}

/** Run a single rule against a group's files. Used by the chain evaluator. */
export function evaluateRule(files: DuplicateFile[], rule: Rule): Set<number> {
  switch (rule.kind) {
    case 'master_root':
      return evalMasterRoot(files, rule);
    case 'path_contains':
      return evalPathContains(files, rule);
    case 'oldest_mtime':
      return evalOldestMtime(files);
    case 'shortest_path':
      return evalShortestPath(files);
  }
}

/** Walk the chain, returning the deletion set produced by the first non-
 *  abstaining rule together with that rule's kind. A rule that would mark every
 *  file in the group is treated as abstaining (safety). */
export function evaluateChainForGroup(
  files: DuplicateFile[],
  chain: RuleChain,
): { deletes: Set<number>; firedRule: Rule | null } {
  for (const rule of chain) {
    if (!rule.enabled) continue;
    const deletes = evaluateRule(files, rule);
    if (deletes.size === 0) continue;
    if (deletes.size >= files.length) continue; // would wipe the group
    return { deletes, firedRule: rule };
  }
  return { deletes: new Set(), firedRule: null };
}

export function groupDeletionStatus(
  group: DuplicateGroup,
  deleteSet: Set<number>,
): DeletionStatus {
  if (deleteSet.size === 0) return 'none';
  if (deleteSet.size >= group.files.length) return 'all';
  return 'partial';
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

export interface AutoDeletesResult {
  /** group key → set of file IDs auto-marked for deletion */
  deletes: Map<string, Set<number>>;
  /** group key → kind of the rule that produced the marks (absent for groups
   *  that fell through the whole chain) */
  firedRules: Map<string, RuleKind>;
}

/** Rebuild the auto-deletes map for every group from the current chain. */
export function buildAutoDeletes(
  groups: DuplicateGroup[],
  chain: RuleChain,
): AutoDeletesResult {
  const deletes = new Map<string, Set<number>>();
  const firedRules = new Map<string, RuleKind>();
  for (const group of groups) {
    const key = groupKey(group);
    const result = evaluateChainForGroup(group.files, chain);
    deletes.set(key, result.deletes);
    if (result.firedRule) firedRules.set(key, result.firedRule.kind);
  }
  return { deletes, firedRules };
}

// ── Chain (de)serialization & sanitisation ───────────────────────────────────
//
// Stored under setting key `duplicates.rule_chain` as a JSON string. We sanitise
// on load so a malformed value (older build, hand-edited DB, etc.) doesn't
// crash the view — we just rebuild the default chain.

const KNOWN_KINDS: RuleKind[] = [
  'master_root',
  'path_contains',
  'oldest_mtime',
  'shortest_path',
];

function sanitiseRule(raw: unknown): Rule | null {
  if (!raw || typeof raw !== 'object') return null;
  const r = raw as Record<string, unknown>;
  const kind = r.kind;
  if (typeof kind !== 'string' || !KNOWN_KINDS.includes(kind as RuleKind)) {
    return null;
  }
  const enabled = typeof r.enabled === 'boolean' ? r.enabled : true;
  const cfg = (r.config && typeof r.config === 'object'
    ? (r.config as Record<string, unknown>)
    : {}) as Record<string, unknown>;

  switch (kind as RuleKind) {
    case 'master_root': {
      const rootIdRaw = cfg.rootId;
      const rootId =
        typeof rootIdRaw === 'number' && Number.isFinite(rootIdRaw)
          ? rootIdRaw
          : null;
      return { kind: 'master_root', enabled, config: { rootId } };
    }
    case 'path_contains': {
      const patternsRaw = cfg.patterns;
      const patterns = Array.isArray(patternsRaw)
        ? patternsRaw.filter((p): p is string => typeof p === 'string')
        : [];
      const caseSensitive =
        typeof cfg.caseSensitive === 'boolean' ? cfg.caseSensitive : false;
      return {
        kind: 'path_contains',
        enabled,
        config: { patterns, caseSensitive },
      };
    }
    case 'oldest_mtime':
      return { kind: 'oldest_mtime', enabled, config: {} };
    case 'shortest_path':
      return { kind: 'shortest_path', enabled, config: {} };
  }
}

/** Parse a chain JSON string. Returns null if the value is missing or malformed
 *  — the caller should fall back to `defaultRuleChain` in that case. */
export function parseRuleChain(raw: string): RuleChain | null {
  if (!raw) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!Array.isArray(parsed)) return null;
  const out: RuleChain = [];
  const seen = new Set<RuleKind>();
  for (const item of parsed) {
    const rule = sanitiseRule(item);
    if (!rule) continue;
    if (seen.has(rule.kind)) continue; // at most one of each kind
    seen.add(rule.kind);
    out.push(rule);
  }
  return out.length > 0 ? out : null;
}

export function serializeRuleChain(chain: RuleChain): string {
  return JSON.stringify(chain);
}
