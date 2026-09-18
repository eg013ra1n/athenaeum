// Settings redesign (spec 2026-09-18 §8): the base level and every module
// row render through one `LevelSelect` each — no more five copies of the
// same `<select>` markup — and the document autosaves through
// `useAutosaveDocument`, so there is no Save button and no success toast
// (the hook notifies on failure only). `general.logging`'s registry
// description already states the `ATHENAEUM_LOG` override rule once; this
// component only renders the LIVE banner when the override is actually
// active right now (`envOverrideActive`, captured from the same
// `get_logging_config` response the document's `load` reads).

import { api } from '../../api';
import { useAutosaveDocument } from '../../hooks/useAutosaveDocument';
import { useSettingsDefaults } from '../../settings/SettingsDefaultsContext';
import { LevelSelect, LEVEL_INHERIT } from './LevelSelect';
import { SettingsSection } from './SettingsSection';
import type { LoggingConfig, LoggingConfigResponse } from '../../types/models';
import { AlertTriangle } from 'lucide-react';
import { useState } from 'react';

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

/** A module row's current selection — its own override, or `LEVEL_INHERIT`
 *  when the key is absent (falls back to the base level). */
function toModuleValue(modules: LoggingConfig['modules'], key: string): string {
  return modules[key] ?? LEVEL_INHERIT;
}

/** Writes a module row's selection back into `modules` — `LEVEL_INHERIT`
 *  deletes the key (absent means inherit), any real level sets it. */
function fromModuleValue(modules: LoggingConfig['modules'], key: string, value: string): LoggingConfig['modules'] {
  const next = { ...modules };
  if (value === LEVEL_INHERIT) {
    delete next[key];
  } else {
    next[key] = value;
  }
  return next;
}

export default function LoggingSettings() {
  const { defaults } = useSettingsDefaults();
  const [envOverrideActive, setEnvOverrideActive] = useState(false);

  const { doc, patch, error, resetAll } = useAutosaveDocument<LoggingConfig>({
    load: async () => {
      const resp = await api.invoke<LoggingConfigResponse>('get_logging_config');
      setEnvOverrideActive(resp.envOverrideActive);
      return resp.config;
    },
    save: (config) => api.invoke('set_logging_config', { config }),
    // There is no `reset_logging_config` command — the default document IS
    // the reset (spec §6: "for a typed config it calls the existing
    // reset_* command and reloads"; here, writing the default achieves the
    // same net effect since there is no dedicated command to call).
    resetAll: async () => {
      if (!defaults?.logging) {
        throw new Error('defaults not loaded yet');
      }
      await api.invoke('set_logging_config', { config: defaults.logging });
    },
    defaults: defaults?.logging ?? null,
    label: 'Logging settings',
  });

  if (doc === null) {
    if (error) {
      return (
        <SettingsSection id="general.logging">
          <div className="p-4 bg-error-muted border border-error/50 rounded-lg">
            <p className="text-sm text-error">Failed to load logging settings: {error}</p>
          </div>
        </SettingsSection>
      );
    }
    return (
      <SettingsSection id="general.logging">
        <div className="text-sm text-content-muted">Loading logging settings…</div>
      </SettingsSection>
    );
  }

  return (
    <SettingsSection id="general.logging" onResetAll={resetAll}>
      <div className="space-y-4">
        {envOverrideActive && (
          <div className="p-3 bg-warning-muted border border-warning/50 rounded-lg flex items-start gap-2">
            <AlertTriangle size={16} className="text-warning flex-shrink-0 mt-0.5" />
            <p className="text-sm text-warning/90">
              Log level is overridden by ATHENAEUM_LOG on this server — UI changes are saved but inactive.
            </p>
          </div>
        )}

        <LevelSelect
          label="Base log level"
          value={doc.level}
          onChange={(level) => patch({ level })}
          className="w-full sm:w-64"
        />

        <div>
          <h4 className="text-sm font-medium text-content-secondary mb-2">Module overrides</h4>
          <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
            {MODULES.map((m) => (
              <div key={m.key}>
                <LevelSelect
                  label={m.label}
                  value={toModuleValue(doc.modules, m.key)}
                  onChange={(value) =>
                    patch((prev) => ({ ...prev, modules: fromModuleValue(prev.modules, m.key, value) }))
                  }
                  inherit={{ base: doc.level }}
                />
                {m.hint && <p className="text-xs text-content-muted mt-1">{m.hint}</p>}
              </div>
            ))}
          </div>
        </div>
      </div>
    </SettingsSection>
  );
}
