import { useState, useMemo } from "react";
import { Filter, X, ChevronDown, ChevronUp } from "lucide-react";
import { CalibrationSetDetail, ImageType } from "../types/models";

export interface FilterState {
  types: ImageType[];
  gains: number[];
  offsets: number[];
  binnings: string[];
  tempBands: string[];
  exposures: number[];
}

interface DarkLibraryFiltersProps {
  sets: CalibrationSetDetail[];
  onFilterChange: (filters: FilterState) => void;
}

export default function DarkLibraryFilters({ sets, onFilterChange }: DarkLibraryFiltersProps) {
  const [isExpanded, setIsExpanded] = useState(false);
  const [filters, setFilters] = useState<FilterState>({
    types: [],
    gains: [],
    offsets: [],
    binnings: [],
    tempBands: [],
    exposures: [],
  });

  // Extract unique values from sets
  const uniqueValues = useMemo(() => {
    const gains = Array.from(new Set(sets.map(s => s.gain).filter((g): g is number => g !== null))).sort((a, b) => a - b);
    const offsets = Array.from(new Set(sets.map(s => s.offset).filter((o): o is number => o !== null))).sort((a, b) => a - b);
    const binnings = Array.from(new Set(sets.map(s => s.binning).filter((b): b is string => b !== null))).sort();
    const exposures = Array.from(new Set(sets.map(s => s.exptime).filter((e): e is number => e !== null))).sort((a, b) => a - b);

    // Generate temperature bands (5°C steps)
    const temps = sets.map(s => s.ccd_temp).filter(t => t !== null && t !== undefined);
    const minTemp = Math.floor(Math.min(...temps) / 5) * 5;
    const maxTemp = Math.ceil(Math.max(...temps) / 5) * 5;

    const tempBands: string[] = [];
    for (let temp = minTemp; temp < maxTemp; temp += 5) {
      tempBands.push(`${temp} to ${temp + 5}°C`);
    }

    return { gains, offsets, binnings, exposures, tempBands };
  }, [sets]);

  const handleTypeToggle = (type: ImageType) => {
    const newTypes = filters.types.includes(type)
      ? filters.types.filter(t => t !== type)
      : [...filters.types, type];

    const newFilters = { ...filters, types: newTypes };
    setFilters(newFilters);
    onFilterChange(newFilters);
  };

  const handleMultiSelect = (field: keyof FilterState, value: any) => {
    const currentValues = filters[field] as any[];
    const newValues = currentValues.includes(value)
      ? currentValues.filter(v => v !== value)
      : [...currentValues, value];

    const newFilters = { ...filters, [field]: newValues };
    setFilters(newFilters);
    onFilterChange(newFilters);
  };

  const handleClearAll = () => {
    const emptyFilters: FilterState = {
      types: [],
      gains: [],
      offsets: [],
      binnings: [],
      tempBands: [],
      exposures: [],
    };
    setFilters(emptyFilters);
    onFilterChange(emptyFilters);
  };

  const activeFilterCount =
    filters.types.length +
    filters.gains.length +
    filters.offsets.length +
    filters.binnings.length +
    filters.tempBands.length +
    filters.exposures.length;

  return (
    <div className="mb-4">
      {/* Filter Toggle Button */}
      <button
        onClick={() => setIsExpanded(!isExpanded)}
        className="flex items-center gap-2 px-4 py-2 bg-gray-800 hover:bg-gray-700 text-gray-100 rounded-md transition-colors border border-gray-700"
      >
        <Filter size={16} />
        <span>Filters</span>
        {activeFilterCount > 0 && (
          <span className="px-2 py-0.5 bg-blue-600 text-white text-xs rounded-full">
            {activeFilterCount}
          </span>
        )}
        {isExpanded ? <ChevronUp size={16} /> : <ChevronDown size={16} />}
      </button>

      {/* Filter Panel */}
      {isExpanded && (
        <div className="mt-4 p-4 bg-gray-800 rounded-lg border border-gray-700 space-y-4">
          {/* Type Filter */}
          <div>
            <label className="block text-sm font-medium text-gray-300 mb-2">
              Type
            </label>
            <div className="flex gap-3">
              <label className="flex items-center gap-2 cursor-pointer">
                <input
                  type="checkbox"
                  checked={filters.types.includes("Dark" as ImageType)}
                  onChange={() => handleTypeToggle("Dark" as ImageType)}
                  className="rounded border-gray-600 text-blue-600 focus:ring-blue-500"
                />
                <span className="text-sm text-gray-100">Dark</span>
              </label>
              <label className="flex items-center gap-2 cursor-pointer">
                <input
                  type="checkbox"
                  checked={filters.types.includes("Bias" as ImageType)}
                  onChange={() => handleTypeToggle("Bias" as ImageType)}
                  className="rounded border-gray-600 text-blue-600 focus:ring-blue-500"
                />
                <span className="text-sm text-gray-100">Bias</span>
              </label>
            </div>
          </div>

          {/* Gain Filter */}
          {uniqueValues.gains.length > 0 && (
            <div>
              <label className="block text-sm font-medium text-gray-300 mb-2">
                Gain
              </label>
              <div className="flex flex-wrap gap-2">
                {uniqueValues.gains.map(gain => (
                  <button
                    key={gain}
                    onClick={() => handleMultiSelect('gains', gain)}
                    className={`px-3 py-1 rounded text-sm transition-colors ${
                      filters.gains.includes(gain)
                        ? 'bg-blue-600 text-white'
                        : 'bg-gray-700 text-gray-300 hover:bg-gray-600'
                    }`}
                  >
                    {gain}
                  </button>
                ))}
              </div>
            </div>
          )}

          {/* Offset Filter */}
          {uniqueValues.offsets.length > 0 && (
            <div>
              <label className="block text-sm font-medium text-gray-300 mb-2">
                Offset
              </label>
              <div className="flex flex-wrap gap-2">
                {uniqueValues.offsets.map(offset => (
                  <button
                    key={offset}
                    onClick={() => handleMultiSelect('offsets', offset)}
                    className={`px-3 py-1 rounded text-sm transition-colors ${
                      filters.offsets.includes(offset)
                        ? 'bg-blue-600 text-white'
                        : 'bg-gray-700 text-gray-300 hover:bg-gray-600'
                    }`}
                  >
                    {offset}
                  </button>
                ))}
              </div>
            </div>
          )}

          {/* Binning Filter */}
          {uniqueValues.binnings.length > 0 && (
            <div>
              <label className="block text-sm font-medium text-gray-300 mb-2">
                Binning
              </label>
              <div className="flex flex-wrap gap-2">
                {uniqueValues.binnings.map(binning => (
                  <button
                    key={binning}
                    onClick={() => handleMultiSelect('binnings', binning)}
                    className={`px-3 py-1 rounded text-sm transition-colors ${
                      filters.binnings.includes(binning)
                        ? 'bg-blue-600 text-white'
                        : 'bg-gray-700 text-gray-300 hover:bg-gray-600'
                    }`}
                  >
                    {binning}
                  </button>
                ))}
              </div>
            </div>
          )}

          {/* Temperature Bands Filter */}
          {uniqueValues.tempBands.length > 0 && (
            <div>
              <label className="block text-sm font-medium text-gray-300 mb-2">
                Temperature Bands
              </label>
              <div className="flex flex-wrap gap-2">
                {uniqueValues.tempBands.map(band => (
                  <button
                    key={band}
                    onClick={() => handleMultiSelect('tempBands', band)}
                    className={`px-3 py-1 rounded text-sm transition-colors ${
                      filters.tempBands.includes(band)
                        ? 'bg-blue-600 text-white'
                        : 'bg-gray-700 text-gray-300 hover:bg-gray-600'
                    }`}
                  >
                    {band}
                  </button>
                ))}
              </div>
            </div>
          )}

          {/* Exposure Filter */}
          {uniqueValues.exposures.length > 0 && (
            <div>
              <label className="block text-sm font-medium text-gray-300 mb-2">
                Exposure (seconds)
              </label>
              <div className="flex flex-wrap gap-2">
                {uniqueValues.exposures.map(exp => (
                  <button
                    key={exp}
                    onClick={() => handleMultiSelect('exposures', exp)}
                    className={`px-3 py-1 rounded text-sm transition-colors ${
                      filters.exposures.includes(exp)
                        ? 'bg-blue-600 text-white'
                        : 'bg-gray-700 text-gray-300 hover:bg-gray-600'
                    }`}
                  >
                    {exp}s
                  </button>
                ))}
              </div>
            </div>
          )}

          {/* Clear All Button */}
          {activeFilterCount > 0 && (
            <div className="pt-2 border-t border-gray-700">
              <button
                onClick={handleClearAll}
                className="flex items-center gap-2 px-4 py-2 bg-red-900/20 hover:bg-red-900/30 text-red-400 rounded-md transition-colors text-sm"
              >
                <X size={16} />
                Clear All Filters
              </button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
