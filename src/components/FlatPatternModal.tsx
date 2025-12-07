import React, { useState } from 'react';
import { FlatPattern } from '../types/models';

interface FlatPatternModalProps {
  isOpen: boolean;
  onSelect: (pattern: FlatPattern, remember: boolean) => void;
  onCancel: () => void;
}

export const FlatPatternModal: React.FC<FlatPatternModalProps> = ({
  isOpen,
  onSelect,
  onCancel,
}) => {
  const [selectedPattern, setSelectedPattern] = useState<FlatPattern>(FlatPattern.Automatic);
  const [rememberChoice, setRememberChoice] = useState(false);

  if (!isOpen) return null;

  const handleContinue = () => {
    onSelect(selectedPattern, rememberChoice);
  };

  const patterns = [
    {
      value: FlatPattern.Automatic,
      label: 'Automatic',
      description: 'Find best matching flats automatically based on timing and parameters (recommended)',
      recommended: true,
    },
    {
      value: FlatPattern.LongTerm,
      label: 'Long-term Reuse',
      description: 'Use the same flats over weeks/months (prefers oldest valid flat set)',
    },
  ];

  return (
    <div className="fixed inset-0 bg-black bg-opacity-50 flex items-center justify-center z-50">
      <div className="bg-gray-800 rounded-lg p-6 max-w-2xl w-full mx-4 border border-gray-700 shadow-xl">
        <h3 className="text-xl font-semibold text-gray-100 mb-2">
          Flat Calibration Pattern
        </h3>
        <p className="text-gray-400 text-sm mb-6">
          How do you typically take flat frames? This helps us find the best matching flats for your light frames.
        </p>

        <div className="space-y-3 mb-6">
          {patterns.map((pattern) => (
            <label
              key={pattern.value}
              className={`block p-4 rounded-lg border-2 cursor-pointer transition-all ${
                selectedPattern === pattern.value
                  ? 'border-blue-500 bg-blue-900 bg-opacity-20'
                  : 'border-gray-600 hover:border-gray-500 bg-gray-700 bg-opacity-30'
              }`}
            >
              <div className="flex items-start">
                <input
                  type="radio"
                  name="pattern"
                  value={pattern.value}
                  checked={selectedPattern === pattern.value}
                  onChange={(e) => setSelectedPattern(e.target.value as FlatPattern)}
                  className="mt-1 mr-3 text-blue-500 focus:ring-blue-500"
                />
                <div className="flex-1">
                  <div className="font-medium text-gray-100 mb-1">
                    {pattern.label}
                  </div>
                  <div className="text-sm text-gray-400">
                    {pattern.description}
                  </div>
                </div>
              </div>
            </label>
          ))}
        </div>

        <div className="mb-6">
          <label className="flex items-center text-gray-300 cursor-pointer">
            <input
              type="checkbox"
              checked={rememberChoice}
              onChange={(e) => setRememberChoice(e.target.checked)}
              className="mr-2 rounded text-blue-500 focus:ring-blue-500"
            />
            <span className="text-sm">
              Remember this choice for this frame set
            </span>
          </label>
        </div>

        <div className="flex gap-3 justify-end">
          <button
            onClick={onCancel}
            className="px-4 py-2 bg-gray-700 text-gray-200 rounded hover:bg-gray-600 transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={handleContinue}
            className="px-4 py-2 bg-blue-600 text-white rounded hover:bg-blue-700 transition-colors"
          >
            Continue
          </button>
        </div>
      </div>
    </div>
  );
};
