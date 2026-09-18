// Settings redesign (spec 2026-09-18 §2) — General tab, "Content index".
// "Build index now" is an action, not a setting (spec §5) — stays a plain
// button driven by `useContentIndex`, unchanged from the pre-redesign page.
import { RefreshCw } from 'lucide-react';
import { SettingsSection } from '../SettingsSection';
import { SettingToggle } from '../SettingToggle';
import { useContentIndex } from '../../../hooks/useContentIndex';

export function ContentIndexSection() {
  const contentIndex = useContentIndex();

  return (
    <SettingsSection id="general.contentIndex">
      <div className="space-y-3">
        {contentIndex.status && (
          <p className="text-sm text-content-secondary">
            {contentIndex.status.total === 0
              ? 'No files catalogued yet.'
              : contentIndex.status.pending === 0
                ? `All ${contentIndex.status.total} files indexed.`
                : `${contentIndex.status.pending} of ${contentIndex.status.total} files not indexed yet.`}
          </p>
        )}

        {contentIndex.status && contentIndex.status.pending > 0 && (
          <p className="text-xs text-content-muted">
            The count only reaches zero for files the app can read. Files on storage that is
            offline, files that changed since the last scan, and files archived into a ZIP are
            skipped and stay counted — running the job again will not clear them. Bring the
            storage back online, restore an archive, or rescan the files that changed, and the
            next run picks them up.
          </p>
        )}

        {/* Hidden while a pass runs: the previous run's counts beside a
            spinning "Indexing…" button would read as this run's result. */}
        {!contentIndex.running && contentIndex.lastFinished && (
          <p className={`text-xs ${contentIndex.lastFinished.failed ? 'text-warning' : 'text-content-muted'}`}>
            {contentIndex.lastFinished.failed
              ? 'The last run could not read the catalog and indexed nothing. See the log for details.'
              : `Last run${contentIndex.lastFinished.cancelled ? ' (cancelled)' : ''}: ${contentIndex.lastFinished.updated} indexed${
                  contentIndex.lastFinished.skipped > 0 ? `, ${contentIndex.lastFinished.skipped} skipped` : ''
                }.`}
          </p>
        )}

        <SettingToggle section="general.contentIndex" field="useContentHash" settingKey="duplicates.use_content_hash" />

        {/* Also gated on pending: with nothing to index the button is
            disabled, and inviting a build it refuses would read as broken. */}
        {contentIndex.status && !contentIndex.status.syncConfigured && contentIndex.status.pending > 0 && (
          <p className="text-xs text-content-muted">
            Sync is not set up on this device, so the index is not built automatically. You can
            still build it now.
          </p>
        )}

        <button
          onClick={contentIndex.start}
          disabled={contentIndex.starting || !contentIndex.status || contentIndex.running || contentIndex.status.pending === 0}
          className="flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover disabled:bg-surface-hover disabled:text-content-muted disabled:cursor-not-allowed text-surface rounded-lg transition-colors"
        >
          <RefreshCw size={18} className={contentIndex.running ? 'animate-spin' : ''} />
          {contentIndex.running ? 'Indexing…' : 'Build index now'}
        </button>
      </div>
    </SettingsSection>
  );
}
