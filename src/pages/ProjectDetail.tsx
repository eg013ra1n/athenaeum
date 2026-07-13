import { useCallback, useEffect, useState } from 'react';
import { useParams } from 'react-router-dom';
import { ExternalLink, Plus, Target } from 'lucide-react';
import { api } from '../api';
import { openUrl } from '../api/desktop';
import { safeExternalUrl } from '../utils/externalUrl';
import { useNotifications } from '../contexts/NotificationContext';
import LinkObjectDialog from '../components/collab/LinkObjectDialog';
import type { FrameGateRow, GateReport, ProjectDetail as Detail } from '../types/models';

export default function ProjectDetail() {
  const { id } = useParams();
  const { notify } = useNotifications();
  const [detail, setDetail] = useState<Detail | null>(null);
  const [gate, setGate] = useState<GateReport | null>(null);
  const [tab, setTab] = useState<'contribute' | 'overview'>('contribute');
  const [linkOpen, setLinkOpen] = useState(false);
  const [missing, setMissing] = useState(false);

  const load = useCallback(async () => {
    if (!id) return;
    setMissing(false);
    try {
      // The detail comes from the local cache of VERIFIED snapshots (core owns
      // verification); the gate is evaluated locally over the linked sets.
      const [d, g] = await Promise.all([
        api.invoke<Detail>('get_collab_project_detail', { projectId: id }),
        api.invoke<GateReport>('evaluate_collab_gate', { projectId: id }),
      ]);
      setDetail(d);
      setGate(g);
    } catch (err) {
      console.error('[projects] detail load failed:', err);
      setMissing(true);
    }
  }, [id]);

  useEffect(() => {
    void load();
  }, [load]);

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
  if (!detail) return <p className="p-6 text-content-muted">Loading…</p>;

  const c = detail.card;
  const portalPath = c.coordinator ? `/p/${c.slug}/admin` : `/p/${c.slug}`;

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
        {(['contribute', 'overview'] as const).map((t) => (
          <button
            key={t}
            onClick={() => setTab(t)}
            className={`px-4 py-2 text-sm capitalize transition-colors ${
              tab === t
                ? 'border-b-2 border-accent font-medium text-content'
                : 'text-content-muted hover:text-content-secondary'
            }`}
          >
            {t}
          </button>
        ))}
      </div>

      {tab === 'contribute' ? (
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

          <GateTable gate={gate} />

          <button
            disabled
            title="Sending arrives with the exchange update"
            className="cursor-not-allowed rounded bg-surface-hover px-4 py-2 text-sm text-content-muted"
          >
            Publish {gate?.publishable ?? 0} passing frames (coming soon)
          </button>
        </div>
      ) : (
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
    </div>
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
