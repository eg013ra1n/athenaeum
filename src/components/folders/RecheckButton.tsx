import { useState } from 'react';
import { RefreshCw } from 'lucide-react';

/**
 * "Check again" for a folder whose drive went away.
 *
 * Availability is a single `Path::exists()` per root, recomputed only on mount
 * and after a mutation — so before this button the only way to re-detect a
 * remounted drive was to scan a DIFFERENT folder, which re-ran the check for
 * all of them. Its own component because both inspectors host the same banner.
 *
 * Deliberately a button and not a background poll: `Path::exists()` against a
 * dead network mount can block for that mount's own timeout, and a timer doing
 * that to every root would freeze the panel.
 */
export function RecheckButton({
  onRecheck,
  disabled,
}: {
  onRecheck: () => Promise<void>;
  disabled?: boolean;
}) {
  const [checking, setChecking] = useState(false);
  const [stillOffline, setStillOffline] = useState(false);

  const run = async () => {
    setChecking(true);
    setStillOffline(false);
    try {
      await onRecheck();
    } catch (e) {
      console.error('[RecheckButton] availability re-check failed:', e);
    } finally {
      setChecking(false);
      // A folder that came back takes this whole banner down with it, so
      // anything still rendering here is still offline — which is the only
      // case the user cannot see for themselves.
      setStillOffline(true);
    }
  };

  return (
    <>
      <button
        onClick={() => void run()}
        disabled={disabled || checking}
        className="flex items-center gap-2 px-3 py-1.5 rounded border border-error/50 text-error text-sm hover:bg-error/10 transition disabled:opacity-50"
      >
        <RefreshCw size={14} className={checking ? 'animate-spin' : ''} />
        {checking ? 'Checking…' : 'Check again'}
      </button>
      {/* `w-full` inside the banner's flex-wrap row puts this on its own line. */}
      {stillOffline && !checking && (
        <p className="w-full text-xs text-error/80">Still not reachable.</p>
      )}
    </>
  );
}
