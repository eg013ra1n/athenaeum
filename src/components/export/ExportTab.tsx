import { useCallback, useEffect, useMemo, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { AlertTriangle, Folder, Loader2, Play, Send } from 'lucide-react';
import { api } from '../../api';
import { pickDirectory } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { useExportSummary, useWbppConfig } from '../../hooks/useExportData';
import { useExportProgressContext } from '../../contexts/ExportProgressContext';
import { ExportSummary } from './ExportSummary';
import { WarningsPanel } from './WarningsPanel';
import { FolderBrowserModal } from '../FolderBrowserModal';
import { SendToNodeDialog } from '../transfers/SendToNodeDialog';
import {
  readFlatNormPref,
  readFlatNormModePref,
  readLightCalParamsPref,
} from './lightCalPrefs';
import type { CalibrationDetail, ExportMode } from '../../types/export';
import type { ExportFileCounts, ExportReadiness } from '../../types/models';

interface ExportTabProps {
  frameSetId: number;
  frameSetName?: string;
}

/** localStorage key for the last-used WBPP export mode (spec §12.2). */
const EXPORT_MODE_KEY = 'athenaeum.export.mode';

/** Radio options for the export-mode selector, in spec §12.2 order. The default
 *  (first-run, unset/corrupt storage) is `rawWithCalibrationSets`. Each row
 *  selects its own file count out of the readiness payload, so a single fetch
 *  feeds all four. */
const EXPORT_MODE_OPTIONS: { value: ExportMode; label: string; hint: string; count: (c: ExportFileCounts) => number }[] = [
  { value: 'lightsOnly', label: 'Lights only', hint: 'Raw light frames, no calibration frames.', count: c => c.lightsOnly },
  { value: 'rawWithCalibrationSets', label: 'Lights + calibration sets', hint: 'Raw light frames with their matched raw calibration frames — WBPP performs all calibration.', count: c => c.rawWithCalibrationSets },
  { value: 'rawWithMasters', label: 'Lights + masters', hint: 'Raw lights with the built master calibration files. Every linked set needs a master.', count: c => c.rawWithMasters },
  { value: 'calibratedLights', label: 'Calibrated lights', hint: 'c_*.fits calibrated artifacts, no calibration frames — WBPP runs with calibration disabled.', count: c => c.calibratedLights },
];

/** Why `mode` is not ready, or null. Mirrors core `check_mode_ready` — including
 *  the calibrated mode's blocker ORDER (masters before links: a build is what
 *  can also change where a light's links resolve to). */
function modeBlocker(r: ExportReadiness, mode: ExportMode): string | null {
  const noMaster = `Build masters first — ${r.rawSetsWithoutMaster} set${r.rawSetsWithoutMaster === 1 ? '' : 's'} without a master`;
  if (mode === 'rawWithMasters' && r.rawSetsWithoutMaster > 0) {
    return noMaster;
  }
  if (mode === 'calibratedLights') {
    if (r.rawSetsWithoutMaster > 0) return noMaster;
    if (r.unlinkedLights > 0) {
      return `${r.unlinkedLights} light${r.unlinkedLights === 1 ? '' : 's'} ${r.unlinkedLights === 1 ? 'has' : 'have'} no calibration links`;
    }
  }
  return null;
}

/** Read the persisted export mode, defaulting to `rawWithCalibrationSets` when
 *  unset or corrupt. */
function readExportModePref(): ExportMode {
  try {
    const raw = localStorage.getItem(EXPORT_MODE_KEY);
    if (raw === 'lightsOnly' || raw === 'calibratedLights' || raw === 'rawWithMasters' || raw === 'rawWithCalibrationSets') {
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
export function ExportTab({ frameSetId, frameSetName }: ExportTabProps) {
  const navigate = useNavigate();
  const [outputDir, setOutputDir] = useState<string>('');
  const [useSymlinks, setUseSymlinks] = useState(false);
  const [showFolderBrowser, setShowFolderBrowser] = useState(false);
  const [exportError, setExportError] = useState<string | null>(null);
  const [sendOpen, setSendOpen] = useState(false);

  const { summary, loading: loadingSummary, error: summaryError } = useExportSummary(frameSetId);
  const { config: wbppConfig, save: saveWbppConfig } = useWbppConfig();
  const { startExport, hasActiveExports } = useExportProgressContext();
  const exporting = hasActiveExports;

  // Export mode (spec §12.2). Persisted to localStorage for cross-session
  // memory; synced into the persisted WbppExportConfig at export time (the
  // backend reads `export_mode` from that config, not from the invoke args).
  const [exportMode, setExportMode] = useState<ExportMode>(readExportModePref);

  // Readiness for the whole set — one fetch, mode-independent: it carries every
  // mode's file count plus the two blocker tallies. The gate here is UX; the
  // backend re-checks the same rule and hard-errors, so the guarantee never
  // depends on this fetch.
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

  // A tick re-runs the cancelled-flag effect below; bumped on mount-independent
  // triggers (Coverage-tab work finishing) without duplicating the fetch.
  const [readinessTick, setReadinessTick] = useState(0);
  const loadReadiness = useCallback(() => setReadinessTick(t => t + 1), []);

  useEffect(() => {
    let cancelled = false;
    setReadinessLoading(true);
    setReadinessError(null);
    // No calibration prefs: readiness is about the INPUTS (masters built,
    // lights linked), which no dialog toggle can change.
    api
      .invoke<ExportReadiness>('get_export_readiness', { setId: frameSetId })
      .then(r => { if (!cancelled) setReadiness(r); })
      .catch(err => {
        if (cancelled) return;
        console.error('[ExportTab] get_export_readiness failed:', err);
        setReadinessError(typeof err === 'string' ? err : (err as Error)?.message ?? String(err));
        setReadiness(null);
      })
      .finally(() => { if (!cancelled) setReadinessLoading(false); });
    return () => { cancelled = true; };
  }, [frameSetId, readinessTick]);

  // A master build or a light calibration finishing elsewhere (Coverage tab)
  // changes what this tab may export — re-read rather than making the user
  // leave and come back.
  useEffect(() => {
    window.addEventListener('light-cal-updated', loadReadiness);
    window.addEventListener('library-updated', loadReadiness);
    return () => {
      window.removeEventListener('light-cal-updated', loadReadiness);
      window.removeEventListener('library-updated', loadReadiness);
    };
  }, [loadReadiness]);

  // Why the selected mode can't run, or null. `readiness === null` (loading /
  // errored) keeps both gates closed so we never fire an operation the backend
  // will hard-error on.
  const blocker = readiness ? modeBlocker(readiness, exportMode) : null;
  const modeReady = readiness !== null && blocker === null;

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

  const canExport = outputDir !== '' && !exporting && modeReady;
  // Sending needs no output folder — the payload is staged by the backend.
  const canSend = modeReady;

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
            {readinessLoading && (
              <p className="flex items-center gap-2 text-sm text-content-muted mb-3">
                <Loader2 size={14} className="animate-spin" /> Checking readiness…
              </p>
            )}
            {readinessError && (
              <p className="text-sm text-error mb-3">Failed to check readiness: {readinessError}</p>
            )}
            <div role="radiogroup" aria-label="Export mode" className="space-y-2">
              {EXPORT_MODE_OPTIONS.map(opt => {
                const active = exportMode === opt.value;
                // Display only — the backend re-checks the same rule.
                const reason = readiness ? modeBlocker(readiness, opt.value) : null;
                const disabled = readiness !== null && reason !== null;
                return (
                  <label
                    key={opt.value}
                    className={`flex items-start gap-3 p-3 rounded-lg border transition-colors ${
                      active
                        ? 'border-accent bg-accent/10'
                        : 'border-border bg-surface-hover/50 hover:bg-surface-hover'
                    } ${disabled ? 'opacity-60 cursor-not-allowed' : 'cursor-pointer'}`}
                  >
                    <input
                      type="radio"
                      name="export-mode"
                      value={opt.value}
                      checked={active}
                      disabled={disabled}
                      onChange={() => handleModeChange(opt.value)}
                      className="mt-0.5 w-4 h-4 text-accent border-border focus:ring-accent"
                    />
                    <span className="flex-1 min-w-0 flex flex-col">
                      <span className="flex items-baseline gap-2">
                        <span className="flex-1 text-sm font-medium text-content">{opt.label}</span>
                        <span className="text-xs text-content-muted tabular-nums">
                          {readiness ? `${opt.count(readiness.fileCounts)} files` : ''}
                        </span>
                      </span>
                      <span className="block text-xs text-content-muted mt-0.5">{opt.hint}</span>
                      {reason && (
                        <span className="mt-1 flex items-center gap-2 text-xs text-error">
                          <AlertTriangle size={12} /> {reason}
                          <button
                            type="button"
                            className="underline hover:no-underline text-content-secondary"
                            onClick={(e) => {
                              e.preventDefault();
                              e.stopPropagation();
                              // handleSetClick resolves the set's REAL kind from
                              // setKindMap — a flat or bias set without a master
                              // must not land on the Dark library. Deep-link only
                              // when the missing master IS the blocker: the
                              // calibrated mode can also be blocked by unlinked
                              // lights, which no set row explains.
                              const first = (readiness?.rawSetsWithoutMaster ?? 0) > 0
                                ? readiness?.rawSetIdsWithoutMaster[0]
                                : undefined;
                              if (first !== undefined) handleSetClick(first);
                              else navigate('?tab=calibration');
                            }}
                          >
                            → Coverage
                          </button>
                        </span>
                      )}
                    </span>
                  </label>
                );
              })}
            </div>
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

          {/* Actions — both run the selected mode: one writes it to a folder,
              the other ships it to another node. Same readiness gate. */}
          <div className="flex gap-3">
            <button onClick={() => { void handleExport(); }} disabled={!canExport}
              title={blocker ?? (outputDir ? 'Export to PixInsight WBPP folder structure' : 'Pick an output folder first')}
              className={`flex-1 py-3 rounded-lg font-medium flex items-center justify-center gap-2 ${canExport ? 'bg-accent hover:bg-accent-hover text-white' : 'bg-surface-hover cursor-not-allowed text-content-muted'}`}>
              {exporting ? (<><Loader2 className="animate-spin" size={20} /> Exporting…</>) : (<><Play size={20} /> Export to WBPP</>)}
            </button>
            <button onClick={() => setSendOpen(true)} disabled={!canSend}
              title={blocker ?? 'Send this frame set to another Athenaeum node'}
              className={`flex-1 py-3 rounded-lg font-medium flex items-center justify-center gap-2 border ${canSend ? 'border-accent text-accent hover:bg-accent/10' : 'border-border cursor-not-allowed text-content-muted'}`}>
              <Send size={20} /> Send to node…
            </button>
          </div>
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

      {/* Send the selected mode's files to another node. Mounted only once
          readiness resolved — the dialog's header shows that file count. */}
      {readiness && (
        <SendToNodeDialog
          target={{
            kind: 'frameSet',
            frameSetId,
            mode: exportMode,
            modeLabel: EXPORT_MODE_OPTIONS.find(o => o.value === exportMode)?.label ?? exportMode,
            fileCount: EXPORT_MODE_OPTIONS.find(o => o.value === exportMode)?.count(readiness.fileCounts) ?? 0,
          }}
          open={sendOpen}
          onClose={() => setSendOpen(false)}
          defaultBatchName={frameSetName}
        />
      )}
    </div>
  );
}
