import type { ReactNode } from 'react';
import { X } from 'lucide-react';
import type { RunSummary, SummaryFrame } from '../../types/stacking';
import { formatTimestamp } from '../../utils/dateFormatting';

function formatMs(ms: number): string {
  return ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)} s`;
}

/** Three compact cache-hit badges — whether this stage was reused from a
 *  prior run's artifact rather than recomputed (`stackingPrefs`-independent,
 *  purely a `SummaryFrame` read). */
function CachedFlags({ frame }: { frame: SummaryFrame }) {
  const flags: { key: string; label: string; on: boolean }[] = [
    { key: 'C', label: 'calibrated', on: frame.cachedCalibrated },
    { key: 'M', label: 'metrics', on: frame.cachedMetrics },
    { key: 'R', label: 'registration', on: frame.cachedRegistration },
  ];
  return (
    <span className="inline-flex gap-1">
      {flags.map((f) => (
        <span
          key={f.key}
          title={`${f.label}: ${f.on ? 'reused from cache' : 'recomputed this run'}`}
          className={`inline-flex items-center justify-center w-4 h-4 rounded text-[9px] font-bold ${
            f.on ? 'bg-success-muted text-success' : 'bg-surface-hover text-content-muted'
          }`}
        >
          {f.key}
        </span>
      ))}
    </span>
  );
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="space-y-2">
      <h4 className="text-sm font-semibold text-content">{title}</h4>
      {children}
    </section>
  );
}

export interface ProvenanceModalProps {
  summary: RunSummary;
  onClose: () => void;
}

/** Full provenance for one finished (or failed/cancelled) run — every
 *  section the brief names: config JSON, reference, per-group frames with
 *  their cache-hit flags, stage timings, warnings, error. Read-only; there
 *  is nothing here for the user to edit (Ruling 7 — the Frames table's
 *  checkbox is the only frame-level write this tab makes, and this modal
 *  isn't it). */
export function ProvenanceModal({ summary, onClose }: ProvenanceModalProps) {
  return (
    <div className="fixed inset-0 bg-black bg-opacity-50 flex items-center justify-center z-50">
      <div className="bg-surface-elevated rounded-lg w-full max-w-3xl mx-4 border border-border shadow-xl flex flex-col max-h-[85vh]">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-border shrink-0">
          <div>
            <h3 className="text-lg font-semibold text-content">
              Run #{summary.runId} provenance
            </h3>
            <p className="text-xs text-content-muted">
              {summary.setName} · {summary.status} · started {formatTimestamp(summary.startedAt)}
              {summary.finishedAt ? ` · finished ${formatTimestamp(summary.finishedAt)}` : ''} · app{' '}
              {summary.appVersion}
            </p>
          </div>
          <button onClick={onClose} className="p-1 hover:bg-surface-hover rounded transition" aria-label="Close">
            <X size={20} />
          </button>
        </div>

        {/* Body */}
        <div className="flex-1 overflow-y-auto min-h-0 p-4 space-y-5">
          <Section title="Config">
            <pre className="text-[11px] bg-surface rounded p-3 overflow-auto max-h-64 text-content-secondary font-mono">
              {JSON.stringify(summary.config, null, 2)}
            </pre>
            <p className="text-xs text-content-muted font-mono">hash {summary.configHash}</p>
          </Section>

          <Section title="Reference">
            <p className="text-sm text-content-secondary">
              {summary.reference.frameId != null
                ? `Frame #${summary.reference.frameId} · ${summary.reference.filename ?? 'unknown filename'}`
                : 'No reference frame recorded'}
              {' · '}
              {summary.reference.mode === 'manual' ? 'manual selection' : 'auto (highest weight)'}
              {summary.reference.weight != null ? ` · weight ${summary.reference.weight.toFixed(3)}` : ''}
              {/* M4a Task 4 (ruling R-M4a-5): stage 5's dry pass re-picked
               *  the reference, so the frame above is NOT the one the
               *  highest weight chose — provenance has to say which was.
               *  `switchedFrom` is null on every run that kept its first
               *  pick, the same guard `ResultsPanel.tsx` uses. */}
              {summary.reference.switchedFrom != null
                ? ` · switched from #${summary.reference.switchedFrom} (two-pass)`
                : ''}
            </p>
            <p className="text-xs text-content-muted">
              Seed source: {summary.measurement.seedSource} · scale estimator: {summary.measurement.scaleEstimator}
            </p>
          </Section>

          <Section title="Groups and frames">
            {summary.groups.length === 0 ? (
              <p className="text-sm text-content-muted">No groups.</p>
            ) : (
              <div className="space-y-2">
                {summary.groups.map((g) => (
                  <details key={g.key} className="border border-border rounded-lg">
                    <summary className="cursor-pointer select-none px-3 py-2 text-sm text-content font-mono">
                      {g.key} — {g.includedCount}/{g.frameCount} included
                    </summary>
                    <div className="px-3 pb-3 overflow-x-auto">
                      <table className="w-full text-xs">
                        <thead>
                          <tr className="text-content-muted text-left border-b border-border">
                            <th className="py-1 px-2 font-medium">Filename</th>
                            <th className="py-1 px-2 font-medium">Included</th>
                            <th className="py-1 px-2 font-medium">Cached</th>
                          </tr>
                        </thead>
                        <tbody>
                          {g.frames.map((f) => (
                            <tr key={f.frameId} className="border-b border-border/50 last:border-0">
                              <td className="py-1 px-2 text-content font-mono truncate max-w-[260px]" title={f.filename}>
                                {f.filename}
                              </td>
                              <td className="py-1 px-2 text-content-secondary">
                                {f.included ? 'yes' : (f.exclusionReason ?? 'no')}
                              </td>
                              <td className="py-1 px-2">
                                <CachedFlags frame={f} />
                              </td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </div>
                  </details>
                ))}
              </div>
            )}
          </Section>

          <Section title="Stage timings">
            {summary.stages.length === 0 ? (
              <p className="text-sm text-content-muted">No stage timings recorded.</p>
            ) : (
              <table className="w-full text-xs max-w-sm">
                <tbody>
                  {summary.stages.map((s) => (
                    <tr key={s.stage} className="border-b border-border/50 last:border-0">
                      <td className="py-1 px-2 text-content-secondary capitalize">{s.stage}</td>
                      <td className="py-1 px-2 text-content tabular-nums text-right">{formatMs(s.durationMs)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </Section>

          <Section title="Warnings">
            {summary.warnings.length === 0 ? (
              <p className="text-sm text-content-muted">None.</p>
            ) : (
              <ul className="list-disc list-inside text-sm text-warning space-y-0.5">
                {summary.warnings.map((w, i) => (
                  <li key={i}>{w}</li>
                ))}
              </ul>
            )}
          </Section>

          <Section title="Error">
            {summary.error ? (
              <p className="text-sm text-error">{summary.error}</p>
            ) : (
              <p className="text-sm text-content-muted">None.</p>
            )}
          </Section>
        </div>
      </div>
    </div>
  );
}
