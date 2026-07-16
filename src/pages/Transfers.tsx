import { useState } from 'react';
import { ArrowLeftRight } from 'lucide-react';
import { ActiveTransferRow } from '../components/transfers/ActiveTransferRow';
import { TransfersHistoryTab } from '../components/transfers/TransfersHistoryTab';
import { useTransferQueue } from '../hooks/useTransferQueue';

type Tab = 'active' | 'history';

/**
 * `/transfers` — the full-screen, torrent-style sync queue (Task 15). A
 * unified table of every in-flight (and page-session-recently-terminal, for
 * Resend) outbound/inbound package, backed by `useTransferQueue`, plus a
 * grouped History tab for the durable audit trail. The sidebar `TransfersPanel`
 * slide-over stays as a quick-glance surface and links here.
 */
export default function Transfers() {
  const [tab, setTab] = useState<Tab>('active');
  const { rows, activeCount, liveFiles, sendNow, cancelOutbound, cancelInbound, resend, busy } =
    useTransferQueue();

  return (
    <div className="flex h-full flex-col p-4 pt-3">
      <div className="mb-3 flex shrink-0 items-center gap-2">
        <ArrowLeftRight size={22} className="text-accent" />
        <h2 className="text-2xl font-bold">Transfers</h2>
      </div>

      <div className="mb-3 flex shrink-0 border-b border-border">
        {(['active', 'history'] as const).map((t) => (
          <button
            key={t}
            type="button"
            onClick={() => setTab(t)}
            className={`px-4 py-2 text-sm font-medium capitalize transition-colors ${
              tab === t
                ? 'border-b-2 border-accent text-content'
                : 'text-content-muted hover:text-content'
            }`}
          >
            {t === 'active' ? `Active${activeCount ? ` (${activeCount})` : ''}` : 'History'}
          </button>
        ))}
      </div>

      <div className="min-h-0 flex-1 overflow-hidden rounded-lg border border-border">
        {tab === 'active' ? (
          <div className="h-full overflow-auto">
            {rows.length === 0 ? (
              <p className="px-4 py-10 text-center text-sm text-content-muted">No active transfers</p>
            ) : (
              <table className="w-full text-sm">
                <thead>
                  <tr className="border-b border-border bg-surface-elevated text-left text-xs text-content-muted">
                    <th className="w-6 px-2 py-2" />
                    <th className="w-6 px-1 py-2" />
                    <th className="px-2 py-2 font-medium">Device</th>
                    <th className="px-2 py-2 font-medium">Batch</th>
                    <th className="px-2 py-2 font-medium">Status</th>
                    <th className="px-2 py-2 font-medium">Progress</th>
                    <th className="px-2 py-2 text-right font-medium">Size</th>
                    <th className="px-2 py-2 text-right font-medium">Speed</th>
                    <th className="px-2 py-2 font-medium">Retry</th>
                    <th className="px-2 py-2 text-right font-medium">Actions</th>
                  </tr>
                </thead>
                <tbody>
                  {rows.map((row) => (
                    <ActiveTransferRow
                      key={row.key}
                      row={row}
                      busy={busy}
                      liveFiles={liveFiles}
                      onSendNow={sendNow}
                      onCancelOutbound={cancelOutbound}
                      onCancelInbound={cancelInbound}
                      onResend={resend}
                    />
                  ))}
                </tbody>
              </table>
            )}
          </div>
        ) : (
          <TransfersHistoryTab />
        )}
      </div>
    </div>
  );
}
