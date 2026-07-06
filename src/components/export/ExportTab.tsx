import { useCallback, useEffect, useMemo, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { Folder, Loader2, Play } from 'lucide-react';
import { api } from '../../api';
import { pickDirectory } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { useExportSummary, useWbppConfig } from '../../hooks/useExportData';
import { useExportProgressContext } from '../../contexts/ExportProgressContext';
import { ExportSummary } from './ExportSummary';
import { WarningsPanel } from './WarningsPanel';
import { FolderBrowserModal } from '../FolderBrowserModal';
import {
  readFlatNormPref,
  readFlatNormModePref,
  readLightCalParamsPref,
} from '../calibration/CalibrateLightsDialog';
import type { CalibrationDetail, ExportMode } from '../../types/export';
import type { ExportReadiness } from '../../types/models';

interface ExportTabProps {
  frameSetId: number;
  frameSetName?: string;
}

/** localStorage key for the last-used WBPP export mode (spec §12.2). */
const EXPORT_MODE_KEY = 'athenaeum.export.mode';

/** Message shown when the calibrated-lights gate blocks the export. */
const CALIBRATE_FIRST_MSG = 'Run Calibrate Lights first';

/** Radio options for the export-mode selector, in spec §12.2 order. The default
 *  (first-run, unset/corrupt storage) is `rawWithCalibrationSets`. */
const EXPORT_MODE_OPTIONS: { value: ExportMode; label: string; hint: string }[] = [
  {
    value: 'rawWithCalibrationSets',
    label: 'Raw lights + calibration sets (current)',
    hint: 'Exports raw light frames with their matched raw calibration frames — WBPP performs all calibration.',
  },
  {
    value: 'rawWithMasters',
    label: 'Raw lights + master calibration files',
    hint: 'Uses built master calibration files. Raw sets without a built master are omitted with a warning.',
  },
  {
    value: 'calibratedLights',
    label: 'Calibrated lights (skip WBPP calibration)',
    hint: 'Exports c_*.fits calibrated artifacts, no calibration frames. Run Calibrate Lights first.',
  },
];

/** Read the persisted export mode, defaulting to `rawWithCalibrationSets` when
 *  unset or corrupt. */
function readExportModePref(): ExportMode {
  try {
    const raw = localStorage.getItem(EXPORT_MODE_KEY);
    if (raw === 'calibratedLights' || raw === 'rawWithMasters' || raw === 'rawWithCalibrationSets') {
      return raw;
    }
  } catch {
    /* ignore — fall through to default */
  }
  return 'rawWithCalibrationSets';
}

/**
 * Embedded Export form for the Object detail page. Slimmed version of the
 * old ExportWizard with the FrameSetSelector picker removed — the frame
 * set is implicit (the one the user is already viewing). Single-column
 * layout that scrolls inside its tab.
 *
 * Bundles fixes that were unsurfaced in the old wizard:
 *   - WarningsPanel rendered at the top (was previously dead code).
 *   - try/catch on the export trigger so pre-progress failures surface.
 *   - Output-dir input has a hover tooltip with the full path.
 *   - Browse button uses accent-tinted styling when no path picked yet.
 *   - Symlinks toggle hidden states explained inline.
 *   - "Export to WBPP" button has a tooltip explaining the acronym.
 */
export function ExportTab({ frameSetId, frameSetName: _frameSetName }: ExportTabProps) {
  const navigate = useNavigate();
  const [outputDir, setOutputDir] = useState<string>('');
  const [useSymlinks, setUseSymlinks] = useState(false);
  const [showFolderBrowser, setShowFolderBrowser] = useState(false);
  const [exportError, setExportError] = useState<string | null>(null);

  const { summary, loading: loadingSummary, error: summaryError } = useExportSummary(frameSetId);
  const { config: wbppConfig, save: saveWbppConfig } = useWbppConfig();
  const { startExport, hasActiveExports } = useExportProgressContext();
  const exporting = hasActiveExports;

  // Export mode (spec §12.2). Persisted to localStorage for cross-session
  // memory; synced into the persisted WbppExportConfig at export time (the
  // backend reads `export_mode` from that config, not from the invoke args).
  const [exportMode, setExportMode] = useState<ExportMode>(readExportModePref);

  // Calibrated-lights readiness — fetched only when that mode is selected. The
  // gate here is UX; the backend re-checks and hard-errors on a stale/missing
  // calibrated output, so the guarantee never depends on this fetch.
  const [readiness, setReadiness] = useState<ExportReadiness | null>(null);
  const [readinessLoading, setReadinessLoading] = useState(false);
  const [readinessError, setReadinessError] = useState<string | null>(null);

  const handleModeChange = useCallback((mode: ExportMode) => {
    setExportMode(mode);
    try {
      localStorage.setItem(EXPORT_MODE_KEY, mode);
    } catch {
      /* ignore — localStorage unavailable (private mode / quota) */
    }
  }, []);

  useEffect(() => {
    if (exportMode !== 'calibratedLights') {
      setReadiness(null);
      setReadinessError(null);
      return;
    }
    let cancelled = false;
    setReadinessLoading(true);
    setReadinessError(null);
    // Resolve staleness against the exact prefs Calibrate Lights would submit,
    // so the dialog's tally agrees with what the backend gate will compute.
    api
      .invoke<ExportReadiness>('get_export_readiness', {
        setId: frameSetId,
        mode: 'calibratedLights',
        flatNorm: readFlatNormPref(),
        flatNormMode: readFlatNormModePref(),
        params: readLightCalParamsPref(),
      })
      .then(r => { if (!cancelled) setReadiness(r); })
      .catch(err => {
        if (cancelled) return;
        console.error('[ExportTab] get_export_readiness failed:', err);
        setReadinessError(typeof err === 'string' ? err : (err as Error)?.message ?? String(err));
        setReadiness(null);
      })
      .finally(() => { if (!cancelled) setReadinessLoading(false); });
    return () => { cancelled = true; };
  }, [exportMode, frameSetId]);

  // Number of lights lacking a fresh calibrated output (stale + missing).
  const notReady = readiness ? readiness.stale + readiness.missing : 0;
  // Calibrated-lights export is gated until every light has a fresh output.
  // The other two modes are always allowed. `readiness === null` (loading /
  // errored) keeps the gate closed so we never fire an export that will
  // hard-error at the backend.
  const calibratedGateOk =
    exportMode !== 'calibratedLights' || (readiness !== null && notReady === 0);

  // In web mode, pull the server-configured export directory once.
  useEffect(() => {
    if (isTauri) return;
    api
      .invoke<string | null>('get_export_dir', {})
      .then(dir => { if (dir) setOutputDir(dir); })
      .catch(err => console.error('Failed to get export dir:', err));
  }, []);

  // Build the WBPP keyword guide from the live config so it always reflects
  // what the backend will actually do.
  const keywordDescriptions: Record<string, string> = useMemo(
    () => ({
      CAMERA: 'Groups files by camera/instrument',
      BIAS: 'Bias calibration level (outermost calibration)',
      DARKS: 'Dark + darkflat calibration level',
      FLAT: 'Flat calibration level (calibrates lights)',
    }),
    [],
  );

  const wbppKeywords = useMemo(() => {
    if (!wbppConfig) return [];
    return wbppConfig.keywordOrder.map(kw => ({
      keyword: kw,
      description: keywordDescriptions[kw] || kw,
    }));
  }, [wbppConfig, keywordDescriptions]);

  const exampleStructure = useMemo(() => {
    if (!wbppConfig) return '';
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

  // Map every calibration set ID surfaced by the summary back to its kind
  // ('flat' / 'dark' / 'bias') so a click on a warning's `#setId` chip can
  // hand off to the Coverage tab with the right table targeted. Walks
  // `subCalibrations` recursively so a sub-cal warning (e.g. a Bias used
  // only via a Dark) still resolves correctly. DarkFlat collapses to 'dark'
  // (it lives in the Darks bucket per the existing CalibrationTableView
  // convention).
  const setKindMap = useMemo<Map<number, 'flat' | 'dark' | 'bias'>>(() => {
    const map = new Map<number, 'flat' | 'dark' | 'bias'>();
    if (!summary) return map;
    const visit = (detail: CalibrationDetail | null | undefined) => {
      if (!detail) return;
      const t = detail.calibrationType;
      const kind: 'flat' | 'dark' | 'bias' =
        t === 'Flat' || t === 'MasterFlat' ? 'flat'
        : t === 'Bias' || t === 'MasterBias' ? 'bias'
        : 'dark'; // Dark, MasterDark, DarkFlat, MasterDarkFlat
      map.set(detail.setId, kind);
      for (const sub of detail.subCalibrations ?? []) {
        visit(sub);
      }
    };
    for (const group of summary.filterGroups) {
      visit(group.flatInfo);
      visit(group.darkInfo);
      visit(group.biasInfo);
    }
    return map;
  }, [summary]);

  // URL-based jump (same mechanism as the Equipment chip → coverage flow).
  // FrameSetDetail watches searchParams and consumes these to switch tab +
  // seed pendingHighlightCalSet, which CalibrationTableView then highlights.
  const handleSetClick = useCallback((setId: number) => {
    const kind = setKindMap.get(setId) ?? 'dark';
    navigate(`?tab=calibration&highlightSet=${setId}&kind=${kind}`);
  }, [navigate, setKindMap]);

  const handleSelectFolder = useCallback(async () => {
    if (!isTauri) {
      setShowFolderBrowser(true);
      return;
    }
    const selected = await pickDirectory();
    if (selected && typeof selected === 'string') {
      setOutputDir(selected);
    }
  }, []);

  // Wrap startExport so synchronous failures (bad path, permission denied
  // before progress events fire) surface inline instead of being swallowed.
  const handleExport = useCallback(async () => {
    if (!frameSetId || !outputDir) return;
    setExportError(null);
    // Best-effort: persist the selected mode into the WbppExportConfig so it
    // survives reloads. No longer correctness-bearing — the mode now travels as
    // an explicit invoke arg below, so a slow/failed/unloaded config can't make
    // the backend export the wrong mode. Fire-and-forget: a persistence error
    // must never block or fail an otherwise-correct export.
    if (wbppConfig && wbppConfig.exportMode !== exportMode) {
      void saveWbppConfig({ ...wbppConfig, exportMode }).catch((err) => {
        console.error('Failed to persist export mode (non-fatal):', err);
      });
    }
    try {
      await startExport(frameSetId, outputDir, useSymlinks, {
        flatNorm: readFlatNormPref(),
        flatNormMode: readFlatNormModePref(),
        params: readLightCalParamsPref(),
      }, exportMode);
    } catch (err) {
      console.error('Failed to start export:', err);
      setExportError(typeof err === 'string' ? err : (err as Error)?.message ?? String(err));
    }
  }, [frameSetId, outputDir, useSymlinks, exportMode, wbppConfig, saveWbppConfig, startExport]);

  const canExport = outputDir !== '' && !exporting && calibratedGateOk;

  // Symlink toggle eligibility — Tauri on macOS / Linux only. On web mode
  // (Docker always copies) and on Windows we hide the toggle but explain
  // why so the user isn't left guessing.
  const isWindows = typeof navigator !== 'undefined' && navigator.userAgent.includes('Windows');
  const symlinksAvailable = isTauri && !isWindows;
  const symlinkUnavailableReason = !isTauri
    ? 'Files will be copied (web mode always copies; symbolic links are not supported in the Docker build).'
    : isWindows
      ? 'Files will be copied (symbolic links are only available on macOS and Linux).'
      : null;

  return (
    <div className="h-full overflow-y-auto">
      {loadingSummary ? (
        <div className="flex items-center justify-center py-12">
          <Loader2 className="w-8 h-8 animate-spin text-accent" />
          <span className="ml-3 text-content-muted">Loading export summary…</span>
        </div>
      ) : summaryError ? (
        <div className="p-4 bg-error/10 border border-error/30 rounded-lg">
          <h3 className="font-medium text-error mb-1">Failed to load export summary</h3>
          <p className="text-sm text-content-muted">{summaryError}</p>
        </div>
      ) : !summary ? (
        <div className="p-4 text-center text-content-muted">
          No export data available
        </div>
      ) : (
        <div className="space-y-6 pb-6">
          {/* Export mode selector (spec §12.2) — controls what the lights +
              calibration side put on disk. Persisted in localStorage and, at
              export time, synced into the WbppExportConfig the backend reads. */}
          <section className="bg-surface-elevated rounded-lg p-4">
            <h3 className="text-lg font-medium mb-1">Export Mode</h3>
            <p className="text-sm text-content-muted mb-3">
              Choose what lands on disk for PixInsight WBPP.
            </p>
            <div role="radiogroup" aria-label="Export mode" className="space-y-2">
              {EXPORT_MODE_OPTIONS.map(opt => {
                const active = exportMode === opt.value;
                return (
                  <label
                    key={opt.value}
                    className={`flex items-start gap-3 p-3 rounded-lg border cursor-pointer transition-colors ${
                      active
                        ? 'border-accent bg-accent/10'
                        : 'border-border bg-surface-hover/50 hover:bg-surface-hover'
                    }`}
                  >
                    <input
                      type="radio"
                      name="export-mode"
                      value={opt.value}
                      checked={active}
                      onChange={() => handleModeChange(opt.value)}
                      className="mt-0.5 w-4 h-4 text-accent border-border focus:ring-accent"
                    />
                    <span className="flex-1">
                      <span className="block text-sm font-medium text-content">{opt.label}</span>
                      <span className="block text-xs text-content-muted mt-0.5">{opt.hint}</span>
                    </span>
                  </label>
                );
              })}
            </div>

            {/* Calibrated-lights readiness tally + gate message. */}
            {exportMode === 'calibratedLights' && (
              <div className="mt-3 text-sm">
                {readinessLoading ? (
                  <span className="flex items-center gap-2 text-content-muted">
                    <Loader2 size={14} className="animate-spin" /> Checking calibrated-lights readiness…
                  </span>
                ) : readinessError ? (
                  <span className="text-error">Failed to check readiness: {readinessError}</span>
                ) : readiness ? (
                  <div
                    className={`rounded-lg p-3 ${
                      notReady > 0
                        ? 'bg-error/10 border border-error/30'
                        : 'bg-success/10 border border-success/30'
                    }`}
                  >
                    <span className="text-content">
                      <span className="font-medium">{readiness.calibrated}</span> of {readiness.total} calibrated,{' '}
                      <span className="font-medium">{readiness.stale}</span> stale,{' '}
                      <span className="font-medium">{readiness.missing}</span> missing.
                    </span>
                    {notReady > 0 && (
                      <span className="block mt-1 text-error font-medium">{CALIBRATE_FIRST_MSG}</span>
                    )}
                  </div>
                ) : null}
              </div>
            )}
          </section>

          {/* Top-level warnings — was previously dead code (WarningsPanel
              existed but was never rendered). Now surfaces missing-cal,
              temperature, age, and parameter mismatches up front. Each
              warning's set ID is a clickable chip that jumps to the
              Calibration Coverage tab and highlights the row. */}
          {summary.warnings.length > 0 && (
            <WarningsPanel
              warnings={summary.warnings}
              onSetClick={handleSetClick}
            />
          )}

          {/* Export Summary (equipment header, filter groups, folder preview) */}
          <section className="bg-surface-elevated rounded-lg p-4">
            <ExportSummary summary={summary} />
          </section>

          {/* Export Options */}
          <section className="bg-surface-elevated rounded-lg p-4">
            <h3 className="text-lg font-medium mb-3">Export Options</h3>
            <div className="space-y-4">
              {/* Output directory */}
              <div>
                <label htmlFor="export-output-dir" className="block text-sm text-content-muted mb-1">
                  Output Directory
                </label>
                <div className="flex gap-2">
                  <input
                    id="export-output-dir"
                    type="text"
                    value={outputDir}
                    readOnly
                    placeholder="Select output folder…"
                    title={outputDir || undefined}
                    className="flex-1 px-3 py-2 bg-surface-hover border border-border rounded-lg text-content placeholder-content-muted truncate"
                  />
                  <button
                    onClick={handleSelectFolder}
                    title="Pick the destination folder"
                    className={`px-4 py-2 rounded-lg flex items-center gap-2 transition-colors ${
                      outputDir
                        ? 'bg-surface-hover border border-border hover:brightness-110'
                        : 'bg-accent/10 border border-accent/40 text-accent hover:bg-accent/20'
                    }`}
                  >
                    <Folder size={16} />
                    Browse
                  </button>
                </div>
              </div>

              {/* Symlinks toggle (macOS / Linux Tauri only) — hidden states
                  explained inline so the absence isn't mysterious. */}
              {symlinksAvailable ? (
                <div>
                  <label className="flex items-center gap-2 cursor-pointer">
                    <input
                      type="checkbox"
                      checked={useSymlinks}
                      onChange={e => setUseSymlinks(e.target.checked)}
                      className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent"
                    />
                    <span className="text-content-secondary">
                      Use symbolic links instead of copying files
                    </span>
                  </label>
                </div>
              ) : symlinkUnavailableReason ? (
                <p className="text-xs text-content-muted">{symlinkUnavailableReason}</p>
              ) : null}

              {/* WBPP setup guide — collapsible because most users only
                  need it once, but the summary is right there. */}
              <details className="p-3 bg-surface-hover/50 rounded-lg text-sm text-content-muted">
                <summary className="font-medium cursor-pointer select-none">
                  WBPP Setup Guide
                </summary>
                <div className="mt-3 space-y-3">
                  <p>
                    To enable automatic grouping in WBPP, add these <strong>Grouping Keywords</strong> in <strong>exactly this order</strong>:
                  </p>
                  <ol className="list-decimal list-inside space-y-1 text-xs">
                    <li>Open WBPP in PixInsight</li>
                    <li>Check <strong>Grouping Keywords</strong></li>
                    {wbppKeywords.map(kw => (
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

          {/* Inline error banner for pre-progress failures (bad path, perms,
              etc.). Progress + completion banners are rendered globally by
              ExportProgressIndicator. */}
          {exportError && (
            <div className="p-3 bg-error/10 border border-error/30 rounded-lg text-sm text-error flex items-start gap-2">
              <span className="flex-1">Export failed to start: {exportError}</span>
              <button
                type="button"
                onClick={() => setExportError(null)}
                className="underline hover:no-underline flex-shrink-0"
              >
                Dismiss
              </button>
            </div>
          )}

          {/* Export trigger */}
          <button
            onClick={() => { void handleExport(); }}
            disabled={!canExport}
            title={
              exportMode === 'calibratedLights' && !calibratedGateOk
                ? CALIBRATE_FIRST_MSG
                : 'Export to PixInsight WBPP folder structure'
            }
            className={`w-full py-3 rounded-lg font-medium flex items-center justify-center gap-2 ${
              canExport
                ? 'bg-accent hover:bg-accent-hover text-white'
                : 'bg-surface-hover cursor-not-allowed text-content-muted'
            }`}
          >
            {exporting ? (
              <>
                <Loader2 className="animate-spin" size={20} />
                Exporting…
              </>
            ) : (
              <>
                <Play size={20} />
                Export to WBPP
              </>
            )}
          </button>
        </div>
      )}

      {/* Web mode: folder browser for export directory */}
      <FolderBrowserModal
        isOpen={showFolderBrowser}
        scope="export"
        onSelect={path => {
          setOutputDir(path);
          setShowFolderBrowser(false);
        }}
        onClose={() => setShowFolderBrowser(false)}
      />
    </div>
  );
}
