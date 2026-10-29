// Settings redesign (spec 2026-09-18 §2) — General tab, "Archive". Folder
// management lives in File Manager → Archive Folders (registry description);
// only the global compression preference is here. `set_archive_compression`
// (`crates/athenaeum-tauri/src/commands/archive.rs`) only persists the
// `archive.compression` KV key after validating it's "store"/"deflate" —
// exactly what `enumCodec` already validates — so this writes the key
// directly through the default `set_setting` path rather than overriding
// `write`.
//
// The zip extraction cache (zips in the catalog): its size limit is the KV key `archive.zip_cache_limit_gb`
// (generic `set_setting`); its size and "Clear cache" go through `get_zip_cache_status` / `clear_zip_cache`.
import { useCallback, useEffect, useState } from 'react';
import { Trash2 } from 'lucide-react';
import { api } from '../../../api';
import { useNotifications } from '../../../contexts/NotificationContext';
import type { ZipCacheStatus } from '../../../types/archive';
import { fieldMeta } from '../../../settings/registry';
import { SettingsSection } from '../SettingsSection';
import { SettingSelect } from '../SettingSelect';
import { SettingNumber } from '../SettingNumber';
import { enumCodec, intCodec } from '../../../settings/codecs';
import { formatBytes } from '../../dualpane/types';

const COMPRESSION_OPTIONS = [
  { value: 'store', label: 'Store (no compression — fastest, archive size ≈ source size)' },
  { value: 'deflate', label: 'Deflate (smaller, slower — marginal savings on raw FITS)' },
] as const;

const compressionCodec = enumCodec(['store', 'deflate'] as const);

/** Size of the unpacked-zip cache, with a button that empties what nothing is using. */
export function ZipCacheStatusRow() {
  const { notify } = useNotifications();
  const [status, setStatus] = useState<ZipCacheStatus | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);
  const [clearing, setClearing] = useState(false);
  const meta = fieldMeta('general.archive', 'zipCache');

  const load = useCallback(() => {
    api.invoke<ZipCacheStatus>('get_zip_cache_status')
      .then((st) => { setStatus(st); setLoadFailed(false); })
      .catch((err) => {
        console.error('[Settings] get_zip_cache_status failed:', err);
        setLoadFailed(true);
      });
  }, []);

  useEffect(load, [load]);

  const clear = async () => {
    setClearing(true);
    try {
      setStatus(await api.invoke<ZipCacheStatus>('clear_zip_cache'));
    } catch (err) {
      console.error('[Settings] clear_zip_cache failed:', err);
      notify({ kind: 'generic', tone: 'warning', title: 'Zip cache not cleared', detail: String(err), hasErrors: true });
      load();
    } finally {
      setClearing(false);
    }
  };

  return (
    <div className="flex items-start justify-between gap-4">
      <div className="min-w-0">
        <p className="text-sm text-content">{meta.label}</p>
        <p className="text-xs text-content-muted">{meta.help}</p>
        <p className="mt-0.5 text-xs text-content-secondary" data-testid="zip-cache-status">
          {loadFailed
            ? 'Size unavailable.'
            : status === null
              ? 'Loading…'
              : status.entries === 0
                ? 'Empty.'
                : `${formatBytes(status.bytes)} in ${status.entries.toLocaleString()} unpacked frame${status.entries === 1 ? '' : 's'}.`}
        </p>
      </div>
      <button
        onClick={() => void clear()}
        disabled={clearing || status === null || status.entries === 0}
        className="shrink-0 inline-flex items-center gap-1.5 px-2.5 py-1 rounded text-xs bg-surface-hover text-content-secondary hover:text-error disabled:opacity-50 disabled:cursor-not-allowed"
      >
        <Trash2 className="w-3.5 h-3.5" />
        Clear cache
      </button>
    </div>
  );
}

export function ArchiveSection() {
  return (
    <SettingsSection id="general.archive">
      <div className="space-y-4">
        <SettingSelect
          section="general.archive"
          field="compression"
          settingKey="archive.compression"
          codec={compressionCodec}
          options={COMPRESSION_OPTIONS}
        />
        <SettingNumber
          section="general.archive"
          field="zipCacheLimitGb"
          settingKey="archive.zip_cache_limit_gb"
          codec={intCodec(1, 4096)}
          unit="GB"
        />
        <ZipCacheStatusRow />
      </div>
    </SettingsSection>
  );
}
