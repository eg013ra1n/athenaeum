import { RefreshCw, Target, Users } from 'lucide-react';
import { Link } from 'react-router-dom';
import { useProjects } from '../hooks/useProjects';

export default function Projects() {
  const { projects, loading, refreshing, signedOut, refresh } = useProjects();

  if (loading) return <p className="p-6 text-content-muted">Loading projects…</p>;

  return (
    <div className="p-6 space-y-4">
      <div className="flex items-center gap-3">
        <Users size={20} className="text-content-secondary" />
        <h1 className="text-lg font-semibold text-content">Projects</h1>
        <button
          onClick={() => void refresh()}
          disabled={refreshing}
          className="ml-auto inline-flex items-center gap-2 rounded-lg border border-border px-3 py-1.5 text-sm text-content-secondary hover:bg-surface-hover disabled:opacity-50"
        >
          <RefreshCw size={14} className={refreshing ? 'animate-spin' : ''} />
          Refresh
        </button>
      </div>

      {signedOut && (
        <p className="text-sm text-content-muted">
          Sign in (Settings → Account) to see your collaboration projects.
        </p>
      )}
      {!signedOut && projects.length === 0 && (
        <p className="text-sm text-content-muted">
          No projects yet — browse and join on the portal, or publish a frame set as a project.
        </p>
      )}

      <div className="grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3">
        {projects.map((p) => (
          <Link
            key={p.projectId}
            to={`/projects/${p.projectId}`}
            className="rounded-lg border border-border bg-surface p-4 hover:bg-surface-hover"
          >
            <div className="flex items-center gap-2">
              <span className="font-medium text-content truncate">{p.title}</span>
              {p.coordinator && (
                <span className="rounded bg-accent/20 px-1.5 py-0.5 text-xs text-accent">coordinator</span>
              )}
              {p.projectStatus === 'closed' && (
                <span className="rounded bg-surface-hover px-1.5 py-0.5 text-xs text-content-muted">closed</span>
              )}
            </div>
            <p className="mt-1 flex items-center gap-1 text-xs text-content-muted">
              <Target size={12} className="shrink-0" />{' '}
              <span className="truncate">{p.targetName}</span> · r {p.targetRadiusDeg.toFixed(1)}°
            </p>
            <p className="mt-2 text-sm text-content-secondary">
              {p.publishable} publishable of {p.candidates}
              {p.linkedSets === 0 ? ' — link an object to start' : ` · ${p.linkedSets} linked set${p.linkedSets === 1 ? '' : 's'}`}
            </p>
            {p.coordinator && p.pendingAnnouncements > 0 && (
              <p className="mt-1 text-xs text-warning">
                {p.pendingAnnouncements} contribution{p.pendingAnnouncements === 1 ? '' : 's'} awaiting approval
              </p>
            )}
          </Link>
        ))}
      </div>
    </div>
  );
}
