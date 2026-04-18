import React, { useMemo } from 'react';
import { CheckCircle2, Loader2 } from 'lucide-react';
import type { MissingMetadataRow } from '../../types/models';

/** Returns the parent directory of a file path (without trailing slash). */
function dirname(filePath: string): string {
  const idx = Math.max(filePath.lastIndexOf('/'), filePath.lastIndexOf('\\'));
  if (idx <= 0) return '.';
  return filePath.slice(0, idx);
}

/** All the "missing" field predicates, matching the SQL in operations.rs. */
export interface MissingFlags {
  coordinates: boolean;
  object: boolean;
  date: boolean;
  camera: boolean;
  type: boolean;
}

function stringIsZero(s: string | null | undefined): boolean {
  if (!s) return false;
  return s.replace(/[\s+\-:]/g, '') === '000000';
}

export function computeMissingFlags(item: MissingMetadataRow): MissingFlags {
  const frame = item.frame;

  const numericMissing =
    (frame.ra == null && frame.dec == null) ||
    (frame.ra === 0 && frame.dec === 0);
  const sexagesimalMissing =
    frame.objctra == null ||
    frame.objctdec == null ||
    stringIsZero(frame.objctra) ||
    stringIsZero(frame.objctdec);

  return {
    coordinates: numericMissing && sexagesimalMissing,
    object: !frame.object,
    date: !frame.date_obs,
    camera: !frame.instrume,
    type: !frame.imagetyp,
  };
}

interface MissingTagProps {
  label: string;
  colorClass: string;
}

const MissingTag: React.FC<MissingTagProps> = ({ label, colorClass }) => (
  <span className={`px-2 py-0.5 rounded text-xs font-medium ${colorClass}`}>
    {label}
  </span>
);

const TAG_STYLES: Record<keyof MissingFlags, string> = {
  coordinates: 'bg-error-muted text-error border border-error/50',
  object: 'bg-orange/25 text-orange border border-orange/50',
  date: 'bg-warning-muted text-warning border border-warning/50',
  camera: 'bg-info-muted text-accent border border-info/50',
  type: 'bg-accent/20 text-accent border border-accent/50',
};

const TAG_LABELS: Record<keyof MissingFlags, string> = {
  coordinates: 'Coordinates',
  object: 'Object',
  date: 'Date',
  camera: 'Camera',
  type: 'Type',
};

interface MissingMetadataTableProps {
  rows: MissingMetadataRow[];
  loading: boolean;
  error: string | null;
  selectedIds: Set<number>;
  onToggleRow: (frameId: number) => void;
  onToggleAll: () => void;
  /** Pixel offset from the top of the scroll container at which the sticky
      table header should stop. Used to park the header just below the
      sticky toolbar above. */
  stickyHeaderTop?: number;
}

