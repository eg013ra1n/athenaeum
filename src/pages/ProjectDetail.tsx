import { useCallback, useEffect, useState } from 'react';
import { useParams } from 'react-router-dom';
import { ExternalLink, Loader2, Plus, Send, Target } from 'lucide-react';
import { api } from '../api';
import { openUrl } from '../api/desktop';
import { safeExternalUrl } from '../utils/externalUrl';
import { useNotifications } from '../contexts/NotificationContext';
import LinkObjectDialog from '../components/collab/LinkObjectDialog';
import ReceiveTab from '../components/collab/ReceiveTab';
import ModerationQueue from '../components/collab/ModerationQueue';
import { formatBytes } from '../components/collab/format';
import { formatTimestamp } from '../utils/dateFormatting';
import type {
  FrameGateRow,
  GateReport,
  ProjectDetail as Detail,
  ProjectPackageView,
  PublishResult,
} from '../types/models';

type Tab = 'contribute' | 'receive' | 'moderation' | 'overview';

// Rough per-frame size for the PRE-publish confirm estimate only (a calibrated
// 32-bit-float light frame). The exact size is measured when the package is
// built and reported back in `PublishResult.byteSize`; the dialog labels this
// figure "estimated" so it never reads as an authoritative stored value (S6).
const APPROX_FRAME_BYTES = 45 * 1024 * 1024;

