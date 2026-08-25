import { useState } from 'react';
import {
  ChevronDown,
  ChevronRight,
  Camera,
  Telescope,
  Thermometer,
  Check,
  X,
  AlertTriangle,
  FileImage,
} from 'lucide-react';
import type {
  FilterGroupSummary,
  CalibrationDetail,
  FrameDetail,
  CameraType,
  DetailedWarning,
} from '../../types/export';
import { getFilterColor } from '../../utils/filterColors';

interface FilterGroupCardProps {
  group: FilterGroupSummary;
  warnings?: DetailedWarning[];
}

/**
 * Filter-grouped summary card showing exposure breakdown, equipment, and calibrations
 */
export function FilterGroupCard({ group, warnings = [] }: FilterGroupCardProps) {
  const [framesExpanded, setFramesExpanded] = useState(false);

  const filterColor = getFilterColor(group.filter);
  const filterLabel = group.filter || 'Luminance';

  // Get warnings for specific calibration types
  const getCalibrationWarnings = (calibType: string): DetailedWarning[] => {
    return warnings.filter(
      (w) =>
        w.warningType === 'temperature_mismatch' ||
        w.warningType === 'calibration_age'
    ).filter((w) => w.title.toLowerCase().includes(calibType.toLowerCase()));
  };

  return (
    <div className="border border-border rounded-lg overflow-hidden bg-surface">
      {/* Header */}
      <div className="p-4 bg-surface-elevated border-b border-border">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <div
              className="w-3 h-3 rounded-full"
              style={{ backgroundColor: filterColor }}
              title={filterLabel}
            />
            <h3 className="font-medium text-content">
              {filterLabel}
              {group.filter && isNarrowband(group.filter) && (
                <span className="ml-2 text-xs px-1.5 py-0.5 bg-purple/20 text-purple rounded">
                  Narrowband
                </span>
              )}
            </h3>
            <CameraTypeBadge type={group.cameraType} />
          </div>
          <div className="flex items-center gap-4 text-sm text-content-muted">
            <span>{group.frameCount} frames</span>
            <span className="font-medium text-content">{formatExposure(group.totalExposure)}</span>
          </div>
        </div>
      </div>

      {/* Body */}
      <div className="p-4 space-y-4">
        {/* Exposure Breakdown */}
        <div>
          <h4 className="text-xs font-medium text-content-muted uppercase tracking-wide mb-2">
            Exposure Breakdown
          </h4>
          <div className="flex flex-wrap gap-2">
            {group.exposureGroups.map((eg, idx) => (
              <div key={idx} className="px-3 py-1.5 bg-surface-elevated rounded text-sm">
                <span className="font-medium">{formatExptime(eg.exptime)}</span>
                <span className="text-content-muted"> x {eg.count} = </span>
                <span className="text-accent">{formatExposure(eg.totalSeconds)}</span>
              </div>
            ))}
          </div>
        </div>

        {/* Equipment Info */}
        <div>
          <h4 className="text-xs font-medium text-content-muted uppercase tracking-wide mb-2">
            Equipment
          </h4>
          <div className="flex flex-wrap gap-4 text-sm">
            {group.camera && (
              <div className="flex items-center gap-1.5">
                <Camera size={14} className="text-content-muted" />
                <span>{group.camera}</span>
              </div>
            )}
            {group.telescope && (
              <div className="flex items-center gap-1.5">
                <Telescope size={14} className="text-content-muted" />
                <span>{group.telescope}</span>
              </div>
            )}
            {group.gain !== null && (
              <div className="text-content-muted">
                Gain: <span className="text-content">{group.gain}</span>
              </div>
            )}
            {group.offset !== null && (
              <div className="text-content-muted">
                Offset: <span className="text-content">{group.offset}</span>
              </div>
            )}
            {group.binning && (
              <div className="text-content-muted">
                Binning: <span className="text-content">{group.binning}</span>
              </div>
            )}
            {group.avgTemp !== null && (
              <div className="flex items-center gap-1.5">
                <Thermometer size={14} className="text-content-muted" />
                <span>{group.avgTemp.toFixed(1)}C</span>
              </div>
            )}
          </div>
        </div>

        {/* Calibrations */}
        <div>
          <h4 className="text-xs font-medium text-content-muted uppercase tracking-wide mb-2">
            Calibrations
          </h4>
          <div className="space-y-2">
            <CalibrationRow
              label="Flat"
              info={group.flatInfo}
              lightTemp={group.avgTemp}
              warnings={getCalibrationWarnings('flat')}
            />
            <CalibrationRow
              label="Dark"
              info={group.darkInfo}
              lightTemp={group.avgTemp}
              warnings={getCalibrationWarnings('dark')}
            />
            <CalibrationRow
              label="Bias"
              info={group.biasInfo}
              lightTemp={group.avgTemp}
              warnings={getCalibrationWarnings('bias')}
            />
          </div>
        </div>

        {/* Expandable Frame List */}
        {group.frames.length > 0 && (
          <div>
            <button
              onClick={() => setFramesExpanded(!framesExpanded)}
              className="flex items-center gap-2 text-sm text-content-muted hover:text-content transition-colors"
            >
              {framesExpanded ? <ChevronDown size={16} /> : <ChevronRight size={16} />}
              <FileImage size={14} />
              <span>Show {group.frames.length} frames...</span>
            </button>

            {framesExpanded && (
              <div className="mt-3 max-h-80 overflow-y-auto border border-border rounded">
                <FrameList frames={group.frames} />
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

interface CalibrationRowProps {
  label: string;
  info: CalibrationDetail | null;
  lightTemp: number | null;
  warnings?: DetailedWarning[];
}

function CalibrationRow({ label, info, lightTemp, warnings = [] }: CalibrationRowProps) {
  const tempDelta =
    info && lightTemp !== null && info.avgTemp !== null
      ? Math.abs(lightTemp - info.avgTemp)
      : 0;
  const hasTempWarning = tempDelta > 2;

  // Find calibration age warning if any
  const ageWarning = warnings.find((w) => w.warningType === 'calibration_age');

  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-center gap-3 text-sm">
        <div className="w-16 text-content-muted">{label}:</div>
        {info ? (
          <>
            <Check size={14} className="text-success" />
            <span>
              Set #{info.setId} ({info.frameCount} frames
              {info.avgExptime !== null && ` @ ${formatExptime(info.avgExptime)}`}
              {info.avgTemp !== null && `, ${info.avgTemp.toFixed(1)}C`})
            </span>
            {hasTempWarning && (
              <span className="flex items-center gap-1 text-warning">
                <AlertTriangle size={12} />
                <span className="text-xs">
                  {tempDelta.toFixed(1)}C from lights
                </span>
              </span>
            )}
          </>
        ) : (
          <>
            <X size={14} className="text-content-muted" />
            <span className="text-content-muted italic">Not linked</span>
          </>
        )}
      </div>
      {ageWarning && (
        <div className="ml-16 pl-3 text-xs text-warning/80 flex items-center gap-1">
          <AlertTriangle size={10} />
          {ageWarning.description}
        </div>
      )}
    </div>
  );
}

interface FrameListProps {
  frames: FrameDetail[];
}

function FrameList({ frames }: FrameListProps) {
  return (
    <table className="w-full text-sm">
      <thead className="bg-surface-elevated sticky top-0">
        <tr className="text-left text-xs text-content-muted uppercase">
          <th className="px-3 py-2">Filename</th>
          <th className="px-3 py-2">Date</th>
          <th className="px-3 py-2">Exp</th>
          <th className="px-3 py-2">Temp</th>
          <th className="px-3 py-2">Calibration</th>
        </tr>
      </thead>
      <tbody className="divide-y divide-border">
        {frames.map((frame) => (
          <tr key={frame.frameId} className="hover:bg-surface-elevated/50">
            <td className="px-3 py-2 font-mono text-xs">{frame.filename}</td>
            <td className="px-3 py-2 text-content-muted">
              {frame.dateObs ? formatDateTime(frame.dateObs) : '-'}
            </td>
            <td className="px-3 py-2">{frame.exptime ? formatExptime(frame.exptime) : '-'}</td>
            <td className="px-3 py-2">{frame.temp !== null ? `${frame.temp.toFixed(1)}C` : '-'}</td>
            <td className="px-3 py-2 text-content-muted text-xs">{frame.calibrationChain}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function CameraTypeBadge({ type }: { type: CameraType }) {
  const isOsc = type === 'osc';
  return (
    <span
      className={`text-xs px-1.5 py-0.5 rounded ${
        isOsc ? 'bg-success/20 text-success' : 'bg-accent/20 text-accent'
      }`}
    >
      {isOsc ? 'OSC' : 'Mono'}
    </span>
  );
}

function isNarrowband(filter: string): boolean {
  const f = filter.toLowerCase();
  return f.includes('ha') || f.includes('oiii') || f.includes('sii') || f.includes('nii');
}

function formatExptime(seconds: number): string {
  if (seconds < 1) {
    return `${(seconds * 1000).toFixed(0)}ms`;
  }
  return `${seconds.toFixed(1)}s`;
}

function formatExposure(seconds: number): string {
  if (seconds < 60) {
    return `${seconds.toFixed(0)}s`;
  } else if (seconds < 3600) {
    const minutes = seconds / 60;
    return `${minutes.toFixed(1)}m`;
  } else {
    const hours = seconds / 3600;
    return `${hours.toFixed(2)}h`;
  }
}

function formatDateTime(dateStr: string): string {
  try {
    const date = new Date(dateStr);
    return date.toLocaleString('en-US', {
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
    });
  } catch {
    return dateStr.substring(0, 16);
  }
}
