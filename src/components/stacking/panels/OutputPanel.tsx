// Stage 9 (Output) inspector panel: the two per-set folder overrides
// (`config.paths.workingDir`/`outputDir`), cleanup policy, format. The
// folder picker reuses the Transfers picker verbatim (plan Ruling 6):
// desktop opens the native dialog via `pickDirectory`, web opens
// `FolderBrowserModal` with `scope="stacking"`.

import { useEffect, useState } from 'react';
import { api } from '../../../api';
import { pickDirectory } from '../../../api/desktop';
import { isTauri } from '../../../utils/platform';
import { FolderBrowserModal } from '../../FolderBrowserModal';
import { FolderCard } from '../FolderCard';
import { cleanupLabel } from '../stageSummary';
import type { PathSetting } from '../../../types/models';
import type { CleanupPolicy, OutputFormat, StackingConfig, StackingPaths, StackingPlan } from '../../../types/stacking';

const CLEANUP_POLICIES: CleanupPolicy[] = ['keepAll', 'deleteRegistered', 'deleteIntermediates'];

const FORMATS: { value: OutputFormat; label: string }[] = [
  { value: 'fits', label: 'FITS — float32, the format every tool here reads' },
  { value: 'xisf', label: 'XISF: one image, uncompressed, the same cards' },
];

export interface OutputPanelProps {
  config: StackingConfig;
  onChange: (next: StackingConfig) => void;
  plan: StackingPlan | null;
  disabled?: boolean;
  /** Task 5: `'global'` is `StackingSection`'s usage (Settings → Stacking) —
   *  a per-set folder OVERRIDE is meaningless against the global defaults
   *  themselves (`config.paths` stays `null`/`null` there; the two DEFAULT
   *  folders are their own cards elsewhere on that page), so the two
   *  override cards below and their picker modal are hidden, and the
   *  `get_stacking_paths` fetch that only feeds their "Default: …" line is
   *  skipped. Cleanup policy and format are unaffected — they still apply
   *  to the global default. Defaults to `'perSet'`, Task 3/4's unchanged
   *  behavior. */
  mode?: 'perSet' | 'global';
}

export function OutputPanel({ config, onChange, plan, disabled, mode = 'perSet' }: OutputPanelProps) {
  const [globalPaths, setGlobalPaths] = useState<StackingPaths | null>(null);
  const [browsing, setBrowsing] = useState<'working' | 'output' | null>(null);

  useEffect(() => {
    if (mode === 'global') return;
    let cancelled = false;
    (async () => {
      try {
        const p = await api.invoke<StackingPaths>('get_stacking_paths', {});
        if (!cancelled) setGlobalPaths(p);
      } catch (err) {
        console.error('[OutputPanel] get_stacking_paths failed:', err);
      }
    })();
    return () => { cancelled = true; };
  }, [mode]);

  const setPath = (which: 'workingDir' | 'outputDir', value: string | null) => {
    onChange({ ...config, paths: { ...config.paths, [which]: value } });
  };

  const choose = async (which: 'working' | 'output') => {
    if (disabled) return;
    if (isTauri) {
      try {
        const picked = await pickDirectory();
        if (!picked) return;
        setPath(which === 'working' ? 'workingDir' : 'outputDir', picked);
      } catch (err) {
        console.error('[OutputPanel] folder picker failed:', err);
      }
    } else {
      setBrowsing(which);
    }
  };

  const workingSetting: PathSetting = {
    configured: config.paths.workingDir,
    effective: plan?.workingDir ?? '',
    default: globalPaths?.working.effective ?? '',
    restartRequired: false,
  };
  const outputSetting: PathSetting = {
    configured: config.paths.outputDir,
    effective: plan?.outputDir ?? '',
    default: globalPaths?.output.effective ?? '',
    restartRequired: false,
  };

  return (
    <div className="space-y-3">
      {mode === 'perSet' && (
        <>
          <FolderCard
            title="Working folder"
            hint="Where this run stages registered/intermediate frames."
            setting={workingSetting}
            onChoose={() => choose('working')}
            onReset={() => setPath('workingDir', null)}
            error={null}
            busy={!!disabled}
          />
          <FolderCard
            title="Output folder"
            hint="Where this run writes its master(s)."
            setting={outputSetting}
            onChoose={() => choose('output')}
            onReset={() => setPath('outputDir', null)}
            error={null}
            busy={!!disabled}
          />
        </>
      )}

      <div>
        <label className="block text-xs text-content-secondary mb-1">Cleanup policy</label>
        <select
          value={config.output.cleanup}
          disabled={disabled}
          onChange={(e) => onChange({ ...config, output: { ...config.output, cleanup: e.target.value as CleanupPolicy } })}
          className="w-full px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent disabled:opacity-50"
        >
          {CLEANUP_POLICIES.map((v) => (
            <option key={v} value={v}>{cleanupLabel(v)}</option>
          ))}
        </select>
      </div>

      {/* Format (M4d Task 2, ruling R-M4d-3) — a radio, like the Register
       *  panel's geometry: two choices whose labels carry the whole
       *  explanation. The rejection maps stay FITS in both cases, which is
       *  what the note below says. */}
      <div>
        <span className="block text-xs text-content-secondary mb-1">Format</span>
        <div className="space-y-1.5">
          {FORMATS.map((f) => (
            <label key={f.value} className="flex items-start gap-2 cursor-pointer">
              <input
                type="radio"
                name="stacking-output-format"
                value={f.value}
                checked={config.output.format === f.value}
                disabled={disabled}
                onChange={() => onChange({ ...config, output: { ...config.output, format: f.value } })}
                className="mt-0.5 w-4 h-4 border-border bg-surface-hover text-accent focus:ring-accent disabled:opacity-50"
              />
              <span className="text-sm text-content-secondary">{f.label}</span>
            </label>
          ))}
        </div>
        <p className="mt-1 text-[11px] text-content-muted">
          Applies to the master, the drizzled master and the drizzle weight map. The rejection
          maps stay FITS.
        </p>
      </div>

      <FolderBrowserModal
        isOpen={browsing !== null}
        scope="stacking"
        onSelect={(path) => {
          const which = browsing;
          setBrowsing(null);
          if (!which) {
            console.error('[OutputPanel] folder selected with no target — dropping', path);
            return;
          }
          setPath(which === 'working' ? 'workingDir' : 'outputDir', path);
        }}
        onClose={() => setBrowsing(null)}
      />
    </div>
  );
}
