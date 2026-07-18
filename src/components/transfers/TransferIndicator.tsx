import { ArrowUp, ArrowDown, RefreshCw } from 'lucide-react';
import { useTransfers } from '../../contexts/TransfersContext';
import { transportHealthView } from './transportHealth';

interface TransferIndicatorProps {
  collapsed: boolean;
}

/**
 * Sidebar transfers badge (task M3), sibling of `ComputeQueueIndicator`.
 * Visible only when this device is signed in with a role set, or dev pairing is
 * enabled (`useSyncStatus.visible`). Shows a compact ↑ (queued + transferring
 * out) / ↓ (frames received) summary plus a small transport-health dot (Task
 * 3.3); clicking opens the `TransfersPanel`.
 *
 * The snapshot behind it is polled by the single shared `useSyncStatus` in
 * `TransfersProvider` — this component is presentational only.
 */
export function TransferIndicator({ collapsed }: TransferIndicatorProps) {
  const { status, visible, openPanel } = useTransfers();
  if (!visible || !status) return null;

  const up = status.sender.queued + status.sender.transferring;
  const down = status.receiver.receivedTotal;
  const health = transportHealthView(status.transport);
  const title = `Transfers — ${up} sending, ${down} received\n${health.detail}`;

  if (collapsed) {
    return (
      <div className="px-2 pb-2">
        <button
          type="button"
          onClick={openPanel}
          title={title}
          className="relative flex w-full items-center justify-center py-3 text-content-secondary transition-colors hover:text-content"
        >
          <RefreshCw size={20} className={up > 0 ? 'text-accent' : 'text-content-muted'} />
          {up > 0 && (
            <span className="absolute -top-0.5 -right-0.5 flex h-3.5 w-3.5 items-center justify-center rounded-full bg-accent text-[9px] font-bold text-surface">
              {up}
            </span>
          )}
          {/* Transport-health dot (Task 3.3), bottom-right so it never collides
              with the top-right in-flight count. */}
          <span
            className={`absolute -bottom-0.5 -right-0.5 h-2.5 w-2.5 rounded-full ring-2 ring-surface ${health.dot}`}
            aria-label={health.label}
          />
        </button>
      </div>
    );
  }

  return (
    <div className="px-4 pb-2">
      <button
        type="button"
        onClick={openPanel}
        title={title}
        className="flex w-full items-center justify-between gap-2 rounded-lg border border-border bg-surface p-2.5 transition-colors hover:bg-surface-hover"
      >
        <div className="flex items-center gap-1.5 min-w-0">
          <span className="relative flex shrink-0">
            <RefreshCw size={14} className={up > 0 ? 'text-accent' : 'text-content-muted'} />
            {/* Transport-health dot (Task 3.3) tucked on the transfers icon. */}
            <span
              className={`absolute -bottom-1 -right-1 h-2 w-2 rounded-full ring-2 ring-surface ${health.dot}`}
              aria-label={health.label}
            />
          </span>
          <span className="truncate text-xs text-content-secondary">Transfers</span>
        </div>
        <div className="flex shrink-0 items-center gap-2 text-xs">
          <span
            className={`flex items-center gap-0.5 ${up > 0 ? 'text-accent' : 'text-content-muted'}`}
            title={`${up} sending`}
          >
            <ArrowUp size={12} />
            {up}
          </span>
          <span
            className={`flex items-center gap-0.5 ${down > 0 ? 'text-content-secondary' : 'text-content-muted'}`}
            title={`${down} received`}
          >
            <ArrowDown size={12} />
            {down}
          </span>
        </div>
      </button>
    </div>
  );
}
