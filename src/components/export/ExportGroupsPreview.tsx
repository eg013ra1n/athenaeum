import {
  Camera,
  Sun,
  Moon,
  Circle,
  Check,
  X,
  AlertTriangle,
  Layers,
} from 'lucide-react';
import type { ExportGroupsSummary, ExportGroupInfo } from '../../types/export';

interface ExportGroupsPreviewProps {
  summary: ExportGroupsSummary;
}

export function ExportGroupsPreview({ summary }: ExportGroupsPreviewProps) {
  return (
    <div className="space-y-4">
      {/* Overall Summary */}
      <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
        <SummaryCard
          icon={<Layers size={16} />}
          label="Groups"
          value={summary.totalGroups}
        />
        <SummaryCard
          icon={<Sun size={16} />}
          label="Frames"
          value={summary.totalFrames}
        />
        <SummaryCard
          icon={<Camera size={16} />}
          label="Exposure"
          value={formatExposure(summary.totalExposure)}
        />
        <SummaryCard
          icon={<Moon size={16} />}
          label="Masters"
          value={summary.mastersToCreate}
        />
      </div>

      {/* Group Cards */}
      <div className="space-y-2">
        <h4 className="text-sm font-medium text-gray-400">Export Groups</h4>
        {summary.groups.map((group) => (
          <GroupCard key={group.groupKey} group={group} />
        ))}
      </div>
    </div>
  );
}

interface SummaryCardProps {
  icon: React.ReactNode;
  label: string;
  value: string | number;
}

function SummaryCard({ icon, label, value }: SummaryCardProps) {
  return (
    <div className="flex items-center gap-2 p-2 bg-gray-800 rounded-lg">
      <div className="text-gray-400">{icon}</div>
      <div>
        <div className="text-xs text-gray-500">{label}</div>
        <div className="font-medium text-gray-200">{value}</div>
      </div>
    </div>
  );
}

interface GroupCardProps {
  group: ExportGroupInfo;
}

function GroupCard({ group }: GroupCardProps) {
  const isOsc = group.cameraType === 'OSC';

  return (
    <div className="p-3 bg-gray-800/50 border border-gray-700 rounded-lg">
      <div className="flex items-start justify-between">
        {/* Group Info */}
        <div className="flex items-center gap-3">
          <div
            className={`p-2 rounded-lg ${
              isOsc ? 'bg-green-900/30' : 'bg-blue-900/30'
            }`}
          >
            <Camera
              size={16}
              className={isOsc ? 'text-green-400' : 'text-blue-400'}
            />
          </div>
          <div>
            <div className="font-medium text-gray-200">{group.displayName}</div>
            <div className="flex items-center gap-3 text-sm text-gray-400">
              <span>{group.frameCount} frames</span>
              <span>{formatExposure(group.totalExposure)}</span>
              {group.subgroupCount > 1 && (
                <span className="text-blue-400">
                  {group.subgroupCount} subgroups
                </span>
              )}
            </div>
          </div>
        </div>

        {/* Calibration Status */}
        <div className="flex items-center gap-2">
          <CalibrationBadge
            type="Flat"
            available={group.hasFlat}
            icon={<Circle size={12} />}
          />
          <CalibrationBadge
            type="Dark"
            available={group.hasDark}
            icon={<Moon size={12} />}
          />
          <CalibrationBadge
            type="Bias"
            available={group.hasBias}
            icon={<Circle size={12} />}
          />
        </div>
      </div>

      {/* Warnings */}
      {group.warnings.length > 0 && (
        <div className="mt-2 flex items-start gap-2 p-2 bg-yellow-900/20 rounded">
          <AlertTriangle size={14} className="text-yellow-500 mt-0.5 flex-shrink-0" />
          <div className="text-xs text-yellow-400">
            {group.warnings.map((w, i) => (
              <div key={i}>{w}</div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

interface CalibrationBadgeProps {
  type: string;
  available: boolean;
  icon: React.ReactNode;
}

function CalibrationBadge({ type, available, icon }: CalibrationBadgeProps) {
  return (
    <div
      className={`flex items-center gap-1 px-2 py-1 rounded text-xs ${
        available
          ? 'bg-green-900/30 text-green-400'
          : 'bg-red-900/30 text-red-400'
      }`}
      title={`${type}: ${available ? 'Available' : 'Missing'}`}
    >
      {icon}
      {available ? <Check size={10} /> : <X size={10} />}
    </div>
  );
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
