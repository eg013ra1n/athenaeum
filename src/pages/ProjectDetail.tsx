import { useCallback, useEffect, useMemo, useState } from 'react';
import { useParams, useSearchParams } from 'react-router-dom';
import { ExternalLink, Loader2, Monitor, Send, Target } from 'lucide-react';
import { api } from '../api';
import { HistoryNav } from '../components/HistoryNav';
import { useSessionState } from '../contexts/SessionStateContext';
import { useCollabExchange } from '../contexts/CollabExchangeContext';
import { openUrl } from '../api/desktop';
import { safeExternalUrl } from '../utils/externalUrl';
import { useNotifications } from '../contexts/NotificationContext';
import AutoReplicateBar from '../components/collab/AutoReplicateBar';
import UpdateRequired from '../components/collab/UpdateRequired';
import CollabLiveStatus from '../components/collab/CollabLiveStatus';
import { formatBytes } from '../components/collab/format';
import { ConfirmDialog } from '../components/ConfirmDialog';
import OverviewTab from '../components/collab/project/OverviewTab';
import MyFramesTab, { type Segment } from '../components/collab/project/MyFramesTab';
import LibraryTab, { libraryInFlight, libraryToCome } from '../components/collab/project/LibraryTab';
import MembersTab from '../components/collab/project/MembersTab';
import ExchangeTab from '../components/collab/project/ExchangeTab';
import ModerationTab from '../components/collab/project/ModerationTab';
import FrameDrawer from '../components/collab/project/FrameDrawer';
import RepublishGuardDialog from '../components/collab/project/RepublishGuardDialog';
import { deviceLabel, leading, OTHER_DEVICE, usePublishing } from '../components/collab/project/usePublishing';
import { fromLibrary, fromOwn, ownFrameKey, type FrameVM } from '../components/collab/project/frames';
import type {
  MemberSummary,
  OwnFrameRow,
  ProjectDetail as Detail,
  ProjectFrameView,
} from '../types/models';

type Tab = 'overview' | 'mine' | 'library' | 'members' | 'exchange' | 'moderation';

const TAB_LABEL: Record<Tab, string> = {
  overview: 'Overview',
  mine: 'My frames',
  library: 'Library',
  members: 'Members',
  exchange: 'Exchange',
  moderation: 'Moderation',
};

/** Every accepted `?tab=` value (and stored session value) → its tab. The
 *  old four-tab page's ids stay valid: `receive` (collab notifications,
 *  `CollabAttention`) → Library, `contribute` → My frames; `moderation` and
 *  `overview` kept their names. A Map, so an arbitrary string can never hit
 *  an `Object.prototype` key. */
const TAB_ALIASES = new Map<string, Tab>([
  ['overview', 'overview'],
  ['mine', 'mine'],
  ['library', 'library'],
  ['members', 'members'],
  ['exchange', 'exchange'],
  ['moderation', 'moderation'],
  ['receive', 'library'],
  ['contribute', 'mine'],
]);

function resolveTab(v: string | null | undefined): Tab | null {
  return v ? (TAB_ALIASES.get(v) ?? null) : null;
}

// Rough per-frame size for the PRE-publish confirm estimate only (a calibrated
// 32-bit-float light frame). The exact size is measured when each frame is
// calibrated; the dialog labels this figure "estimated" so it never reads as
// an authoritative stored value (S6).
const APPROX_FRAME_BYTES = 45 * 1024 * 1024;

const BADGE = 'rounded-full px-1.5 text-[10px] font-medium';

/**
 * A collab project: the header (title, target, publishing device, live
 * status, portal link, auto-replication) and six tabs — Overview, My frames,
 * Library, Members, Exchange, Moderation. The shell owns the loads shared by
 * several tabs, the tab/segment state and deep links, the frame drawer, and
 * the publish confirm + republish guard (orchestration in `usePublishing`).
 * Every tab body is its own component under `components/collab/project/`.
 */
