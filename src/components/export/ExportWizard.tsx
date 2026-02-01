import { useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { Folder, Loader2, Check, AlertCircle, Play } from 'lucide-react';
import { FrameSetSelector } from './FrameSetSelector';
import { ExportSummary } from './ExportSummary';
import { useExportableFrameSets } from '../../hooks/useExportData';
import type { ExportResult } from '../../types/export';

interface ExportWizardProps {
  initialFrameSetId?: number;
}

export function ExportWizard({ initialFrameSetId }: ExportWizardProps) {
  // State
  const [selectedFrameSetId, setSelectedFrameSetId] = useState<number | null>(
    initialFrameSetId ?? null
  );
  const [outputDir, setOutputDir] = useState<string>('');
  const [useSymlinks, setUseSymlinks] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [result, setResult] = useState<ExportResult | null>(null);

  // Hooks
  const { frameSets, loading: loadingFrameSets } = useExportableFrameSets();

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
    setExporting(true);

    try {
      const exportResult = await invoke<ExportResult>('export_to_wbpp', {
        frameSetId: selectedFrameSetId,
        outputDir,
        useSymlinks,
      });
      setResult(exportResult);
    } catch (error) {
      setResult({
        success: false,
        outputDir,
        filesOrganized: 0,
        scriptsGenerated: [],
        warnings: [],
        error: String(error),
      });
    } finally {
      setExporting(false);
    }
  }, [selectedFrameSetId, outputDir, useSymlinks]);

  // Check if ready to export
  const canExport =
    selectedFrameSetId !== null &&
    outputDir !== '' &&
    !exporting;

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

      {/* Step 2: Export Summary */}
      {selectedFrameSetId && (
        <section className="bg-surface-elevated rounded-lg p-4">
          <h3 className="text-lg font-medium mb-3">2. Export Summary</h3>
          <ExportSummary frameSetId={selectedFrameSetId} />
        </section>
      )}

      {/* Step 3: Export Options */}
      {selectedFrameSetId && (
        <section className="bg-surface-elevated rounded-lg p-4">
          <h3 className="text-lg font-medium mb-3">3. Export Options</h3>
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

            {/* Folder structure info */}
            <div className="p-3 bg-surface-hover/50 rounded-lg text-sm text-content-muted">
              <p className="font-medium mb-1">PixInsight WBPP folder structure:</p>
              <pre className="text-xs font-mono">
{`output/
└── {camera}/
    ├── darks/
    │   └── (bias, dark, darkflat files)
    └── flats_{filter}/
        ├── (flat files)
        └── lights/
            └── (light frames)`}
              </pre>
            </div>
          </div>
        </section>
      )}

      {/* Result */}
      {result && (
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

      {/* Export button */}
      {selectedFrameSetId && (
        <button
          onClick={handleExport}
          disabled={!canExport}
          className={`w-full py-3 rounded-lg font-medium flex items-center justify-center gap-2 ${
            canExport
              ? 'bg-accent hover:bg-accent-hover'
              : 'bg-surface-hover cursor-not-allowed text-content-muted'
          }`}
        >
          {exporting ? (
            <>
              <Loader2 className="animate-spin" size={20} />
              Exporting...
            </>
          ) : (
            <>
              <Play size={20} />
              Export to WBPP
            </>
          )}
        </button>
      )}
    </div>
  );
}
