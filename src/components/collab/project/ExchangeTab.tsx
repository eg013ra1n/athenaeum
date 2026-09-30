import { useEffect, useRef, useState, type JSX } from 'react';
import { api } from '../../../api';
import { useCollabExchange } from '../../../contexts/CollabExchangeContext';
import { formatTimestamp } from '../../../utils/dateFormatting';
import { formatDurationPadded, formatRate, formatSize, pluralize } from '../format';
import { distinctMembers, flowKey, peerLabel, sumRate } from '../exchange/state';
import { PeerFlowRow } from '../exchange/PeerFlowRow';
import { useMemberColor } from './MemberColorsContext';
import { TD, TH } from './tableStyle';
import { Card, Chip, EmptyState, MemberDot } from '../../ui';
import type { CollabFramesLanded, FlowView, ReceiveSessionView } from '../../../types/models';

/**
 * Exchange tab — live per-peer rows in both directions, plus the receive
 * session history (Task 14, wave 2 "Exchange", mockup `liveHtml`/`peerRow`
 * and the Exchange tab's history table). `toGo`/`waitingForPublisher` come
 * from `state.summary[projectId]` (Task 8 ruling) so they survive the
 * project's flows going idle; flows come from `state.projects[projectId]`,
 * `[]` when the project has nothing live right now.
 */

export default function ExchangeTab({
  projectId,
  canReceive,
}: {
  projectId: string;
  canReceive: boolean;
}): JSX.Element {
  const colorOf = useMemberColor();
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
            color={colorOf(label.member)}
          />
        );
      })}
    </div>
  );

  return (
    <div className="flex flex-col gap-3.5">
      <Card title="Receiving" subtitle={recvSub}>
        {canReceive ? (
          recv.length === 0 ? (
            <EmptyState>Nothing is being received.</EmptyState>
          ) : (
            renderRows(recv)
          )
        ) : (
          <EmptyState>
            Contributors only send. Ask the coordinator for the Processor role to receive the project.
          </EmptyState>
        )}
      </Card>

      <Card title="Sending" subtitle={sendSub}>
        {send.length === 0 ? <EmptyState>Nothing is being sent.</EmptyState> : renderRows(send)}
      </Card>

      {canReceive && (
        <Card title="Received" subtitle="sessions · a session ends after 5 min without a landing">
          {sessionsError && (
            <p className="text-[12.5px] text-error">Could not load receive sessions — see console.</p>
          )}
          {sessions === null && !sessionsError && <EmptyState>Loading…</EmptyState>}
          {sessions !== null && sessions.length === 0 && <EmptyState>No sessions yet.</EmptyState>}
          {sessions !== null && sessions.length > 0 && (
            <div className="overflow-x-auto">
              <table className="w-full border-collapse text-[12.5px]">
                <thead>
                  <tr>
                    <th className={`${TH} text-left text-content-faint`}>Started</th>
                    <th className={`${TH} text-left text-content-faint`}>From</th>
                    <th className={`${TH} text-right text-content-faint`}>Frames</th>
                    <th className={`${TH} text-right text-content-faint`}>Size</th>
                    <th className={`${TH} text-right text-content-faint`}>Duration</th>
                    <th className={`${TH} text-right text-content-faint`}>Avg rate</th>
                    <th className={`${TH} text-left text-content-faint`}>Outcome</th>
                  </tr>
                </thead>
                <tbody>
                  {sessions.map((s) => {
                    const spanSecs = (Date.parse(s.finishedAt) - Date.parse(s.startedAt)) / 1000;
                    const rate = spanSecs > 0 ? formatRate(s.bytes / spanSecs) : '—';
                    return (
                      <tr key={s.id}>
                        <td className={`${TD} text-content-muted`}>{formatTimestamp(s.startedAt, { seconds: true })}</td>
                        <td className={TD}>
                          {s.sources.map((src, i) => {
                            const name = src.memberName ?? src.deviceName ?? src.device.slice(0, 8);
                            return (
                              <span key={src.device}>
                                {i > 0 && ', '}
                                <MemberDot color={colorOf(src.memberName)} /> {name}
                              </span>
                            );
                          })}
                        </td>
                        <td className={`${TD} text-right`}>{s.frames}</td>
                        <td className={`${TD} text-right`}>{formatSize(s.bytes)}</td>
                        <td className={`${TD} text-right`}>{formatDurationPadded(spanSecs)}</td>
                        <td className={`${TD} text-right`}>{rate}</td>
                        <td className={TD}>
                          {s.failed > 0 ? (
                            <Chip tone="warn">partial · {s.failed} failed</Chip>
                          ) : (
                            <Chip tone="ok">landed</Chip>
                          )}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}
        </Card>
      )}
    </div>
  );
}
