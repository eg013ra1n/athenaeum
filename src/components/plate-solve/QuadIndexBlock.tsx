// "Quad Index" block of the Star Catalog section: the downloadable quad-index
// packs that make blind / scale-free plate solves fast. Sibling of the
// Catalog Tiers table (same table styling and tokens); state in `useIndexPacks`.
import { CheckCircle, Download } from 'lucide-react';
import { useIndexPacks, getIndexStatusText } from './useIndexPacks';
import { formatSize, formatElapsed } from './downloadFormat';

function packLabel(name: string): string {
  return name ? name.charAt(0).toUpperCase() + name.slice(1) : name;
}

export function QuadIndexBlock() {
  const { packs, loading, downloading, progress, error, startedAt, nowTs, download } =
    useIndexPacks();

  const missing = packs.filter((p) => !p.installed && p.compatible);

  return (
    <div>
      <div className="flex items-center justify-between mb-2">
        <div className="text-xs font-semibold uppercase tracking-wide text-content-muted">
          Quad Index
        </div>
        {missing.length >= 2 && (
          <button
            onClick={() => void download(missing.map((p) => p.name))}
            disabled={downloading}
            className="inline-flex items-center gap-1 text-xs font-medium text-accent hover:text-accent-hover disabled:opacity-50 transition-colors"
          >
            <Download size={11} />
            Download all
          </button>
        )}
      </div>
      <p className="text-xs text-content-muted mb-2">
        Makes solves fast when a frame has no coordinates or no pixel scale. Frames with both
        solve from the star catalog alone and never use the index. Needs the star catalog tiers
        installed.
      </p>
      {loading ? (
        <div className="flex items-center gap-2 text-xs text-content-muted py-1">
          <div className="animate-spin rounded-full h-3 w-3 border-b-2 border-accent" />
          Checking index status...
        </div>
      ) : packs.length === 0 ? (
        <p className="text-xs text-content-muted">
          The index server could not be reached, and no index packs are installed.
        </p>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full text-xs">
            <thead>
              <tr className="text-content-muted border-b border-border">
                <th className="text-left pb-1.5 pr-4 font-medium">Pack</th>
                <th className="text-center pb-1.5 px-4 font-medium">Status</th>
                <th className="text-right pb-1.5 font-medium">Size</th>
              </tr>
            </thead>
            <tbody>
              {packs.map((pack) => (
                <tr key={pack.name} className="border-b border-border/40">
                  <td className="py-2 pr-4 align-top">
                    <span className="font-medium text-content">{packLabel(pack.name)}</span>
                    <span className="ml-2 text-content-muted">
                      &middot; fields &ge; {pack.minFovDeg.toFixed(2)}&deg;
                    </span>
                  </td>
                  <td className="py-2 px-4 align-top text-center">
                    {pack.installed ? (
                      <span className="inline-flex items-center gap-1 text-success">
                        <CheckCircle size={12} />
                        Installed
                      </span>
                    ) : !pack.compatible ? (
                      <span className="text-content-muted">Needs a newer Athenaeum</span>
                    ) : (
                      <span className="inline-flex flex-col items-center gap-0.5">
                        <button
                          onClick={() => void download([pack.name])}
                          disabled={downloading}
                          className="inline-flex items-center gap-1 font-medium text-accent hover:text-accent-hover disabled:opacity-50 transition-colors"
                        >
                          <Download size={11} />
                          Download
                        </button>
                        {pack.error && <span className="text-error">{pack.error}</span>}
                      </span>
                    )}
                  </td>
                  <td className="py-2 text-right text-content-muted tabular-nums align-top">
                    {pack.sizeBytes != null ? formatSize(pack.sizeBytes) : '—'}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {error && <p className="text-xs text-error mt-2">{error}</p>}
      {downloading && (
        <div className="space-y-1.5 mt-3">
          <div className="flex items-center gap-2 text-xs text-content-muted">
            <div className="animate-spin rounded-full h-3 w-3 border-b-2 border-accent flex-shrink-0" />
            <span>{getIndexStatusText(progress)}</span>
          </div>
          <div className="w-full h-1.5 bg-surface-hover rounded-full overflow-hidden">
            <div
              className="h-full bg-accent rounded-full transition-all duration-300"
              style={{ width: progress ? `${progress.percent}%` : '4%' }}
            />
          </div>
          <p className="text-xs text-content-muted flex justify-between">
            <span>
              {startedAt != null
                ? `elapsed ${formatElapsed(nowTs - startedAt)} · safe to leave running`
                : 'safe to leave running'}
            </span>
            {progress && <span>{progress.percent.toFixed(0)}%</span>}
          </p>
        </div>
      )}
    </div>
  );
}
