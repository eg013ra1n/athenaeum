import { useCallback, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { ArrowLeft } from 'lucide-react';
import { api } from '../api';
import { MissingMetadataView } from '../components/missing-metadata/MissingMetadataView';
import type { ExcludedFrameRow, MissingMetadataRow } from '../types/models';

/**
 * Excluded Frames page — frames the auto-generate-frame-sets pass refused
 * to cluster (almost always because their metadata is wrong or missing).
 *
 * The page is now a thin wrapper around MissingMetadataView, which owns the
 * toolbar, table, and side-panel metadata editor (the same MetadataPane the
 * dual-pane file browser uses). The only page-specific bits left here:
 *
 *   - mode="excluded" so the view fetches from get_excluded_frames_with_metadata
 *   - extraColumn renders the per-row exclusion reason
 *   - hideFilterChips drops the per-flag filter (every row is already an
 *     exclusion, so the chips would be noise)
 *   - onMetadataSaved removes the saved file IDs from excluded_frames so the
 *     row leaves the list once the user has fixed its metadata
 */
export default function ExcludedFrames() {
  const navigate = useNavigate();
  const [count, setCount] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  const renderReason = useCallback((row: MissingMetadataRow) => {
    const reason = (row as ExcludedFrameRow).reason;
    return (
      <span
        className="text-xs text-content-muted block leading-snug break-words"
        title={reason}
      >
        {reason}
      </span>
    );
  }, []);

  // After a successful save in the side-panel editor: remove the saved
  // file IDs from excluded_frames. The user fixed what was wrong, so the
  // row should leave the list. The view triggers its own reload after.
  const handleMetadataSaved = useCallback(async (savedFileIds: number[]) => {
    if (savedFileIds.length === 0) return;
    try {
      setError(null);
      await api.invoke<number>('remove_files_from_excluded', { fileIds: savedFileIds });
    } catch (err) {
      console.error('Failed to remove from excluded list:', err);
      setError(String(err));
    }
  }, []);

  return (
    <div className="p-4 pt-3 h-full flex flex-col min-h-0">
      <div className="mb-4 flex-shrink-0">
        <button
          onClick={() => navigate('/objects')}
          className="flex items-center gap-2 text-content-muted hover:text-content transition-colors mb-2"
        >
          <ArrowLeft size={18} />
          Back to Objects
        </button>
        <h2 className="text-2xl font-bold">
          Excluded Frames
          <span className="text-sm font-normal text-content-muted ml-3">
            {count == null
              ? 'Loading…'
              : `${count} frame${count !== 1 ? 's' : ''} excluded during auto-generation — fix metadata in bulk and re-run auto-generate to adopt them`}
          </span>
        </h2>
      </div>

      {error && (
        <div className="mb-3 p-3 bg-error-muted border border-error/50 rounded text-sm text-error flex-shrink-0">
          {error}
          <button
            type="button"
            onClick={() => setError(null)}
            className="ml-2 underline hover:no-underline"
          >
            Dismiss
          </button>
        </div>
      )}

      <div className="flex-1 min-h-0">
        <MissingMetadataView
          mode="excluded"
          onCountChange={setCount}
          extraColumn={{ header: 'Reason', render: renderReason }}
          hideFilterChips
          onMetadataSaved={handleMetadataSaved}
        />
      </div>
    </div>
  );
}
