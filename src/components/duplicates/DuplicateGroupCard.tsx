import React from 'react';
import { AlertTriangle, CheckCircle2, ChevronDown, ChevronRight, Circle, Loader2, Sparkles, Trash2 } from 'lucide-react';
import type { DuplicateFile, DuplicateGroup, RuleKind } from '../../types/models';
import type { FileVerifyResult, GroupVerifyStatus } from '../../hooks/useDeepVerify';
import { groupDeletionStatus, RULE_KIND_META, type DeletionStatus } from './keepRules';

interface DuplicateGroupCardProps {
  group: DuplicateGroup;
  deletes: Set<number>;
  masterRootId: number | null;
  /** Kind of the rule that produced the auto-marks for this group, if any. */
  firedRuleKind?: RuleKind;
  /** Active path-contains patterns — used to highlight matched substrings
   *  inside file paths when path_contains is the rule that fired. */
  pathContainsPatterns?: string[];
  pathContainsCaseSensitive?: boolean;
  isExpanded: boolean;
  onToggleExpanded: () => void;
  /** Flip the deletion flag for a single file in this group. */
  onToggleDelete: (fileId: number) => void;
  /** Aggregate verification status for this group (undefined = not verified yet). */
  verifyStatus?: GroupVerifyStatus;
  /** Per-file results, for inline status indicators when expanded. */
  fileVerifyResults?: Map<number, FileVerifyResult>;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const mb = bytes / 1024 / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  return `${(mb / 1024).toFixed(2)} GB`;
}

