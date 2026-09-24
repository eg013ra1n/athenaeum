import { AlertTriangle } from 'lucide-react';
import { useUpdates } from '../../contexts/UpdatesContext';

/**
 * Shown by `Projects.tsx` and `ProjectDetail.tsx` when a hub call refused
 * this build with the `collab_api_outdated` conflict (P17): this device's
 * collab API is older than the hub requires. The one action opens the
 * existing update dialog (`UpdatesContext`) — there is nothing collab-specific
 * to do here, the fix is always "update the app".
 */
export default function UpdateRequired() {
  const { openAvailable } = useUpdates();
  return (
    <div className="flex flex-wrap items-center gap-2 rounded border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-content-secondary">
      <AlertTriangle size={14} className="shrink-0 text-warning" />
      <span>This project hub needs a newer Athenaeum. Update to keep collaborating.</span>
      <button
        type="button"
        onClick={() => openAvailable()}
        className="ml-auto rounded border border-border px-2 py-1 text-xs text-content-secondary transition-colors hover:bg-surface-hover"
      >
        Update
      </button>
    </div>
  );
}
