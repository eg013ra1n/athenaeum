import { Camera, Clock, Calendar, Database } from "lucide-react";
import { CameraStats } from "../types/models";
import { format } from "date-fns";

interface CameraCardProps {
  camera: CameraStats;
  onOpenDarkLibrary: (instrume: string) => void;
}

export default function CameraCard({ camera, onOpenDarkLibrary }: CameraCardProps) {
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
    <div className="bg-gray-800 rounded-lg p-5 border border-gray-700 hover:border-gray-600 transition-colors">
      {/* Camera name */}
      <div className="flex items-start justify-between mb-4">
        <div className="flex items-center gap-2">
          <Camera className="text-blue-400" size={20} />
          <h3 className="text-lg font-semibold text-gray-100 truncate">
            {camera.instrume}
          </h3>
        </div>
      </div>

      {/* Stats */}
      <div className="space-y-3 mb-4">
        <div className="flex items-center justify-between text-sm">
          <span className="text-gray-400">Frames</span>
          <span className="text-gray-100 font-medium">{camera.frame_count.toLocaleString()}</span>
        </div>

        <div className="flex items-center justify-between text-sm">
          <span className="text-gray-400 flex items-center gap-1">
            <Clock size={14} />
            Total Time
          </span>
          <span className="text-gray-100 font-medium">{formatHours(camera.total_hours)}</span>
        </div>

        <div className="pt-2 border-t border-gray-700">
          <div className="flex items-start gap-1 text-xs text-gray-400">
            <Calendar size={12} className="mt-0.5 flex-shrink-0" />
            <div className="flex flex-col">
              <span>First: {formatDate(camera.first_use)}</span>
              <span>Last: {formatDate(camera.last_use)}</span>
            </div>
          </div>
        </div>
      </div>

      {/* Dark Library Button */}
      <button
        onClick={() => onOpenDarkLibrary(camera.instrume)}
        className="w-full flex items-center justify-center gap-2 px-4 py-2 bg-blue-600 hover:bg-blue-700 text-white rounded-md transition-colors text-sm font-medium"
      >
        <Database size={16} />
        Dark Library
      </button>
    </div>
  );
}
