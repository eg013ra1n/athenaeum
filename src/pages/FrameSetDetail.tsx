import { useState, useEffect } from 'react';
import { useParams, useNavigate } from 'react-router-dom';
import { invoke } from '@tauri-apps/api/core';
import { ArrowLeft, Calendar, Clock, MapPin, Camera, AlertCircle, File as FileIcon, ChevronDown, ChevronRight } from 'lucide-react';
import type { FrameSetDetail, ImagingNightWithSessions, SessionWithFrames } from '../types/models';

export default function FrameSetDetail() {
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const [detail, setDetail] = useState<FrameSetDetail | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [expandedSessions, setExpandedSessions] = useState<Set<number>>(new Set());

  useEffect(() => {
    loadDetail();
  }, [id]);

  const loadDetail = async () => {
    if (!id) return;

    try {
      setLoading(true);
      setError(null);
      const result = await invoke<FrameSetDetail>('get_frame_set_detail', {
        framesSetId: parseInt(id),
      });
      setDetail(result);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  };

  const formatDate = (isoString: string) => {
    try {
      return new Date(isoString).toLocaleString('en-US', {
        month: 'short',
        day: 'numeric',
        year: 'numeric',
        hour: '2-digit',
        minute: '2-digit',
      });
    } catch {
      return isoString;
    }
  };

  const formatDateRange = (startTime: string, endTime: string) => {
    try {
      const start = new Date(startTime);
      const end = new Date(endTime);
      const startDate = start.toLocaleDateString('en-US', {
        day: '2-digit',
        month: 'short',
        year: 'numeric',
      });
      const endDate = end.toLocaleDateString('en-US', {
        day: '2-digit',
        month: 'short',
        year: 'numeric',
      });
      return startDate === endDate ? startDate : `${startDate} - ${endDate}`;
    } catch {
      return `${startTime} - ${endTime}`;
    }
  };

  const formatExposureTime = (seconds: number | null | undefined) => {
    if (!seconds) return 'N/A';
    const hours = (seconds / 3600).toFixed(1);
    const minutes = Math.round((seconds % 3600) / 60);
    return parseFloat(hours) >= 1 ? `${hours}h` : `${minutes}m`;
  };

  const toggleSession = (sessionId: number | null | undefined) => {
    if (!sessionId) return;
    setExpandedSessions(prev => {
      const newSet = new Set(prev);
      if (newSet.has(sessionId)) {
        newSet.delete(sessionId);
      } else {
        newSet.add(sessionId);
      }
      return newSet;
    });
  };

  if (loading) {
    return (
      <div className="p-6">
        <div className="text-center py-12 text-gray-400">
          <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-blue-500 mx-auto"></div>
          <p className="mt-4">Loading frame set details...</p>
        </div>
      </div>
    );
  }

  if (error) {
    return (
      <div className="p-6">
        <div className="mb-4">
          <button
            onClick={() => navigate('/objects')}
            className="flex items-center gap-2 px-4 py-2 bg-gray-700 hover:bg-gray-600 rounded-lg transition"
          >
            <ArrowLeft size={18} />
            Back to Objects
          </button>
        </div>
        <div className="bg-red-900/20 border border-red-800 rounded-lg p-6">
          <div className="flex items-start gap-3">
            <AlertCircle size={20} className="text-red-400 flex-shrink-0 mt-0.5" />
            <div className="flex-1">
              <h3 className="text-red-400 font-semibold mb-2">Error Loading Frame Set</h3>
              <p className="text-red-300 text-sm">{error}</p>
            </div>
          </div>
        </div>
      </div>
    );
  }

  if (!detail) {
    return (
      <div className="p-6">
        <div className="mb-4">
          <button
            onClick={() => navigate('/objects')}
            className="flex items-center gap-2 px-4 py-2 bg-gray-700 hover:bg-gray-600 rounded-lg transition"
          >
            <ArrowLeft size={18} />
            Back to Objects
          </button>
        </div>
        <div className="bg-gray-800 rounded-lg p-6 text-center text-gray-400">
          No data available
        </div>
      </div>
    );
  }

  const totalFrames = detail.nights.length > 0
    ? detail.nights.reduce(
        (acc, night) => acc + night.sessions.reduce((acc2, session) => acc2 + (session.session?.frame_count || 0), 0),
        0
      )
    : 0;

  return (
    <div className="p-6">
      {/* Back Button */}
      <div className="mb-6">
        <button
          onClick={() => navigate('/objects')}
          className="flex items-center gap-2 px-4 py-2 bg-gray-700 hover:bg-gray-600 rounded-lg transition"
        >
          <ArrowLeft size={18} />
          Back to Objects
        </button>
      </div>

      {/* Frame Set Header */}
      <div className="bg-gray-800 rounded-lg p-6 mb-6 border border-gray-700">
        <h1 className="text-3xl font-bold mb-4">{detail.frames_set?.name || 'Untitled'}</h1>

        <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
          <div className="bg-gray-900/50 rounded p-4">
            <p className="text-gray-500 text-sm mb-1">Coordinates</p>
            {detail.frames_set?.objctra && detail.frames_set?.objctdec ? (
              <div className="flex items-center gap-2">
                <MapPin size={16} className="text-gray-400" />
                <span className="font-mono text-sm text-gray-200">
                  {detail.frames_set.objctra} / {detail.frames_set.objctdec}
                </span>
              </div>
            ) : (
              <span className="text-gray-500">-</span>
            )}
          </div>

          <div className="bg-gray-900/50 rounded p-4">
            <p className="text-gray-500 text-sm mb-1">Total Frames</p>
            <p className="text-2xl font-bold text-gray-200">{totalFrames}</p>
          </div>

          <div className="bg-gray-900/50 rounded p-4">
            <p className="text-gray-500 text-sm mb-1 flex items-center gap-1">
              <Clock size={14} />
              Total Exposure
            </p>
            <p className="text-2xl font-bold text-gray-200">
              {formatExposureTime(detail.frames_set?.total_exp_time)}
            </p>
          </div>
        </div>
      </div>

      {/* Imaging Nights */}
      <div>
        <h2 className="text-2xl font-bold mb-4">Imaging Sessions</h2>
        {!detail.nights || detail.nights.length === 0 ? (
          <div className="bg-yellow-900/20 border border-yellow-800 rounded-lg p-8 text-center">
            <AlertCircle size={48} className="mx-auto mb-4 text-yellow-500" />
            <h3 className="text-xl font-semibold text-yellow-400 mb-2">No Sessions Available</h3>
            <p className="text-gray-400 mb-4">
              This frame set was created without session data. Sessions are created automatically
              when you use "Auto-Generate Sets" with frames that have DATE-OBS metadata.
            </p>
            <div className="flex gap-3 justify-center">
              <button
                onClick={() => navigate('/objects')}
                className="px-4 py-2 bg-gray-700 hover:bg-gray-600 rounded-lg transition"
              >
                Back to Objects
              </button>
              <button
                onClick={async () => {
                  if (confirm('Delete this frame set? You can recreate it using "Auto-Generate Sets".')) {
                    try {
                      await invoke('delete_frames_set', { framesSetId: parseInt(id!) });
                      navigate('/objects');
                    } catch (err) {
                      alert('Failed to delete: ' + String(err));
                    }
                  }
                }}
                className="px-4 py-2 bg-red-600 hover:bg-red-700 text-white rounded-lg transition"
              >
                Delete Frame Set
              </button>
            </div>
          </div>
        ) : (
          <div className="space-y-8">
            {detail.nights.map((night) => (
              <div key={night.imaging_night?.id || Math.random()} className="mb-8">
                {/* Night Header */}
                <div className="bg-gray-750 rounded-lg px-4 py-3 mb-4 border border-gray-700">
                  <div className="flex items-center gap-3">
                    <Calendar size={20} className="text-purple-400" />
                    <div>
                      <h3 className="font-semibold text-lg text-gray-100">
                        {formatDateRange(night.imaging_night?.start_time || '', night.imaging_night?.end_time || '')}
                      </h3>
                      <div className="flex items-center gap-4 text-sm text-gray-400 mt-1">
                        <div className="flex items-center gap-1">
                          <Clock size={14} />
                          {formatDate(night.imaging_night?.start_time || '')} → {formatDate(night.imaging_night?.end_time || '')}
                        </div>
                        <div>{night.sessions?.length || 0} session{(night.sessions?.length || 0) !== 1 ? 's' : ''}</div>
                      </div>
                    </div>
                  </div>
                </div>

                {/* Sessions within this night */}
                {night.sessions?.map((sessionData) => {
                  const sessionId = sessionData.session?.id;
                  const isExpanded = sessionId ? expandedSessions.has(sessionId) : false;

                  return (
                    <div key={sessionId || Math.random()} className="bg-gray-800 rounded-lg overflow-hidden mb-4">
                      {/* Session Header - Clickable */}
                      <button
                        onClick={() => toggleSession(sessionId)}
                        className="w-full bg-gray-750 border-b border-gray-700 px-4 py-3 hover:bg-gray-700 transition-colors text-left"
                      >
                        <div className="flex items-center gap-3">
                          {isExpanded ? (
                            <ChevronDown size={18} className="text-gray-400" />
                          ) : (
                            <ChevronRight size={18} className="text-gray-400" />
                          )}
                          <Camera size={18} className="text-blue-400" />
                          <div>
                            <h4 className="font-semibold text-gray-100">{sessionData.session?.instrume || 'Unknown'}</h4>
                            <p className="text-xs text-gray-400 mt-0.5">
                              {sessionData.session?.frame_count || 0} frame{(sessionData.session?.frame_count || 0) !== 1 ? 's' : ''} •
                              {' '}{formatExposureTime(sessionData.session?.total_exp_time)} total
                            </p>
                          </div>
                        </div>
                      </button>

                      {/* Frames Table - Collapsible */}
                      {isExpanded && sessionData.frames && sessionData.frames.length > 0 && (
                      <div className="overflow-x-auto">
                        <div className="grid grid-cols-12 gap-4 px-4 py-3 bg-gray-800 text-xs font-semibold text-gray-400 uppercase border-b border-gray-700">
                          <div className="col-span-3">Filename</div>
                          <div className="col-span-2">Time</div>
                          <div className="col-span-2">Object</div>
                          <div className="col-span-1">Filter</div>
                          <div className="col-span-1 text-right">Exposure</div>
                          <div className="col-span-1 text-right">Type</div>
                          <div className="col-span-1 text-right">Focal Len</div>
                          <div className="col-span-1 text-right">Temp</div>
                        </div>
                        <div className="divide-y divide-gray-700">
                          {sessionData.frames.map((item, idx) => (
                            <div
                              key={item.file?.id || idx}
                              className="grid grid-cols-12 gap-4 px-4 py-3 hover:bg-gray-700 transition items-center"
                            >
                              <div className="col-span-3 flex items-center gap-2 min-w-0">
                                <FileIcon size={14} className="text-gray-500 flex-shrink-0" />
                                <span className="font-mono text-sm truncate" title={item.file?.filename || ''}>
                                  {item.file?.filename || '-'}
                                </span>
                              </div>
                              <div className="col-span-2 text-sm text-gray-400">
                                {item.frame?.date_obs
                                  ? new Date(item.frame.date_obs).toLocaleTimeString('en-US', {
                                      hour: '2-digit',
                                      minute: '2-digit',
                                    })
                                  : '-'}
                              </div>
                              <div className="col-span-2 truncate text-sm text-gray-300">
                                {item.frame?.object || '-'}
                              </div>
                              <div className="col-span-1 truncate text-sm text-gray-400">
                                {item.frame?.filter || '-'}
                              </div>
                              <div className="col-span-1 text-right text-sm text-gray-400">
                                {item.frame?.exptime ? `${item.frame.exptime}s` : '-'}
                              </div>
                              <div className="col-span-1 text-right">
                                {item.frame?.imagetyp && (
                                  <span
                                    className={`px-2 py-0.5 rounded text-xs ${
                                      item.frame.imagetyp === 'Light'
                                        ? 'bg-blue-900 text-blue-200'
                                        : 'bg-gray-700 text-gray-300'
                                    }`}
                                  >
                                    {item.frame.imagetyp}
                                  </span>
                                )}
                              </div>
                              <div className="col-span-1 text-right text-sm text-gray-500">
                                {item.frame?.focallen ? `${item.frame.focallen}mm` : '-'}
                              </div>
                              <div className="col-span-1 text-right text-sm text-gray-500">
                                {item.frame?.ccd_temp ? `${item.frame.ccd_temp}°C` : '-'}
                              </div>
                            </div>
                          ))}
                        </div>
                      </div>
                      )}
                    </div>
                  );
                })}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
