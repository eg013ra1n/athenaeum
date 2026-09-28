import { useCallback, useEffect, useState } from 'react';
import { useParams, useSearchParams } from 'react-router-dom';
import { ExternalLink, Loader2, Monitor, Plus, RefreshCw, Send, Target } from 'lucide-react';
import { api } from '../api';
import { HistoryNav } from '../components/HistoryNav';
import { useSessionState } from '../contexts/SessionStateContext';
import { openUrl } from '../api/desktop';
import { safeExternalUrl } from '../utils/externalUrl';
import { useNotifications } from '../contexts/NotificationContext';
import AutoReplicateBar from '../components/collab/AutoReplicateBar';
import LinkObjectDialog from '../components/collab/LinkObjectDialog';
import ReceiveTab from '../components/collab/ReceiveTab';
import ModerationQueue from '../components/collab/ModerationQueue';
import UpdateRequired from '../components/collab/UpdateRequired';
import CollabLiveStatus from '../components/collab/CollabLiveStatus';
import { formatBytes } from '../components/collab/format';
import { ConfirmDialog } from '../components/ConfirmDialog';
import type {
  FrameGateRow,
  GateReport,
  ProjectCard,
  ProjectDetail as Detail,
  ProjectFrameView,
  PublishResult,
} from '../types/models';

type Tab = 'contribute' | 'receive' | 'moderation' | 'overview';

function isTab(v: string | null): v is Tab {
  return v === 'contribute' || v === 'receive' || v === 'moderation' || v === 'overview';
}

// Rough per-frame size for the PRE-publish confirm estimate only (a calibrated
// 32-bit-float light frame). The exact size is measured when each frame is
// calibrated; the dialog labels this figure "estimated" so it never reads as
// an authoritative stored value (S6).
const APPROX_FRAME_BYTES = 45 * 1024 * 1024;

/** A hub call refused this build with the stable `collab_api_outdated`
 *  prefix (P17). */
function isOutdated(msg: string): boolean {
  return msg.startsWith('collab_api_outdated');
}

/** The backend's refusal while another publish run of the same project
 *  (manual, republish or the background auto-publish) is in progress —
 *  `api::collab::PUBLISH_BUSY_MSG`, owner decision 2026-09-24: refused,
 *  never queued. */
const PUBLISH_BUSY = 'publication of this project is already running';

function isPublishBusy(msg: string): boolean {
  return msg.includes(PUBLISH_BUSY);
}

/** Inline text + toast for a busy refusal: not a failure of the user's
 *  data, just "try again once the running one ends". */
const PUBLISH_BUSY_INLINE =
  'Publication of this project is already running — wait for it to finish, then try again.';

/** Amendment A6: another device of this account is the project's publishing
 *  device. A publish/republish rejects with
 *  `collab_publishing_device:<name>` (`account::client::publishing_device_msg`)
 *  when nothing else went out; the prefix is stable, the rest is the device
 *  name or `OTHER_DEVICE`. */
const PUBLISHING_DEVICE_PREFIX = 'collab_publishing_device:';

/** The name core uses for a bound device the hub reports without one
 *  (`account::client::publishing_device_label`). */
const OTHER_DEVICE = 'another device of this account';

function publishingDeviceRefusal(msg: string): string | null {
  if (!msg.startsWith(PUBLISHING_DEVICE_PREFIX)) return null;
  return msg.slice(PUBLISHING_DEVICE_PREFIX.length).trim() || OTHER_DEVICE;
}

/** A run that ALSO posted versions resolves Ok, with the refused new frames
 *  in `heldBack` carrying `publishingDevice` — the bound device's name (or
 *  `OTHER_DEVICE`). Keyed on that field only, never on the reason text. */
function heldForPublishingDevice(res: PublishResult | null | undefined): string | null {
  for (const frame of res?.heldBack ?? []) {
    if (frame.publishingDevice != null) return deviceLabel(frame.publishingDevice);
  }
  return null;
}

/** The bound device's name for display; never "this device". */
function deviceLabel(name: string | null | undefined): string {
  return name?.trim() || OTHER_DEVICE;
}

