import { useEffect, useRef, useState, type JSX } from 'react';
import { api } from '../../../api';
import { useCollabExchange } from '../../../contexts/CollabExchangeContext';
import { formatTimestamp } from '../../../utils/dateFormatting';
import { formatBytes, formatRate, pluralize } from '../format';
import { flowKey, peerLabel } from '../exchange/state';
import { PeerFlowRow } from '../exchange/PeerFlowRow';
import { memberTone } from './memberColors';
import type { CollabFramesLanded, FlowView, MemberSummary, ReceiveSessionView } from '../../../types/models';

/**
 * Exchange tab — live per-peer rows in both directions, plus the receive
 * session history (Task 14, wave 2 "Exchange", mockup `liveHtml`/`peerRow`
 * and the Exchange tab's history table). `toGo`/`waitingForPublisher` come
 * from `state.summary[projectId]` (Task 8 ruling) so they survive the
 * project's flows going idle; flows come from `state.projects[projectId]`,
 * `[]` when the project has nothing live right now.
 */

const CARD = 'rounded-lg border border-border bg-surface-elevated p-3';
const HEADER = 'flex flex-wrap items-baseline gap-x-1.5 text-sm font-semibold text-content';
const SUB = 'font-normal text-xs text-content-muted';
const EMPTY = 'py-1.5 text-xs text-content-muted';

function sumRate(flows: FlowView[]): number {
  return flows.reduce((a, f) => a + f.rateBps, 0);
}

/** Distinct MEMBERS behind a direction's flows, not flow (device) count — two
 * devices of the same member (`peerLabel(...).member`) count once; an
 * unnamed device falls back to its own id so it still counts as one peer. */
function distinctMembers(flows: FlowView[], label: (device: string) => { member: string | null }): number {
  return new Set(flows.map((f) => label(f.device).member ?? f.device)).size;
}

function toneFor(member: string | null, members: MemberSummary[] | null): string | undefined {
  if (!member || !members) return undefined;
  const m = members.find((mm) => mm.displayName === member);
  return m ? memberTone(m.accountId, members) : undefined;
}

