import { CheckCircle, AlertTriangle, XCircle, Loader2 } from 'lucide-react';
import type { FrameCalibrationStatus } from '../types/models';

interface CalibrationStatusBadgesProps {
  status: FrameCalibrationStatus | null;
  loading?: boolean;
}

export function CalibrationStatusBadges({ status, loading }: CalibrationStatusBadgesProps) {
  if (loading) {
    return (
      <div className="flex items-center gap-2">
        <Loader2 size={16} className="text-gray-400 animate-spin" />
        <span className="text-xs text-gray-400">Loading calibration status...</span>
      </div>
    );
  }

  if (!status) {
    return (
      <div className="flex items-center gap-1">
        <div className="px-2 py-0.5 bg-gray-700 text-gray-400 text-xs rounded flex items-center gap-1">
          <span>No calibration</span>
        </div>
      </div>
    );
  }

  const badges = [];

  // Flat badge
  if (status.has_flats) {
    badges.push({
      type: 'Flat',
      status: status.flats_warning ? 'warning' : 'success',
      icon: status.flats_warning ? AlertTriangle : CheckCircle,
      tooltip: status.flats_warning
        ? 'Flat calibration found (with warnings)'
        : 'Flat calibration found'
    });
  } else {
    badges.push({
      type: 'Flat',
      status: 'missing',
      icon: XCircle,
      tooltip: 'Flat calibration missing'
    });
  }

  // Dark badge
  if (status.has_darks) {
    badges.push({
      type: 'Dark',
      status: status.darks_warning ? 'warning' : 'success',
      icon: status.darks_warning ? AlertTriangle : CheckCircle,
      tooltip: status.darks_warning
        ? 'Dark calibration found (with warnings)'
        : 'Dark calibration found'
    });
  } else {
    badges.push({
      type: 'Dark',
      status: 'missing',
      icon: XCircle,
      tooltip: 'Dark calibration missing'
    });
  }

  // Bias badge (optional)
  if (status.has_bias) {
    badges.push({
      type: 'Bias',
      status: status.bias_warning ? 'warning' : 'success',
      icon: status.bias_warning ? AlertTriangle : CheckCircle,
      tooltip: status.bias_warning
        ? 'Bias calibration found (with warnings)'
        : 'Bias calibration found'
    });
  }

  const getStatusColors = (badgeStatus: string) => {
    switch (badgeStatus) {
      case 'success':
        return 'bg-green-900/30 text-green-400 border-green-700';
      case 'warning':
        return 'bg-yellow-900/30 text-yellow-400 border-yellow-700';
      case 'missing':
        return 'bg-red-900/30 text-red-400 border-red-700';
      default:
        return 'bg-gray-700 text-gray-400 border-gray-600';
    }
  };

  return (
    <div className="flex items-center gap-1.5">
      {badges.map((badge, index) => {
        const Icon = badge.icon;
        return (
          <div
            key={index}
            className={`px-2 py-0.5 text-xs rounded border flex items-center gap-1 ${getStatusColors(badge.status)}`}
            title={badge.tooltip}
          >
            <Icon size={12} />
            <span>{badge.type}</span>
          </div>
        );
      })}
      {(status.flats_warning || status.darks_warning || status.bias_warning) && (
        <span className="text-xs text-yellow-400" title="Has warnings">
          (⚠)
        </span>
      )}
    </div>
  );
}
