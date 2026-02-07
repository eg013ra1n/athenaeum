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
import type { ExportGroup } from '../../types/export';

interface ExportGroupsPreviewProps {
  groups: ExportGroup[];
  totalFrames: number;
  totalExposure: number;
}

export function ExportGroupsPreview({ groups, totalFrames, totalExposure }: ExportGroupsPreviewProps) {
  return (
    <div className="space-y-4">
      {/* Overall Summary */}
      <div className="grid grid-cols-2 sm:grid-cols-3 gap-3">
        <SummaryCard
          icon={<Layers size={16} />}
          label="Groups"
          value={groups.length}
        />
        <SummaryCard
          icon={<Sun size={16} />}
          label="Frames"
          value={totalFrames}
        />
        <SummaryCard
          icon={<Camera size={16} />}
          label="Exposure"
          value={formatExposure(totalExposure)}
        />
      </div>

      {/* Group Cards */}
      <div className="space-y-2">
        <h4 className="text-sm font-medium text-content-muted">Export Groups</h4>
        {groups.map((group) => (
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
    <div className="flex items-center gap-2 p-2 bg-surface-elevated rounded-lg">
      <div className="text-content-muted">{icon}</div>
      <div>
        <div className="text-xs text-content-muted">{label}</div>
        <div className="font-medium text-content">{value}</div>
      </div>
    </div>
  );
}

interface GroupCardProps {
  group: ExportGroup;
}

function GroupCard({ group }: GroupCardProps) {
  const isOsc = group.cameraType === 'osc';

  // Check calibration availability from subgroups
  const hasFlat = group.subgroups.some(sg => sg.flat !== null);
  const hasDark = group.subgroups.some(sg => sg.dark !== null);
  const hasBias = group.subgroups.some(sg => sg.bias !== null);

  return (
    <div className="p-3 bg-surface-elevated/50 border border-border rounded-lg">
      <div className="flex items-start justify-between">
        {/* Group Info */}
        <div className="flex items-center gap-3">
          <div
            className={`p-2 rounded-lg ${
              isOsc ? 'bg-success-muted' : 'bg-info-muted'
            }`}
          >
            <Camera
              size={16}
              className={isOsc ? 'text-success' : 'text-accent'}
            />
          </div>
          <div>
            <div className="font-medium text-content">{group.displayName}</div>
            <div className="flex items-center gap-3 text-sm text-content-muted">
              <span>{group.totalFrames} frames</span>
              <span>{formatExposure(group.totalExposure)}</span>
              {group.subgroups.length > 1 && (
                <span className="text-accent">
                  {group.subgroups.length} subgroups
                </span>
              )}
            </div>
          </div>
        </div>

        {/* Calibration Status */}
        <div className="flex items-center gap-2">
          <CalibrationBadge
            type="Flat"
            available={hasFlat}
            icon={<Circle size={12} />}
          />
          <CalibrationBadge
            type="Dark"
            available={hasDark}
            icon={<Moon size={12} />}
          />
          <CalibrationBadge
            type="Bias"
            available={hasBias}
            icon={<Circle size={12} />}
          />
        </div>
      </div>

      {/* Warnings */}
      {group.warnings.length > 0 && (
        <div className="mt-2 flex items-start gap-2 p-2 bg-warning-muted rounded">
          <AlertTriangle size={14} className="text-warning mt-0.5 flex-shrink-0" />
          <div className="text-xs text-warning">
            {group.warnings.map((w: string, i: number) => (
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
          ? 'bg-success-muted text-success'
          : 'bg-error-muted text-error'
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
