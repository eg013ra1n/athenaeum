import { useEffect, useState } from 'react';
import { Archive as ArchiveIcon, Star, ExternalLink } from 'lucide-react';
import { listArchiveZips } from '../../api/archive';
import { revealItemInDir } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { Stat, Section } from './MonitoredInspector';
import { basename, formatBytes } from './format';
import type { ArchiveRoot, ArchivedFrameSetSummary, ArchiveZip } from '../../types/helpers';

interface ArchiveInspectorProps {
  root: ArchiveRoot;
  archivedSets: ArchivedFrameSetSummary[];
  totalZipBytes: number;
  onSetDefault: () => void;
  onRemove: () => void;
}

export function ArchiveInspector({ root, archivedSets, totalZipBytes, onSetDefault, onRemove }: ArchiveInspectorProps) {
  const sets = archivedSets.filter((s) => (s.archive_root_path ?? '') === root.path);
  const [zipsBySet, setZipsBySet] = useState<Record<number, ArchiveZip[]>>({});
  const [zipsError, setZipsError] = useState<string | null>(null);
  // The set list is the real dependency: a held selection must pick up sets that
  // arrive after mount (async load) and re-read after a Move-and-ZIP or a delete.
  const setsKey = sets.map((s) => s.operation_id).join(',');

  useEffect(() => {
    setZipsBySet({});
    setZipsError(null);
    let cancelled = false;
    (async () => {
      for (const s of sets) {
        if (!s.operation_id) continue;
        try {
          const zips = await listArchiveZips(s.operation_id);
          if (cancelled) return;
          setZipsBySet((prev) => ({ ...prev, [s.operation_id!]: zips }));
        } catch (e) {
          console.error('[ArchiveInspector] list zips failed:', e);
          if (cancelled) return;
          setZipsError(String(e));
        }
      }
    })();
    return () => { cancelled = true; };
    // `sets` is a fresh array each render — `setsKey` is its identity here.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [root.id, setsKey]);

  return (
    <div className="flex-1 min-w-0 bg-surface-elevated rounded-lg p-5 overflow-y-auto">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2 text-lg font-bold text-content">
            <span className="truncate">{basename(root.path)}</span>
            {root.is_default
              ? <span className="px-2 py-0.5 rounded-full text-[10px] font-semibold bg-warning/20 text-warning border border-warning/40 flex items-center gap-1"><Star size={10} fill="currentColor" /> Default destination</span>
              : <button onClick={onSetDefault} className="px-2 py-0.5 rounded-full text-[10px] font-semibold bg-surface-hover text-content-muted border border-border hover:text-warning transition flex items-center gap-1"><Star size={10} /> Make default</button>}
          </div>
          <div className="flex items-center gap-2 font-mono text-xs text-content-muted">
            <span className="truncate">{root.path}</span>
            {isTauri && (
              <button onClick={() => revealItemInDir(root.path).catch((e) => console.error('[ArchiveInspector] reveal failed:', e))}
                title="Reveal in file manager" aria-label="Reveal in file manager" className="p-0.5 rounded hover:text-accent transition"><ExternalLink size={12} /></button>
            )}
          </div>
        </div>
      </div>

      <div className="mt-4 p-3 rounded-lg bg-surface border border-warning/40 text-xs text-content-muted">
        &ldquo;Move and ZIP&rdquo; writes finished frame sets here. Never scanned — it may live anywhere, even inside a monitored folder.
      </div>

      <div className="flex flex-wrap gap-2 mt-3">
        <Stat label="archived frame sets" value={String(sets.length)} />
        <Stat label="frame-set zips" value={totalZipBytes > 0 ? formatBytes(totalZipBytes) : '—'} />
      </div>

      <Section title="Contents">
        {sets.length === 0 && <p className="text-xs text-content-muted">No archived frame sets stored in this folder yet.</p>}
        {zipsError && <p className="text-xs text-error mb-2 break-all">Some zip lists could not be loaded — {zipsError}</p>}
        <div className="space-y-2">
          {sets.map((set) => {
            const zips = set.operation_id ? zipsBySet[set.operation_id] : undefined;
            return (
              <div key={set.frames_set_id} className="rounded-lg border border-border bg-surface p-3">
                <div className="flex items-center gap-2 text-sm font-medium text-content">
                  <ArchiveIcon size={14} className="text-content-muted" /> {set.name ?? `Frame Set #${set.frames_set_id}`}
                </div>
                <div className="text-xs text-content-muted mt-0.5">
                  {set.archived_at?.slice(0, 10) ?? ''} · {set.lights_count} lights / {set.flats_count} flats / {set.darks_count} darks / {set.bias_count} bias / {set.darkflats_count} darkflats
                </div>
                {zips && zips.length > 0 && (
                  <ul className="mt-2 space-y-1">
                    {zips.map((z) => (
                      <li key={z.path} className="flex items-center gap-2 text-xs">
                        <span className="font-mono text-content-muted truncate flex-1">{z.filename}</span>
                        <span className="text-content-muted whitespace-nowrap">{formatBytes(z.size_bytes)}</span>
                        {!z.exists && <span className="text-error whitespace-nowrap">missing</span>}
                        {isTauri && z.exists && (
                          <button onClick={() => revealItemInDir(z.path).catch((e) => console.error('[ArchiveInspector] reveal failed:', e))}
                            title="Reveal in file manager" aria-label="Reveal in file manager" className="p-0.5 rounded text-content-muted hover:text-accent transition"><ExternalLink size={11} /></button>
                        )}
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            );
          })}
        </div>
      </Section>

      <Section title="Remove">
        <div className="flex items-center gap-3 p-3 rounded-lg border border-error/30 bg-surface">
          <p className="flex-1 text-xs text-content-muted">Removes it from this list only — zips on disk stay.</p>
          <button onClick={onRemove} className="shrink-0 px-3 py-1.5 rounded-lg border border-error/50 text-error text-sm hover:bg-error-muted transition">Remove…</button>
        </div>
      </Section>
    </div>
  );
}
