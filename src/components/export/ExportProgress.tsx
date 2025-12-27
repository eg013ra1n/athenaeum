import { Loader2, Check, X } from 'lucide-react';
import type { ExportProgress as ExportProgressType, ExportStage } from '../../types/export';

interface ExportProgressProps {
  progress: ExportProgressType | null;
}

function getStageLabel(stage: ExportStage): string {
  switch (stage) {
    case 'collecting':
      return 'Collecting data';
    case 'organizing':
      return 'Organizing files';
    case 'generating_scripts':
      return 'Generating scripts';
    case 'siril_calibrating':
      return 'Calibrating frames';
    case 'siril_registering':
      return 'Registering frames';
    case 'siril_stacking':
      return 'Stacking frames';
    case 'complete':
      return 'Complete';
    case 'failed':
      return 'Failed';
    default:
      return stage;
  }
}

export function ExportProgress({ progress }: ExportProgressProps) {
  if (!progress) {
    return null;
  }

  const isComplete = progress.stage === 'complete';
  const isFailed = progress.stage === 'failed';

  return (
    <div className="p-4 bg-gray-800 rounded-lg border border-gray-700">
      <div className="flex items-center gap-3 mb-3">
        {isComplete ? (
          <Check className="text-green-500" size={24} />
        ) : isFailed ? (
          <X className="text-red-500" size={24} />
        ) : (
          <Loader2 className="animate-spin text-blue-500" size={24} />
        )}
        <div>
          <div className="font-medium">{getStageLabel(progress.stage)}</div>
          <div className="text-sm text-gray-400">{progress.message}</div>
        </div>
      </div>

      {!isComplete && !isFailed && (
        <>
          <div className="w-full bg-gray-700 rounded-full h-2 mb-2">
            <div
              className="bg-blue-500 h-2 rounded-full transition-all duration-300"
              style={{ width: `${progress.progress}%` }}
            />
          </div>
          <div className="text-sm text-gray-400 text-right">
            {progress.progress.toFixed(0)}%
          </div>
        </>
      )}

      {progress.currentFile && (
        <div className="mt-2 text-sm text-gray-500 truncate">
          Processing: {progress.currentFile}
        </div>
      )}
    </div>
  );
}
