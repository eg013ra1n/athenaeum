import type { ConflictResolution, ZipFilenameConflict } from '../../types/archive';
import { Button, DialogShell } from '../ui';

interface Props {
  conflicts: ZipFilenameConflict[];
  onCancel: () => void;
  onResolve: (resolution: ConflictResolution) => void;
}

export function ArchiveConflictDialog({ conflicts, onCancel, onResolve }: Props) {
  return (
    <DialogShell
      title="Archive name conflict"
      onClose={onCancel}
      footer={
        <>
          <Button onClick={onCancel}>Cancel</Button>
          <Button onClick={() => onResolve('add_suffix')} data-autofocus>Add suffix (_2, _3…)</Button>
          <Button variant="danger" onClick={() => onResolve('overwrite')}>Overwrite existing</Button>
        </>
      }
    >
      <p className="mb-2.5">
        The following zip file{conflicts.length === 1 ? '' : 's'} already exist{conflicts.length === 1 ? 's' : ''} in the archive folder:
      </p>
      <ul className="mb-2.5 max-h-40 space-y-1 overflow-y-auto">
        {conflicts.map(c => (
          <li key={c.zip_path} className="truncate font-mono text-content" title={c.zip_path}>{c.zip_filename}</li>
        ))}
      </ul>
      <p>How would you like to proceed?</p>
    </DialogShell>
  );
}
