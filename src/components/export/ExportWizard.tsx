import { useState, useCallback, useEffect, useMemo } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { listen } from '@tauri-apps/api/event';
import { Folder, Loader2, Check, AlertCircle, ChevronDown, ChevronRight, Play, FileCheck } from 'lucide-react';
import { FrameSetSelector } from './FrameSetSelector';
import { ExportModeSelector } from './ExportModeSelector';
import { CalibrationPreview } from './CalibrationPreview';
import { CalibrationTreeView } from './CalibrationTreeView';
import { ExportProgress } from './ExportProgress';
import { PipelineVisualizer, PipelineStepsSummary } from './PipelineVisualizer';
import { ValidationWarnings } from './ValidationWarnings';
import {
  useExportableFrameSets,
  useExportData,
  useCalibrationRoute,
  useExportV2,
} from '../../hooks/useExportData';
import {
  useExportJobCreate,
  usePipelinePlan,
  useExportPipeline,
} from '../../hooks/useExportPipeline';
import type {
  ExportMode,
  ExportTarget,
  ExportProgress as ExportProgressType,
  ExportResult,
  ExportConfig,
  ReferenceFrameMode,
  RejectionAlgorithm,
  ImageWeightingMethod,
  DrizzleScale,
  SirilWorkflow,
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
  const [mode, setMode] = useState<ExportMode>('direct_execution');
  const [outputDir, setOutputDir] = useState<string>('');
  const [rejectionLow, setRejectionLow] = useState(2.5);
  const [rejectionHigh, setRejectionHigh] = useState(2.5);
  const [useSymlinks, setUseSymlinks] = useState(false);
  const [createMasters, setCreateMasters] = useState(true);
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
  const [exptimeToleranceEnabled, setExptimeToleranceEnabled] = useState(true);
  const [exptimeToleranceSeconds, setExptimeToleranceSeconds] = useState(3);

  // Pipeline job state
  const [activeJobId, setActiveJobId] = useState<number | null>(null);
  const [showValidation, setShowValidation] = useState(false);
  const [usePipelineMode, setUsePipelineMode] = useState(true); // New pipeline mode by default

  // Hooks
  const { frameSets, loading: loadingFrameSets } = useExportableFrameSets();
  const { data: exportData, loading: loadingExportData } = useExportData(selectedFrameSetId);
  const { route: calibrationRoute, loading: loadingRoute } = useCalibrationRoute(selectedFrameSetId);
  const { execute: executeExport, loading: executing } = useExportV2();

  // Pipeline hooks
  const { create: createJob, loading: creatingJob } = useExportJobCreate();

  // Build export config for pipeline plan
  const exportConfig: ExportConfig | null = useMemo(() => {
    if (!selectedFrameSetId) return null;
    return {
      frameSetId: selectedFrameSetId,
      outputDir,
      target: exportTarget,
      mode,
      workflow: 'mono_preprocessing' as SirilWorkflow, // Auto-detected per group
      createMasters,
      rejectionLow,
      rejectionHigh,
      useSymlinks,
      referenceFrameMode,
      rejectionAlgorithm,
      imageWeighting,
      drizzleEnabled,
      drizzleScale,
      exptimeToleranceMode: exptimeToleranceEnabled ? 'absolute' : 'disabled',
      exptimeToleranceValue: exptimeToleranceSeconds,
    };
  }, [
    selectedFrameSetId, outputDir, exportTarget, mode, createMasters,
    rejectionLow, rejectionHigh, useSymlinks, referenceFrameMode,
    rejectionAlgorithm, imageWeighting, drizzleEnabled, drizzleScale,
    exptimeToleranceEnabled, exptimeToleranceSeconds,
  ]);

  const { plan: pipelinePlan, loading: loadingPlan } = usePipelinePlan(
    showValidation ? selectedFrameSetId : null,
    showValidation ? exportConfig : null
  );

  // Pipeline execution
  const {
    job: activeJob,
    startJob,
    pauseJob,
    resumeJob,
    cancelJob,
    retryStep,
    progress: pipelineProgress,
    isRunning: jobRunning,
    isComplete: jobComplete,
  } = useExportPipeline(activeJobId);

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

  // Handle validate (show validation step)
  const handleValidate = useCallback(() => {
    setShowValidation(true);
  }, []);

  // Handle start pipeline job
  const handleStartPipeline = useCallback(async () => {
    if (!selectedFrameSetId || !outputDir || !exportConfig) return;

    setResult(null);
    setProgress(null);

    try {
      const job = await createJob({
        frameSetId: selectedFrameSetId,
        outputDir,
        config: exportConfig,
      });
      setActiveJobId(job.id);
      setShowValidation(false);
      // Start the job with the explicit job ID (don't rely on state which hasn't updated yet)
      await startJob(job.id);
    } catch {
      // Error is handled by the hook
    }
  }, [selectedFrameSetId, outputDir, exportConfig, createJob, startJob]);

  // Handle legacy export (non-pipeline mode)
  const handleExport = useCallback(async () => {
    if (!selectedFrameSetId || !outputDir) return;

    setResult(null);
    setProgress(null);

    try {
      const exportResult = await executeExport({
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
        exptimeToleranceMode: exptimeToleranceEnabled ? 'absolute' : 'disabled',
        exptimeToleranceValue: exptimeToleranceSeconds,
      });
      setResult(exportResult);
    } catch {
      // Error is handled by the hook
    }
  }, [
    selectedFrameSetId,
    outputDir,
    mode,
    exportTarget,
    createMasters,
    rejectionLow,
    rejectionHigh,
    useSymlinks,
    executeExport,
    // Advanced Siril options
    referenceFrameMode,
    rejectionAlgorithm,
    imageWeighting,
    drizzleEnabled,
    drizzleScale,
    // Exposure time grouping
    exptimeToleranceEnabled,
    exptimeToleranceSeconds,
  ]);

  // Handle cancel validation
  const handleCancelValidation = useCallback(() => {
    setShowValidation(false);
  }, []);

  // Handle reset (after job complete)
  const handleReset = useCallback(() => {
    setActiveJobId(null);
    setShowValidation(false);
    setResult(null);
    setProgress(null);
  }, []);

  // Check if ready to export
  const canExport =
    selectedFrameSetId !== null &&
    outputDir !== '' &&
    !executing &&
    !loadingExportData &&
    !jobRunning &&
    !creatingJob;

  // Check if a job is active (running or complete)
  const hasActiveJob = activeJobId !== null && activeJob !== null;

  return (
    <div className="space-y-6">
      {/* Step 1: Select Frame Set */}
      <section className="bg-surface-elevated rounded-lg p-4">
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
        <section className="bg-surface-elevated rounded-lg p-4">
          <h3 className="text-lg font-medium mb-3">2. Export Target</h3>
          <div className="flex gap-4">
            <label className="flex items-center gap-2 cursor-pointer">
              <input
                type="radio"
                name="exportTarget"
                value="siril"
                checked={exportTarget === 'siril'}
                onChange={() => setExportTarget('siril')}
                className="w-4 h-4 border-border bg-surface-hover text-accent focus:ring-accent"
              />
              <div>
                <span className="text-content">Siril</span>
                <p className="text-sm text-content-muted">Flat structure with generated scripts</p>
              </div>
            </label>
            <label className="flex items-center gap-2 cursor-pointer">
              <input
                type="radio"
                name="exportTarget"
                value="pixinsight_wbpp"
                checked={exportTarget === 'pixinsight_wbpp'}
                onChange={() => setExportTarget('pixinsight_wbpp')}
                className="w-4 h-4 border-border bg-surface-hover text-accent focus:ring-accent"
              />
              <div>
                <span className="text-content">PixInsight WBPP</span>
                <p className="text-sm text-content-muted">Grouped structure for auto-detection</p>
              </div>
            </label>
          </div>
        </section>
      )}

      {/* Step 3: Calibration Summary */}
      {selectedFrameSetId && (
        <section className="bg-surface-elevated rounded-lg p-4">
          <h3 className="text-lg font-medium mb-3">3. Calibration Summary</h3>

          {loadingExportData || loadingRoute ? (
            <div className="flex items-center gap-2 text-content-muted">
              <Loader2 className="animate-spin" size={16} />
              Loading calibration data...
            </div>
          ) : calibrationRoute ? (
            <div className="space-y-4">
              {/* Groups Summary */}
              {exportData && exportData.groups.length > 0 && (
                <div className="mb-4">
                  <div className="text-sm text-content-muted mb-2">
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
                <div className="p-3 bg-warning-muted border border-warning/30 rounded-lg">
                  <div className="flex items-center gap-2 text-warning mb-2">
                    <AlertCircle size={16} />
                    <span className="font-medium">Warnings</span>
                  </div>
                  <ul className="text-sm text-warning/80 space-y-1">
                    {calibrationRoute.summary.warnings.map((warning, index) => (
                      <li key={index}>• {warning}</li>
                    ))}
                  </ul>
                </div>
              )}
            </div>
          ) : exportData ? (
            <div>
              <div className="mb-3 text-sm text-content-muted">
                {exportData.totalLightFrames} light frames •{' '}
                {(exportData.totalExposureSeconds / 3600).toFixed(1)}h total exposure
              </div>
              <CalibrationPreview summary={exportData.calibrationSummary} />
            </div>
          ) : (
            <div className="text-content-muted">No calibration data available</div>
          )}
        </section>
      )}

      {/* Step 4: Export Mode (Siril only) */}
      {selectedFrameSetId && exportData && exportTarget === 'siril' && (
        <section className="bg-surface-elevated rounded-lg p-4">
          <h3 className="text-lg font-medium mb-3">4. Export Mode</h3>
          <ExportModeSelector value={mode} onChange={setMode} />
        </section>
      )}

      {/* Step 5: Processing Options (Siril only) */}
      {selectedFrameSetId && exportData && exportTarget === 'siril' && (
        <section className="bg-surface-elevated rounded-lg p-4">
          <h3 className="text-lg font-medium mb-3">5. Processing Options</h3>
          <div className="p-3 bg-surface-hover/50 rounded-lg">
            <label className="flex items-center gap-2 cursor-pointer">
              <input
                type="checkbox"
                checked={createMasters}
                onChange={(e) => setCreateMasters(e.target.checked)}
                className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent"
              />
              <div>
                <span className="text-content">Create master calibration frames</span>
                <p className="text-sm text-content-muted">
                  Generates master bias, dark, and flat frames before preprocessing
                </p>
              </div>
            </label>
          </div>
          <p className="mt-3 text-sm text-content-muted">
            Siril workflow is automatically detected based on camera type (OSC/Mono) for each
            export group.
          </p>
        </section>
      )}

      {/* Step 6: Options (or Step 4 for WBPP) */}
      {selectedFrameSetId && exportData && (
        <section className="bg-surface-elevated rounded-lg p-4">
          <h3 className="text-lg font-medium mb-3">
            {exportTarget === 'siril' ? '6' : '4'}. Options
          </h3>
          <div className="space-y-4">
            {/* Output directory */}
            <div>
              <label className="block text-sm text-content-muted mb-1">
                Output Directory
              </label>
              <div className="flex gap-2">
                <input
                  type="text"
                  value={outputDir}
                  readOnly
                  placeholder="Select output folder..."
                  className="flex-1 px-3 py-2 bg-surface-hover border border-border rounded-lg text-content placeholder-content-muted"
                />
                <button
                  onClick={handleSelectFolder}
                  className="px-4 py-2 bg-surface-hover hover:brightness-110 rounded-lg flex items-center gap-2"
                >
                  <Folder size={16} />
                  Browse
                </button>
              </div>
            </div>

            {/* Advanced Siril Options (collapsible) */}
            {exportTarget === 'siril' && (
              <div className="border border-border rounded-lg overflow-hidden">
                <button
                  onClick={() => setShowAdvancedOptions(!showAdvancedOptions)}
                  className="w-full px-4 py-3 bg-surface-hover/50 hover:bg-surface-hover flex items-center justify-between text-left"
                >
                  <span className="font-medium text-content">Advanced Siril Options</span>
                  {showAdvancedOptions ? (
                    <ChevronDown size={20} className="text-content-muted" />
                  ) : (
                    <ChevronRight size={20} className="text-content-muted" />
                  )}
                </button>
                {showAdvancedOptions && (
                  <div className="p-4 space-y-4 bg-surface-elevated/50">
                    {/* Rejection Algorithm */}
                    <div>
                      <label className="block text-sm text-content-muted mb-1">
                        Rejection Algorithm
                      </label>
                      <select
                        value={rejectionAlgorithm}
                        onChange={(e) => setRejectionAlgorithm(e.target.value as RejectionAlgorithm)}
                        className="w-full px-3 py-2 bg-surface-hover border border-border rounded-lg text-content"
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
                        <label className="block text-sm text-content-muted mb-1">
                          Rejection Low (sigma)
                        </label>
                        <input
                          type="number"
                          value={rejectionLow}
                          onChange={(e) => setRejectionLow(parseFloat(e.target.value) || 2.5)}
                          step="0.1"
                          min="1"
                          max="6"
                          className="w-full px-3 py-2 bg-surface-hover border border-border rounded-lg text-content"
                        />
                      </div>
                      <div>
                        <label className="block text-sm text-content-muted mb-1">
                          Rejection High (sigma)
                        </label>
                        <input
                          type="number"
                          value={rejectionHigh}
                          onChange={(e) => setRejectionHigh(parseFloat(e.target.value) || 2.5)}
                          step="0.1"
                          min="1"
                          max="6"
                          className="w-full px-3 py-2 bg-surface-hover border border-border rounded-lg text-content"
                        />
                      </div>
                    </div>

                    {/* Image Weighting */}
                    <div>
                      <label className="block text-sm text-content-muted mb-1">
                        Image Weighting
                      </label>
                      <select
                        value={imageWeighting}
                        onChange={(e) => setImageWeighting(e.target.value as ImageWeightingMethod)}
                        className="w-full px-3 py-2 bg-surface-hover border border-border rounded-lg text-content"
                      >
                        <option value="wfwhm">Weighted FWHM (recommended)</option>
                        <option value="stars">Number of Stars</option>
                        <option value="noise">Noise Level</option>
                        <option value="exposure_time">Exposure Time</option>
                        <option value="none">No Weighting</option>
                      </select>
                      <p className="text-xs text-content-muted mt-1">
                        wFWHM weights by seeing quality and star count
                      </p>
                    </div>

                    {/* Reference Frame Mode */}
                    <div>
                      <label className="block text-sm text-content-muted mb-1">
                        Reference Frame Selection
                      </label>
                      <select
                        value={referenceFrameMode}
                        onChange={(e) => setReferenceFrameMode(e.target.value as ReferenceFrameMode)}
                        className="w-full px-3 py-2 bg-surface-hover border border-border rounded-lg text-content"
                      >
                        <option value="siril_auto">Siril Auto (-2pass)</option>
                        <option value="athenaeum_scoring">Athenaeum Quality Scoring (coming soon)</option>
                      </select>
                      <p className="text-xs text-content-muted mt-1">
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
                          className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent"
                        />
                        <span className="text-content">Enable Drizzle (super-resolution)</span>
                      </label>
                      {drizzleEnabled && (
                        <div className="ml-6">
                          <select
                            value={drizzleScale}
                            onChange={(e) => setDrizzleScale(e.target.value as DrizzleScale)}
                            className="w-full px-3 py-2 bg-surface-hover border border-border rounded-lg text-content"
                          >
                            <option value="x2">2x Scale</option>
                            <option value="x3">3x Scale</option>
                          </select>
                          <p className="text-xs text-content-muted mt-1">
                            For OSC cameras: Bayer pattern is preserved during registration
                          </p>
                        </div>
                      )}
                    </div>

                    {/* Exposure Time Grouping */}
                    <div className="space-y-2">
                      <label className="flex items-center gap-2 cursor-pointer">
                        <input
                          type="checkbox"
                          checked={exptimeToleranceEnabled}
                          onChange={(e) => setExptimeToleranceEnabled(e.target.checked)}
                          className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent"
                        />
                        <span className="text-content">Group frames by exposure time</span>
                      </label>
                      {exptimeToleranceEnabled && (
                        <div className="ml-6">
                          <label className="block text-sm text-content-muted mb-1">
                            Tolerance (seconds)
                          </label>
                          <input
                            type="number"
                            value={exptimeToleranceSeconds}
                            onChange={(e) => setExptimeToleranceSeconds(parseFloat(e.target.value) || 3)}
                            step="1"
                            min="1"
                            max="60"
                            className="w-32 px-3 py-2 bg-surface-hover border border-border rounded-lg text-content"
                          />
                          <p className="text-xs text-content-muted mt-1">
                            Frames within ±{exptimeToleranceSeconds}s will be stacked together
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
                className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent"
              />
              <span className="text-content-secondary">
                Use symbolic links instead of copying files
              </span>
            </label>
          </div>
        </section>
      )}

      {/* Validation Step (Pipeline Mode) */}
      {showValidation && pipelinePlan && (
        <section className="bg-surface-elevated rounded-lg p-4">
          <h3 className="text-lg font-medium mb-3">
            {exportTarget === 'siril' ? '7' : '5'}. Validate Export
          </h3>
          <ValidationWarnings
            plan={pipelinePlan}
            onContinue={handleStartPipeline}
            onCancel={handleCancelValidation}
            showActions={true}
          />
          {!pipelinePlan.hasBlockingErrors && (
            <div className="mt-4">
              <h4 className="text-sm font-medium text-content-muted mb-2">Pipeline Steps Preview</h4>
              <PipelineStepsSummary steps={pipelinePlan.steps} />
            </div>
          )}
        </section>
      )}

      {/* Loading Validation */}
      {showValidation && loadingPlan && (
        <section className="bg-surface-elevated rounded-lg p-4">
          <h3 className="text-lg font-medium mb-3">Validating...</h3>
          <div className="flex items-center gap-2 text-content-muted">
            <Loader2 className="animate-spin" size={16} />
            Checking source files and calibration links...
          </div>
        </section>
      )}

      {/* Pipeline Progress (Active Job) */}
      {hasActiveJob && (
        <section className="bg-surface-elevated rounded-lg p-4">
          <h3 className="text-lg font-medium mb-3">Export Progress</h3>
          <PipelineVisualizer
            job={activeJob}
            progress={pipelineProgress}
            onPause={pauseJob}
            onResume={() => resumeJob()}
            onCancel={cancelJob}
            onRetryStep={retryStep}
            showControls={true}
          />
          {jobComplete && (
            <div className="mt-4 flex justify-end">
              <button
                onClick={handleReset}
                className="px-4 py-2 text-sm font-medium bg-accent text-white rounded-lg hover:bg-accent/90"
              >
                Start New Export
              </button>
            </div>
          )}
        </section>
      )}

      {/* Legacy Progress (non-pipeline mode) */}
      {!usePipelineMode && (executing || progress) && (
        <ExportProgress progress={progress} />
      )}

      {/* Result (Legacy mode) */}
      {!usePipelineMode && result && (
        <div
          className={`p-4 rounded-lg border ${
            result.success
              ? 'bg-success-muted border-success/30'
              : 'bg-error-muted border-error/30'
          }`}
        >
          <div className="flex items-center gap-2 mb-2">
            {result.success ? (
              <Check className="text-success" size={20} />
            ) : (
              <AlertCircle className="text-error" size={20} />
            )}
            <span className="font-medium">
              {result.success ? 'Export Complete' : 'Export Failed'}
            </span>
          </div>
          {result.success ? (
            <div className="text-sm text-content-muted">
              <div>Files organized: {result.filesOrganized}</div>
              <div>Scripts generated: {result.scriptsGenerated.length}</div>
              <div className="mt-2 text-content-muted truncate">
                Output: {result.outputDir}
              </div>
            </div>
          ) : (
            <div className="text-sm text-error">{result.error}</div>
          )}
          {result.warnings.length > 0 && (
            <div className="mt-3 text-sm text-warning">
              <div className="font-medium mb-1">Warnings:</div>
              {result.warnings.map((warning, i) => (
                <div key={i}>• {warning}</div>
              ))}
            </div>
          )}
        </div>
      )}

      {/* Export buttons */}
      {selectedFrameSetId && exportData && !showValidation && !hasActiveJob && (
        <div className="space-y-3">
          {/* Pipeline Mode Toggle */}
          <div className="flex items-center justify-between p-3 bg-surface-elevated rounded-lg border border-border">
            <div>
              <div className="font-medium text-content">Pipeline Mode</div>
              <div className="text-sm text-content-muted">
                {usePipelineMode
                  ? 'Validate files, track progress step-by-step, and resume on failure'
                  : 'Direct execution (legacy mode)'}
              </div>
            </div>
            <label className="relative inline-flex items-center cursor-pointer">
              <input
                type="checkbox"
                checked={usePipelineMode}
                onChange={(e) => setUsePipelineMode(e.target.checked)}
                className="sr-only peer"
              />
              <div className="w-11 h-6 bg-surface-hover peer-focus:ring-2 peer-focus:ring-accent rounded-full peer peer-checked:after:translate-x-full after:content-[''] after:absolute after:top-[2px] after:left-[2px] after:bg-white after:rounded-full after:h-5 after:w-5 after:transition-all peer-checked:bg-accent"></div>
            </label>
          </div>

          {/* Export Button */}
          {usePipelineMode ? (
            <button
              onClick={handleValidate}
              disabled={!canExport}
              className={`w-full py-3 rounded-lg font-medium flex items-center justify-center gap-2 ${
                canExport
                  ? 'bg-accent hover:bg-accent-hover'
                  : 'bg-surface-hover cursor-not-allowed text-content-muted'
              }`}
            >
              <FileCheck size={20} />
              Validate & Export
            </button>
          ) : (
            <button
              onClick={handleExport}
              disabled={!canExport}
              className={`w-full py-3 rounded-lg font-medium flex items-center justify-center gap-2 ${
                canExport
                  ? 'bg-accent hover:bg-accent-hover'
                  : 'bg-surface-hover cursor-not-allowed text-content-muted'
              }`}
            >
              {executing ? (
                <>
                  <Loader2 className="animate-spin" size={20} />
                  Exporting...
                </>
              ) : (
                <>
                  <Play size={20} />
                  Export (Direct)
                </>
              )}
            </button>
          )}
        </div>
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
          ? 'bg-success-muted text-success'
          : 'bg-error-muted text-error'
      }`}
    >
      {label}: {complete ? <Check size={12} className="inline" /> : '—'}
    </span>
  );
}
