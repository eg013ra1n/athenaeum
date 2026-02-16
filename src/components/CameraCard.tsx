import { Camera, Clock, Calendar, Database } from "lucide-react";
import { CameraStats } from "../types/models";
import { format } from "date-fns";

interface CameraCardProps {
  camera: CameraStats;
  onOpenCamera: (instrume: string) => void;
}

export default function CameraCard({ camera, onOpenCamera }: CameraCardProps) {
  const formatDate = (dateStr: string | null) => {
    if (!dateStr) return "N/A";
    try {
      return format(new Date(dateStr), "MMM d, yyyy");
    } catch {
      return "N/A";
    }
  };

  const formatHours = (hours: number) => {
    if (hours < 1) {
      return `${Math.round(hours * 60)}m`;
    }
    return `${hours.toFixed(1)}h`;
  };

  return (
    <div className="bg-surface-elevated rounded-lg p-5 border border-border hover:border-border transition-colors">
      {/* Camera name */}
      <div className="flex items-start justify-between mb-4">
        <div className="flex items-center gap-2 flex-1 min-w-0">
          <Camera className="text-accent flex-shrink-0" size={20} />
          <h3
            className="text-lg font-semibold text-content truncate max-w-full"
            title={camera.instrume}
          >
            {camera.instrume}
          </h3>
        </div>
      </div>

      {/* Stats */}
      <div className="space-y-3 mb-4">
        <div className="flex items-center justify-between text-sm">
          <span className="text-content-muted">Frames</span>
          <span className="text-content font-medium">{camera.frame_count.toLocaleString()}</span>
        </div>

        <div className="flex items-center justify-between text-sm">
          <span className="text-content-muted flex items-center gap-1">
            <Clock size={14} />
            Total Time
          </span>
          <span className="text-content font-medium">{formatHours(camera.total_hours)}</span>
        </div>

        <div className="pt-2 border-t border-border">
          <div className="flex items-start gap-1 text-xs text-content-muted">
            <Calendar size={12} className="mt-0.5 flex-shrink-0" />
            <div className="flex flex-col">
              <span>First: {formatDate(camera.first_use)}</span>
              <span>Last: {formatDate(camera.last_use)}</span>
            </div>
          </div>
        </div>
      </div>

      {/* View Calibration Button */}
      <button
        onClick={() => onOpenCamera(camera.instrume)}
        className="w-full flex items-center justify-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover text-white rounded-md transition-colors text-sm font-medium"
      >
        <Database size={16} />
        View  Library
      </button>
    </div>
  );
}
