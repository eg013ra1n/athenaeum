import { useState, useCallback, useMemo } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { Folder, Loader2, Play, Layers } from 'lucide-react';
import { FrameSetSelector } from './FrameSetSelector';
import { ExportSummary } from './ExportSummary';
import { useExportableFrameSets, useWbppConfig } from '../../hooks/useExportData';
import { useExportProgressContext } from '../../contexts/ExportProgressContext';

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

  // Hooks
  const { frameSets, loading: loadingFrameSets } = useExportableFrameSets();
  const { config: wbppConfig } = useWbppConfig();
  const { startExport, hasActiveExports } = useExportProgressContext();
  const exporting = hasActiveExports;

  // Build dynamic WBPP keyword instructions from config
  const keywordDescriptions: Record<string, string> = useMemo(() => ({
    CAMERA: 'Groups files by camera/instrument',
    BIAS: 'Bias calibration level (outermost calibration)',
    DARKS: 'Dark + darkflat calibration level',
    FLAT: 'Flat calibration level (calibrates lights)',
  }), []);

  const wbppKeywords = useMemo(() => {
    if (!wbppConfig) return [];
    return wbppConfig.keywordOrder.map((kw) => ({
      keyword: kw,
      description: keywordDescriptions[kw] || kw,
    }));
  }, [wbppConfig, keywordDescriptions]);

  const exampleStructure = useMemo(() => {
    if (!wbppConfig) return '';
    // Build example structure based on keyword order
    const order = wbppConfig.keywordOrder;
    const lines: string[] = ['output/'];
    let indent = '└── ';
    let prefix = '';
    for (const kw of order) {
      switch (kw) {
        case 'CAMERA':
          lines.push(`${prefix}${indent}camera_{instrume}/`);
          prefix = '    ';
          indent = '└── ';
          break;
        case 'BIAS':
          lines.push(`${prefix}${indent}BIAS_{id}/`);
          lines.push(`${prefix}    bias frames...`);
          prefix += '    ';
          indent = '└── ';
          break;
        case 'DARKS':
          lines.push(`${prefix}${indent}DARKS_{id}/`);
          lines.push(`${prefix}    dark + darkflat frames...`);
          prefix += '    ';
          indent = '└── ';
          break;
        case 'FLAT':
          lines.push(`${prefix}${indent}FLAT_{id}/`);
          lines.push(`${prefix}    flat frames...`);
          prefix += '    ';
          indent = '└── ';
          break;
      }
    }
    lines.push(`${prefix}${indent}lights/`);
    lines.push(`${prefix}    light frames...`);
    return lines.join('\n');
  }, [wbppConfig]);

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

  // Handle export — delegates to global context (progress modal survives navigation)
  const handleExport = useCallback(async () => {
    if (!selectedFrameSetId || !outputDir) return;
    startExport(selectedFrameSetId, outputDir, useSymlinks);
  }, [selectedFrameSetId, outputDir, useSymlinks, startExport]);

  // Check if ready to export
  const canExport =
    selectedFrameSetId !== null &&
    outputDir !== '' &&
    !exporting;

  return (
    <div className="flex gap-6 h-full">
      {/* Left sidebar — Frame set selector */}
      <div className="w-[380px] flex-shrink-0 flex flex-col bg-surface-elevated rounded-lg p-4">
        <h3 className="text-lg font-medium mb-3">Frame Sets</h3>
        <FrameSetSelector
          frameSets={frameSets}
          loading={loadingFrameSets}
          selectedId={selectedFrameSetId}
          onSelect={setSelectedFrameSetId}
        />
      </div>

      {/* Right panel — Export summary, options, and actions */}
      <div className="flex-1 min-w-0 overflow-y-auto">
        {selectedFrameSetId ? (
          <div className="space-y-6">
            {/* Export Summary */}
            <section className="bg-surface-elevated rounded-lg p-4">
              <ExportSummary frameSetId={selectedFrameSetId} />
            </section>

            {/* Export Options */}
            <section className="bg-surface-elevated rounded-lg p-4">
              <h3 className="text-lg font-medium mb-3">Export Options</h3>
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
                {(() => {
                  const isWindows = navigator.userAgent.includes('Windows');
                  return (
                    <div>
                      <label className={`flex items-center gap-2 ${isWindows ? 'opacity-50 cursor-not-allowed' : 'cursor-pointer'}`}>
                        <input
                          type="checkbox"
                          checked={useSymlinks}
                          onChange={(e) => setUseSymlinks(e.target.checked)}
                          disabled={isWindows}
                          className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent"
                        />
                        <span className="text-content-secondary">
                          Use symbolic links instead of copying files
                        </span>
                      </label>
                      {isWindows && (
                        <p className="mt-1 ml-6 text-xs text-content-muted">
                          Symbolic links are only available on macOS and Linux.
                        </p>
                      )}
                    </div>
                  );
                })()}

                {/* WBPP Setup Guide */}
                <details className="p-3 bg-surface-hover/50 rounded-lg text-sm text-content-muted">
                  <summary className="font-medium cursor-pointer select-none">
                    WBPP Setup Guide
                  </summary>
                  <div className="mt-3 space-y-3">
                    <p>To enable automatic grouping in WBPP, add these <strong>Grouping Keywords</strong> in <strong>exactly this order</strong>:</p>
                    <ol className="list-decimal list-inside space-y-1 text-xs">
                      <li>Open WBPP in PixInsight</li>
                      <li>Check <strong>Grouping Keywords</strong></li>
                      {wbppKeywords.map((kw) => (
                        <li key={kw.keyword}>
                          Add <code className="px-1 bg-surface-hover rounded">{kw.keyword}</code> with <strong>Pre</strong> checked
                          <span className="text-content-muted ml-1">({kw.description})</span>
                        </li>
                      ))}
                    </ol>
                    {exampleStructure && (
                      <div>
                        <p className="font-medium mb-1">Expected folder structure:</p>
                        <pre className="text-xs font-mono">{exampleStructure}</pre>
                      </div>
                    )}
                  </div>
                </details>
              </div>
            </section>

            {/* Export button */}
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
          </div>
        ) : (
          <div className="flex flex-col items-center justify-center h-full text-content-muted">
            <Layers size={48} className="mb-4 opacity-30" />
            <p className="text-lg">Select a frame set to preview export</p>
          </div>
        )}
      </div>
    </div>
  );
}
