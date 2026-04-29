import type { ConflictResolution, ZipFilenameConflict } from '../../types/archive';

interface Props {
  conflicts: ZipFilenameConflict[];
  onCancel: () => void;
  onResolve: (resolution: ConflictResolution) => void;
}

export function ArchiveConflictDialog({ conflicts, onCancel, onResolve }: Props) {
  return (
    <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-[60] p-4">
      <div className="bg-surface-elevated border border-border rounded-lg shadow-2xl p-6 max-w-lg w-full">
        <h2 className="text-lg font-semibold mb-3 text-content">Archive name conflict</h2>
        <p className="text-sm mb-3 text-content-muted">
          The following zip file{conflicts.length === 1 ? '' : 's'} already exist in the archive folder:
        </p>
        <ul className="list-disc ml-6 mb-4 space-y-1">
          {conflicts.map(c => (
            <li key={c.zip_path} className="text-sm font-mono text-content">{c.zip_filename}</li>
          ))}
        </ul>
        <p className="text-sm mb-5 text-content-muted">How would you like to proceed?</p>
        <div className="flex flex-col gap-2 sm:flex-row sm:justify-end sm:gap-2">
          <button
            onClick={onCancel}
            className="px-4 py-1.5 rounded-lg border border-border text-sm text-content hover:bg-surface-hover transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={() => onResolve('add_suffix')}
            className="px-4 py-1.5 rounded-lg bg-surface-hover border border-border text-sm text-content hover:brightness-110 transition-colors"
          >
            Add suffix (_2, _3…)
          </button>
          <button
            onClick={() => onResolve('overwrite')}
            className="px-4 py-1.5 rounded-lg bg-warning text-white text-sm hover:brightness-110 transition-colors"
          >
            Overwrite existing
          </button>
        </div>
      </div>
    </div>
  );
}
