import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useParams, useSearchParams } from 'react-router-dom';
import { Loader2 } from 'lucide-react';
import { api } from '../api';
import { HistoryNav } from '../components/HistoryNav';
import { useSessionState } from '../contexts/SessionStateContext';
import { useCollabExchange } from '../contexts/CollabExchangeContext';
import { useCollabLiveState } from '../hooks/useCollabLiveState';
import { openUrl } from '../api/desktop';
import { safeExternalUrl } from '../utils/externalUrl';
import { useNotifications } from '../contexts/NotificationContext';
import UpdateRequired from '../components/collab/UpdateRequired';
import CollabLiveStatus from '../components/collab/CollabLiveStatus';
import { ConfirmDialog } from '../components/ConfirmDialog';
import { Button, Chip, PanelLayout } from '../components/ui';
import ProjectSettingsCard from '../components/collab/project/ProjectSettingsCard';
import { useCollabPublishRun } from '../components/collab/project/useCollabPublishRun';
import { MemberColorsProvider } from '../components/collab/project/MemberColorsContext';
import OverviewTab from '../components/collab/project/OverviewTab';
import type { NavTarget } from '../components/collab/project/attention';
import { EMPTY_FACETS, type Facets } from '../components/collab/project/table/model';
import MyFramesTab, { type Segment } from '../components/collab/project/MyFramesTab';
import LibraryTab, { libraryInFlight, libraryToCome } from '../components/collab/project/LibraryTab';
import MembersTab from '../components/collab/project/MembersTab';
import ExchangeTab from '../components/collab/project/ExchangeTab';
import ModerationTab from '../components/collab/project/ModerationTab';
import FramePanel from '../components/collab/project/FramePanel';
import PublishConfirmDialog from '../components/collab/project/PublishConfirmDialog';
import RepublishGuardDialog from '../components/collab/project/RepublishGuardDialog';
import { deviceLabel, leading, OTHER_DEVICE, usePublishing } from '../components/collab/project/usePublishing';
import { fromLibrary, fromOwn, ownFrameKey, type FrameVM } from '../components/collab/project/frames';
import type {
  AccountStatus,
  CollabPeersChanged,
  CollabProjectSynced,
  MemberSummary,
  OwnFrameRow,
  ProjectDetail as Detail,
  ProjectFrameView,
  CollabPublishFinished,
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

// `collab-peers-changed` fires per event, throttled core-side to one per
// project per second (`LANDED_BURST`, `runtime.rs`). These mirror that on the
// frontend as a schedule-if-none-pending throttle: the FIRST event schedules
// the reload; further events inside the window are absorbed and never
// restart it. `loadOwn` is the expensive gate read, so it gets its own,
// longer window.
const PEERS_RELOAD_MS = 1000;
const OWN_RELOAD_MS = 5000;

/** The colour provider's members while the summary loads — one stable array,
 *  so the lookup (and every dot on the page) is not rebuilt each render. */
const NO_MEMBERS: MemberSummary[] = [];

/** This account: a published own frame's publisher, else the member whose
 *  devices include this device. `null` while unknown — every member then
 *  takes the palette from slot 0 (`memberColor`). */
export function resolveSelfAccount(
  frames: ProjectFrameView[] | null,
  members: MemberSummary[] | null,
  deviceId: string | null,
): string | null {
  const own = frames?.find((f) => f.own)?.publisherAccountId;
  if (own) return own;
  if (!deviceId || !members) return null;
  return members.find((m) => m.devices.some((d) => d.device === deviceId))?.accountId ?? null;
}

/**
 * A collab project (spec 2026-09-30 §8): the header on the app's page pattern
 * (back/forward, title, target subtitle, role chip, the live pill that also
 * runs Sync, the portal link) and six tabs — Overview, My
 * frames, Library, Members, Exchange, Moderation. The project's local settings
 * (publishing device, mode, auto-replicate) are the Overview's settings card. The shell owns the loads
 * shared by several tabs, the tab/segment state and deep links, the frame
 * panel (docked beside the frame tables, `PanelLayout`), the member colours
 * (`MemberColorsProvider`), and the publish confirm + republish guard
 * (orchestration in `usePublishing`). Every tab body is its own component
 * under `components/collab/project/`.
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
  // Unknown (not read yet, or the read failed) counts as running: the page
  // never claims the live exchange is off without being told so.
  const liveState = useCollabLiveState();
  const liveRunning = liveState === null || liveState === 'live';
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
  // Write-only handles on the tables' facet session keys (`collab.<project>.<tableId>.facets`).
  const [, setReadyFacets] = useSessionState<Facets>(`collab.${id}.ready.facets`, EMPTY_FACETS);
  const [, setReviewFacets] = useSessionState<Facets>(`collab.${id}.review.facets`, EMPTY_FACETS);
  const [, setHeldFacets] = useSessionState<Facets>(`collab.${id}.held.facets`, EMPTY_FACETS);
  const [, setPublishedFacets] = useSessionState<Facets>(`collab.${id}.published.facets`, EMPTY_FACETS);
  const [, setLibraryFacets] = useSessionState<Facets>(`collab.${id}.library.facets`, EMPTY_FACETS);

  // `?tab=…` (a collab notification's link, `CollabAttention`, the frame
  // set's Project block) jumps to that tab on arrival, then cleans the URL.
  const [searchParams, setSearchParams] = useSearchParams();
  useEffect(() => {
    const t = resolveTab(searchParams.get('tab'));
    if (!t) return;
    // A tab change closes the frame panel (spec §6.1).
    setDrawer(null);
    // `&segment=` picks the My frames segment (a run notification's link).
    const seg = searchParams.get('segment');
    if (seg === 'ready' || seg === 'review' || seg === 'published' || seg === 'held') setSegment(seg);
    setTab(t);
    const next = new URLSearchParams(searchParams);
    next.delete('tab');
    next.delete('segment');
    setSearchParams(next, { replace: true });
  }, [searchParams, setSearchParams, setTab, setSegment]);

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

  // The project's manifest mirror: the Library and Moderation tabs' rows, the
  // Library count pill and the member-colour self lookup.
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
  // the Library count pill current.
  useEffect(() => {
    void loadLibrary();
  }, [loadLibrary, storedTab]);

  // Bumped to make the Moderation and Exchange tabs re-fetch their own lists:
  // by the pill's confirmation, a changed synced report and a finished run.
  const [syncToken, setSyncToken] = useState(0);

  // Core emits `collab-publish-finished` at the end of EVERY publish run of a
  // project — manual, republish and the background auto-publish — so this is
  // the one place that keeps own frames (Ready counts, `Publish all N`), the
  // library (the Library count pill), the card, the members and the
  // Moderation / Exchange tabs (`syncToken`) current after a run nobody
  // clicked (spec §6.5: the finished event reloads everything).
  // StrictMode-safe listener pattern (CLAUDE.md); the loaders are stable per
  // project (the page is keyed on `id`).
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<CollabPublishFinished>('collab-publish-finished', (p) => {
        if (cancelled || p.projectId !== id) return;
        void loadOwn();
        void loadLibrary();
        void loadDetail();
        void loadMembersRef.current();
        setSyncToken((n) => n + 1);
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[projects] collab-publish-finished listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [id, loadOwn, loadLibrary, loadDetail]);

  // Loaded once here for the Overview, Members and Exchange tabs; the
  // throttled `collab-peers-changed` reload calls this directly (below).
  const loadMembers = useCallback(async () => {
    if (!id) return;
    setMembersError(false);
    try {
      setMembers(await api.invoke<MemberSummary[]>('get_collab_member_summary', { projectId: id }));
    } catch (err) {
      console.error('[projects] member summary failed:', err);
      setMembersError(true);
    }
  }, [id]);

  useEffect(() => {
    void loadMembers();
  }, [loadMembers]);

  // This device's id, for `resolveSelfAccount` — this account's member colour
  // is the accent everywhere on the page (spec §4.4).
  const [deviceId, setDeviceId] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    api
      .invoke<AccountStatus>('account_status')
      .then((s) => {
        if (!cancelled) setDeviceId(s?.deviceId ?? null);
      })
      .catch((err) => console.error('[projects] account_status failed:', err));
    return () => {
      cancelled = true;
    };
  }, []);
  const selfAccountId = useMemo(() => resolveSelfAccount(frames, members, deviceId), [frames, members, deviceId]);

  // Presence/holder changes (core: `collab-peers-changed`). Loaders are read
  // through refs so the listener can subscribe once per project
  // (StrictMode-safe, CLAUDE.md pattern) with `id` as the effect's only real
  // dependency — it never changes across this component's life
  // (`ProjectDetail` keys `ProjectPage` on it).
  //
  // Fix round 1 (Important, controller-ruled): this used to be a trailing
  // debounce that RESTARTED on every event. The core's `PeerBurst` sends one
  // of these per project roughly every second for as long as a transfer
  // keeps landing frames at a peer (every landed frame is a new holder) — a
  // restarting debounce would never let the 5s `ownTimer` fire at all for
  // the whole length of a transfer, freezing My frames, and the 1s
  // `peersTimer` would race the next event forever. Both are now
  // schedule-if-none-pending throttles, the same shape as the core's
  // `PeerBurst`: an event schedules a timer only when none is already
  // pending; further events while one is pending are absorbed (they do NOT
  // reset it); a fired timer clears its own pending marker so the next event
  // schedules again. A burst still collapses to one reload each.
  const loadLibraryRef = useRef(loadLibrary);
  loadLibraryRef.current = loadLibrary;
  const loadMembersRef = useRef(loadMembers);
  loadMembersRef.current = loadMembers;
  const loadOwnRef = useRef(loadOwn);
  loadOwnRef.current = loadOwn;
  const loadDetailRef = useRef(loadDetail);
  loadDetailRef.current = loadDetail;

  // The pill's confirmation (and a changed synced report below) re-read every
  // list; the token makes the Moderation and Exchange tabs re-fetch their own.
  const reloadAll = useCallback(() => {
    void loadDetail();
    void loadOwn();
    void loadLibrary();
    void loadMembers();
    setSyncToken((n) => n + 1);
  }, [loadDetail, loadOwn, loadLibrary, loadMembers]);

  // The hub's per-project confirmation (`collab-project-synced`): when it says
  // something changed, refresh the page on the same throttles as the peers
  // listener above (schedule-if-none-pending).
  useEffect(() => {
    if (!id) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    let pageTimer: ReturnType<typeof setTimeout> | undefined;
    let ownTimer: ReturnType<typeof setTimeout> | undefined;
    api
      .listen<CollabProjectSynced>('collab-project-synced', (p) => {
        if (cancelled || p.projectId !== id || !p.changed) return;
        if (pageTimer === undefined) {
          pageTimer = setTimeout(() => {
            pageTimer = undefined;
            void loadDetailRef.current();
            void loadLibraryRef.current();
            void loadMembersRef.current();
            setSyncToken((n) => n + 1);
          }, PEERS_RELOAD_MS);
        }
        if (ownTimer === undefined) {
          ownTimer = setTimeout(() => {
            ownTimer = undefined;
            void loadOwnRef.current();
          }, OWN_RELOAD_MS);
        }
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[projects] collab-project-synced listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
      if (pageTimer !== undefined) clearTimeout(pageTimer);
      if (ownTimer !== undefined) clearTimeout(ownTimer);
    };
  }, [id]);

  useEffect(() => {
    if (!id) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    let peersTimer: ReturnType<typeof setTimeout> | undefined;
    let ownTimer: ReturnType<typeof setTimeout> | undefined;
    api
      .listen<CollabPeersChanged>('collab-peers-changed', (p) => {
        if (cancelled || p.projectId !== id) return;
        if (peersTimer === undefined) {
          peersTimer = setTimeout(() => {
            peersTimer = undefined;
            void loadLibraryRef.current();
            void loadMembersRef.current();
          }, PEERS_RELOAD_MS);
        }
        if (ownTimer === undefined) {
          ownTimer = setTimeout(() => {
            ownTimer = undefined;
            void loadOwnRef.current();
          }, OWN_RELOAD_MS);
        }
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[projects] collab-peers-changed listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
      if (peersTimer !== undefined) clearTimeout(peersTimer);
      if (ownTimer !== undefined) clearTimeout(ownTimer);
    };
  }, [id]);

  const run = useCollabPublishRun(id);
  const publishing = usePublishing(id, {
    reloadDetail: loadDetail,
    reloadOwn: loadOwn,
    onCard: (card) => setDetail((d) => (d ? { ...d, card } : d)),
  });

  const toCome = useMemo(
    () => libraryToCome(frames, libraryInFlight(exchange.projects, id ?? '')),
    [frames, exchange.projects, id],
  );

  // The drawer renders a FRESH view of its open frame, never the possibly
  // stale snapshot `onOpen` captured: an own frame's key found in the latest
  // `own` wins (so its own Restore/Exclude reads back immediately — a
  // `FramePanel` kept mounted across the reload never sees old props
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
    try {
      await openUrl(safe);
    } catch (err) {
      console.error('[projects] open portal failed:', err);
      notify({
        title: 'Could not open the portal',
        detail: err instanceof Error ? err.message : String(err),
        kind: 'project',
        tone: 'warning',
        hasErrors: true,
      });
    }
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
  const canModerate = c.canModerate;
  const needsApproval = c.requireApproval && !c.canModerate;
  const coordinatorName = detail.members.find((m) => m.coordinator)?.displayName ?? 'the coordinator';
  const ownRows = own ?? [];
  const publishBytes = ownRows
    .filter((r) => publishIds?.includes(r.frameId))
    .reduce((sum, r) => sum + (r.calibratedBytes ?? r.byteSize), 0);
  const readyCount = ownRows.filter((r) => r.segment === 'ready').length;
  const reviewCount = ownRows.filter((r) => r.segment === 'review').length;
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
  // Tab count pills (spec §8): "136 ready", "278 to go", the pending count.
  const badge: Partial<Record<Tab, { n: number; text: string; warn?: boolean }>> = {
    mine: { n: readyCount + reviewCount, text: [readyCount > 0 && `${readyCount} ready`, reviewCount > 0 && `${reviewCount} to review`].filter(Boolean).join(' · ') },
    library: { n: toCome, text: `${toCome} to go` },
    moderation: { n: c.pendingFrames, text: String(c.pendingFrames), warn: true },
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
        <p className="mb-2.5 text-[12.5px] text-error">{publishing.publishError}</p>
      )}
      {publishing.refusedBy && !c.publishingHere && (
        <div
          data-testid="publishing-refusal"
          className="mb-2.5 flex flex-wrap items-center gap-2 rounded border border-warning/40 bg-warning-muted px-2.5 py-2 text-[12px] text-content"
        >
          <span className="break-words">{`${leading(publishing.refusedBy)} publishes new frames to this project.`}</span>
          {switchButton}
        </div>
      )}
    </>
  );

  // Every tab change closes the frame panel (spec §6.1).
  const selectTab = (t: Tab) => {
    setDrawer(null);
    setTab(t);
  };
  const openTab = (t: string) => {
    const r = resolveTab(t);
    if (r) selectTab(r);
  };
  // A Needs-attention action: pre-set the target table's state facet (the
  // same session keys `ProjectFrameTable` reads), then open it.
  const openAttention = (t: NavTarget) => {
    const facets: Facets = { ...EMPTY_FACETS, state: t.state ?? null };
    if (t.kind === 'segment') {
      ({ ready: setReadyFacets, review: setReviewFacets, held: setHeldFacets, published: setPublishedFacets })[t.segment](facets);
      setSegment(t.segment);
      selectTab('mine');
    } else {
      if (t.tab === 'library') setLibraryFacets(facets);
      selectTab(t.tab);
    }
  };

  // The frame panel, docked beside the My frames / Library / Moderation
  // tables.
  const framePanel = drawerFrame ? (
    <FramePanel
      key={drawerFrame.key}
      projectId={id}
      frame={drawerFrame}
      canModerate={canModerate}
      thresholdsVersion={detail.thresholdsVersion}
      onClose={() => setDrawer(null)}
      onChanged={() => {
        void loadOwn();
        void loadLibrary();
      }}
    />
  ) : null;
  const toggleDrawer = (vm: FrameVM) => setDrawer((d) => (d?.key === vm.key ? null : vm));
  const activeKey = drawerFrame?.key ?? null;

  return (
    <MemberColorsProvider members={members ?? NO_MEMBERS} selfAccountId={selfAccountId}>
      <div className="space-y-0 p-6 text-[13px] leading-[1.4] [font-variant-numeric:tabular-nums]">
        {/* Row 1 (spec §8, U5): the app's page-header pattern. The row never
            wraps: the title is the one item that shrinks (ellipsis), so a long
            title can never push the pill and the portal link to a second row. */}
        <div className="flex items-center gap-x-3.5">
          <HistoryNav fallback="/projects" />
          <h2 className="min-w-0 truncate text-2xl font-bold text-content" title={c.title}>
            {c.title}
          </h2>
          <span className="shrink-0 whitespace-nowrap text-sm font-normal text-content-muted">
            ◎ {c.targetName} · r {c.targetRadiusDeg.toFixed(1)}° · {detail.members.length}{' '}
            {detail.members.length === 1 ? 'member' : 'members'}
          </span>
          {c.coordinator && (
            <Chip tone="info" className="shrink-0">
              coordinator
            </Chip>
          )}
          <span className="ml-auto flex shrink-0 items-center gap-3.5">
            <CollabLiveStatus variant="pill" projectId={id} syncedAt={c.syncedAt} onSynced={reloadAll} />
            <Button variant="link" onClick={() => void openPortal(portalPath)}>
              Manage on portal ↗
            </Button>
          </span>
        </div>

        {publishing.updateRequired && (
          <div className="mt-3">
            <UpdateRequired />
          </div>
        )}

        <div role="tablist" className="mt-3.5 flex gap-0.5 overflow-x-auto border-b border-border">
          {tabs.map((t) => {
            const b = badge[t];
            return (
              <button
                key={t}
                type="button"
                role="tab"
                aria-selected={activeTab === t}
                onClick={() => selectTab(t)}
                className={`inline-flex items-center gap-1.5 whitespace-nowrap border-b-2 px-3.5 py-2 ${
                  activeTab === t
                    ? 'border-accent font-semibold text-content'
                    : 'border-transparent text-content-faint hover:text-content-secondary'
                }`}
              >
                {TAB_LABEL[t]}
                {b && b.n > 0 && (
                  <>
                    {/* A space for the accessible name ("My frames 1 ready");
                        whitespace in a flex row renders nothing. */}{' '}
                    <span
                      className={`rounded-full px-[5px] text-[10.5px] font-medium ${
                        b.warn ? 'bg-warning-muted text-warning' : 'bg-surface-hover text-content-muted'
                      }`}
                    >
                      {b.text}
                    </span>
                  </>
                )}
              </button>
            );
          })}
        </div>

        <div className="pt-3.5">
          {activeTab === 'overview' && (
            <div className="space-y-4">
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
                library={canReceive ? frames : []}
                pending={c.pendingFrames}
                canModerate={canModerate}
                thresholds={detail.thresholds}
                thresholdsVersion={detail.thresholdsVersion}
                onOpenSegment={(s) => {
                  // A tile opens the whole segment: clear its state facet.
                  const clear = (f: Facets): Facets => ({ ...f, state: null });
                  ({ ready: setReadyFacets, review: setReviewFacets, held: setHeldFacets, published: setPublishedFacets })[s](clear);
                  setSegment(s);
                  selectTab('mine');
                }}
                onOpenTab={openTab}
                canReceive={canReceive}
                liveRunning={liveRunning}
                onReloadOwn={() => void loadOwn()}
                onAttention={openAttention}
                settings={
                  <ProjectSettingsCard
                    card={c}
                    canReceive={canReceive}
                    run={run}
                    liveState={liveState}
                    onChanged={() => void loadDetail()}
                    onSwitchHere={() => setSwitchConfirm(true)}
                    switchBusy={publishing.switchBusy}
                    onOpenMyFrames={() => selectTab('mine')}
                  />
                }
              />
            </div>
          )}

          {activeTab === 'mine' && (
            <PanelLayout panel={framePanel}>
              <MyFramesTab
                projectId={id}
                rows={own}
                error={ownError}
                links={detail.links}
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
                canModerate={canModerate}
                republishError={republishReq ? null : publishing.republishError}
                refusal={refusal}
                onOpen={toggleDrawer}
                activeKey={activeKey}
                run={run}
                onCalibrate={(ids) => void publishing.calibrate(ids)}
                calibrateBusy={publishing.calibrateBusy}
                calibrateError={publishing.calibrateError}
                onUpdate={(ids) => void publishing.publish(ids)}
              />
            </PanelLayout>
          )}

          {activeTab === 'library' && (
            <PanelLayout panel={framePanel}>
              <LibraryTab
                projectId={id}
                projectTitle={c.title}
                frames={frames}
                error={framesError}
                reload={() => void loadLibrary()}
                canModerate={canModerate}
                onOpen={toggleDrawer}
                activeKey={activeKey}
              />
            </PanelLayout>
          )}

          {activeTab === 'members' && (
            <MembersTab members={members} error={membersError} goals={detail.goals} />
          )}

          {activeTab === 'exchange' && <ExchangeTab projectId={id} canReceive={canReceive} syncToken={syncToken} />}

          {activeTab === 'moderation' && (
            <PanelLayout panel={framePanel}>
              <ModerationTab
                projectId={id}
                syncToken={syncToken}
                requireApproval={c.requireApproval}
                library={canReceive ? frames : []}
                libraryError={framesError}
                onDecided={() => {
                  void loadDetail();
                  void loadLibrary();
                }}
                onOpen={toggleDrawer}
                activeKey={activeKey}
              />
            </PanelLayout>
          )}
        </div>

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
          <PublishConfirmDialog
            title={c.title}
            count={publishIds.length}
            bytes={publishBytes}
            needsApproval={needsApproval}
            coordinatorName={coordinatorName}
            onConfirm={() => {
              // Closed at once (final-review ruling): the run panel and its
              // Cancel stay reachable for the whole run; a refusal shows in
              // My frames.
              const ids = publishIds;
              setPublishIds(null);
              void publishing.publish(ids);
            }}
            onCancel={() => setPublishIds(null)}
          />
        )}

        {republishReq && guard && (
          <RepublishGuardDialog
            count={guard.count}
            sourceBytes={guard.sourceBytes}
            all={guard.all}
            onConfirm={() => {
              const ids = guard.ids;
              setRepublishReq(null);
              void publishing.republish(ids);
            }}
            onCancel={() => setRepublishReq(null)}
          />
        )}
      </div>
    </MemberColorsProvider>
  );
}