export default function ExchangeTab({
  projectId,
  canReceive,
  members,
}: {
  projectId: string;
  canReceive: boolean;
  members: MemberSummary[] | null;
}): JSX.Element {
  const { state, refreshProject } = useCollabExchange();
  const [sessions, setSessions] = useState<ReceiveSessionView[] | null>(null);
  const [sessionsError, setSessionsError] = useState<string | null>(null);

  // Fills `summary[projectId]` (toGo/waitingForPublisher) even when nothing
  // is moving right now — the project-scoped snapshot read.
  useEffect(() => {
    void refreshProject(projectId);
  }, [projectId, refreshProject]);

  // Held in a ref so the listener effect below subscribes exactly once per
  // `projectId` regardless of how many times this reload function is
  // recreated (Task 10 listener ruling). The loader runs both on mount and
  // on every `collab-frames-landed` for this project, so a stale in-flight
  // request can resolve after a newer one has already started — a
  // monotonically increasing request id (fix round 1) guards `sessions`/
  // `sessionsError` writes to the LATEST call only, the same idea as
  // MembersTab's `cancelled` flag generalized to more than one call per
  // mount. `sessionsError` is also reset synchronously at the top of every
  // load — previously it was only ever set, never cleared, so one transient
  // failure produced a permanent phantom error banner even after a later
  // reload succeeded.
  const requestIdRef = useRef(0);
  const loadSessionsRef = useRef<() => void>(() => {});
  loadSessionsRef.current = () => {
    const requestId = ++requestIdRef.current;
    setSessionsError(null);
    api
      .invoke<ReceiveSessionView[]>('list_collab_receive_sessions', { projectId, limit: 50 })
      .then((rows) => {
        if (requestId !== requestIdRef.current) return;
        setSessions(rows);
      })
      .catch((err) => {
        console.error('[collab-exchange] list_collab_receive_sessions failed:', err);
        if (requestId !== requestIdRef.current) return;
        setSessionsError(err instanceof Error ? err.message : String(err));
      });
  };

  useEffect(() => {
    loadSessionsRef.current();
  }, [projectId]);

  // StrictMode-safe listener pattern (CLAUDE.md): re-read the sessions list
  // whenever a landing lands for THIS project.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<CollabFramesLanded>('collab-frames-landed', (p) => {
        if (cancelled || p.projectId !== projectId) return;
        loadSessionsRef.current();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[collab-exchange] collab-frames-landed listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [projectId]);

  const recv = state.projects[projectId]?.recv ?? [];
  const send = state.projects[projectId]?.send ?? [];
  const summary = state.summary[projectId];
  const toGo = summary?.toGo ?? 0;
  const waitingForPublisher = summary?.waitingForPublisher ?? null;

  const peerLabelHere = (device: string) => peerLabel(state, projectId, device);
  const recvMembers = distinctMembers(recv, peerLabelHere);
  const sendMembers = distinctMembers(send, peerLabelHere);

  const recvSub = canReceive
    ? `${formatRate(sumRate(recv))} from ${recvMembers} ${pluralize(recvMembers, 'member')} · ${toGo} frames to go${
        waitingForPublisher ? ` · waiting for publisher: ${waitingForPublisher}` : ''
      }`
    : 'Your role does not receive project data';

  const sendSub = `${formatRate(sumRate(send))} to ${sendMembers} ${pluralize(sendMembers, 'member')}`;

  const renderRows = (flows: FlowView[]) => (
    <div>
      {flows.map((flow) => {
        const label = peerLabel(state, projectId, flow.device);
        return (
          <PeerFlowRow
            key={flowKey(flow)}
            flow={flow}
            label={label}
            rates={state.rates[flowKey(flow)] ?? []}
            tone={toneFor(label.member, members)}
          />
        );
      })}
    </div>
  );

  return (
    <div className="space-y-3">
      <div className={CARD}>
        <h2 className={HEADER}>
          Receiving <span className={SUB}>{recvSub}</span>
        </h2>
        {canReceive ? (
          recv.length === 0 ? (
            <p className={EMPTY}>Nothing arriving right now.</p>
          ) : (
            renderRows(recv)
          )
        ) : (
          <p className={EMPTY}>
            Contributors only send. Ask the coordinator for the Processor role to receive the project.
          </p>
        )}
      </div>

      <div className={CARD}>
        <h2 className={HEADER}>
          Sending <span className={SUB}>{sendSub}</span>
        </h2>
        {send.length === 0 ? <p className={EMPTY}>Nothing being served right now.</p> : renderRows(send)}
      </div>

      {canReceive && (
        <div className={CARD}>
          <h2 className={HEADER}>
            Received <span className={SUB}>sessions</span>
          </h2>
          {sessionsError && (
            <p className="mt-1 text-xs text-error">Could not load receive sessions — see console.</p>
          )}
          {sessions === null && !sessionsError && <p className={EMPTY}>Loading…</p>}
          {sessions !== null && (
            <div className="mt-2 overflow-auto rounded border border-border">
              <table className="w-full border-collapse text-sm">
                <thead>
                  <tr className="bg-surface">
                    <th className="border-b border-border px-2 py-1.5 text-left font-medium text-content-muted">
                      Started
                    </th>
                    <th className="border-b border-border px-2 py-1.5 text-right font-medium text-content-muted">
                      Frames
                    </th>
                    <th className="border-b border-border px-2 py-1.5 text-right font-medium text-content-muted">
                      Size
                    </th>
                    <th className="border-b border-border px-2 py-1.5 text-left font-medium text-content-muted">
                      Sources
                    </th>
                    <th className="border-b border-border px-2 py-1.5 text-right font-medium text-content-muted">
                      Rate
                    </th>
                    <th className="border-b border-border px-2 py-1.5 text-right font-medium text-content-muted">
                      Failed
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {sessions.length === 0 ? (
                    <tr>
                      <td colSpan={6} className="px-2 py-2 text-xs text-content-muted">
                        No receive sessions yet.
                      </td>
                    </tr>
                  ) : (
                    sessions.map((s) => {
                      const spanSecs = (Date.parse(s.finishedAt) - Date.parse(s.startedAt)) / 1000;
                      const rate = spanSecs > 0 ? formatRate(s.bytes / spanSecs) : '—';
                      const sources = s.sources
                        .map((src) => src.memberName ?? src.deviceName ?? src.device.slice(0, 8))
                        .join(', ');
                      return (
                        <tr key={s.id} className="border-b border-border last:border-b-0">
                          <td className="px-2 py-1.5 text-content-muted">{formatTimestamp(s.startedAt)}</td>
                          <td className="px-2 py-1.5 text-right">{s.frames}</td>
                          <td className="px-2 py-1.5 text-right">{formatBytes(s.bytes)}</td>
                          <td className="px-2 py-1.5">{sources}</td>
                          <td className="px-2 py-1.5 text-right">{rate}</td>
                          <td className={`px-2 py-1.5 text-right ${s.failed > 0 ? 'text-error' : ''}`}>
                            {s.failed}
                          </td>
                        </tr>
                      );
                    })
                  )}
                </tbody>
              </table>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