export const MissingMetadataTable: React.FC<MissingMetadataTableProps> = ({
  rows,
  loading,
  error,
  selectedIds,
  onToggleRow,
  onToggleAll,
  stickyHeaderTop = 0,
}) => {
  // Sort by parent folder then filename — stable, client-side
  const sortedRows = useMemo(() => {
    return [...rows].sort((a, b) => {
      const da = dirname(a.file.path);
      const db = dirname(b.file.path);
      if (da < db) return -1;
      if (da > db) return 1;
      return a.file.filename.localeCompare(b.file.filename);
    });
  }, [rows]);

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <Loader2 className="animate-spin mr-2" size={24} />
        <span className="text-content-muted">Loading frames…</span>
      </div>
    );
  }

  if (error) {
    return (
      <div className="p-3 bg-error-muted border border-error/50 rounded">
        <p className="text-error text-sm">Error: {error}</p>
      </div>
    );
  }

  if (sortedRows.length === 0) {
    return (
      <div className="text-content-muted text-center py-12">
        <CheckCircle2 className="mx-auto mb-3 text-success" size={48} />
        <p className="font-semibold mb-1">All metadata complete!</p>
        <p className="text-sm">No frames match the active filters.</p>
      </div>
    );
  }

  const allFrameIds = sortedRows
    .map(r => r.frame.id)
    .filter((id): id is number => id != null);

  const allSelected =
    allFrameIds.length > 0 && allFrameIds.every(id => selectedIds.has(id));
  const someSelected = allFrameIds.some(id => selectedIds.has(id));

  return (
    <div>
      <table className="w-full" role="table">
        {/* Sticky is applied per-<th> (not on <thead>) because sticky thead
            is unreliable under the default border-collapse behaviour. Top is
            offset by the measured toolbar height so the header parks just
            below the sticky control panel. */}
        <thead>
          <tr>
            <th
              scope="col"
              style={{ top: stickyHeaderTop }}
              className="sticky z-10 bg-surface w-10 px-1.5 py-1.5 text-center"
            >
              <input
                type="checkbox"
                checked={allSelected}
                ref={el => { if (el) el.indeterminate = someSelected && !allSelected; }}
                onChange={onToggleAll}
                className="rounded border-border text-accent focus:ring-accent cursor-pointer"
              />
            </th>
            {/* Folder column: bounded width so the start-ellipsis kicks in
                and the File column can expand to hold full filenames. */}
            <th
              scope="col"
              style={{ top: stickyHeaderTop }}
              className="sticky z-10 bg-surface w-64 px-1.5 py-1.5 text-left text-xs font-semibold text-content-secondary"
            >
              Folder
            </th>
            <th
              scope="col"
              style={{ top: stickyHeaderTop }}
              className="sticky z-10 bg-surface px-1.5 py-1.5 text-left text-xs font-semibold text-content-secondary"
            >
              File
            </th>
            <th
              scope="col"
              style={{ top: stickyHeaderTop }}
              className="sticky z-10 bg-surface px-1.5 py-1.5 text-left text-xs font-semibold text-content-secondary"
            >
              Missing
            </th>
            <th
              scope="col"
              style={{ top: stickyHeaderTop }}
              className="sticky z-10 bg-surface px-1.5 py-1.5 text-left text-xs font-semibold text-content-secondary"
            >
              Frame Type
            </th>
            <th
              scope="col"
              style={{ top: stickyHeaderTop }}
              className="sticky z-10 bg-surface w-20 px-1.5 py-1.5 text-center text-xs font-semibold text-content-secondary"
            >
              Duplicate
            </th>
          </tr>
        </thead>
        <tbody className="divide-y divide-border">
          {sortedRows.map((item, idx) => {
            const frameId = item.frame.id ?? null;
            const isSelected = frameId != null && selectedIds.has(frameId);
            const flags = computeMissingFlags(item);
            const missingKeys = (Object.keys(flags) as (keyof MissingFlags)[]).filter(k => flags[k]);
            const folder = dirname(item.file.path);

            return (
              <tr
                key={item.file.id ?? idx}
                className={`${
                  idx % 2 === 0 ? 'bg-surface-elevated' : 'bg-surface'
                } hover:bg-surface-hover transition-colors`}
              >
                <td className="w-10 px-1.5 py-1 text-center">
                  <input
                    type="checkbox"
                    checked={isSelected}
                    disabled={frameId == null}
                    onChange={() => { if (frameId != null) onToggleRow(frameId); }}
                    className="rounded border-border text-accent focus:ring-accent cursor-pointer disabled:cursor-default disabled:opacity-30"
                  />
                </td>
                {/* Folder path truncated from the START (keep the end of
                    the path, which is the distinguishing part). Uses
                    dir="rtl" + a bdi wrapper so the overflow ellipsis
                    appears on the left while the path renders in normal
                    left-to-right order. */}
                <td className="w-64 px-1.5 py-1">
                  <span
                    className="block text-xs text-content-muted font-mono overflow-hidden whitespace-nowrap"
                    style={{ direction: 'rtl', textOverflow: 'ellipsis', textAlign: 'left' }}
                    title={folder}
                  >
                    <bdi>{folder}</bdi>
                  </span>
                </td>
                {/* Filename — always full, never truncated */}
                <td className="px-1.5 py-1">
                  <span
                    className="text-sm text-content-secondary font-mono whitespace-nowrap"
                    title={item.file.filename}
                  >
                    {item.file.filename}
                  </span>
                </td>
                <td className="px-1.5 py-1">
                  <div className="flex flex-wrap gap-1">
                    {missingKeys.map(key => (
                      <MissingTag
                        key={key}
                        label={TAG_LABELS[key]}
                        colorClass={TAG_STYLES[key]}
                      />
                    ))}
                  </div>
                </td>
                <td className="px-1.5 py-1">
                  <span className="text-sm text-content-secondary font-mono">
                    {item.frame.imagetyp ?? (
                      <span className="text-content-muted italic">—</span>
                    )}
                  </span>
                </td>
                <td className="w-20 px-1.5 py-1 text-center">
                  {item.hasDuplicate ? (
                    <span className="px-2 py-0.5 rounded text-xs font-medium bg-warning-muted text-warning border border-warning/50">
                      Yes
                    </span>
                  ) : (
                    <span className="text-xs text-content-muted">No</span>
                  )}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
};
