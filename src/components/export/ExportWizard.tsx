import { useState, useCallback, useEffect } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { listen } from '@tauri-apps/api/event';
import { Folder, Loader2, Check, AlertCircle, Sparkles } from 'lucide-react';
import { FrameSetSelector } from './FrameSetSelector';
import { ExportModeSelector } from './ExportModeSelector';
import { WorkflowSelector } from './WorkflowSelector';
import { CalibrationPreview } from './CalibrationPreview';
import { CalibrationTreeView } from './CalibrationTreeView';
import { ExportProgress } from './ExportProgress';
import {
  useExportableFrameSets,
  useExportData,
  useExport,
  useCalibrationRoute,
  useExportV2,
} from '../../hooks/useExportData';
import type {
  ExportMode,
  SirilWorkflow,
  ExportProgress as ExportProgressType,
  ExportResult,
} from '../../types/export';

interface ExportWizardProps {
  initialFrameSetId?: number;
}

export function ExportWizard({ initialFrameSetId }: ExportWizardProps) {
  // State
  const [selectedFrameSetId, setSelectedFrameSetId] = useState<number | null>(
    initialFrameSetId ?? null
  );
  const [mode, setMode] = useState<ExportMode>('organize_and_script');
  const [workflow, setWorkflow] = useState<SirilWorkflow>('mono_preprocessing');
  const [outputDir, setOutputDir] = useState<string>('');
  const [rejectionLow, setRejectionLow] = useState(3.0);
  const [rejectionHigh, setRejectionHigh] = useState(3.0);
  const [useSymlinks, setUseSymlinks] = useState(false);
  const [createMasters, setCreateMasters] = useState(true);
  const [useV2Export, setUseV2Export] = useState(true); // Default to v2
  const [progress, setProgress] = useState<ExportProgressType | null>(null);
  const [result, setResult] = useState<ExportResult | null>(null);

  // Hooks
  const { frameSets, loading: loadingFrameSets } = useExportableFrameSets();
  const { data: exportData, loading: loadingExportData } = useExportData(selectedFrameSetId);
  const { route: calibrationRoute, loading: loadingRoute } = useCalibrationRoute(
    useV2Export ? selectedFrameSetId : null
  );
  const { execute: executeLegacy, loading: executingLegacy } = useExport();
  const { execute: executeV2, loading: executingV2 } = useExportV2();

  const executing = executingLegacy || executingV2;

  // Listen for progress events
  useEffect(() => {
    const unlisten = listen<ExportProgressType>('export-progress', (event) => {
      setProgress(event.payload);
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // Handle folder selection
  const handleSelectFolder = useCallback(async () => {
    const selected = await open({
      directory: true,
      multiple: false,
      title: 'Select Export Directory',
    });

    if (selected && typeof selected === 'string') {
      setOutputDir(selected);
    }
  }, []);

  // Handle export
  const handleExport = useCallback(async () => {
    if (!selectedFrameSetId || !outputDir) return;

    setResult(null);
    setProgress(null);

    try {
      let exportResult: ExportResult;

      if (useV2Export) {
        // Use v2 export with auto-detected workflows
        exportResult = await executeV2({
          frameSetId: selectedFrameSetId,
          outputDir,
          mode,
          createMasters,
          rejectionLow,
          rejectionHigh,
          useSymlinks,
        });
      } else {
        // Use legacy export with manual workflow selection
        exportResult = await executeLegacy({
          frameSetId: selectedFrameSetId,
          outputDir,
          mode,
          workflow,
          rejectionLow,
          rejectionHigh,
          useSymlinks,
        });
      }
      setResult(exportResult);
    } catch {
      // Error is handled by the hook
    }
  }, [
    selectedFrameSetId,
    outputDir,
    mode,
    workflow,
    createMasters,
    rejectionLow,
    rejectionHigh,
    useSymlinks,
    useV2Export,
    executeLegacy,
    executeV2,
  ]);

  // Check if ready to export
  const canExport =
    selectedFrameSetId !== null &&
    outputDir !== '' &&
    !executing &&
    !loadingExportData;

  return (
    <div className="space-y-6">
      {/* Step 1: Select Frame Set */}
      <section className="bg-gray-800 rounded-lg p-4">
        <h3 className="text-lg font-medium mb-3">1. Select Frame Set</h3>
        <FrameSetSelector
          frameSets={frameSets}
          loading={loadingFrameSets}
          selectedId={selectedFrameSetId}
          onSelect={setSelectedFrameSetId}
        />
      </section>

      {/* Step 2: Calibration Summary */}
      {selectedFrameSetId && (
        <section className="bg-gray-800 rounded-lg p-4">
          <div className="flex items-center justify-between mb-3">
            <h3 className="text-lg font-medium">2. Calibration Summary</h3>
            {/* V2 Export Toggle */}
            <label className="flex items-center gap-2 cursor-pointer">
              <input
                type="checkbox"
                checked={useV2Export}
                onChange={(e) => setUseV2Export(e.target.checked)}
                className="w-4 h-4 rounded border-gray-600 bg-gray-700 text-blue-500 focus:ring-blue-500"
              />
              <span className="text-sm text-gray-400 flex items-center gap-1">
                <Sparkles size={14} className="text-yellow-500" />
                Smart Export (v2)
              </span>
            </label>
          </div>

          {loadingExportData || loadingRoute ? (
            <div className="flex items-center gap-2 text-gray-400">
              <Loader2 className="animate-spin" size={16} />
              Loading calibration data...
            </div>
          ) : useV2Export && calibrationRoute ? (
            <div className="space-y-4">
              {/* Groups Summary */}
              {exportData && exportData.groups.length > 0 && (
                <div className="mb-4">
                  <div className="text-sm text-gray-400 mb-2">
                    {calibrationRoute.summary.totalLights} light frames in{' '}
                    {calibrationRoute.summary.groupCount} groups •{' '}
                    {(calibrationRoute.summary.totalExposure / 3600).toFixed(1)}h total •{' '}
                    {calibrationRoute.summary.mastersToCreate} masters to create
                  </div>
                  {/* Completeness badges */}
                  <div className="flex gap-2">
                    <StatusBadge
                      label="Flats"
                      complete={calibrationRoute.summary.flatsComplete}
                    />
                    <StatusBadge
                      label="Darks"
                      complete={calibrationRoute.summary.darksComplete}
                    />
                    <StatusBadge
                      label="Bias"
                      complete={calibrationRoute.summary.biasComplete}
                    />
                  </div>
                </div>
              )}

              {/* Calibration Tree */}
              <CalibrationTreeView groups={calibrationRoute.groups} />

              {/* Warnings */}
              {calibrationRoute.summary.warnings.length > 0 && (
                <div className="p-3 bg-yellow-900/20 border border-yellow-600/30 rounded-lg">
                  <div className="flex items-center gap-2 text-yellow-500 mb-2">
                    <AlertCircle size={16} />
                    <span className="font-medium">Warnings</span>
                  </div>
                  <ul className="text-sm text-yellow-400/80 space-y-1">
                    {calibrationRoute.summary.warnings.map((warning, index) => (
                      <li key={index}>• {warning}</li>
                    ))}
                  </ul>
                </div>
              )}
            </div>
          ) : exportData ? (
            <div>
              <div className="mb-3 text-sm text-gray-400">
                {exportData.totalLightFrames} light frames •{' '}
                {(exportData.totalExposureSeconds / 3600).toFixed(1)}h total exposure
              </div>
              <CalibrationPreview summary={exportData.calibrationSummary} />
            </div>
          ) : (
            <div className="text-gray-500">No calibration data available</div>
          )}
        </section>
      )}

      {/* Step 3: Export Mode */}
      {selectedFrameSetId && exportData && (
        <section className="bg-gray-800 rounded-lg p-4">
          <h3 className="text-lg font-medium mb-3">3. Export Mode</h3>
          <ExportModeSelector value={mode} onChange={setMode} />
        </section>
      )}

      {/* Step 4: Workflow (only in legacy mode) */}
      {selectedFrameSetId && exportData && !useV2Export && (
        <section className="bg-gray-800 rounded-lg p-4">
          <h3 className="text-lg font-medium mb-3">4. Siril Workflow</h3>
          <WorkflowSelector value={workflow} onChange={setWorkflow} />
        </section>
      )}

      {/* V2 Mode: Master creation option */}
      {selectedFrameSetId && exportData && useV2Export && (
        <section className="bg-gray-800 rounded-lg p-4">
          <h3 className="text-lg font-medium mb-3">4. Processing Options</h3>
          <div className="p-3 bg-gray-700/50 rounded-lg">
            <label className="flex items-center gap-2 cursor-pointer">
              <input
                type="checkbox"
                checked={createMasters}
                onChange={(e) => setCreateMasters(e.target.checked)}
                className="w-4 h-4 rounded border-gray-600 bg-gray-700 text-blue-500 focus:ring-blue-500"
              />
              <div>
                <span className="text-gray-200">Create master calibration frames</span>
                <p className="text-sm text-gray-500">
                  Generates master bias, dark, and flat frames before preprocessing
                </p>
              </div>
            </label>
          </div>
          <p className="mt-3 text-sm text-gray-500">
            Siril workflow is automatically detected based on camera type (OSC/Mono) for each
            export group.
          </p>
        </section>
      )}

      {/* Step 5: Options */}
      {selectedFrameSetId && exportData && (
        <section className="bg-gray-800 rounded-lg p-4">
          <h3 className="text-lg font-medium mb-3">5. Options</h3>
          <div className="space-y-4">
            {/* Output directory */}
            <div>
              <label className="block text-sm text-gray-400 mb-1">
                Output Directory
              </label>
              <div className="flex gap-2">
                <input
                  type="text"
                  value={outputDir}
                  readOnly
                  placeholder="Select output folder..."
                  className="flex-1 px-3 py-2 bg-gray-700 border border-gray-600 rounded-lg text-gray-200 placeholder-gray-500"
                />
                <button
                  onClick={handleSelectFolder}
                  className="px-4 py-2 bg-gray-600 hover:bg-gray-500 rounded-lg flex items-center gap-2"
                >
                  <Folder size={16} />
                  Browse
                </button>
              </div>
            </div>

            {/* Rejection parameters */}
            <div className="grid grid-cols-2 gap-4">
              <div>
                <label className="block text-sm text-gray-400 mb-1">
                  Rejection Low (sigma)
                </label>
                <input
                  type="number"
                  value={rejectionLow}
                  onChange={(e) => setRejectionLow(parseFloat(e.target.value) || 3.0)}
                  step="0.1"
                  min="1"
                  max="6"
                  className="w-full px-3 py-2 bg-gray-700 border border-gray-600 rounded-lg text-gray-200"
                />
              </div>
              <div>
                <label className="block text-sm text-gray-400 mb-1">
                  Rejection High (sigma)
                </label>
                <input
                  type="number"
                  value={rejectionHigh}
                  onChange={(e) => setRejectionHigh(parseFloat(e.target.value) || 3.0)}
                  step="0.1"
                  min="1"
                  max="6"
                  className="w-full px-3 py-2 bg-gray-700 border border-gray-600 rounded-lg text-gray-200"
                />
              </div>
            </div>

            {/* Use symlinks */}
            <label className="flex items-center gap-2 cursor-pointer">
              <input
                type="checkbox"
                checked={useSymlinks}
                onChange={(e) => setUseSymlinks(e.target.checked)}
                className="w-4 h-4 rounded border-gray-600 bg-gray-700 text-blue-500 focus:ring-blue-500"
              />
              <span className="text-gray-300">
                Use symbolic links instead of copying files
              </span>
            </label>
          </div>
        </section>
      )}

      {/* Progress */}
      {(executing || progress) && (
        <ExportProgress progress={progress} />
      )}

      {/* Result */}
      {result && (
        <div
          className={`p-4 rounded-lg border ${
            result.success
              ? 'bg-green-900/20 border-green-600/30'
              : 'bg-red-900/20 border-red-600/30'
          }`}
        >
          <div className="flex items-center gap-2 mb-2">
            {result.success ? (
              <Check className="text-green-500" size={20} />
            ) : (
              <AlertCircle className="text-red-500" size={20} />
            )}
            <span className="font-medium">
              {result.success ? 'Export Complete' : 'Export Failed'}
            </span>
          </div>
          {result.success ? (
            <div className="text-sm text-gray-400">
              <div>Files organized: {result.filesOrganized}</div>
              <div>Scripts generated: {result.scriptsGenerated.length}</div>
              <div className="mt-2 text-gray-500 truncate">
                Output: {result.outputDir}
              </div>
            </div>
          ) : (
            <div className="text-sm text-red-400">{result.error}</div>
          )}
          {result.warnings.length > 0 && (
            <div className="mt-3 text-sm text-yellow-400">
              <div className="font-medium mb-1">Warnings:</div>
              {result.warnings.map((warning, i) => (
                <div key={i}>• {warning}</div>
              ))}
            </div>
          )}
        </div>
      )}

      {/* Export button */}
      {selectedFrameSetId && exportData && (
        <button
          onClick={handleExport}
          disabled={!canExport}
          className={`w-full py-3 rounded-lg font-medium flex items-center justify-center gap-2 ${
            canExport
              ? 'bg-blue-600 hover:bg-blue-500'
              : 'bg-gray-600 cursor-not-allowed'
          }`}
        >
          {executing ? (
            <>
              <Loader2 className="animate-spin" size={20} />
              Exporting...
            </>
          ) : (
            'Export'
          )}
        </button>
      )}
    </div>
  );
}

// Helper component for calibration status badges
interface StatusBadgeProps {
  label: string;
  complete: boolean;
}

function StatusBadge({ label, complete }: StatusBadgeProps) {
  return (
    <span
      className={`px-2 py-1 rounded text-xs font-medium ${
        complete
          ? 'bg-green-900/30 text-green-400'
          : 'bg-red-900/30 text-red-400'
      }`}
    >
      {label}: {complete ? <Check size={12} className="inline" /> : '—'}
    </span>
  );
}
