import { useEffect, useState } from 'react';
import { api } from '../../api';
import type { FolderTypeBreakdown as Breakdown, FrameTypeCounts } from '../../types/models';
import { ToolbarButton } from '../Toolbar';

const columns: [keyof FrameTypeCounts, string][] = [
  ['total', 'Total'],
  ['lights', 'Lights'],
  ['darks', 'Darks'],
  ['flats', 'Flats'],
  ['bias', 'Bias'],
  ['darkFlats', 'Dark flats'],
  ['masters', 'Masters'],
  ['unknown', 'Unknown'],
];
export function FolderCountsTable({
  data,
  onFolder,
}: {
  data: Breakdown;
  onFolder: (path: string) => void;
}) {
  const cells = (counts: FrameTypeCounts) =>
    columns.map(([key]) => (
      <td key={key} className="p-2 text-right tabular-nums">
        {counts[key].toLocaleString()}
      </td>
    ));
  return (
    <div className="overflow-auto">
      <table className="w-full text-xs text-left">
        <thead>
          <tr>
            <th className="p-2">Folder / scope</th>
            {columns.map(([key, label]) => (
              <th key={key} className="p-2 text-right">
                {label}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          <tr className="border-t border-border">
            <th className="p-2">This folder only</th>
            {cells(data.direct)}
          </tr>
          <tr className="border-t border-border">
            <th className="p-2">Including subfolders</th>
            {cells(data.recursive)}
          </tr>
          {data.children.map(child => (
            <tr key={child.path} className="border-t border-border">
              <th className="p-2">
                <button
                  className="text-accent underline"
                  title={child.path}
                  onClick={() => onFolder(child.path)}
                >
                  {child.path.split('/').pop()} /
                </button>
              </th>
              {cells(child.counts)}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
export function FolderTypeBreakdown({ path, refreshKey }: { path: string; refreshKey?: string }) {
  const [trail, setTrail] = useState<string[]>([]);
  const [reload, setReload] = useState(0);
  const [data, setData] = useState<Breakdown | null>(null);
  const [error, setError] = useState('');
  const current = trail[trail.length - 1] ?? path;
  useEffect(() => setTrail([]), [path]);
  useEffect(() => {
    let cancelled = false;
    setData(null);
    setError('');
    if (current)
      api
        .invoke<Breakdown>('get_folder_type_breakdown', { path: current })
        .then(value => {
          if (!cancelled) setData(value);
        })
        .catch(error => {
          console.error('[FolderTypeBreakdown]', error);
          if (!cancelled) setError(String(error));
        });
    return () => {
      cancelled = true;
    };
  }, [current, reload, refreshKey]);
  return (
    <section className="mt-4 p-3 bg-surface border border-border rounded-lg space-y-2">
      <div className="flex gap-3 items-center">
        <h4 className="font-semibold">Frame types by folder</h4>
        <ToolbarButton onClick={() => setReload(x => x + 1)}>Refresh counts</ToolbarButton>
        {trail.length > 0 && (
          <ToolbarButton onClick={() => setTrail(t => t.slice(0, -1))}>
            Back one folder
          </ToolbarButton>
        )}
      </div>
      <p className="text-xs text-content-muted break-all">{current}</p>
      <p className="text-xs text-content-muted">
        Cataloged files from the latest scan, including offline files. Each file counts once;
        exposure versions count separately. Child rows include all descendants. Explicit master
        frame types are separate; Unknown includes missing or unrecognized frame types.
      </p>
      {error ? (
        <p role="alert" className="text-error text-sm">
          {error}
        </p>
      ) : data ? (
        <FolderCountsTable data={data} onFolder={folder => setTrail(t => [...t, folder])} />
      ) : (
        <p role="status">Loading counts…</p>
      )}
    </section>
  );
}
