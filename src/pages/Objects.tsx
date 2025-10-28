import { useState, useEffect } from 'react';
import { useNavigate } from 'react-router-dom';
import { invoke } from '@tauri-apps/api/core';
import { Sparkles, Trash2, Eye, Clock, MapPin, AlertCircle, Target } from 'lucide-react';
import type { FramesSetWithCount, AutoGenerateResult } from '../types/models';

export default function Objects() {
  const navigate = useNavigate();
  const [frameSets, setFrameSets] = useState<FramesSetWithCount[]>([]);
  const [loading, setLoading] = useState(false);
  const [generating, setGenerating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [generateResult, setGenerateResult] = useState<AutoGenerateResult | null>(null);

  // For now, using project_id = 1 as default
  const PROJECT_ID = 1;

  useEffect(() => {
    loadFrameSets();
  }, []);

  const loadFrameSets = async () => {
    try {
      setLoading(true);
      setError(null);
      const sets = await invoke<FramesSetWithCount[]>('get_frames_sets', {
        projectId: PROJECT_ID,
      });
      setFrameSets(sets);
    } catch (err) {
      setError(err as string);
      console.error('Failed to load frame sets:', err);
    } finally {
      setLoading(false);
    }
  };

  const handleAutoGenerate = async () => {
    if (!confirm('Auto-generate frame sets from LIGHT frames? This will cluster frames by sky coordinates.')) {
      return;
    }

    try {
      setGenerating(true);
      setError(null);
      setGenerateResult(null);

      const result = await invoke<AutoGenerateResult>('auto_generate_frame_sets', {
        projectId: PROJECT_ID,
      });

      setGenerateResult(result);

      if (result.sets_created > 0) {
        await loadFrameSets();
      }
    } catch (err) {
      setError(err as string);
      console.error('Failed to auto-generate frame sets:', err);
    } finally {
      setGenerating(false);
    }
  };

  const handleDelete = async (setId: number, setName: string | null) => {
    if (!confirm(`Delete frame set "${setName || 'Untitled'}"? This will not delete the frames themselves.`)) {
      return;
    }

    try {
      await invoke('delete_frames_set', { framesSetId: setId });
      await loadFrameSets();
    } catch (err) {
      setError(err as string);
      console.error('Failed to delete frame set:', err);
    }
  };

  const formatExposureTime = (seconds: number | null) => {
    if (!seconds) return 'N/A';
    const hours = (seconds / 3600).toFixed(1);
    return `${hours}h`;
  };

  return (
    <div className="p-6">
      <div className="mb-6">
        <div className="flex items-center justify-between mb-2">
          <div>
            <h2 className="text-3xl font-bold">Objects Library</h2>
            <p className="text-gray-400">
              Frame sets grouped by sky coordinates
            </p>
          </div>
          <button
            onClick={handleAutoGenerate}
            disabled={generating}
            className="flex items-center gap-2 px-4 py-2 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-600 disabled:cursor-not-allowed text-white rounded-lg transition-colors"
          >
            <Sparkles size={18} />
            {generating ? 'Generating...' : 'Auto-Generate Sets'}
          </button>
        </div>
      </div>

      {error && (
        <div className="mb-4 p-4 bg-red-900/20 border border-red-800 rounded-lg flex items-start gap-3">
          <AlertCircle className="text-red-500 flex-shrink-0 mt-0.5" size={20} />
          <div className="flex-1">
            <p className="font-medium text-red-400">Error</p>
            <p className="text-sm text-red-300">{error}</p>
          </div>
        </div>
      )}

      {generateResult && (
        <div className="mb-4 p-4 bg-green-900/20 border border-green-800 rounded-lg">
          <p className="font-medium text-green-400 mb-2">Generation Complete</p>
          <div className="text-sm text-green-300 space-y-1">
            <p>Sets created: {generateResult.sets_created}</p>
            <p>Frames clustered: {generateResult.frames_clustered}</p>
            {generateResult.frames_already_in_sets > 0 && (
              <p>Frames already in sets (skipped): {generateResult.frames_already_in_sets}</p>
            )}
            {generateResult.frames_excluded > 0 && (
              <p>Frames excluded: {generateResult.frames_excluded}</p>
            )}
          </div>
          {generateResult.exclusion_reasons.length > 0 && (
            <details className="mt-3">
              <summary className="text-sm text-green-400 cursor-pointer">
                View exclusion reasons ({generateResult.exclusion_reasons.length})
              </summary>
              <div className="mt-2 text-xs text-gray-400 max-h-32 overflow-y-auto">
                {generateResult.exclusion_reasons.slice(0, 10).map((reason, i) => (
                  <p key={i} className="truncate">{reason}</p>
                ))}
                {generateResult.exclusion_reasons.length > 10 && (
                  <p className="text-gray-500 italic">
                    ... and {generateResult.exclusion_reasons.length - 10} more
                  </p>
                )}
              </div>
            </details>
          )}
        </div>
      )}

      {loading ? (
        <div className="text-center py-12 text-gray-400">
          <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-blue-500 mx-auto"></div>
          <p className="mt-4">Loading frame sets...</p>
        </div>
      ) : frameSets.length === 0 ? (
        <div className="bg-gray-800 rounded-lg p-8 text-center">
          <Target className="mx-auto mb-4 text-gray-600" size={48} />
          <p className="text-gray-400 mb-4">
            No frame sets yet. Use "Auto-Generate Sets" to cluster your LIGHT frames by sky coordinates.
          </p>
        </div>
      ) : (
        <div className="grid grid-cols-1 lg:grid-cols-2 xl:grid-cols-3 gap-4">
          {frameSets.map(({ frames_set, member_count }) => (
            <div
              key={frames_set.id}
              className="bg-gray-800 rounded-lg p-4 border border-gray-700 hover:border-gray-600 transition-colors"
            >
              <div className="flex items-start justify-between mb-3">
                <div className="flex-1 min-w-0">
                  <h3 className="text-lg font-semibold text-gray-100 truncate">
                    {frames_set.name || 'Untitled'}
                  </h3>
                  {frames_set.objctra && frames_set.objctdec && (
                    <div className="flex items-center gap-1 text-sm text-gray-400 mt-1">
                      <MapPin size={14} />
                      <span className="font-mono text-xs">
                        RA {frames_set.objctra} / Dec {frames_set.objctdec}
                      </span>
                    </div>
                  )}
                </div>
              </div>

              <div className="grid grid-cols-2 gap-3 mb-3 text-sm">
                <div className="bg-gray-900/50 rounded p-2">
                  <p className="text-gray-500 text-xs">Frames</p>
                  <p className="text-gray-200 font-medium">{member_count}</p>
                </div>
                <div className="bg-gray-900/50 rounded p-2">
                  <p className="text-gray-500 text-xs flex items-center gap-1">
                    <Clock size={12} />
                    Total Exp.
                  </p>
                  <p className="text-gray-200 font-medium">
                    {formatExposureTime(frames_set.total_exp_time)}
                  </p>
                </div>
              </div>

              {frames_set.date_obs && (
                <p className="text-xs text-gray-500 mb-3">
                  {new Date(frames_set.date_obs).toLocaleDateString()}
                </p>
              )}

              <div className="flex gap-2">
                <button
                  onClick={() => navigate(`/objects/${frames_set.id}`)}
                  className="flex-1 flex items-center justify-center gap-2 px-3 py-2 bg-gray-700 hover:bg-gray-600 text-gray-200 rounded transition-colors text-sm"
                  title="View members"
                >
                  <Eye size={16} />
                  View
                </button>
                <button
                  onClick={() => handleDelete(frames_set.id!, frames_set.name)}
                  className="px-3 py-2 bg-red-900/20 hover:bg-red-900/40 text-red-400 rounded transition-colors"
                  title="Delete set"
                >
                  <Trash2 size={16} />
                </button>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
