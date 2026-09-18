// Settings redesign (spec 2026-09-18 §2) — General tab, "Data file
// locations". Read-only (desktop only) — no setting to autosave, so no
// `useSettingField` here; this is a plain effect + reveal buttons, unchanged
// from the pre-redesign page.
import { useEffect, useState } from 'react';
import { FolderOpen } from 'lucide-react';
import { api } from '../../../api';
import { revealItemInDir, openPath } from '../../../api/desktop';
import { isTauri } from '../../../utils/platform';
import { SettingsSection } from '../SettingsSection';

export function DataLocationsSection() {
  const [dbPath, setDbPath] = useState<string>('');
  const [logDir, setLogDir] = useState<string>('');

  useEffect(() => {
    if (!isTauri) return;
    let cancelled = false;
    api.invoke<string>('get_database_path')
      .then((p) => { if (!cancelled) setDbPath(p ?? ''); })
      .catch((err) => console.error('[Settings] get_database_path failed:', err));
    api.invoke<string>('get_log_path')
      .then((p) => { if (!cancelled) setLogDir(p ?? ''); })
      .catch((err) => console.error('[Settings] get_log_path failed:', err));
    return () => { cancelled = true; };
  }, []);

  if (!isTauri) return null;

  return (
    <SettingsSection id="general.dataLocations">
      <div className="bg-surface-secondary rounded p-4 text-sm font-mono space-y-3">
        <div className="flex items-center gap-3">
          <span className="text-content-muted min-w-[80px]">Database:</span>
          <span className="text-content truncate flex-1" title={dbPath || undefined}>{dbPath || '—'}</span>
          {dbPath && (
            <button
              onClick={() => revealItemInDir(dbPath)}
              className="text-content-muted hover:text-content transition flex-shrink-0"
              title="Reveal in file manager"
              aria-label="Reveal in file manager"
            >
              <FolderOpen size={16} />
            </button>
          )}
        </div>
        <div className="flex items-center gap-3">
          <span className="text-content-muted min-w-[80px]">Log folder:</span>
          <span className="text-content truncate flex-1" title={logDir || undefined}>{logDir || '—'}</span>
          {logDir && (
            <button
              onClick={() => openPath(logDir)}
              className="text-content-muted hover:text-content transition flex-shrink-0"
              title="Open log folder"
            >
              <FolderOpen size={16} />
            </button>
          )}
        </div>
      </div>
    </SettingsSection>
  );
}
