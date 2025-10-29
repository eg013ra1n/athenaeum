import { useState, useEffect } from 'react';
import { useNavigate } from 'react-router-dom';
import { invoke } from '@tauri-apps/api/core';
import { Sparkles, Trash2, Eye, Clock, MapPin, AlertCircle, Target, Pencil, Check, X, Star } from 'lucide-react';
import type { FramesSetWithCount, AutoGenerateResult } from '../types/models';

export default function Objects() {
  const navigate = useNavigate();
  const [frameSets, setFrameSets] = useState<FramesSetWithCount[]>([]);
  const [loading, setLoading] = useState(false);
  const [generating, setGenerating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [generateResult, setGenerateResult] = useState<AutoGenerateResult | null>(null);
  const [editingSetId, setEditingSetId] = useState<number | null>(null);
  const [editingName, setEditingName] = useState<string>('');

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

  const startEditing = (setId: number, currentName: string | null) => {
    setEditingSetId(setId);
    setEditingName(currentName || '');
  };

  const cancelEditing = () => {
    setEditingSetId(null);
    setEditingName('');
  };

  const saveRename = async (setId: number) => {
    if (!editingName.trim()) {
      setError('Name cannot be empty');
      return;
    }

    try {
      await invoke('rename_frames_set', {
        framesSetId: setId,
        newName: editingName.trim()
      });
      await loadFrameSets();
      setEditingSetId(null);
      setEditingName('');
    } catch (err) {
      setError(err as string);
      console.error('Failed to rename frame set:', err);
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
            <p className="text-sm text-red-300">{String(error)}</p>
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
              className="bg-gray-800 rounded-lg p-4 border border-gray-700 hover:border-gray-600 transition-colors group"
            >
              <div className="flex items-start justify-between mb-3">
                <div className="flex-1 min-w-0">
                  {editingSetId === frames_set.id ? (
                    <div className="flex items-center gap-2">
                      <input
                        type="text"
                        value={editingName}
                        onChange={(e) => setEditingName(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === 'Enter') {
                            saveRename(frames_set.id!);
                          } else if (e.key === 'Escape') {
                            cancelEditing();
                          }
                        }}
                        className="flex-1 px-2 py-1 bg-gray-700 text-gray-100 rounded border border-gray-600 focus:outline-none focus:border-blue-500"
                        autoFocus
                      />
                      <button
                        onClick={() => saveRename(frames_set.id!)}
                        className="p-1 text-green-400 hover:text-green-300"
                        title="Save"
                      >
                        <Check size={18} />
                      </button>
                      <button
                        onClick={cancelEditing}
                        className="p-1 text-red-400 hover:text-red-300"
                        title="Cancel"
                      >
                        <X size={18} />
                      </button>
                    </div>
                  ) : (
                    <div className="flex items-center gap-2">
                      {frames_set.is_custom && (
                        <Star size={16} className="text-orange-500 fill-orange-500 flex-shrink-0" title="Custom Set" />
                      )}
                      <h3 className="text-lg font-semibold text-gray-100 truncate">
                        {frames_set.name || 'Untitled'}
                      </h3>
                      <button
                        onClick={() => startEditing(frames_set.id!, frames_set.name)}
                        className="p-1 text-gray-400 hover:text-gray-200 opacity-0 group-hover:opacity-100 transition-opacity"
                        title="Rename"
                      >
                        <Pencil size={14} />
                      </button>
                    </div>
                  )}
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
