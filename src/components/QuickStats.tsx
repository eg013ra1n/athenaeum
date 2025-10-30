import { Package, Image, Thermometer, Calendar } from "lucide-react";
import { CalibrationSetDetail } from "../types/models";
import { format } from "date-fns";

interface QuickStatsProps {
  sets: CalibrationSetDetail[];
}

export default function QuickStats({ sets }: QuickStatsProps) {
  if (sets.length === 0) {
    return null;
  }

  // Calculate stats
  const totalSets = sets.length;
  const totalFrames = sets.reduce((sum, set) => sum + set.frame_count, 0);

  const avgTemp = sets.reduce((sum, set) => sum + set.ccd_temp, 0) / sets.length;

  const allDates = sets.flatMap(set => [
    new Date(set.date_start),
    new Date(set.date_end)
  ]);
  const firstDate = new Date(Math.min(...allDates.map(d => d.getTime())));
  const lastDate = new Date(Math.max(...allDates.map(d => d.getTime())));

  return (
    <div className="grid grid-cols-2 md:grid-cols-4 gap-4 mb-6">
      {/* Total Sets */}
      <div className="bg-gray-800 rounded-lg p-4 border border-gray-700">
        <div className="flex items-center gap-2 mb-2">
          <Package className="text-blue-400" size={18} />
          <span className="text-gray-400 text-sm">Total Sets</span>
        </div>
        <div className="text-2xl font-bold text-gray-100">{totalSets}</div>
      </div>

      {/* Total Frames */}
      <div className="bg-gray-800 rounded-lg p-4 border border-gray-700">
        <div className="flex items-center gap-2 mb-2">
          <Image className="text-green-400" size={18} />
          <span className="text-gray-400 text-sm">Total Frames</span>
        </div>
        <div className="text-2xl font-bold text-gray-100">{totalFrames.toLocaleString()}</div>
      </div>

      {/* Average Temperature */}
      <div className="bg-gray-800 rounded-lg p-4 border border-gray-700">
        <div className="flex items-center gap-2 mb-2">
          <Thermometer className="text-purple-400" size={18} />
          <span className="text-gray-400 text-sm">Avg Temperature</span>
        </div>
        <div className="text-2xl font-bold text-gray-100">{avgTemp.toFixed(1)}°C</div>
      </div>

      {/* Date Coverage */}
      <div className="bg-gray-800 rounded-lg p-4 border border-gray-700">
        <div className="flex items-center gap-2 mb-2">
          <Calendar className="text-orange-400" size={18} />
          <span className="text-gray-400 text-sm">Date Coverage</span>
        </div>
        <div className="text-sm font-medium text-gray-100">
          {format(firstDate, "MMM yyyy")} - {format(lastDate, "MMM yyyy")}
        </div>
      </div>
    </div>
  );
}
