import React, { useMemo, useState } from 'react';
import { CheckCircle2, ChevronDown, ChevronRight, Loader2, Pencil } from 'lucide-react';
import type { Frame, MissingMetadataRow } from '../../types/models';

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

export function computeMissingFlags(frame: Frame): MissingFlags {
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
  /** Bulk-select or bulk-deselect a group of frame IDs at once. Used by the
      per-directory group checkbox. `select=true` adds all IDs to the
      selection; `select=false` removes them. */
  onToggleGroup: (frameIds: number[], select: boolean) => void;
  /** Click handler for the filename cell. When provided, filenames render
      as a link that asks the parent to navigate to the file's location in
      the dual-pane file browser. */
  onRevealInBrowser?: (filePath: string) => void;
  /** Pixel offset from the top of the scroll container at which the sticky
      table header should stop. Used to park the header just below the
      sticky toolbar above. */
  stickyHeaderTop?: number;
  /** Optional extra column appended after Frame Type. Used by the Excluded
      Frames page to surface the exclusion `reason` per row. */
  extraColumn?: {
    header: string;
    render: (row: MissingMetadataRow) => React.ReactNode;
  };
}

/** A directory group: shared parent folder path + the rows that live in it. */
interface DirectoryGroup {
  folder: string;
  rows: MissingMetadataRow[];
}

