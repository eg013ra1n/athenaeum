// One folder card: effective path, default hint, Choose… / Use default,
// restart badge. Lifted out of `TransfersSection.tsx` (Plan 5b Task 3) so
// the Stacking tab's Output panel can reuse it verbatim — identical props,
// identical markup; `TransfersSection.tsx` now imports this file instead of
// declaring its own copy.

import { AlertTriangle, FolderOpen, RotateCcw } from 'lucide-react';
import type { PathSetting } from '../../types/models';

export interface FolderCardProps {
  title: string;
  hint: string;
  setting: PathSetting;
  onChoose: () => void;
  onReset: () => void;
  error: string | null;
  busy: boolean;
}

export function FolderCard({ title, hint, setting, onChoose, onReset, error, busy }: FolderCardProps) {
  return (
    <div className="rounded-lg border border-border bg-surface p-3">
      <div className="flex items-center justify-between gap-3">
        <h4 className="text-sm font-medium text-content-secondary">{title}</h4>
        {setting.restartRequired && (
          <span className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-medium bg-warning/15 text-warning">
            <AlertTriangle size={11} /> Restart Athenaeum to apply
          </span>
        )}
      </div>
      <p className="mt-1 font-mono text-xs text-content break-all" title={setting.effective}>
        {setting.effective}
      </p>
      <p className="mt-1 text-[11px] text-content-muted">
        {setting.configured ? `Default: ${setting.default}` : 'Default location'} · {hint}
      </p>
      {error && <p className="mt-1 text-[11px] text-error">{error}</p>}
      <div className="mt-2 flex items-center gap-2">
        <button
          type="button"
          onClick={onChoose}
          disabled={busy}
          className="inline-flex items-center gap-1 rounded border border-border bg-surface-elevated px-2 py-1 text-xs text-content hover:bg-surface disabled:opacity-50"
        >
          <FolderOpen size={12} /> Choose…
        </button>
        {setting.configured && (
          <button
            type="button"
            onClick={onReset}
            disabled={busy}
            className="inline-flex items-center gap-1 rounded px-2 py-1 text-xs text-content-muted hover:text-content disabled:opacity-50"
          >
            <RotateCcw size={12} /> Use default
          </button>
        )}
      </div>
    </div>
  );
}
