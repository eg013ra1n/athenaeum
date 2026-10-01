import { useState } from 'react';
import { Link } from 'react-router-dom';
import { Loader2, Users } from 'lucide-react';
import { api } from '../../api';
import { useNotifications } from '../../contexts/NotificationContext';
import type { FrameSetProjectLink, FrameSetProjectStatus } from '../../types/models';

function summary(l: FrameSetProjectLink): string {
  const c = l.counts;
  const parts: string[] = [];
  if (c.published) parts.push(`${c.published} published`);
  if (c.prepared) parts.push(`${c.prepared} to review`);
  if (c.pendingApproval) parts.push(`${c.pendingApproval} pending`);
  if (c.failsGate) parts.push(`${c.failsGate} fail gate`);
  if (c.withheld) parts.push(`${c.withheld} withheld`);
  if (c.updatePending) parts.push(`${c.updatePending} update pending`);
  if (c.rejected) parts.push(`${c.rejected} rejected`);
  if (c.publishedNotOnDisk) parts.push(`${c.publishedNotOnDisk} not on disk`);
  if (c.publishedNowFailsGate) parts.push(`${c.publishedNowFailsGate} now fail gate`);
  if (c.notPublished) parts.push(`${c.notPublished} not published`);
  return parts.join(' · ');
}

/** Spec 2026-09-28 §8.3 — the set's Project block under the header stats. */
export default function FrameSetProjectBlock({ framesSetId, status, onChanged }: { framesSetId: number; status: FrameSetProjectStatus; onChanged: () => void }) {
  const { notify } = useNotifications();
  const [busy, setBusy] = useState<string | null>(null);
  if (status.links.length === 0 && status.candidates.length === 0) return null;
  const link = async (projectId: string) => {
    setBusy(projectId);
    try {
      await api.invoke('set_collab_link', { projectId, framesSetId, linked: true });
      onChanged();
    } catch (err) {
      console.error('[projects] link from the set page failed:', err);
      notify({ title: 'Could not link to the project', detail: err instanceof Error ? err.message : String(err), kind: 'project', tone: 'warning' });
    } finally {
      setBusy(null);
    }
  };
  return (
    <div className="space-y-1 text-sm">
      {status.links.map((l) => (
        <div key={l.projectId} className="flex flex-wrap items-center gap-2 text-content-secondary">
          <Users size={14} className="text-content-muted" />
          <span><span className="text-content">Project {l.title}</span> · {summary(l)}</span>
          <Link to={`/projects/${l.projectId}?tab=mine`} className="rounded border border-border px-2 py-0.5 text-xs hover:bg-surface-hover">Open project</Link>
        </div>
      ))}
      {status.links.length === 0 && status.candidates.map((c) => (
        <div key={c.projectId} className="flex flex-wrap items-center gap-2 text-content-secondary">
          <Users size={14} className="text-content-muted" />
          <span>Matches project {c.title} ({c.distanceDeg.toFixed(1)}° away)</span>
          <button type="button" disabled={busy != null} onClick={() => void link(c.projectId)} className="inline-flex items-center gap-1 rounded border border-border px-2 py-0.5 text-xs hover:bg-surface-hover disabled:opacity-50">
            {busy === c.projectId && <Loader2 size={11} className="animate-spin" />} Link to project
          </button>
        </div>
      ))}
    </div>
  );
}
