import { useState, useEffect } from 'react';
import { useParams, useNavigate } from 'react-router-dom';
import { invoke } from '@tauri-apps/api/core';
import { ArrowLeft, Calendar, Clock, MapPin, Camera, AlertCircle, File as FileIcon } from 'lucide-react';
import type { FrameSetDetail, ImagingNightWithSessions, SessionWithFrames } from '../types/models';

export default function FrameSetDetail() {
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const [detail, setDetail] = useState<FrameSetDetail | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

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
      setError(err as string);
      console.error('Failed to load frame set detail:', err);
    } finally {
      setLoading(false);
    }
  };

  const formatDate = (isoString: string) => {
    return new Date(isoString).toLocaleString('en-US', {
      month: 'short',
      day: 'numeric',
      year: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
    });
  };

  const formatDateRange = (startTime: string, endTime: string) => {
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

    if (startDate === endDate) {
      return startDate;
    }
    return `${startDate} - ${endDate}`;
  };

  const formatExposureTime = (seconds: number | null) => {
    if (!seconds) return 'N/A';
    const hours = (seconds / 3600).toFixed(1);
    const minutes = Math.round((seconds % 3600) / 60);
    if (parseFloat(hours) >= 1) {
      return `${hours}h`;
    }
    return `${minutes}m`;
  };

  const renderSession = (session: SessionWithFrames) => {
    const { session: sessionInfo, frames } = session;

    return (
      <div key={sessionInfo.id} className="bg-gray-800 rounded-lg overflow-hidden mb-4">
        {/* Session Header */}
        <div className="bg-gray-750 border-b border-gray-700 px-4 py-3">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-3">
              <Camera size={18} className="text-blue-400" />
              <div>
                <h4 className="font-semibold text-gray-100">{sessionInfo.instrume}</h4>
                <p className="text-xs text-gray-400 mt-0.5">
                  {sessionInfo.frame_count} frame{sessionInfo.frame_count !== 1 ? 's' : ''} •
                  {' '}{formatExposureTime(sessionInfo.total_exp_time)} total
                </p>
              </div>
            </div>
          </div>
        </div>

        {/* Frames Table */}
        <div className="overflow-x-auto">
          {/* Table Header */}
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

          {/* File Rows */}
          <div className="divide-y divide-gray-700">
            {frames.map((item, idx) => (
              <div
                key={item.file.id || idx}
                className="grid grid-cols-12 gap-4 px-4 py-3 hover:bg-gray-700 transition items-center"
              >
                <div className="col-span-3 flex items-center gap-2 min-w-0">
                  <FileIcon size={14} className="text-gray-500 flex-shrink-0" />
                  <span className="font-mono text-sm truncate" title={item.file.filename}>
                    {item.file.filename}
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
      </div>
    );
  };

  const renderNight = (night: ImagingNightWithSessions) => {
    return (
      <div key={night.imaging_night.id} className="mb-8">
        {/* Night Header */}
        <div className="bg-gray-750 rounded-lg px-4 py-3 mb-4 border border-gray-700">
          <div className="flex items-center gap-3">
            <Calendar size={20} className="text-purple-400" />
            <div>
              <h3 className="font-semibold text-lg text-gray-100">
                {formatDateRange(night.imaging_night.start_time, night.imaging_night.end_time)}
              </h3>
              <div className="flex items-center gap-4 text-sm text-gray-400 mt-1">
                <div className="flex items-center gap-1">
                  <Clock size={14} />
                  {formatDate(night.imaging_night.start_time)} → {formatDate(night.imaging_night.end_time)}
                </div>
                <div>{night.sessions.length} session{night.sessions.length !== 1 ? 's' : ''}</div>
              </div>
            </div>
          </div>
        </div>

        {/* Sessions within this night */}
        {night.sessions.map(session => renderSession(session))}
      </div>
    );
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

  if (error || !detail) {
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
        <div className="bg-red-900/20 border border-red-800 rounded-lg p-4">
          <div className="flex items-center gap-2 text-red-400">
            <AlertCircle size={20} />
            <span>Error: {error || 'Failed to load frame set details'}</span>
          </div>
        </div>
      </div>
    );
  }

  const totalFrames = detail.nights.reduce(
    (acc, night) => acc + night.sessions.reduce((acc2, session) => acc2 + session.session.frame_count, 0),
    0
  );

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
        <h1 className="text-3xl font-bold mb-4">{detail.frames_set.name || 'Untitled'}</h1>

        <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
          <div className="bg-gray-900/50 rounded p-4">
            <p className="text-gray-500 text-sm mb-1">Coordinates</p>
            {detail.frames_set.objctra && detail.frames_set.objctdec ? (
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
              {formatExposureTime(detail.frames_set.total_exp_time)}
            </p>
          </div>
        </div>
      </div>

      {/* Imaging Nights */}
      <div>
        <h2 className="text-2xl font-bold mb-4">Imaging Sessions</h2>
        {detail.nights.length === 0 ? (
          <div className="bg-gray-800 rounded-lg p-8 text-center text-gray-500">
            No imaging sessions found for this frame set.
          </div>
        ) : (
          detail.nights.map(night => renderNight(night))
        )}
      </div>
    </div>
  );
}
