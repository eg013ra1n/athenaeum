import React, { useState, useEffect } from 'react';
import { FlatGroup } from '../types/models';
import { Calendar, Thermometer, Hash, Clock } from 'lucide-react';

interface ManualFlatSelectionModalProps {
  isOpen: boolean;
  flatGroupOptions: Record<string, FlatGroup[]>; // filter -> FlatGroup[]
  onSelect: (selections: Record<string, number>) => void; // filter -> set_id
  onCancel: () => void;
}

export const ManualFlatSelectionModal: React.FC<ManualFlatSelectionModalProps> = ({
  isOpen,
  flatGroupOptions,
  onSelect,
  onCancel,
}) => {
  // State: filter -> selected group index
  const [selections, setSelections] = useState<Record<string, number>>({});
  const [isValid, setIsValid] = useState(false);

  // Initialize selections with first group for each filter
  useEffect(() => {
    const initialSelections: Record<string, number> = {};
    Object.keys(flatGroupOptions).forEach((filter) => {
      if (flatGroupOptions[filter].length > 0) {
        initialSelections[filter] = 0; // Select first group by default
      }
    });
    setSelections(initialSelections);
  }, [flatGroupOptions]);

  // Validate that all filters have a selection
  useEffect(() => {
    const allSelected = Object.keys(flatGroupOptions).every(
      (filter) => selections[filter] !== undefined
    );
    setIsValid(allSelected);
  }, [selections, flatGroupOptions]);

  if (!isOpen) return null;

  const handleApply = () => {
    // Convert selections from group index to actual FlatGroup for creating calibration sets
    // The backend will call create_flat_calibration_set for each selected group
    // For now, we'll pass the group index and let the backend handle set creation
    const groupSelections: Record<string, number> = {};
    Object.entries(selections).forEach(([filter, groupIndex]) => {
      // We'll need to pass the group index, and the backend will create the set
      groupSelections[filter] = groupIndex;
    });
    onSelect(groupSelections);
  };

  const formatDate = (isoString: string) => {
    const date = new Date(isoString);
    return date.toLocaleDateString('en-US', {
      month: 'short',
      day: 'numeric',
      year: 'numeric',
      hour: '2-digit',
      minute: '2-digit'
    });
  };

  const formatDuration = (startTime: string, endTime: string) => {
    const start = new Date(startTime);
    const end = new Date(endTime);
    const durationMs = end.getTime() - start.getTime();
    const minutes = Math.floor(durationMs / 60000);
    if (minutes < 60) {
      return `${minutes}min`;
    }
    const hours = Math.floor(minutes / 60);
    const remainingMinutes = minutes % 60;
    return `${hours}h ${remainingMinutes}m`;
  };

  return (
    <div className="fixed inset-0 bg-black bg-opacity-50 flex items-center justify-center z-50 overflow-y-auto">
      <div className="bg-gray-800 rounded-lg p-6 max-w-4xl w-full mx-4 my-8 border border-gray-700 shadow-xl">
        <h3 className="text-xl font-semibold text-gray-100 mb-2">
          Select Flat Groups
        </h3>
        <p className="text-gray-400 text-sm mb-6">
          Choose which flat group to use for each filter. Groups are sorted by recency.
        </p>

        <div className="space-y-6 mb-6 max-h-[60vh] overflow-y-auto pr-2">
          {Object.entries(flatGroupOptions).map(([filter, groups]) => (
            <div key={filter} className="bg-gray-700 bg-opacity-30 rounded-lg p-4">
              <h4 className="font-medium text-gray-100 mb-3 flex items-center">
                <span className="bg-gray-700 px-3 py-1 rounded text-sm">
                  {filter || 'No Filter'}
                </span>
                <span className="ml-3 text-gray-400 text-sm">
                  ({groups.length} group{groups.length !== 1 ? 's' : ''} available)
                </span>
              </h4>

              {groups.length === 0 ? (
                <div className="text-gray-500 text-sm italic py-2">
                  No flat groups found for this filter
                </div>
              ) : (
                <div className="space-y-2">
                  {groups.map((group, index) => (
                    <label
                      key={index}
                      className={`block p-3 rounded border-2 cursor-pointer transition-all ${
                        selections[filter] === index
                          ? 'border-blue-500 bg-blue-900 bg-opacity-20'
                          : 'border-gray-600 hover:border-gray-500 bg-gray-800 bg-opacity-30'
                      }`}
                    >
                      <div className="flex items-start">
                        <input
                          type="radio"
                          name={`filter-${filter}`}
                          checked={selections[filter] === index}
                          onChange={() => setSelections({ ...selections, [filter]: index })}
                          className="mt-1 mr-3 text-blue-500 focus:ring-blue-500"
                        />
                        <div className="flex-1 grid grid-cols-2 gap-3 text-sm">
                          <div className="flex items-center text-gray-300">
                            <Calendar className="w-4 h-4 mr-2 text-gray-500" />
                            <span>{formatDate(group.start_time)}</span>
                            {index === 0 && (
                              <span className="ml-2 text-xs px-2 py-0.5 rounded bg-green-900 text-green-300">
                                Recommended
                              </span>
                            )}
                          </div>
                          <div className="flex items-center text-gray-300">
                            <Hash className="w-4 h-4 mr-2 text-gray-500" />
                            <span>{group.frame_count} frames</span>
                          </div>
                          <div className="flex items-center text-gray-300">
                            <Clock className="w-4 h-4 mr-2 text-gray-500" />
                            <span>{formatDuration(group.start_time, group.end_time)}</span>
                          </div>
                          {group.avg_temp !== null && (
                            <div className="flex items-center text-gray-300">
                              <Thermometer className="w-4 h-4 mr-2 text-gray-500" />
                              <span>{group.avg_temp.toFixed(1)}°C</span>
                            </div>
                          )}
                        </div>
                      </div>
                    </label>
                  ))}
                </div>
              )}
            </div>
          ))}
        </div>

        <div className="flex gap-3 justify-end pt-4 border-t border-gray-700">
          <button
            onClick={onCancel}
            className="px-4 py-2 bg-gray-700 text-gray-200 rounded hover:bg-gray-600 transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={handleApply}
            disabled={!isValid}
            className={`px-4 py-2 rounded transition-colors ${
              isValid
                ? 'bg-blue-600 text-white hover:bg-blue-700'
                : 'bg-gray-600 text-gray-400 cursor-not-allowed'
            }`}
          >
            Apply Selection
          </button>
        </div>
      </div>
    </div>
  );
};