export const MissingMetadataTable: React.FC<MissingMetadataTableProps> = ({
  rows,
  loading,
  error,
  selectedIds,
  onToggleRow,
  onToggleAll,
  onToggleGroup,
  onRevealInBrowser,
  stickyHeaderTop = 0,
  extraColumn,
}) => {
  const totalDataCols = 3 + (extraColumn ? 1 : 0); // File + Missing + Frame Type [+ extra]
  // Group rows by parent folder, then sort folders alphabetically and
  // filenames within each folder. Same key ordering as before, just folded
  // into a tree shape so we can render group headers.
  const groups = useMemo<DirectoryGroup[]>(() => {
    const byFolder = new Map<string, MissingMetadataRow[]>();
    for (const row of rows) {
      const folder = dirname(row.file.path);
      const list = byFolder.get(folder);
      if (list) list.push(row);
      else byFolder.set(folder, [row]);
    }
    const result: DirectoryGroup[] = [];
    for (const [folder, list] of byFolder.entries()) {
      list.sort((a, b) => a.file.filename.localeCompare(b.file.filename));
      result.push({ folder, rows: list });
    }
    result.sort((a, b) => (a.folder < b.folder ? -1 : a.folder > b.folder ? 1 : 0));
    return result;
  }, [rows]);

  // Collapsed-group state — folders the user has explicitly collapsed. Default
  // is "all expanded" so freshly-loaded data behaves like the previous flat list.
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());
  const toggleCollapse = (folder: string) => {
    setCollapsed(prev => {
      const next = new Set(prev);
      if (next.has(folder)) next.delete(folder);
      else next.add(folder);
      return next;
    });
  };

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

  if (groups.length === 0) {
    return (
      <div className="text-content-muted text-center py-12">
        <CheckCircle2 className="mx-auto mb-3 text-success" size={48} />
        <p className="font-semibold mb-1">All metadata complete!</p>
        <p className="text-sm">No frames match the active filters.</p>
      </div>
    );
  }

  const allFrameIds = groups
    .flatMap(g => g.rows)
    .map(r => r.frame.id)
    .filter((id): id is number => id != null);

  const allSelected =
    allFrameIds.length > 0 && allFrameIds.every(id => selectedIds.has(id));
  const someSelected = allFrameIds.some(id => selectedIds.has(id));

  // When an extra column is present (Excluded Frames page) we set strict
  // column widths via `table-fixed`:
  //   - File: 28% (honors the original sweet spot — long names truncate)
  //   - Missing: 11rem (fits the 1-2 pills typically shown without wrapping)
  //   - Frame Type: 6rem (fits "MasterDarkFlat" comfortably)
  //   - Reason: no explicit width — claims everything else
  const fileColStyle = extraColumn
    ? { top: stickyHeaderTop, width: '28%' }
    : { top: stickyHeaderTop };
  const missingColStyle = extraColumn
    ? { top: stickyHeaderTop, width: '11rem' }
    : { top: stickyHeaderTop };
  const typeColStyle = extraColumn
    ? { top: stickyHeaderTop, width: '6rem' }
    : { top: stickyHeaderTop };

  return (
    <div>
      <table className={`w-full ${extraColumn ? 'table-fixed' : ''}`} role="table">
        {/* Sticky is applied per-<th> (not on <thead>) because sticky thead
            is unreliable under the default border-collapse behaviour. Top is
            offset by the measured toolbar height so the header parks just
            below the sticky control panel. */}
        <thead>
          <tr>
            <th
              scope="col"
              style={{ top: stickyHeaderTop, width: '2.5rem' }}
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
            {/* Filename column — folder is now shown once per group header
                row instead of being repeated on every row. */}
            <th
              scope="col"
              style={fileColStyle}
              className="sticky z-10 bg-surface px-1.5 py-1.5 text-left text-xs font-semibold text-content-secondary"
            >
              File
            </th>
            <th
              scope="col"
              style={missingColStyle}
              className="sticky z-10 bg-surface px-1.5 py-1.5 text-left text-xs font-semibold text-content-secondary"
            >
              Missing
            </th>
            <th
              scope="col"
              style={typeColStyle}
              className="sticky z-10 bg-surface px-1.5 py-1.5 text-left text-xs font-semibold text-content-secondary"
            >
              Frame Type
            </th>
            {extraColumn && (
              <th
                scope="col"
                style={{ top: stickyHeaderTop }}
                className="sticky z-10 bg-surface px-1.5 py-1.5 text-left text-xs font-semibold text-content-secondary"
              >
                {extraColumn.header}
              </th>
            )}
          </tr>
        </thead>
        <tbody className="divide-y divide-border">
          {groups.map((group) => {
            const isCollapsed = collapsed.has(group.folder);
            // Eligible group IDs for the group-checkbox — frames without
            // an id (shouldn't normally happen) can't be selected.
            const groupIds = group.rows
              .map(r => r.frame.id)
              .filter((id): id is number => id != null);
            const groupAllSelected =
              groupIds.length > 0 && groupIds.every(id => selectedIds.has(id));
            const groupSomeSelected = groupIds.some(id => selectedIds.has(id));

            return (
              <React.Fragment key={group.folder}>
                {/* Group header row — checkbox toggles select-all-in-group;
                    the rest of the row toggles expand/collapse. */}
                <tr className="bg-surface border-t border-border">
                  <td className="w-10 px-1.5 py-1 text-center">
                    <input
                      type="checkbox"
                      checked={groupAllSelected}
                      ref={el => {
                        if (el) el.indeterminate = groupSomeSelected && !groupAllSelected;
                      }}
                      onChange={() => onToggleGroup(groupIds, !groupAllSelected)}
                      disabled={groupIds.length === 0}
                      onClick={e => e.stopPropagation()}
                      className="rounded border-border text-accent focus:ring-accent cursor-pointer disabled:cursor-default disabled:opacity-30"
                      aria-label={`Select all frames in ${group.folder}`}
                    />
                  </td>
                  <td
                    colSpan={totalDataCols}
                    onClick={() => toggleCollapse(group.folder)}
                    className="px-1.5 py-1 cursor-pointer select-none"
                  >
                    <span className="inline-flex items-center gap-1.5">
                      {isCollapsed ? (
                        <ChevronRight size={14} className="flex-shrink-0 text-content-muted" />
                      ) : (
                        <ChevronDown size={14} className="flex-shrink-0 text-content-muted" />
                      )}
                      <span
                        className="text-xs text-content-secondary font-mono break-all"
                        title={group.folder}
                      >
                        {group.folder}
                      </span>
                      <span className="text-xs text-content-muted ml-1">({group.rows.length})</span>
                    </span>
                  </td>
                </tr>
                {!isCollapsed && group.rows.map((item, idx) => {
                  const frameId = item.frame.id ?? null;
                  const isSelected = frameId != null && selectedIds.has(frameId);
                  const flags = computeMissingFlags(item.frame);
                  const missingKeys = (Object.keys(flags) as (keyof MissingFlags)[]).filter(k => flags[k]);

                  return (
                    <tr
                      key={item.file.id ?? `${group.folder}/${idx}`}
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
                      {/* Filename — when an extra column is present the
                          File column is constrained, so we truncate with an
                          ellipsis and rely on the existing `title` attribute
                          for hover-to-see-full. Without an extra column the
                          column auto-sizes and the filename never wraps. */}
                      <td className={`px-1.5 py-1 ${extraColumn ? 'overflow-hidden' : ''}`}>
                        <span className="flex items-center gap-1.5 min-w-0">
                          {onRevealInBrowser ? (
                            <button
                              type="button"
                              onClick={() => onRevealInBrowser(item.file.path)}
                              title={`Open in file browser — ${item.file.filename}`}
                              className={`text-sm text-accent hover:underline font-mono text-left min-w-0 ${
                                extraColumn ? 'truncate' : 'whitespace-nowrap'
                              }`}
                            >
                              {item.file.filename}
                            </button>
                          ) : (
                            <span
                              className={`text-sm text-content-secondary font-mono min-w-0 ${
                                extraColumn ? 'truncate' : 'whitespace-nowrap'
                              }`}
                              title={item.file.filename}
                            >
                              {item.file.filename}
                            </span>
                          )}
                          {item.frame.override_ === true && (
                            <Pencil
                              size={11}
                              className="flex-shrink-0 text-amber-400"
                              aria-label="Custom metadata applied"
                            >
                              <title>Custom metadata applied — open metadata pane (⌘I) to compare with original</title>
                            </Pencil>
                          )}
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
                      {extraColumn && (
                        <td className="px-1.5 py-1">{extraColumn.render(item)}</td>
                      )}
                    </tr>
                  );
                })}
              </React.Fragment>
            );
          })}
        </tbody>
      </table>
    </div>
  );
};
