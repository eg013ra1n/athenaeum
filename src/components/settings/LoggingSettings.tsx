import { useEffect, useState } from 'react';
import { AlertTriangle, Save } from 'lucide-react';
import { api } from '../../api';
import { useNotifications } from '../../contexts/NotificationContext';
import type { LoggingConfig, LoggingConfigResponse } from '../../types/models';

/** Levels selectable from the UI. `trace` is intentionally excluded — it is
 *  env-only (`ATHENAEUM_LOG`) so users can't accidentally melt their disk. */
const LEVELS: Array<{ value: string; label: string }> = [
  { value: 'error', label: 'Error' },
  { value: 'warn', label: 'Warn' },
  { value: 'info', label: 'Info' },
  { value: 'debug', label: 'Debug' },
];

/** UI module keys — must match `MODULE_TARGETS` in
 *  `athenaeum-core/src/logging/config.rs`. */
const MODULES: Array<{ key: string; label: string; hint?: string }> = [
  { key: 'scanner', label: 'Scanner' },
  { key: 'solver', label: 'Plate Solver' },
  { key: 'calibration', label: 'Calibration' },
  { key: 'archive', label: 'Archive / File Ops' },
  {
    key: 'transport',
    label: 'Transport (iroh / relays)',
    hint: 'Relay, hole-punching and blob-transfer internals. Very verbose at Debug — turn it on to diagnose a transfer or relay problem, then back to Inherit.',
  },
];

/** Sentinel select value meaning "key absent from `modules`" — falls back to
 *  the base level. */
const INHERIT = 'inherit';

export default function LoggingSettings() {
  const { notify } = useNotifications();

  const [level, setLevel] = useState('info');
  // Partial, not Record: LoggingConfig.modules is a HashMap-backed optional-
  // index map on the generated type ({ [key in string]?: string }).
  const [modules, setModules] = useState<Partial<Record<string, string>>>({});
  const [envOverrideActive, setEnvOverrideActive] = useState(false);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        setLoading(true);
        setLoadError(null);
        const resp = await api.invoke<LoggingConfigResponse>('get_logging_config');
        if (cancelled) return;
        setLevel(resp.config.level);
        setModules(resp.config.modules ?? {});
        setEnvOverrideActive(resp.envOverrideActive);
      } catch (err) {
        if (cancelled) return;
        setLoadError(String(err));
        console.error('Failed to load logging config:', err);
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const handleModuleChange = (key: string, value: string) => {
    setModules((prev) => {
      const next = { ...prev };
      if (value === INHERIT) {
        delete next[key];
      } else {
        next[key] = value;
      }
      return next;
    });
  };

  const handleSave = async () => {
    try {
      setSaving(true);
      const config: LoggingConfig = { level, modules };
      await api.invoke('set_logging_config', { config });

      const overrideCount = Object.keys(modules).length;
      notify({
        title: 'Logging settings saved',
        detail: envOverrideActive
          ? 'Saved, but ATHENAEUM_LOG on this server overrides the active level.'
          : `Base level set to "${level}"${overrideCount ? ` with ${overrideCount} module override${overrideCount === 1 ? '' : 's'}` : ''}.`,
        kind: 'generic',
        tone: 'success',
      });
    } catch (err) {
      console.error('Failed to save logging config:', err);
      notify({
        title: 'Failed to save logging settings',
        detail: err instanceof Error ? err.message : String(err),
        kind: 'generic',
        tone: 'warning',
        hasErrors: true,
      });
    } finally {
      setSaving(false);
    }
  };

  if (loading) {
    return <div className="text-sm text-content-muted">Loading logging settings…</div>;
  }

  if (loadError) {
    return (
      <div className="p-4 bg-error-muted border border-error/50 rounded-lg">
        <p className="text-sm text-error">Failed to load logging settings: {loadError}</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {envOverrideActive && (
        <div className="p-3 bg-warning-muted border border-warning/50 rounded-lg flex items-start gap-2">
          <AlertTriangle size={16} className="text-warning flex-shrink-0 mt-0.5" />
          <p className="text-sm text-warning/90">
            Log level is overridden by ATHENAEUM_LOG on this server — UI changes are saved but inactive.
          </p>
        </div>
      )}

      <div>
        <label className="block text-sm font-medium text-content-secondary mb-2">
          Base log level
        </label>
        <select
          value={level}
          onChange={(e) => setLevel(e.target.value)}
          className="w-full sm:w-64 bg-surface-hover border border-border rounded-lg px-3 py-2 text-content focus:outline-none focus:border-accent"
        >
          {LEVELS.map((l) => (
            <option key={l.value} value={l.value}>
              {l.label}
            </option>
          ))}
        </select>
        <p className="text-xs text-content-muted mt-2">
          Applies everywhere unless overridden per module below. Higher verbosity (Debug) writes
          more to the JSONL log file in the folder above.
        </p>
      </div>

      <div>
        <h4 className="text-sm font-medium text-content-secondary mb-2">Module overrides</h4>
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
          {MODULES.map((m) => (
            <div key={m.key}>
              <label className="block text-xs text-content-muted mb-1">{m.label}</label>
              <select
                value={modules[m.key] ?? INHERIT}
                onChange={(e) => handleModuleChange(m.key, e.target.value)}
                className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
              >
                <option value={INHERIT}>Inherit ({level})</option>
                {LEVELS.map((l) => (
                  <option key={l.value} value={l.value}>
                    {l.label}
                  </option>
                ))}
              </select>
              {m.hint && <p className="text-xs text-content-muted mt-1">{m.hint}</p>}
            </div>
          ))}
        </div>
      </div>

      <div className="pt-2">
        <button
          onClick={handleSave}
          disabled={saving}
          className="flex items-center gap-2 px-6 py-2 bg-accent hover:bg-accent-hover disabled:bg-surface-hover disabled:cursor-not-allowed text-surface rounded-lg transition-colors"
        >
          <Save size={18} />
          {saving ? 'Saving...' : 'Save Logging Settings'}
        </button>
      </div>
    </div>
  );
}