function formatDate(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function formatDateShort(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

function splitPath(fullPath: string, scanRootPath: string | null): [string, string] {
  if (!scanRootPath) return ['', fullPath];
  if (fullPath.startsWith(scanRootPath)) {
    return [scanRootPath, fullPath.slice(scanRootPath.length)];
  }
  return ['', fullPath];
}

/** Render a string with one or more substrings underlined. The match positions
 *  are computed against the haystack's lowercase form when `caseSensitive` is
 *  false, but the original-cased text is displayed verbatim. Patterns are
 *  trimmed and empty patterns are skipped by the caller. Matches are merged
 *  when they overlap so the underline is contiguous. */
function highlightSubstrings(
  text: string,
  patterns: string[],
  caseSensitive: boolean,
): React.ReactNode {
  if (patterns.length === 0) return text;
  const haystack = caseSensitive ? text : text.toLowerCase();

  // Collect all match ranges across all patterns.
  const ranges: Array<[number, number]> = [];
  for (const raw of patterns) {
    const needle = caseSensitive ? raw : raw.toLowerCase();
    if (needle.length === 0) continue;
    let from = 0;
    while (true) {
      const at = haystack.indexOf(needle, from);
      if (at === -1) break;
      ranges.push([at, at + needle.length]);
      from = at + needle.length;
    }
  }
  if (ranges.length === 0) return text;

  // Merge overlapping ranges.
  ranges.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  const merged: Array<[number, number]> = [];
  for (const r of ranges) {
    const last = merged[merged.length - 1];
    if (last && r[0] <= last[1]) {
      last[1] = Math.max(last[1], r[1]);
    } else {
      merged.push([r[0], r[1]]);
    }
  }

  const out: React.ReactNode[] = [];
  let cursor = 0;
  for (let i = 0; i < merged.length; i++) {
    const [start, end] = merged[i];
    if (start > cursor) out.push(text.slice(cursor, start));
    out.push(
      <span
        key={`m${i}`}
        className="text-warning underline decoration-warning decoration-1 underline-offset-2"
      >
        {text.slice(start, end)}
      </span>,
    );
    cursor = end;
  }
  if (cursor < text.length) out.push(text.slice(cursor));
  return out;
}

function summarize<T>(values: Array<T | null | undefined>): T | 'mixed' | null {
  const defined = values.filter((v): v is T => v != null);
  if (defined.length === 0) return null;
  const first = defined[0];
  return defined.every((v) => v === first) ? first : 'mixed';
}

const IMAGETYP_STYLES: Record<string, string> = {
  LIGHT: 'bg-accent/15 text-accent border-accent/40',
  FLAT: 'bg-warning-muted text-warning border-warning/50',
  DARK: 'bg-surface-hover text-content-secondary border-border',
  BIAS: 'bg-info-muted text-info border-info/50',
  DARKFLAT: 'bg-surface-hover text-content-secondary border-border',
};

function imagetypClass(value: string): string {
  const key = value.toUpperCase();
  return IMAGETYP_STYLES[key] ?? 'bg-surface-hover text-content-secondary border-border';
}

function headerSummary(files: DuplicateFile[]): {
  filename: string | null;
  imagetyp: string | 'mixed' | null;
  dateObs: string | 'mixed' | null;
} {
  const filename = summarize(files.map((f) => f.filename));
  const imagetyp = summarize(files.map((f) => f.imagetyp));
  const dateObs = summarize(files.map((f) => f.dateObs));
  return {
    filename: filename === 'mixed' ? files[0]?.filename ?? null : filename,
    imagetyp,
    dateObs,
  };
}

function statusBadge(status: DeletionStatus, count: number, total: number): { text: string; className: string; Icon: React.ElementType } {
  switch (status) {
    case 'all':
      return {
        text: `All ${total} copies will be deleted`,
        className: 'text-warning bg-warning-muted border-warning/50',
        Icon: AlertTriangle,
      };
    case 'partial':
      return {
        text: `Delete ${count} of ${total}`,
        className: 'text-error bg-error-muted border-error/50',
        Icon: Trash2,
      };
    case 'none':
    default:
      return {
        text: 'Nothing marked',
        className: 'text-content-muted bg-surface-hover border-border',
        Icon: Circle,
      };
  }
}

/** Visual representation of the group-level verification status. */
function verifyBadgeProps(status: GroupVerifyStatus): {
  text: string;
  className: string;
  Icon: React.ElementType;
  iconClassName?: string;
} | null {
  switch (status) {
    case 'unverified':
      return null; // no badge — not yet run
    case 'running':
      return {
        text: 'Verifying…',
        className: 'text-info bg-info-muted border-info/40',
        Icon: Loader2,
        iconClassName: 'animate-spin',
      };
    case 'verified':
      return {
        text: 'Verified',
        className: 'text-success bg-surface-hover border-success/40',
        Icon: CheckCircle2,
      };
    case 'error':
    case 'partial':
      return {
        text: 'Verify error',
        className: 'text-warning bg-warning-muted border-warning/50',
        Icon: AlertTriangle,
      };
    default:
      return null;
  }
}

export const DuplicateGroupCard: React.FC<DuplicateGroupCardProps> = ({
  group,
  deletes,
  masterRootId,
  firedRuleKind,
  pathContainsPatterns,
  pathContainsCaseSensitive,
  isExpanded,
  onToggleExpanded,
  onToggleDelete,
  verifyStatus,
  fileVerifyResults,
}) => {
  const wasted = group.size * (group.file_count - 1);
  const summary = headerSummary(group.files);
  const status = groupDeletionStatus(group, deletes);
  const badge = statusBadge(status, deletes.size, group.files.length);
  const BadgeIcon = badge.Icon;

  const vBadge = verifyStatus ? verifyBadgeProps(verifyStatus) : null;

  // Only highlight matched substrings when path_contains was the rule that
  // actually fired for this group — otherwise the highlight would be
  // misleading (the rule didn't drive the marks).
  const showPathHighlight =
    firedRuleKind === 'path_contains' &&
    !!pathContainsPatterns &&
    pathContainsPatterns.length > 0;
  const firedRuleName = firedRuleKind ? RULE_KIND_META[firedRuleKind].name : null;

  return (
    <div className={isExpanded ? 'bg-surface-hover/40' : ''}>
      {/* Header — always visible, toggles expansion. */}
      <button
        onClick={onToggleExpanded}
        className="w-full flex items-center gap-2 px-3 py-2 text-left hover:bg-surface-hover transition-colors"
      >
        {isExpanded ? <ChevronDown size={16} className="flex-shrink-0" /> : <ChevronRight size={16} className="flex-shrink-0" />}

        {summary.imagetyp && summary.imagetyp !== 'mixed' ? (
          <span className={`inline-flex items-center h-5 px-1.5 rounded text-[10px] font-semibold uppercase tracking-wide border flex-shrink-0 ${imagetypClass(summary.imagetyp)}`}>
            {summary.imagetyp}
          </span>
        ) : summary.imagetyp === 'mixed' ? (
          <span className="inline-flex items-center h-5 px-1.5 rounded text-[10px] font-semibold uppercase tracking-wide border bg-warning-muted text-warning border-warning/50 flex-shrink-0">
            Mixed
          </span>
        ) : null}

        {summary.filename && (
          <span className="font-mono text-xs text-content truncate min-w-0" title={summary.filename}>
            {summary.filename}
          </span>
        )}

        {summary.dateObs && summary.dateObs !== 'mixed' && (
          <span className="text-[11px] text-content-muted font-mono flex-shrink-0">
            {formatDateShort(summary.dateObs)}
          </span>
        )}

        <span className="text-border flex-shrink-0">·</span>
        <span className="text-xs font-semibold flex-shrink-0">{group.file_count}</span>
        <span className="text-[11px] text-content-muted flex-shrink-0">×</span>
        <span className="text-[11px] text-content-muted flex-shrink-0">{formatBytes(group.size)}</span>
        <span className="text-border flex-shrink-0">·</span>
        <span className="text-[11px] text-error/80 flex-shrink-0">{formatBytes(wasted)} wasted</span>

        <div className="ml-auto flex items-center gap-2 flex-shrink-0">
          {firedRuleName && deletes.size > 0 && (
            <span
              className="inline-flex items-center gap-1 text-[10px] text-content-muted"
              title="Auto-marked for deletion by this rule. Reapply rules from the toolbar to undo manual edits."
            >
              <Sparkles size={10} className="text-accent" />
              <span>{firedRuleName}</span>
            </span>
          )}

          {/* Deletion status — hide the noisy "Nothing marked" pill */}
          {status !== 'none' && (
            <span className={`inline-flex items-center gap-1 h-6 px-2 rounded-full text-[11px] border ${badge.className}`}>
              <BadgeIcon size={12} />
              {badge.text}
            </span>
          )}

          {/* Verification status badge — only shown after a verify pass */}
          {vBadge && (
            <span className={`inline-flex items-center gap-1 h-6 px-2 rounded-full text-[11px] border ${vBadge.className}`}>
              <vBadge.Icon size={12} className={vBadge.iconClassName} />
              {vBadge.text}
            </span>
          )}

          <span className="font-mono text-[10px] text-content-muted/60">
            {group.content_hash.substring(0, 8)}
          </span>
        </div>
      </button>

      {/* Expanded body — checkbox per file. Checked = delete. */}
      {isExpanded && (
        <div className="px-3 pb-3 pt-1 space-y-1">
          {group.files.map((file) => {
            const willDelete = deletes.has(file.fileId);
            const showLabel = deletes.size > 0;
            const inMaster = masterRootId != null && file.scanRootId === masterRootId;
            const [prefix, remainder] = splitPath(file.path, file.scanRootPath);

            // Per-file verification indicator — shown for all files except the
            // anchor (files[0]).  Mismatch files are filtered before this
            // renders, so mismatch status will never appear here.
            const fileResult = fileVerifyResults?.get(file.fileId);

            return (
              <label
                key={file.fileId}
                className={`flex items-center gap-2 px-2 py-1.5 rounded cursor-pointer transition-colors border ${
                  willDelete
                    ? 'bg-error-muted/40 border-error/40'
                    : 'hover:bg-surface-hover border-transparent'
                }`}
              >
                <input
                  type="checkbox"
                  checked={willDelete}
                  onChange={() => onToggleDelete(file.fileId)}
                  className="rounded border-border text-error focus:ring-error cursor-pointer"
                />
                {showLabel && (
                  <span
                    className={`text-xs font-medium w-12 flex-shrink-0 inline-flex items-center ${willDelete ? 'text-error' : 'text-success'}`}
                    title={willDelete ? 'Will move to Black Hole' : 'Will be kept'}
                  >
                    {willDelete ? <Trash2 size={13} /> : 'Keep'}
                  </span>
                )}
                <span
                  className={`flex-1 min-w-0 font-mono text-xs truncate ${willDelete ? 'line-through text-content-muted' : ''}`}
                  title={file.path}
                  style={{ direction: 'rtl', textAlign: 'left' }}
                >
                  <bdi>
                    {prefix && (
                      <span className={inMaster ? 'text-accent' : 'text-content-muted'}>
                        {showPathHighlight
                          ? highlightSubstrings(
                              prefix,
                              pathContainsPatterns!,
                              pathContainsCaseSensitive ?? false,
                            )
                          : prefix}
                      </span>
                    )}
                    <span className={willDelete ? '' : 'text-content'}>
                      {showPathHighlight
                        ? highlightSubstrings(
                            remainder,
                            pathContainsPatterns!,
                            pathContainsCaseSensitive ?? false,
                          )
                        : remainder}
                    </span>
                  </bdi>
                </span>
                <span className="text-[11px] text-content-muted flex-shrink-0">
                  {formatDate(file.modifiedAt)}
                </span>

                {/* Per-file verification indicator.
                    Mismatch files are filtered out before this component renders,
                    so only verified / running / error / pending statuses appear. */}
                {fileResult && (
                  <span
                    className={`flex-shrink-0 ${
                      fileResult.status === 'verified'
                        ? 'text-success'
                        : fileResult.status === 'running'
                        ? 'text-info'
                        : fileResult.status === 'error'
                        ? 'text-warning'
                        : 'text-content-muted'
                    }`}
                    title={
                      fileResult.status === 'error'
                        ? `Verification error: ${fileResult.errorMessage ?? 'unknown'}`
                        : fileResult.status === 'verified'
                        ? 'Byte-identical to the group anchor'
                        : fileResult.status === 'running'
                        ? 'Verifying…'
                        : undefined
                    }
                  >
                    {fileResult.status === 'verified' && <CheckCircle2 size={13} />}
                    {fileResult.status === 'running' && <Loader2 size={13} className="animate-spin" />}
                    {fileResult.status === 'error' && <AlertTriangle size={13} />}
                  </span>
                )}
              </label>
            );
          })}
        </div>
      )}
    </div>
  );
};
