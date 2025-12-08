import { Package, Calendar } from "lucide-react";
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

  const allDates = sets.flatMap(set => [
    new Date(set.date_start),
    new Date(set.date_end)
  ]);
  const firstDate = new Date(Math.min(...allDates.map(d => d.getTime())));
  const lastDate = new Date(Math.max(...allDates.map(d => d.getTime())));

  return (
    <div className="flex items-center gap-6 mb-6">
      {/* Total Sets */}
      <div className="flex items-center gap-2">
        <Package className="text-blue-400" size={18} />
        <span className="text-gray-400 text-sm">Sets:</span>
        <span className="text-lg font-bold text-gray-100">{totalSets}</span>
      </div>

      {/* Date Coverage */}
      <div className="flex items-center gap-2">
        <Calendar className="text-orange-400" size={18} />
        <span className="text-gray-400 text-sm">Coverage:</span>
        <span className="text-sm font-medium text-gray-100">
          {format(firstDate, "MMM yyyy")} - {format(lastDate, "MMM yyyy")}
        </span>
      </div>
    </div>
  );
}