/** A device name at the start of a sentence. */
function leading(name: string): string {
  return name === OTHER_DEVICE ? 'Another device of this account' : name;
}

export default function ProjectDetail() {
  const { id } = useParams();
  const { notify } = useNotifications();
  const [detail, setDetail] = useState<Detail | null>(null);
  const [gate, setGate] = useState<GateReport | null>(null);
  const [gateError, setGateError] = useState(false);
  // Session-scoped so stepping into a linked object and back returns to the
  // tab you were on.
  const [tab, setTab] = useSessionState<Tab>('projectDetail.tab', 'contribute');
  // `?tab=receive` (from a collab notification's link, e.g. a replication
  // pause or a landed-frames toast — see `useCollabNotifications`) jumps to
  // that tab on arrival, then cleans the URL. Mirrors `FrameSetDetail`'s
  // `?tab=…` pattern.
  const [searchParams, setSearchParams] = useSearchParams();
  useEffect(() => {
    const tabParam = searchParams.get('tab');
    if (!isTab(tabParam)) return;
    setTab(tabParam);
    const next = new URLSearchParams(searchParams);
    next.delete('tab');
    setSearchParams(next, { replace: true });
  }, [searchParams, setSearchParams, setTab]);
  const [linkOpen, setLinkOpen] = useState(false);
  const [missing, setMissing] = useState(false);
  const [updateRequired, setUpdateRequired] = useState(false);
  const [frames, setFrames] = useState<ProjectFrameView[] | null>(null);
  const [framesError, setFramesError] = useState(false);
  const [publishConfirm, setPublishConfirm] = useState(false);
  const [publishBusy, setPublishBusy] = useState(false);
  const [publishError, setPublishError] = useState<string | null>(null);
  const [republishConfirm, setRepublishConfirm] = useState(false);
  const [republishBusy, setRepublishBusy] = useState(false);
  const [republishError, setRepublishError] = useState<string | null>(null);
  /** The publishing device a publish/republish was refused for (A6). */
  const [refusedBy, setRefusedBy] = useState<string | null>(null);
  const [switchConfirm, setSwitchConfirm] = useState(false);
  const [switchBusy, setSwitchBusy] = useState(false);

  const load = useCallback(async () => {
    if (!id) return;
    setMissing(false);
    // Detail comes from the local cache of VERIFIED snapshots (core owns
    // verification). Its failure means "not in my local list".
    let d: Detail;
    try {
      d = await api.invoke<Detail>('get_collab_project_detail', { projectId: id });
      setDetail(d);
    } catch (err) {
      console.error('[projects] detail load failed:', err);
      setMissing(true);
      return;
    }
    // The gate is evaluated locally over the linked sets, in its own try so a
    // gate failure never masquerades as "project not found" — keep the detail
    // rendered and surface an inline gate error instead.
    setGateError(false);
    try {
      const g = await api.invoke<GateReport>('evaluate_collab_gate', { projectId: id });
      setGate(g);
    } catch (err) {
      console.error('[projects] gate evaluation failed:', err);
      setGate(null);
      setGateError(true);
    }
  }, [id]);

  const loadFrames = useCallback(async () => {
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
    void load();
  }, [load]);

  useEffect(() => {
    void loadFrames();
  }, [loadFrames]);

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

  // R29 fix round 2: the backend always emits `collab-published` on a
  // successful publish/republish (manual or auto), and the app-root
  // `useCollabNotifications` hook is the one place that turns it into a
  // toast — a second, inline success notify here would double-toast on
  // every manual click. `doPublish`/`doRepublish` below do their own local
  // UI work (close the confirm dialog, reload the frames/detail) and raise
  // a notify() only for a failed invoke, which the backend never emits an
  // event for — nothing else would ever tell the user. The live listener
  // sets no dedupeKey at all (final review I4), and these error toasts use a
  // per-click `Date.now()` key, so neither can swallow the other. A busy
  // refusal (another run of this project in progress) is an info toast, not
  // a failure.

  /** A6 refusal: the other device publishes new frames here. Inline with
   *  the switch action, plus one warning toast (the backend raises no event
   *  for a rejected run); the reload picks up the binding core recorded. */
  const showPublishingDeviceRefusal = (name: string) => {
    setRefusedBy(name);
    notify({
      title: `${leading(name)} publishes new frames to this project`,
      detail: 'Use "Publish from this device" on the project page to publish new frames from here.',
      kind: 'project',
      tone: 'warning',
      link: `/projects/${id}`,
      dedupeKey: `publishing-device-${id}-${Date.now()}`,
    });
    void load();
  };

  /** "Publish from this device": moves the binding here (confirmed first). */
  const doSwitch = async () => {
    if (!id) return;
    setSwitchConfirm(false);
    setSwitchBusy(true);
    try {
      const card = await api.invoke<ProjectCard>('set_collab_publishing_device', { projectId: id });
      setDetail((d) => (d ? { ...d, card } : d));
      setRefusedBy(null);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error('[projects] set publishing device failed:', err);
      if (isOutdated(msg)) {
        setUpdateRequired(true);
      } else {
        notify({
          title: 'Could not publish from this device',
          detail: msg,
          kind: 'project',
          tone: 'warning',
          hasErrors: true,
          link: `/projects/${id}`,
          dedupeKey: `publishing-device-switch-failed-${id}-${Date.now()}`,
        });
      }
    } finally {
      setSwitchBusy(false);
    }
  };

  const doPublish = async () => {
    if (!id) return;
    setPublishBusy(true);
    setPublishError(null);
    try {
      const res = await api.invoke<PublishResult>('publish_collab_frames', { projectId: id });
      setPublishConfirm(false);
      setRefusedBy(heldForPublishingDevice(res));
      await loadFrames();
      await load();
    } catch (err) {
      // S6 — a failed publish surfaces inline AND as a toast, never silently
      // swallowed (the backend raises no event to notify from otherwise).
      const msg = err instanceof Error ? err.message : String(err);
      console.error('[projects] publish failed:', err);
      const refused = publishingDeviceRefusal(msg);
      if (isOutdated(msg)) {
        setPublishConfirm(false);
        setUpdateRequired(true);
      } else if (refused) {
        setPublishConfirm(false);
        showPublishingDeviceRefusal(refused);
      } else if (isPublishBusy(msg)) {
        setPublishError(PUBLISH_BUSY_INLINE);
        notify({
          title: 'Publication already running',
          detail: 'Wait for the current run of this project to finish, then publish again.',
          kind: 'project',
          tone: 'info',
          link: `/projects/${id}`,
          dedupeKey: `publish-busy-${id}-${Date.now()}`,
        });
      } else {
        setPublishError(msg);
        notify({
          title: 'Publish failed',
          detail: msg,
          kind: 'project',
          tone: 'warning',
          hasErrors: true,
          link: `/projects/${id}`,
          dedupeKey: `publish-failed-${id}-${Date.now()}`,
        });
      }
    } finally {
      setPublishBusy(false);
    }
  };

  const doRepublish = async () => {
    if (!id) return;
    setRepublishBusy(true);
    setRepublishError(null);
    try {
      const res = await api.invoke<PublishResult>('republish_collab_frames', { projectId: id });
      setRepublishConfirm(false);
      setRefusedBy(heldForPublishingDevice(res));
      await loadFrames();
      await load();
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error('[projects] republish failed:', err);
      const refused = publishingDeviceRefusal(msg);
      if (isOutdated(msg)) {
        setRepublishConfirm(false);
        setUpdateRequired(true);
      } else if (refused) {
        setRepublishConfirm(false);
        showPublishingDeviceRefusal(refused);
      } else if (isPublishBusy(msg)) {
        setRepublishError(PUBLISH_BUSY_INLINE);
        notify({
          title: 'Publication already running',
          detail: 'Wait for the current run of this project to finish, then republish again.',
          kind: 'project',
          tone: 'info',
          link: `/projects/${id}`,
          dedupeKey: `republish-busy-${id}-${Date.now()}`,
        });
      } else {
        setRepublishError(msg);
        notify({
          title: 'Republish failed',
          detail: msg,
          kind: 'project',
          tone: 'warning',
          hasErrors: true,
          link: `/projects/${id}`,
          dedupeKey: `republish-failed-${id}-${Date.now()}`,
        });
      }
    } finally {
      setRepublishBusy(false);
    }
  };

  if (missing)
    return (
      <p className="p-6 text-sm text-content-muted">
        This project is not in your local list — refresh the Projects page.
      </p>
    );
  if (!detail) return <p className="p-6 text-content-muted">Loading…</p>;

  const c = detail.card;
  const portalPath = c.coordinator ? `/p/${c.slug}/admin` : `/p/${c.slug}`;
  const canReceive = c.dataRole === 'send_receive' || c.coordinator;
  const canModerate = c.coordinator && c.requireApproval;
  const needsApproval = c.requireApproval && !c.coordinator;
  const coordinatorName = detail.members.find((m) => m.coordinator)?.displayName ?? 'the coordinator';
  const publishable = gate?.publishable ?? 0;
  // Decision C (spec 2026-08-31 §8a): the calibration precondition resolves to
  // `NotCalibrated` unconditionally for every candidate frame, so as long as
  // there IS at least one candidate frame, that is always the reason the gate
  // is fully closed — check the rows rather than hard-coding "always show
  // this" so a future generate-at-publish rework (which gives the status a
  // real resolver again) doesn't leave a permanently-wrong message behind.
  const publishBlockedByCalibration =
    !!gate &&
    gate.total > 0 &&
    publishable === 0 &&
    gate.rows.every((r) => r.failures.some((f) => f.startsWith('not calibrated')));
  const publishTooltip = publishBlockedByCalibration
    ? 'Publishing calibrated lights from this device is not available in this version — export or send them instead.'
    : publishable === 0
      ? 'No passing frames to publish yet'
      : undefined;
  const own = frames?.filter((f) => f.own) ?? [];
  // The project's published volume, client-side from the rows already listed.
  const publishedBytes =
    frames === null ? null : own.filter((f) => f.state === 'published').reduce((sum, f) => sum + f.byteSize, 0);
  const canRepublish = own.length > 0;

  const tabs: Tab[] = [
    'contribute',
    ...(canReceive ? (['receive'] as const) : []),
    ...(canModerate ? (['moderation'] as const) : []),
    'overview',
  ];
  const activeTab = tabs.includes(tab) ? tab : 'contribute';
  // The device the switch takes the binding from: the card's (freshest), else
  // the one a refusal named.
  const switchFrom = c.publishingDevice ? deviceLabel(c.publishingDevice.name) : (refusedBy ?? OTHER_DEVICE);
  const switchButton = (
    <button
      type="button"
      onClick={() => setSwitchConfirm(true)}
      disabled={switchBusy}
      className="inline-flex items-center gap-1 rounded border border-border px-2 py-0.5 text-xs text-content-secondary transition-colors hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-50"
    >
      {switchBusy && <Loader2 size={11} className="animate-spin" />}
      Publish from this device
    </button>
  );

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

      {updateRequired && <UpdateRequired />}

      {/* Auto-replication is role-gated in core (`role_allows_replication`:
          coordinator or send_receive) exactly like the Receive tab, so the bar
          shows on the same condition — a send-only member has nothing to pull. */}
      {id && canReceive && (
        <AutoReplicateBar
          projectId={id}
          autoReplicate={c.autoReplicate}
          autoPublish={c.autoPublish}
          publishedBytes={publishedBytes}
          onToggled={() => void load()}
        />
      )}

      <div className="flex gap-1 border-b border-border">
        {tabs.map((t) => (
          <button
            key={t}
            onClick={() => setTab(t)}
            className={`inline-flex items-center gap-1.5 px-4 py-2 text-sm capitalize transition-colors ${
              activeTab === t
                ? 'border-b-2 border-accent font-medium text-content'
                : 'text-content-muted hover:text-content-secondary'
            }`}
          >
            {t}
            {t === 'moderation' && c.pendingFrames > 0 && (
              <span className="rounded-full bg-warning/20 px-1.5 text-[10px] font-medium text-warning">
                {c.pendingFrames}
              </span>
            )}
          </button>
        ))}
      </div>

      {activeTab === 'contribute' && (
        <div className="space-y-4">
          <div className="flex items-center gap-3">
            <span className="text-sm font-medium text-content">Linked objects</span>
            <button
              onClick={() => setLinkOpen(true)}
              className="inline-flex items-center gap-1 rounded border border-border px-2 py-1 text-xs text-content-secondary transition-colors hover:bg-surface-hover"
            >
              <Plus size={12} /> Link an object
            </button>
          </div>

          {detail.links.length === 0 ? (
            <p className="text-sm text-content-muted">Link an object to start.</p>
          ) : (
            <ul className="flex flex-wrap gap-2">
              {detail.links.map((l) => (
                <li
                  key={l.framesSetId}
                  className="rounded border border-border px-2 py-1 text-xs text-content-secondary"
                >
                  <span className="break-words">{l.name ?? `Set #${l.framesSetId}`}</span> ·{' '}
                  {l.lightCount} lights
                  {l.withinRadius ? ' · on target' : ''}
                </li>
              ))}
            </ul>
          )}

          {gateError ? (
            <p className="text-sm text-error">Gate evaluation failed — see console.</p>
          ) : (
            <GateTable gate={gate} />
          )}

          <div className="flex flex-wrap items-center gap-2">
            <div className="space-y-1">
              <button
                onClick={() => {
                  setPublishError(null);
                  setPublishConfirm(true);
                }}
                disabled={publishable === 0}
                className="inline-flex items-center gap-1.5 rounded bg-accent px-4 py-2 text-sm text-surface transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
                title={publishTooltip}
              >
                <Send size={14} /> Publish {publishable} passing frames
              </button>
              {/* Honest line near the button (D-2, review fix) — the tooltip alone
                  is easy to miss on a dead-looking disabled button, and decision C
                  means this is not a temporary/data-dependent state a user could
                  fix by linking more objects or waiting for analysis. */}
              {publishBlockedByCalibration && (
                <p className="text-xs text-content-muted">{publishTooltip}</p>
              )}
              {publishError && !publishConfirm && <p className="text-sm text-error">{publishError}</p>}
            </div>

            {canRepublish && (
              <button
                onClick={() => {
                  setRepublishError(null);
                  setRepublishConfirm(true);
                }}
                className="inline-flex items-center gap-1.5 rounded border border-border px-3 py-2 text-sm text-content-secondary transition-colors hover:bg-surface-hover"
                title="Regenerate every one of your published frames as a new content version"
              >
                <RefreshCw size={14} /> Recalibrate and republish all
              </button>
            )}
          </div>
          {republishError && !republishConfirm && (
            <p className="text-sm text-error">{republishError}</p>
          )}
          {refusedBy && !c.publishingHere && (
            <div
              data-testid="publishing-refusal"
              className="flex flex-wrap items-center gap-2 rounded border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-content"
            >
              <span className="break-words">{`${leading(refusedBy)} publishes new frames to this project.`}</span>
              {switchButton}
            </div>
          )}

          <PublicationHistory frames={own} error={framesError} loaded={frames !== null} />
        </div>
      )}

      {activeTab === 'receive' && id && (
        <ReceiveTab projectId={id} projectTitle={c.title} frames={frames} reload={loadFrames} />
      )}

      {activeTab === 'moderation' && id && (
        <ModerationQueue
          projectId={id}
          onDecided={() => {
            void load();
            void loadFrames();
          }}
        />
      )}

      {activeTab === 'overview' && (
        <div className="space-y-4 text-sm">
          <section>
            <h2 className="mb-1 font-medium text-content">Members</h2>
            <ul className="text-content-secondary">
              {detail.members.map((m, i) => (
                <li key={`${m.displayName}-${i}`} className="break-words">
                  {m.displayName} — {m.coordinator ? 'coordinator' : m.dataRole}
                </li>
              ))}
            </ul>
          </section>

          <section>
            <h2 className="mb-1 font-medium text-content">
              Quality thresholds
              {detail.thresholdsVersion != null ? ` (v${detail.thresholdsVersion})` : ''}
            </h2>
            {detail.thresholds.length === 0 ? (
              <p className="text-content-muted">No thresholds set.</p>
            ) : (
              <ul className="text-content-secondary">
                {detail.thresholds.map((r, i) => (
                  <li key={`${r.metricKey}-${i}`} className="break-words">
                    {r.op === 'reject_if'
                      ? r.metricKey === 'not_trailed'
                        ? 'Reject trailed frames'
                        : `${r.metricKey} — reject if ${String(r.value)}`
                      : `${r.metricKey} ${r.op === 'lte' ? '≤' : r.op === 'gte' ? '≥' : r.op} ${String(r.value)}`}
                  </li>
                ))}
              </ul>
            )}
            <p className="mt-1 text-xs text-content-muted">
              Thresholds are set by the coordinator on the portal. Changes are prospective —
              already-published frames stay published.
            </p>
          </section>
        </div>
      )}

      {linkOpen && id && (
        <LinkObjectDialog
          projectId={id}
          onClose={() => setLinkOpen(false)}
          onChanged={() => void load()}
        />
      )}

      <ConfirmDialog
        isOpen={switchConfirm}
        title="Publish from this device?"
        message={`${leading(switchFrom)} will stop publishing new frames to this project; it can still update the frames it already published.`}
        confirmText="Switch"
        onConfirm={() => void doSwitch()}
        onCancel={() => setSwitchConfirm(false)}
      />

      {publishConfirm && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
          onClick={() => !publishBusy && setPublishConfirm(false)}
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
              {publishable} passing {publishable === 1 ? 'frame' : 'frames'} will be calibrated and
              announced to the project.
            </p>
            <p className="mb-2 text-xs text-content-muted">
              Estimated size ≈ {formatBytes(publishable * APPROX_FRAME_BYTES)} — the exact size is
              measured when each frame is generated.
            </p>
            {needsApproval && (
              <p className="mb-2 text-xs text-warning">
                This project requires approval — your contribution goes to {coordinatorName} for
                review.
              </p>
            )}
            {publishError && <p className="mb-2 text-sm text-error">{publishError}</p>}
            <div className="mt-3 flex justify-end gap-2">
              <button
                type="button"
                onClick={() => setPublishConfirm(false)}
                disabled={publishBusy}
                className="rounded border border-border px-3 py-1.5 text-sm text-content-secondary transition-colors hover:bg-surface-hover disabled:opacity-50"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={() => void doPublish()}
                disabled={publishBusy}
                className="inline-flex items-center gap-1 rounded bg-accent px-3 py-1.5 text-sm text-surface transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
              >
                {publishBusy && <Loader2 size={12} className="animate-spin" />} Publish
              </button>
            </div>
          </div>
        </div>
      )}

      {republishConfirm && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
          onClick={() => !republishBusy && setRepublishConfirm(false)}
        >
          <div
            className="w-[30rem] max-w-[90vw] rounded-lg border border-border bg-surface p-4"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="mb-2 flex items-center gap-2">
              <RefreshCw size={16} className="text-accent" />
              <h2 className="font-medium text-content">Recalibrate and republish all</h2>
            </div>
            <p className="mb-2 text-sm text-content-secondary">
              Every one of your published frames is regenerated and, where the bytes changed, posted
              as a new content version.
            </p>
            <p className="mb-2 text-xs text-warning">
              Every processor holding one of your frames re-downloads it once this finishes.
            </p>
            {republishError && <p className="mb-2 text-sm text-error">{republishError}</p>}
            <div className="mt-3 flex justify-end gap-2">
              <button
                type="button"
                onClick={() => setRepublishConfirm(false)}
                disabled={republishBusy}
                className="rounded border border-border px-3 py-1.5 text-sm text-content-secondary transition-colors hover:bg-surface-hover disabled:opacity-50"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={() => void doRepublish()}
                disabled={republishBusy}
                className="inline-flex items-center gap-1 rounded bg-accent px-3 py-1.5 text-sm text-surface transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
              >
                {republishBusy && <Loader2 size={12} className="animate-spin" />} Republish
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/** Own frames with their hub-mirrored state. */
function PublicationHistory({
  frames,
  error,
  loaded,
}: {
  frames: ProjectFrameView[];
  error: boolean;
  loaded: boolean;
}) {
  if (error)
    return (
      <div className="space-y-1">
        <h2 className="text-sm font-medium text-content">Your publications</h2>
        <p className="text-sm text-error">Could not load your publications — see console.</p>
      </div>
    );
  if (!loaded) return null;
  return (
    <div className="space-y-2">
      <h2 className="text-sm font-medium text-content">Your publications</h2>
      {frames.length === 0 ? (
        <p className="text-sm text-content-muted">Nothing published yet.</p>
      ) : (
        <ul className="space-y-1.5">
          {frames.map((f) => (
            <li key={f.frameUuid} className="rounded border border-border px-3 py-2 text-sm">
              <div className="flex flex-wrap items-center gap-2">
                <span
                  className="max-w-[16rem] truncate text-xs text-content-secondary"
                  title={f.fileName}
                >
                  {f.fileName}
                </span>
                <span className="text-xs text-content-muted">
                  v{f.contentVersion} · {formatBytes(f.byteSize)}
                </span>
                <StateChip state={f.state} rejectReason={f.acceptedReason} />
              </div>
              <p className="mt-0.5 text-[11px] text-content-muted">
                held by {f.holdersOnline} online / {f.holdersTotal}
                {f.localState === 'own_missing' ? ' · not on disk' : ''}
                {f.localState === 'own_changed' ? ' · file changed' : ''}
                {f.lastError ? ` · ${f.lastError}` : ''}
              </p>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** Hub-mirrored publication state chip. Rejected carries its reason on the title. */
function StateChip({ state, rejectReason }: { state: string; rejectReason: string | null }) {
  const map: Record<string, string> = {
    pending: 'bg-warning/20 text-warning',
    published: 'bg-success/20 text-success',
    rejected: 'bg-error/20 text-error',
  };
  const cls = map[state] ?? 'bg-surface-hover text-content-muted';
  return (
    <span
      className={`rounded px-1.5 py-0.5 text-[10px] font-medium ${cls}`}
      title={state === 'rejected' && rejectReason ? rejectReason : undefined}
    >
      {state}
    </span>
  );
}

/** Candidate frames of the linked sets with their per-rule gate verdict. */
function GateTable({ gate }: { gate: GateReport | null }) {
  if (!gate) return null;
  if (gate.total === 0)
    return (
      <p className="text-sm text-content-muted">
        No candidate frames yet — link an object that has LIGHT frames.
      </p>
    );

  return (
    <div className="overflow-x-auto">
      <p className="mb-1 text-sm text-content-secondary">
        {gate.publishable} publishable of {gate.total}
      </p>
      <table className="w-full text-left text-xs">
        <thead className="text-content-muted">
          <tr>
            <th className="py-1 pr-3 font-normal">Frame</th>
            <th className="pr-3 font-normal">FWHM″</th>
            <th className="pr-3 font-normal">Ecc</th>
            <th className="pr-3 font-normal">Stars</th>
            <th className="font-normal">Gate</th>
          </tr>
        </thead>
        <tbody>
          {gate.rows.map((r: FrameGateRow) => (
            <tr key={r.frameId} className="border-t border-border/50">
              <td className="max-w-[16rem] truncate py-1 pr-3 text-content">{r.filename}</td>
              <td className="pr-3 text-content-secondary">
                {r.fwhmArcsec != null ? r.fwhmArcsec.toFixed(2) : '—'}
              </td>
              <td className="pr-3 text-content-secondary">
                {r.eccentricity != null ? r.eccentricity.toFixed(2) : '—'}
              </td>
              <td className="pr-3 text-content-secondary">{r.starsDetected ?? '—'}</td>
              <td
                className={r.publishable ? 'text-success' : 'text-error'}
                title={r.publishable ? undefined : r.failures.join('; ')}
              >
                {r.publishable ? '✓ publishable' : (r.failures[0] ?? 'not publishable')}
                {!r.publishable && r.failures.length > 1 ? ` (+${r.failures.length - 1})` : ''}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
