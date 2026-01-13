import { Play, Files } from 'lucide-react';
import type { ExportMode } from '../../types/export';

interface ExportModeSelectorProps {
  value: ExportMode;
  onChange: (mode: ExportMode) => void;
}

const EXPORT_MODES: { mode: ExportMode; label: string; description: string; icon: React.ReactNode }[] = [
  {
    mode: 'direct_execution',
    label: 'Direct execution (run Siril)',
    description: 'Organize files, generate scripts, and automatically run Siril',
    icon: <Play size={20} />,
  },
  {
    mode: 'organize_and_script',
    label: 'Organize + Generate scripts',
    description: 'Copy files into folder structure and generate Siril scripts',
    icon: <Files size={20} />,
  },
];

export function ExportModeSelector({ value, onChange }: ExportModeSelectorProps) {
  return (
    <div className="space-y-2">
      {EXPORT_MODES.map(({ mode, label, description, icon }) => (
        <button
          key={mode}
          onClick={() => onChange(mode)}
          className={`w-full text-left p-3 rounded-lg border transition-colors flex items-start gap-3 ${
            value === mode
              ? 'bg-blue-600/20 border-blue-500'
              : 'bg-gray-700/50 border-gray-600 hover:bg-gray-700 hover:border-gray-500'
          }`}
        >
          <div className={`mt-0.5 ${value === mode ? 'text-blue-400' : 'text-gray-400'}`}>
            {icon}
          </div>
          <div className="flex-1">
            <div className="font-medium">{label}</div>
            <div className="text-sm text-gray-400 mt-0.5">{description}</div>
          </div>
          {value === mode && <div className="w-4 h-4 rounded-full bg-blue-500 mt-1" />}
        </button>
      ))}
    </div>
  );
}