export default function ProjectDetail() {
  const { id } = useParams();
  const { notify } = useNotifications();
  const [detail, setDetail] = useState<Detail | null>(null);
  const [gate, setGate] = useState<GateReport | null>(null);
  const [gateError, setGateError] = useState(false);
  const [tab, setTab] = useState<Tab>('contribute');
  const [linkOpen, setLinkOpen] = useState(false);
  const [missing, setMissing] = useState(false);
  const [packages, setPackages] = useState<ProjectPackageView[] | null>(null);
  const [packagesError, setPackagesError] = useState(false);
  const [publishConfirm, setPublishConfirm] = useState(false);
  const [publishBusy, setPublishBusy] = useState(false);
  const [publishError, setPublishError] = useState<string | null>(null);

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

  const loadPackages = useCallback(async () => {
    if (!id) return;
    setPackagesError(false);
    try {
      setPackages(await api.invoke<ProjectPackageView[]>('list_collab_packages', { projectId: id }));
    } catch (err) {
      console.error('[projects] list packages failed:', err);
      setPackagesError(true);
    }
  }, [id]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    void loadPackages();
  }, [loadPackages]);

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

  const doPublish = async () => {
    if (!id) return;
    setPublishBusy(true);
    setPublishError(null);
    try {
      const res = await api.invoke<PublishResult>('publish_collab_package', { projectId: id });
      setPublishConfirm(false);
      // Message reflects the hub-returned state + seed target, never optimistic (S6).
      let title: string;
      let tone: 'info' | 'success' | 'warning' = 'info';
      if (res.seedTarget == null) {
        title = 'Announced — waiting for a receive-capable member';
      } else if (res.state === 'published') {
        title = `Publication announced — seeding to ${res.seedTarget}`;
        tone = 'success';
      } else {
        title = `Sent for approval to ${res.seedTarget}`;
      }
      notify({
        title,
        detail: `${res.frameCount} frames · ${formatBytes(res.byteSize)}`,
        kind: 'project',
        tone,
        link: `/projects/${id}`,
        dedupeKey: `publish-${res.packageId}`,
      });
      await loadPackages();
      await load();
    } catch (err) {
      // S6 — a failed publish surfaces inline, never silently swallowed.
      const msg = err instanceof Error ? err.message : String(err);
      console.error('[projects] publish failed:', err);
      setPublishError(msg);
    } finally {
      setPublishBusy(false);
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

  const tabs: Tab[] = [
    'contribute',
    ...(canReceive ? (['receive'] as const) : []),
    ...(canModerate ? (['moderation'] as const) : []),
    'overview',
  ];
  const activeTab = tabs.includes(tab) ? tab : 'contribute';

  return (
    <div className="space-y-4 p-6">
      <div className="flex flex-wrap items-center gap-3">
        <h1 className="truncate text-lg font-semibold text-content">{c.title}</h1>
        <span className="flex items-center gap-1 text-xs text-content-muted">
          <Target size={12} /> {c.targetName} · r {c.targetRadiusDeg.toFixed(1)}°
        </span>
        {c.coordinator && (
          <span className="rounded bg-accent/20 px-1.5 py-0.5 text-xs text-accent">coordinator</span>
        )}
        <button
          onClick={() => void openPortal(portalPath)}
          className="ml-auto inline-flex items-center gap-1 text-sm text-content-secondary transition-colors hover:text-content"
        >
          Manage on portal <ExternalLink size={13} />
        </button>
      </div>

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
            {t === 'moderation' && c.pendingAnnouncements > 0 && (
              <span className="rounded-full bg-warning/20 px-1.5 text-[10px] font-medium text-warning">
                {c.pendingAnnouncements}
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

          <div className="space-y-1">
            <button
              onClick={() => {
                setPublishError(null);
                setPublishConfirm(true);
              }}
              disabled={publishable === 0}
              className="inline-flex items-center gap-1.5 rounded bg-accent px-4 py-2 text-sm text-surface transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
              title={publishable === 0 ? 'No passing frames to publish yet' : undefined}
            >
              <Send size={14} /> Publish {publishable} passing frames
            </button>
            {publishError && !publishConfirm && <p className="text-sm text-error">{publishError}</p>}
          </div>

          <PublicationHistory packages={packages} error={packagesError} />
        </div>
      )}

      {activeTab === 'receive' && id && (
        <ReceiveTab projectId={id} projectTitle={c.title} packages={packages} reload={loadPackages} />
      )}

      {activeTab === 'moderation' && id && (
        <ModerationQueue
          projectId={id}
          onDecided={() => {
            void load();
            void loadPackages();
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
                      ? `${r.metricKey} — reject when ${String(r.value)}`
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
              {publishable} passing {publishable === 1 ? 'frame' : 'frames'} will be packaged and
              announced to the project.
            </p>
            <p className="mb-2 text-xs text-content-muted">
              Estimated size ≈ {formatBytes(publishable * APPROX_FRAME_BYTES)} — the exact size is
              measured when the package is built.
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
    </div>
  );
}

/** Own publications with their hub-mirrored state + replication line. */
function PublicationHistory({
  packages,
  error,
}: {
  packages: ProjectPackageView[] | null;
  error: boolean;
}) {
  if (error)
    return (
      <div className="space-y-1">
        <h2 className="text-sm font-medium text-content">Your publications</h2>
        <p className="text-sm text-error">Could not load your publications — see console.</p>
      </div>
    );
  if (packages === null) return null;
  const own = packages.filter((p) => p.own);
  return (
    <div className="space-y-2">
      <h2 className="text-sm font-medium text-content">Your publications</h2>
      {own.length === 0 ? (
        <p className="text-sm text-content-muted">Nothing published yet.</p>
      ) : (
        <ul className="space-y-1.5">
          {own.map((p) => (
            <li
              key={p.packageId}
              className={`rounded border border-border px-3 py-2 text-sm ${
                p.superseded ? 'opacity-50' : ''
              }`}
            >
              <div className="flex flex-wrap items-center gap-2">
                <span className="text-xs text-content-muted">{formatTimestamp(p.createdAt)}</span>
                <span className="text-xs text-content-secondary">
                  {p.frameCount} frames · {formatBytes(p.byteSize)}
                </span>
                <StateChip state={p.state} rejectReason={p.rejectReason} />
                {p.superseded && (
                  <span className="rounded bg-surface-hover px-1.5 py-0.5 text-[10px] text-content-muted">
                    superseded
                  </span>
                )}
              </div>
              <p className="mt-0.5 text-[11px] text-content-muted">
                held by {p.holderCount} ({p.onlineCount} online)
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