export default function ProjectDetail() {
  const { id } = useParams();
  // Keyed on the project: the router keeps this element mounted across
  // `/projects/a` → `/projects/b`, and nothing of one project (rows, drawer,
  // a pending confirm's frame ids, busy flags) may carry over to the next.
  return <ProjectPage key={id} id={id} />;
}

function ProjectPage({ id }: { id: string | undefined }) {
  const { notify } = useNotifications();
  const { state: exchange } = useCollabExchange();
  const [detail, setDetail] = useState<Detail | null>(null);
  const [missing, setMissing] = useState(false);
  const [own, setOwn] = useState<OwnFrameRow[] | null>(null);
  const [ownError, setOwnError] = useState(false);
  const [frames, setFrames] = useState<ProjectFrameView[] | null>(null);
  const [framesError, setFramesError] = useState(false);
  const [members, setMembers] = useState<MemberSummary[] | null>(null);
  const [membersError, setMembersError] = useState(false);
  const [drawer, setDrawer] = useState<FrameVM | null>(null);
  /** The publish confirm's requested frame ids; `null` = closed. */
  const [publishIds, setPublishIds] = useState<number[] | null>(null);
  /** The republish guard's request (`ids: null` = "all"); `null` = closed. */
  const [republishReq, setRepublishReq] = useState<{ ids: number[] | null } | null>(null);
  const [switchConfirm, setSwitchConfirm] = useState(false);

  // Session-scoped so stepping into a linked object and back returns to the
  // tab and segment you were on. A value stored by the old four-tab page
  // resolves through the same alias table as a deep link.
  const [storedTab, setTab] = useSessionState<string>('projectDetail.tab', 'overview');
  const [segment, setSegment] = useSessionState<Segment>('projectDetail.segment', 'ready');

  // `?tab=…` (a collab notification's link, `CollabAttention`, the frame
  // set's Project block) jumps to that tab on arrival, then cleans the URL.
  const [searchParams, setSearchParams] = useSearchParams();
  useEffect(() => {
    const t = resolveTab(searchParams.get('tab'));
    if (!t) return;
    setTab(t);
    const next = new URLSearchParams(searchParams);
    next.delete('tab');
    setSearchParams(next, { replace: true });
  }, [searchParams, setSearchParams, setTab]);

  // Detail comes from the local cache of VERIFIED snapshots (core owns
  // verification). Its failure means "not in my local list".
  const loadDetail = useCallback(async () => {
    if (!id) return;
    setMissing(false);
    try {
      setDetail(await api.invoke<Detail>('get_collab_project_detail', { projectId: id }));
    } catch (err) {
      console.error('[projects] detail load failed:', err);
      setMissing(true);
    }
  }, [id]);

  const loadOwn = useCallback(async () => {
    if (!id) return;
    setOwnError(false);
    try {
      setOwn(await api.invoke<OwnFrameRow[]>('list_project_own_frames', { projectId: id }));
    } catch (err) {
      console.error('[projects] list own frames failed:', err);
      setOwnError(true);
    }
  }, [id]);

  // The project's manifest mirror: the Library and Moderation tabs' rows and
  // the published volume the auto-replication bar shows.
  const loadLibrary = useCallback(async () => {
    if (!id) return;
    setFramesError(false);
    try {
      setFrames(await api.invoke<ProjectFrameView[]>('list_collab_frames', { projectId: id }));
    } catch (err) {
      console.error('[projects] list frames failed:', err);
      setFramesError(true);
    }
  }, [id]);

  useEffect(() => {
    void loadDetail();
  }, [loadDetail]);

  useEffect(() => {
    void loadOwn();
  }, [loadOwn]);

  // Re-read on every tab change, as the old page did — cheap, and it keeps
  // the Library badge and the replication bar's volume current.
  useEffect(() => {
    void loadLibrary();
  }, [loadLibrary, storedTab]);

  // Core emits `collab-published` at the end of EVERY publish run of a
  // project — manual, republish and the background auto-publish — so this is
  // the one place that keeps own frames (Ready counts, `Publish all N`), the
  // library (the replication bar's published volume) and the card current
  // after a run nobody clicked. StrictMode-safe listener pattern (CLAUDE.md);
  // the loaders are stable per project (the page is keyed on `id`).
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<{ projectId: string }>('collab-published', (p) => {
        if (cancelled || p.projectId !== id) return;
        void loadOwn();
        void loadLibrary();
        void loadDetail();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[projects] collab-published listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [id, loadOwn, loadLibrary, loadDetail]);

  // Loaded once here for the Overview and Exchange tabs; the Members tab
  // refreshes it through `onMembers` whenever it mounts.
  useEffect(() => {
    if (!id) return;
    let cancelled = false;
    setMembersError(false);
    api
      .invoke<MemberSummary[]>('get_collab_member_summary', { projectId: id })
      .then((list) => {
        if (!cancelled) setMembers(list);
      })
      .catch((err) => {
        console.error('[projects] member summary failed:', err);
        if (!cancelled) setMembersError(true);
      });
    return () => {
      cancelled = true;
    };
  }, [id]);

  const publishing = usePublishing(id, {
    reloadDetail: loadDetail,
    reloadOwn: loadOwn,
    onCard: (card) => setDetail((d) => (d ? { ...d, card } : d)),
    closeConfirm: () => {
      setPublishIds(null);
      setRepublishReq(null);
    },
  });

  const toCome = useMemo(
    () => libraryToCome(frames, libraryInFlight(exchange.projects, id ?? '')),
    [frames, exchange.projects, id],
  );

  // The drawer renders a FRESH view of its open frame, never the possibly
  // stale snapshot `onOpen` captured: an own frame's key found in the latest
  // `own` wins (so its own Restore/Exclude reads back immediately — a
  // `FrameDrawer` kept mounted across the reload never sees old props
  // otherwise), then a library frame's key found in the latest `frames`
  // (`fromLibrary`, the same builder the Library badge's `toCome` uses).
  // Only when the row can't be found this render — a moderation row (no
  // `own`/`lib` mirror to re-derive from) or a row that vanished entirely —
  // does the original snapshot render as a fallback.
  const drawerFrame: FrameVM | null = useMemo(() => {
    if (!drawer) return null;
    const ownRow = own?.find((r) => ownFrameKey(r) === drawer.key);
    if (ownRow) return fromOwn(ownRow);
    const libRow = frames?.find((f) => f.frameUuid === drawer.key);
    if (libRow) return fromLibrary(libRow, libraryInFlight(exchange.projects, id ?? ''));
    return drawer;
  }, [drawer, own, frames, exchange.projects, id]);

  const openPortal = async (path: string) => {
    if (!detail) return;
    const candidate = `${detail.portalBase}${path}`;
    const safe = safeExternalUrl(candidate);
    if (!safe) {
      console.error('[projects] refused non-http(s) portal url:', candidate);
      notify({
        title: 'Could not open the portal',
        detail: 'The configured hub address is not a valid web address.',
        kind: 'project',
        tone: 'warning',
      });
      return;
    }
    await openUrl(safe);
  };

  if (missing)
    return (
      <p className="p-6 text-sm text-content-muted">
        This project is not in your local list — refresh the Projects page.
      </p>
    );
  if (!detail || !id) return <p className="p-6 text-content-muted">Loading…</p>;

  const c = detail.card;
  const portalPath = c.coordinator ? `/p/${c.slug}/admin` : `/p/${c.slug}`;
  const canReceive = c.dataRole === 'send_receive' || c.coordinator;
  const canModerate = c.coordinator && c.requireApproval;
  const needsApproval = c.requireApproval && !c.coordinator;
  const coordinatorName = detail.members.find((m) => m.coordinator)?.displayName ?? 'the coordinator';
  // The project's published volume, client-side from the rows already listed.
  const publishedBytes =
    frames === null
      ? null
      : frames.filter((f) => f.own && f.state === 'published').reduce((sum, f) => sum + f.byteSize, 0);
  const ownRows = own ?? [];
  const readyCount = ownRows.filter((r) => r.segment === 'ready').length;
  const publishedRows = ownRows.filter((r) => r.segment === 'published');
  const canRepublish = publishedRows.length > 0;

  const tabs: Tab[] = [
    'overview',
    'mine',
    ...(canReceive ? (['library'] as const) : []),
    'members',
    'exchange',
    ...(canModerate ? (['moderation'] as const) : []),
  ];
  const requested = resolveTab(storedTab) ?? 'overview';
  const activeTab: Tab = tabs.includes(requested) ? requested : 'overview';
  const badge: Partial<Record<Tab, { n: number; cls: string }>> = {
    mine: { n: own === null ? 0 : readyCount, cls: 'bg-accent/20 text-accent' },
    library: { n: toCome, cls: 'bg-accent/20 text-accent' },
    moderation: { n: c.pendingFrames, cls: 'bg-warning/20 text-warning' },
  };

  // The republish guard's figures and the ids it sends. "All" = the
  // published frames not excluded — sent EXPLICITLY, never as `null`: a null
  // republish runs over every gate candidate and would announce Ready frames
  // that were never published (controller ruling, fix round 1).
  let guard: { all: boolean; count: number; sourceBytes: number; ids: number[] } | null = null;
  if (republishReq) {
    if (republishReq.ids === null) {
      const targets = publishedRows.filter((r) => r.accepted !== false);
      guard = {
        all: true,
        count: targets.length,
        sourceBytes: targets.reduce((s, r) => s + r.byteSize, 0),
        ids: targets.map((r) => r.frameId),
      };
    } else {
      const wanted = new Set(republishReq.ids);
      guard = {
        all: false,
        count: republishReq.ids.length,
        sourceBytes: ownRows.filter((r) => wanted.has(r.frameId)).reduce((s, r) => s + r.byteSize, 0),
        ids: republishReq.ids,
      };
    }
  }

  // The device the switch takes the binding from: the card's (freshest), else
  // the one a refusal named.
  const switchFrom = c.publishingDevice
    ? deviceLabel(c.publishingDevice.name)
    : (publishing.refusedBy ?? OTHER_DEVICE);
  const switchButton = (
    <button
      type="button"
      onClick={() => setSwitchConfirm(true)}
      disabled={publishing.switchBusy}
      className="inline-flex items-center gap-1 rounded border border-border px-2 py-0.5 text-xs text-content-secondary transition-colors hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-50"
    >
      {publishing.switchBusy && <Loader2 size={11} className="animate-spin" />}
      Publish from this device
    </button>
  );

  // Shown inside My frames, under its toolbar: a publish error once its
  // confirm is closed, and the A6 publishing-device refusal.
  const refusal = (
    <>
      {publishing.publishError && publishIds === null && (
        <p className="text-sm text-error">{publishing.publishError}</p>
      )}
      {publishing.refusedBy && !c.publishingHere && (
        <div
          data-testid="publishing-refusal"
          className="flex flex-wrap items-center gap-2 rounded border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-content"
        >
          <span className="break-words">{`${leading(publishing.refusedBy)} publishes new frames to this project.`}</span>
          {switchButton}
        </div>
      )}
    </>
  );

  const openTab = (t: string) => {
    const r = resolveTab(t);
    if (r) setTab(r);
  };

  return (
    <div className="space-y-4 p-6">
      <div className="flex flex-wrap items-center gap-3">
        <HistoryNav fallback="/projects" />
        <h1 className="truncate text-lg font-semibold text-content">{c.title}</h1>
        <span className="flex items-center gap-1 text-xs text-content-muted">
          <Target size={12} /> {c.targetName} · r {c.targetRadiusDeg.toFixed(1)}°
        </span>
        {c.coordinator && (
          <span className="rounded bg-accent/20 px-1.5 py-0.5 text-xs text-accent">coordinator</span>
        )}
        <div className="ml-auto">
          <CollabLiveStatus />
        </div>
        <button
          onClick={() => void openPortal(portalPath)}
          className="inline-flex items-center gap-1 text-sm text-content-secondary transition-colors hover:text-content"
        >
          Manage on portal <ExternalLink size={13} />
        </button>
      </div>

      {/* A6: one device of this account announces new frames here. `null`
          = nobody yet (the next device that publishes becomes it) — never
          "this device". */}
      <div className="flex flex-wrap items-center gap-2 text-xs text-content-muted">
        <Monitor size={12} className="shrink-0" />
        <span className="break-words">
          {c.publishingHere
            ? 'Publishing from this device'
            : c.publishingDevice
              ? `Publishing from ${deviceLabel(c.publishingDevice.name)}`
              : 'Nobody is publishing to this project yet'}
        </span>
        {!c.publishingHere && c.publishingDevice && switchButton}
      </div>

      {publishing.updateRequired && <UpdateRequired />}

      {/* Auto-replication is role-gated in core (`role_allows_replication`:
          coordinator or send_receive) exactly like the Library tab, so the bar
          shows on the same condition — a send-only member has nothing to pull. */}
      {canReceive && (
        <AutoReplicateBar
          projectId={id}
          autoReplicate={c.autoReplicate}
          publishedBytes={publishedBytes}
          onToggled={() => void loadDetail()}
        />
      )}

      <div role="tablist" className="flex flex-wrap gap-1 border-b border-border">
        {tabs.map((t) => {
          const b = badge[t];
          return (
            <button
              key={t}
              type="button"
              role="tab"
              aria-selected={activeTab === t}
              onClick={() => setTab(t)}
              className={`inline-flex items-center gap-1.5 px-4 py-2 text-sm transition-colors ${
                activeTab === t
                  ? 'border-b-2 border-accent font-medium text-content'
                  : 'text-content-muted hover:text-content-secondary'
              }`}
            >
              {TAB_LABEL[t]}
              {b && b.n > 0 && (
                <>
                  {' '}
                  <span className={`${BADGE} ${b.cls}`}>{b.n}</span>
                </>
              )}
            </button>
          );
        })}
      </div>

      {activeTab === 'overview' && (
        <>
          {ownError && own === null && (
            <p className="text-sm text-error">Could not load your frames — see console.</p>
          )}
          {membersError && members === null && (
            <p className="text-sm text-error">Could not load the members — see console.</p>
          )}
          <OverviewTab
            projectId={id}
            goals={detail.goals}
            members={members}
            own={own}
            ownError={ownError}
            libraryToCome={canReceive ? toCome : 0}
            pending={c.pendingFrames}
            canModerate={canModerate}
            thresholds={detail.thresholds}
            thresholdsVersion={detail.thresholdsVersion}
            onOpenSegment={(s) => {
              setSegment(s);
              setTab('mine');
            }}
            onOpenTab={openTab}
          />
        </>
      )}

      {activeTab === 'mine' && (
        <MyFramesTab
          projectId={id}
          rows={own}
          error={ownError}
          links={detail.links}
          autoPublish={c.autoPublish}
          segment={segment}
          onSegment={setSegment}
          onReload={() => void loadOwn()}
          onDetailReload={() => void loadDetail()}
          onRequestPublish={(ids) => {
            publishing.clearPublishError();
            setPublishIds(ids);
          }}
          publishBusy={publishing.publishBusy}
          onRequestRepublish={(ids) => {
            publishing.clearRepublishError();
            setRepublishReq({ ids });
          }}
          republishBusy={publishing.republishBusy}
          canRepublish={canRepublish}
          coordinator={c.coordinator}
          republishError={republishReq ? null : publishing.republishError}
          refusal={refusal}
          onOpen={setDrawer}
        />
      )}

      {activeTab === 'library' && (
        <LibraryTab
          projectId={id}
          projectTitle={c.title}
          frames={frames}
          error={framesError}
          reload={() => void loadLibrary()}
          coordinator={c.coordinator}
          onOpen={setDrawer}
        />
      )}

      {activeTab === 'members' && (
        <MembersTab
          projectId={id}
          onMembers={(m) => {
            setMembers(m);
            setMembersError(false);
          }}
        />
      )}

      {activeTab === 'exchange' && <ExchangeTab projectId={id} canReceive={canReceive} members={members} />}

      {activeTab === 'moderation' && (
        <ModerationTab
          projectId={id}
          library={frames}
          onDecided={() => {
            void loadDetail();
            void loadLibrary();
          }}
          onOpen={setDrawer}
        />
      )}

      {drawerFrame && (
        <FrameDrawer
          key={drawerFrame.key}
          projectId={id}
          frame={drawerFrame}
          coordinator={c.coordinator}
          onClose={() => setDrawer(null)}
          onChanged={() => {
            void loadOwn();
            void loadLibrary();
          }}
        />
      )}

      <ConfirmDialog
        isOpen={switchConfirm}
        title="Publish from this device?"
        message={`${leading(switchFrom)} will stop publishing new frames to this project; it can still update the frames it already published.`}
        confirmText="Switch"
        onConfirm={() => {
          setSwitchConfirm(false);
          void publishing.switchHere();
        }}
        onCancel={() => setSwitchConfirm(false)}
      />

      {publishIds && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
          onClick={() => !publishing.publishBusy && setPublishIds(null)}
        >
          <div
            className="w-[30rem] max-w-[90vw] rounded-lg border border-border bg-surface p-4"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="mb-2 flex items-center gap-2">
              <Send size={16} className="text-accent" />
              <h2 className="font-medium text-content">Publish to {c.title}</h2>
            </div>
            <p className="mb-2 text-sm text-content-secondary">
              {publishIds.length} passing {publishIds.length === 1 ? 'frame' : 'frames'} will be calibrated and
              announced to the project.
            </p>
            <p className="mb-2 text-xs text-content-muted">
              Estimated size ≈ {formatBytes(publishIds.length * APPROX_FRAME_BYTES)} — the exact size is
              measured when each frame is generated.
            </p>
            {needsApproval && (
              <p className="mb-2 text-xs text-warning">
                This project requires approval — your contribution goes to {coordinatorName} for
                review.
              </p>
            )}
            {publishing.publishError && <p className="mb-2 text-sm text-error">{publishing.publishError}</p>}
            <div className="mt-3 flex justify-end gap-2">
              <button
                type="button"
                onClick={() => setPublishIds(null)}
                disabled={publishing.publishBusy}
                className="rounded border border-border px-3 py-1.5 text-sm text-content-secondary transition-colors hover:bg-surface-hover disabled:opacity-50"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={() => void publishing.publish(publishIds)}
                disabled={publishing.publishBusy}
                className="inline-flex items-center gap-1 rounded bg-accent px-3 py-1.5 text-sm text-surface transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
              >
                {publishing.publishBusy && <Loader2 size={12} className="animate-spin" />} Publish
              </button>
            </div>
          </div>
        </div>
      )}

      {republishReq && guard && (
        <RepublishGuardDialog
          count={guard.count}
          sourceBytes={guard.sourceBytes}
          all={guard.all}
          busy={publishing.republishBusy}
          error={publishing.republishError}
          onConfirm={() => void publishing.republish(guard.ids)}
          onCancel={() => setRepublishReq(null)}
        />
      )}
    </div>
  );
}
