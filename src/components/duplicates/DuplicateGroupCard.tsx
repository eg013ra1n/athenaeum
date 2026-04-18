import React from 'react';
import { ChevronDown, ChevronRight, Trash2, AlertTriangle, Circle } from 'lucide-react';
import type { DuplicateFile, DuplicateGroup } from '../../types/models';
import { groupDeletionStatus, type DeletionStatus } from './keepRules';

interface DuplicateGroupCardProps {
  group: DuplicateGroup;
  deletes: Set<number>;
  masterRootId: number | null;
  isExpanded: boolean;
  onToggleExpanded: () => void;
  /** Flip the deletion flag for a single file in this group. */
  onToggleDelete: (fileId: number) => void;
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

export const DuplicateGroupCard: React.FC<DuplicateGroupCardProps> = ({
  group,
  deletes,
  masterRootId,
  isExpanded,
  onToggleExpanded,
  onToggleDelete,
}) => {
  const wasted = group.size * (group.file_count - 1);
  const summary = headerSummary(group.files);
  const status = groupDeletionStatus(group, deletes);
  const badge = statusBadge(status, deletes.size, group.files.length);
  const BadgeIcon = badge.Icon;

  return (
    <div className="bg-surface rounded-lg border border-border">
      {/* Header — always visible, toggles expansion. */}
      <button
        onClick={onToggleExpanded}
        className="w-full flex items-center gap-2 px-3 py-2 text-left hover:bg-surface-hover transition-colors rounded-lg"
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

        <span className={`ml-auto inline-flex items-center gap-1 h-6 px-2 rounded-full text-[11px] border flex-shrink-0 ${badge.className}`}>
          <BadgeIcon size={12} />
          {badge.text}
        </span>
        <span className="font-mono text-[10px] text-content-muted ml-1 flex-shrink-0">
          {group.content_hash.substring(0, 10)}
        </span>
      </button>

      {/* Expanded body — checkbox per file. Checked = delete. */}
      {isExpanded && (
        <div className="px-3 pb-3 pt-1 space-y-1">
          {group.files.map((file) => {
            const willDelete = deletes.has(file.fileId);
            const showLabel = deletes.size > 0;
            const inMaster = masterRootId != null && file.scanRootId === masterRootId;
            const [prefix, remainder] = splitPath(file.path, file.scanRootPath);
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
                  <span className={`text-xs font-medium w-24 flex-shrink-0 ${willDelete ? 'text-error' : 'text-success'}`}>
                    {willDelete ? 'Move to Black Hole' : 'Keep'}
                  </span>
                )}
                <span
                  className={`flex-1 min-w-0 font-mono text-xs truncate ${willDelete ? 'line-through text-content-muted' : ''}`}
                  title={file.path}
                  style={{ direction: 'rtl', textAlign: 'left' }}
                >
                  <bdi>
                    {prefix && (
                      <span className={inMaster ? 'text-accent' : 'text-content-muted'}>{prefix}</span>
                    )}
                    <span className={willDelete ? '' : 'text-content'}>{remainder}</span>
                  </bdi>
                </span>
                <span className="text-[11px] text-content-muted flex-shrink-0">
                  {formatDate(file.modifiedAt)}
                </span>
              </label>
            );
          })}
        </div>
      )}
    </div>
  );
};
