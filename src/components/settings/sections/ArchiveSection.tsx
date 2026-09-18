// Settings redesign (spec 2026-09-18 §2) — General tab, "Archive". Folder
// management lives in File Manager → Archive Folders (registry description);
// only the global compression preference is here. `set_archive_compression`
// (`crates/athenaeum-tauri/src/commands/archive.rs`) only persists the
// `archive.compression` KV key after validating it's "store"/"deflate" —
// exactly what `enumCodec` already validates — so this writes the key
// directly through the default `set_setting` path rather than overriding
// `write`.
import { SettingsSection } from '../SettingsSection';
import { SettingSelect } from '../SettingSelect';
import { enumCodec } from '../../../settings/codecs';

const COMPRESSION_OPTIONS = [
  { value: 'store', label: 'Store (no compression — fastest, archive size ≈ source size)' },
  { value: 'deflate', label: 'Deflate (smaller, slower — marginal savings on raw FITS)' },
] as const;

const compressionCodec = enumCodec(['store', 'deflate'] as const);

export function ArchiveSection() {
  return (
    <SettingsSection id="general.archive">
      <SettingSelect
        section="general.archive"
        field="compression"
        settingKey="archive.compression"
        codec={compressionCodec}
        options={COMPRESSION_OPTIONS}
      />
    </SettingsSection>
  );
}
