import { useState, type JSX } from 'react';
import { Link } from 'react-router-dom';
import { useCollabExchange } from '../../contexts/CollabExchangeContext';
import { flowKey, peerLabel } from '../collab/exchange/state';
import { PeerFlowRow } from '../collab/exchange/PeerFlowRow';
import { formatRate } from '../collab/format';
import type { FlowView } from '../../types/models';

/**
 * Collab traffic on `/transfers` (Task 17, wave 2 "Transfers"): one
 * expandable group per project that currently has a live flow, rendered
 * between the filter chips and the unified list. Same per-peer rows as the
 * project's own Exchange tab (`PeerFlowRow`) — the live state comes from the
 * SAME app-root `CollabExchangeProvider`, so this is a second view over one
 * source of data, never a second poller.
 *
 * Returns `null` outright when no project has a live flow — the page renders
 * this unconditionally under the `all`/`sending`/`receiving` filters and
 * relies on that to keep the layout unchanged the rest of the time.
 */
export function CollabTrafficGroups({
  projectTitles,
}: {
  projectTitles: Record<string, string>;
}): JSX.Element | null {
  const { state } = useCollabExchange();
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});

  const projectIds = Object.keys(state.projects);
  if (projectIds.length === 0) return null;

  return (
    <div className="mb-3 flex shrink-0 flex-col gap-2">
      {projectIds.map((projectId) => {
        const flows = state.projects[projectId];
        const allFlows: FlowView[] = [...flows.recv, ...flows.send];
        const recvBps = flows.recv.reduce((a, f) => a + f.rateBps, 0);
        const sendBps = flows.send.reduce((a, f) => a + f.rateBps, 0);
        const peers = new Set(allFlows.map((f) => f.device));
        const title = projectTitles[projectId] ?? projectId.slice(0, 8);
        const isOpen = !collapsed[projectId];

        return (
          <div key={projectId} className="overflow-hidden rounded-lg border border-border bg-surface">
            <div className="flex items-center gap-2 px-3 py-2">
              <button
                type="button"
                onClick={() => setCollapsed((prev) => ({ ...prev, [projectId]: isOpen }))}
                aria-expanded={isOpen}
                className="flex min-w-0 flex-1 items-center gap-2 text-left"
              >
                <span className="shrink-0 text-content-muted" aria-hidden="true">
                  {isOpen ? '▾' : '▸'}
                </span>
                <span className="truncate text-sm font-medium text-content">
                  {`${title} · ↓ ${formatRate(recvBps)} · ↑ ${formatRate(sendBps)} · ${peers.size} peer${
                    peers.size === 1 ? '' : 's'
                  }`}
                </span>
              </button>
              <Link
                to={`/projects/${projectId}?tab=exchange`}
                className="shrink-0 text-xs text-accent hover:underline"
              >
                Open in project →
              </Link>
            </div>
            {isOpen && (
              <div className="border-t border-border">
                {allFlows.map((flow) => {
                  const label = peerLabel(state, projectId, flow.device);
                  return (
                    <PeerFlowRow
                      key={flowKey(flow)}
                      flow={flow}
                      label={label}
                      rates={state.rates[flowKey(flow)] ?? []}
                    />
                  );
                })}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
