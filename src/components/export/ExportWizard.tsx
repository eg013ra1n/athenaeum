import { useState, useCallback, useEffect } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { listen } from '@tauri-apps/api/event';
import { Folder, Loader2, Check, AlertCircle, Sparkles, ChevronDown, ChevronRight } from 'lucide-react';
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
  ExportTarget,
  SirilWorkflow,
  ExportProgress as ExportProgressType,
  ExportResult,
  ReferenceFrameMode,
  RejectionAlgorithm,
  ImageWeightingMethod,
  DrizzleScale,
  ExptimeToleranceMode,
} from '../../types/export';

interface ExportWizardProps {
  initialFrameSetId?: number;
}

export function ExportWizard({ initialFrameSetId }: ExportWizardProps) {
  // State
  const [selectedFrameSetId, setSelectedFrameSetId] = useState<number | null>(
    initialFrameSetId ?? null
  );
  const [exportTarget, setExportTarget] = useState<ExportTarget>('siril');
  const [mode, setMode] = useState<ExportMode>('organize_and_script');
  const [workflow, setWorkflow] = useState<SirilWorkflow>('mono_preprocessing');
  const [outputDir, setOutputDir] = useState<string>('');
  const [rejectionLow, setRejectionLow] = useState(2.5);
  const [rejectionHigh, setRejectionHigh] = useState(2.5);
  const [useSymlinks, setUseSymlinks] = useState(false);
  const [createMasters, setCreateMasters] = useState(true);
  const [useV2Export, setUseV2Export] = useState(true); // Default to v4 (smart export)
  const [progress, setProgress] = useState<ExportProgressType | null>(null);
  const [result, setResult] = useState<ExportResult | null>(null);

  // Advanced Siril options
  const [showAdvancedOptions, setShowAdvancedOptions] = useState(false);
  const [referenceFrameMode, setReferenceFrameMode] = useState<ReferenceFrameMode>('siril_auto');
  const [rejectionAlgorithm, setRejectionAlgorithm] = useState<RejectionAlgorithm>('sigma');
  const [imageWeighting, setImageWeighting] = useState<ImageWeightingMethod>('wfwhm');
  const [drizzleEnabled, setDrizzleEnabled] = useState(false);
  const [drizzleScale, setDrizzleScale] = useState<DrizzleScale>('x2');

  // Exposure time tolerance (for grouping frames with similar exposures)
  const [exptimeToleranceMode, setExptimeToleranceMode] = useState<ExptimeToleranceMode>('disabled');
  const [exptimeToleranceValue, setExptimeToleranceValue] = useState(30);

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
        // Use v4 export with auto-detected workflows and flat folder structure
        exportResult = await executeV2({
          frameSetId: selectedFrameSetId,
          outputDir,
          mode,
          target: exportTarget,
          createMasters,
          rejectionLow,
          rejectionHigh,
          useSymlinks,
          // Advanced Siril options
          referenceFrameMode,
          rejectionAlgorithm,
          imageWeighting,
          drizzleEnabled,
          drizzleScale,
          // Exposure time grouping
          exptimeToleranceMode,
          exptimeToleranceValue,
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
    exportTarget,
    createMasters,
    rejectionLow,
    rejectionHigh,
    useSymlinks,
    useV2Export,
    executeLegacy,
    executeV2,
    // Advanced Siril options
    referenceFrameMode,
    rejectionAlgorithm,
    imageWeighting,
    drizzleEnabled,
    drizzleScale,
    // Exposure time grouping
    exptimeToleranceMode,
    exptimeToleranceValue,
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

      {/* Step 2: Export Target */}
      {selectedFrameSetId && (
        <section className="bg-gray-800 rounded-lg p-4">
          <h3 className="text-lg font-medium mb-3">2. Export Target</h3>
          <div className="flex gap-4">
            <label className="flex items-center gap-2 cursor-pointer">
              <input
                type="radio"
                name="exportTarget"
                value="siril"
                checked={exportTarget === 'siril'}
                onChange={() => setExportTarget('siril')}
                className="w-4 h-4 border-gray-600 bg-gray-700 text-blue-500 focus:ring-blue-500"
              />
              <div>
                <span className="text-gray-200">Siril</span>
                <p className="text-sm text-gray-500">Flat structure with generated scripts</p>
              </div>
            </label>
            <label className="flex items-center gap-2 cursor-pointer">
              <input
                type="radio"
                name="exportTarget"
                value="pixinsight_wbpp"
                checked={exportTarget === 'pixinsight_wbpp'}
                onChange={() => setExportTarget('pixinsight_wbpp')}
                className="w-4 h-4 border-gray-600 bg-gray-700 text-blue-500 focus:ring-blue-500"
              />
              <div>
                <span className="text-gray-200">PixInsight WBPP</span>
                <p className="text-sm text-gray-500">Grouped structure for auto-detection</p>
              </div>
            </label>
          </div>
        </section>
      )}

      {/* Step 3: Calibration Summary */}
      {selectedFrameSetId && (
        <section className="bg-gray-800 rounded-lg p-4">
          <div className="flex items-center justify-between mb-3">
            <h3 className="text-lg font-medium">3. Calibration Summary</h3>
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
                Smart Export (v4)
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

      {/* Step 4: Export Mode (Siril only) */}
      {selectedFrameSetId && exportData && exportTarget === 'siril' && (
        <section className="bg-gray-800 rounded-lg p-4">
          <h3 className="text-lg font-medium mb-3">4. Export Mode</h3>
          <ExportModeSelector value={mode} onChange={setMode} />
        </section>
      )}

      {/* Step 5: Workflow (only in legacy mode) */}
      {selectedFrameSetId && exportData && exportTarget === 'siril' && !useV2Export && (
        <section className="bg-gray-800 rounded-lg p-4">
          <h3 className="text-lg font-medium mb-3">5. Siril Workflow</h3>
          <WorkflowSelector value={workflow} onChange={setWorkflow} />
        </section>
      )}

      {/* V2 Mode: Master creation option (Siril only) */}
      {selectedFrameSetId && exportData && exportTarget === 'siril' && useV2Export && (
        <section className="bg-gray-800 rounded-lg p-4">
          <h3 className="text-lg font-medium mb-3">5. Processing Options</h3>
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

      {/* Step 6: Options (or Step 4 for WBPP) */}
      {selectedFrameSetId && exportData && (
        <section className="bg-gray-800 rounded-lg p-4">
          <h3 className="text-lg font-medium mb-3">
            {exportTarget === 'siril' ? '6' : '4'}. Options
          </h3>
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

            {/* Advanced Siril Options (collapsible) */}
            {exportTarget === 'siril' && (
              <div className="border border-gray-600 rounded-lg overflow-hidden">
                <button
                  onClick={() => setShowAdvancedOptions(!showAdvancedOptions)}
                  className="w-full px-4 py-3 bg-gray-700/50 hover:bg-gray-700 flex items-center justify-between text-left"
                >
                  <span className="font-medium text-gray-200">Advanced Siril Options</span>
                  {showAdvancedOptions ? (
                    <ChevronDown size={20} className="text-gray-400" />
                  ) : (
                    <ChevronRight size={20} className="text-gray-400" />
                  )}
                </button>
                {showAdvancedOptions && (
                  <div className="p-4 space-y-4 bg-gray-800/50">
                    {/* Rejection Algorithm */}
                    <div>
                      <label className="block text-sm text-gray-400 mb-1">
                        Rejection Algorithm
                      </label>
                      <select
                        value={rejectionAlgorithm}
                        onChange={(e) => setRejectionAlgorithm(e.target.value as RejectionAlgorithm)}
                        className="w-full px-3 py-2 bg-gray-700 border border-gray-600 rounded-lg text-gray-200"
                      >
                        <option value="sigma">Sigma Clipping (general purpose)</option>
                        <option value="percentile">Percentile (small datasets &lt;20 frames)</option>
                        <option value="linear_fit">Linear Fit (large sets with gradients)</option>
                        <option value="gesd">GESD (50+ images)</option>
                        <option value="mad">MAD (drizzled CFA data)</option>
                      </select>
                    </div>

                    {/* Rejection Sigma Values */}
                    <div className="grid grid-cols-2 gap-4">
                      <div>
                        <label className="block text-sm text-gray-400 mb-1">
                          Rejection Low (sigma)
                        </label>
                        <input
                          type="number"
                          value={rejectionLow}
                          onChange={(e) => setRejectionLow(parseFloat(e.target.value) || 2.5)}
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
                          onChange={(e) => setRejectionHigh(parseFloat(e.target.value) || 2.5)}
                          step="0.1"
                          min="1"
                          max="6"
                          className="w-full px-3 py-2 bg-gray-700 border border-gray-600 rounded-lg text-gray-200"
                        />
                      </div>
                    </div>

                    {/* Image Weighting */}
                    <div>
                      <label className="block text-sm text-gray-400 mb-1">
                        Image Weighting
                      </label>
                      <select
                        value={imageWeighting}
                        onChange={(e) => setImageWeighting(e.target.value as ImageWeightingMethod)}
                        className="w-full px-3 py-2 bg-gray-700 border border-gray-600 rounded-lg text-gray-200"
                      >
                        <option value="wfwhm">Weighted FWHM (recommended)</option>
                        <option value="stars">Number of Stars</option>
                        <option value="noise">Noise Level</option>
                        <option value="exposure_time">Exposure Time</option>
                        <option value="none">No Weighting</option>
                      </select>
                      <p className="text-xs text-gray-500 mt-1">
                        wFWHM weights by seeing quality and star count
                      </p>
                    </div>

                    {/* Reference Frame Mode */}
                    <div>
                      <label className="block text-sm text-gray-400 mb-1">
                        Reference Frame Selection
                      </label>
                      <select
                        value={referenceFrameMode}
                        onChange={(e) => setReferenceFrameMode(e.target.value as ReferenceFrameMode)}
                        className="w-full px-3 py-2 bg-gray-700 border border-gray-600 rounded-lg text-gray-200"
                      >
                        <option value="siril_auto">Siril Auto (-2pass)</option>
                        <option value="athenaeum_scoring">Athenaeum Quality Scoring (coming soon)</option>
                      </select>
                      <p className="text-xs text-gray-500 mt-1">
                        -2pass automatically selects the best reference frame based on FWHM and star count
                      </p>
                    </div>

                    {/* Drizzle */}
                    <div className="space-y-2">
                      <label className="flex items-center gap-2 cursor-pointer">
                        <input
                          type="checkbox"
                          checked={drizzleEnabled}
                          onChange={(e) => setDrizzleEnabled(e.target.checked)}
                          className="w-4 h-4 rounded border-gray-600 bg-gray-700 text-blue-500 focus:ring-blue-500"
                        />
                        <span className="text-gray-200">Enable Drizzle (super-resolution)</span>
                      </label>
                      {drizzleEnabled && (
                        <div className="ml-6">
                          <select
                            value={drizzleScale}
                            onChange={(e) => setDrizzleScale(e.target.value as DrizzleScale)}
                            className="w-full px-3 py-2 bg-gray-700 border border-gray-600 rounded-lg text-gray-200"
                          >
                            <option value="x2">2x Scale</option>
                            <option value="x3">3x Scale</option>
                          </select>
                          <p className="text-xs text-gray-500 mt-1">
                            For OSC cameras: Bayer pattern is preserved during registration
                          </p>
                        </div>
                      )}
                    </div>

                    {/* Exposure Time Grouping */}
                    <div className="space-y-2">
                      <label className="block text-sm text-gray-400 mb-1">
                        Exposure Time Grouping
                      </label>
                      <select
                        value={exptimeToleranceMode}
                        onChange={(e) => setExptimeToleranceMode(e.target.value as ExptimeToleranceMode)}
                        className="w-full px-3 py-2 bg-gray-700 border border-gray-600 rounded-lg text-gray-200"
                      >
                        <option value="disabled">Disabled (stack all frames together)</option>
                        <option value="absolute">Absolute tolerance (seconds)</option>
                        <option value="relative">Relative tolerance (percent)</option>
                      </select>
                      <p className="text-xs text-gray-500">
                        Group frames with similar exposure times into separate stacks
                      </p>
                      {exptimeToleranceMode !== 'disabled' && (
                        <div className="mt-2">
                          <label className="block text-sm text-gray-400 mb-1">
                            Tolerance {exptimeToleranceMode === 'absolute' ? '(seconds)' : '(%)'}
                          </label>
                          <input
                            type="number"
                            value={exptimeToleranceValue}
                            onChange={(e) => setExptimeToleranceValue(parseFloat(e.target.value) || 30)}
                            step={exptimeToleranceMode === 'absolute' ? 1 : 5}
                            min={exptimeToleranceMode === 'absolute' ? 1 : 1}
                            max={exptimeToleranceMode === 'absolute' ? 300 : 50}
                            className="w-full px-3 py-2 bg-gray-700 border border-gray-600 rounded-lg text-gray-200"
                          />
                          <p className="text-xs text-gray-500 mt-1">
                            {exptimeToleranceMode === 'absolute'
                              ? 'Frames within ±X seconds will be stacked together (e.g., 30s groups 55s-85s frames)'
                              : 'Frames within ±X% will be stacked together (e.g., 10% groups 54s-66s for 60s center)'}
                          </p>
                        </div>
                      )}
                    </div>
                  </div>
                )}
              </div>
            )}

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
