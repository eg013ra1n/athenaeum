import { useState, useEffect } from 'react';
import { useSearchParams } from 'react-router-dom';
import { api } from '../api';
import { Save, AlertCircle, CheckCircle, RefreshCw, Settings as SettingsIcon, Crosshair, BarChart3, ScanSearch, Archive as ArchiveIcon, FolderOpen, Info, ScrollText } from 'lucide-react';
import { revealItemInDir, openPath } from '../api/desktop';
import { CalibrationMatchingConfig } from '../components/calibration';
import LoggingSettings from '../components/settings/LoggingSettings';
import { AnalysisSettingsPanel } from '../components/analysis/AnalysisSettingsPanel';
import { PlateSolveSettingsPanel } from '../components/plate-solve';
import { isTauri } from '../utils/platform';
import { getArchiveSettings, setArchiveCompression as apiSetArchiveCompression } from '../api/archive';
import type { ArchiveCompression } from '../types/archive';
import type { AnnotationSettings } from '../types/analysis-config';
import { DEFAULT_ANNOTATION_SETTINGS } from '../types/helpers';

type ThresholdUnit = 'arcsec' | 'arcmin' | 'deg';

export default function Settings() {
  // Defaults should match backend: settings/mod.rs defaults
  const [thresholdValue, setThresholdValue] = useState('3.0');
  const [thresholdUnit, setThresholdUnit] = useState<ThresholdUnit>('deg');
  const [sessionGapHours, setSessionGapHours] = useState('6.0');
  const [qualityThumbnail, setQualityThumbnail] = useState('70');
  const [qualityPreview, setQualityPreview] = useState('85');
  const [qualityFull, setQualityFull] = useState('95');
  const [blinkResolution, setBlinkResolution] = useState('preview');
  const [blinkThreads, setBlinkThreads] = useState('4');
  const [blinkThreadsMax, setBlinkThreadsMax] = useState(4);
  const [blinkCacheSize, setBlinkCacheSize] = useState('200');
  const [blinkRetentionMinutes, setBlinkRetentionMinutes] = useState('30');
  const [useContentHash, setUseContentHash] = useState(false);
  const [contentHashRescanned, setContentHashRescanned] = useState(false);
  const [checkBeta, setCheckBeta] = useState(false);
  const [autoCheck, setAutoCheck] = useState(true);
  const [annotationSettings, setAnnotationSettings] = useState<AnnotationSettings>(DEFAULT_ANNOTATION_SETTINGS);
  const [monitoringIntervalMinutes, setMonitoringIntervalMinutes] = useState('10');
  const [monitoringEnabledGlobal, setMonitoringEnabledGlobal] = useState(true);
  const [autoMergeOnButtonClick, setAutoMergeOnButtonClick] = useState(false);
  const [autoMergeOnMonitorDetect, setAutoMergeOnMonitorDetect] = useState(false);

  // Flat Contour Plot defaults — drive the per-frame contour rendering in
  // Blink. Values match PixInsight FlatContourPlot v1.3.1 defaults.
  const [flatContourResolution, setFlatContourResolution] = useState('50');
  const [flatContourSigma, setFlatContourSigma] = useState('1.0');
  const [flatContourCount, setFlatContourCount] = useState('15');
  const [flatContourGradient, setFlatContourGradient] = useState('50');

  // Archive compression preference (folder list lives in File Manager now)
  const [archiveCompression, setArchiveCompressionState] = useState<ArchiveCompression>('store');
  const [archiveSaving, setArchiveSaving] = useState(false);

  // Data file locations (desktop only — db path + log directory reported by the backend).
  const [dbPath, setDbPath] = useState<string>('');
  const [logDir, setLogDir] = useState<string>('');

  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);

  // Content hash rescan state
  const [rescanningContentHash, setRescanningContentHash] = useState(false);
  const [rescanSuccess, setRescanSuccess] = useState<{updated: number, total: number} | null>(null);

  // Tab state — initial value comes from `?tab=…` so deep-links (e.g. the
  // "Open Plate-Solve Settings" CTA on the index-missing modal) land on the
  // right tab without an extra click.
  const [searchParams, setSearchParams] = useSearchParams();
  type SettingsTab = 'general' | 'calibration' | 'analysis' | 'plate_solving';
  const tabFromUrl = (searchParams.get('tab') ?? '') as SettingsTab | '';
  const validTabs: readonly SettingsTab[] = ['general', 'calibration', 'analysis', 'plate_solving'];
  const initialTab: SettingsTab = validTabs.includes(tabFromUrl as SettingsTab)
    ? (tabFromUrl as SettingsTab)
    : 'general';
  const [activeTab, _setActiveTab] = useState<SettingsTab>(initialTab);
  const setActiveTab = (tab: SettingsTab) => {
    _setActiveTab(tab);
    // Reflect in the URL so a refresh keeps the tab and back/forward navigation
    // stays in sync. `replace` so we don't pollute the history stack on every click.
    setSearchParams(prev => {
      const next = new URLSearchParams(prev);
      next.set('tab', tab);
      return next;
    }, { replace: true });
  };

  // If the URL `?tab=` changes while Settings is already mounted (e.g. the
  // plate-solve modal navigates here from another page), follow it. Otherwise
  // the user lands on Settings but on the wrong tab.
  useEffect(() => {
    if (validTabs.includes(tabFromUrl as SettingsTab) && tabFromUrl !== activeTab) {
      _setActiveTab(tabFromUrl as SettingsTab);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tabFromUrl]);

  useEffect(() => {
    loadSettings();
    api.invoke<number>('get_blink_threads_max').then(setBlinkThreadsMax).catch(console.error);
    getArchiveSettings()
      .then((s) => {
        setArchiveCompressionState(s.compression);
      })
      .catch(console.error);
    if (isTauri) {
      api.invoke<string>('get_database_path').then(setDbPath).catch(console.error);
      api.invoke<string>('get_log_path').then(setLogDir).catch(console.error);
    }
  }, []);

  const handleArchiveCompressionChange = async (next: ArchiveCompression) => {
    try {
      setArchiveSaving(true);
      await apiSetArchiveCompression(next);
      setArchiveCompressionState(next);
    } catch (e) {
      console.error('Failed to set compression', e);
      alert(`Failed to set compression: ${e}`);
    } finally {
      setArchiveSaving(false);
    }
  };

  const loadSettings = async () => {
    try {
      setLoading(true);
      setError(null);

      const [
        value, unit, sessionGap, qThumbnail, qPreview, qFull, resolution, blinkThreadsVal, cacheSizeVal, retentionMin, contentHash, contentHashRescanned, checkBetaVal, autoCheckVal
      ] = await Promise.all([
        api.invoke<string>('get_setting', {
          key: 'grouping.threshold.value',
          defaultValue: '3.0',
        }),
        api.invoke<string>('get_setting', {
          key: 'grouping.threshold.unit',
          defaultValue: 'deg',
        }),
        api.invoke<string>('get_setting', {
          key: 'session_gap_threshold_hours',
          defaultValue: '6.0',
        }),
        api.invoke<string>('get_setting', {
          key: 'rustafits.quality.thumbnail',
          defaultValue: '70',
        }),
        api.invoke<string>('get_setting', {
          key: 'rustafits.quality.preview',
          defaultValue: '85',
        }),
        api.invoke<string>('get_setting', {
          key: 'rustafits.quality.full',
          defaultValue: '95',
        }),
        api.invoke<string>('get_setting', {
          key: 'blink.resolution',
          defaultValue: 'preview',
        }),
        api.invoke<string>('get_setting', {
          key: 'blink.threads',
          defaultValue: '4',
        }),
        api.invoke<string>('get_setting', {
          key: 'blink.memory_cache_size',
          defaultValue: '200',
        }),
        api.invoke<string>('get_setting', {
          key: 'blink.memory_retention_minutes',
          defaultValue: '30',
        }),
        api.invoke<string>('get_setting', {
          key: 'duplicates.use_content_hash',
          defaultValue: 'false',
        }),
        api.invoke<string>('get_setting', {
          key: 'duplicates.content_hash_rescanned',
          defaultValue: 'false',
        }),
        api.invoke<string>('get_setting', {
          key: 'updates.check_beta',
          defaultValue: 'false',
        }),
        api.invoke<string>('get_setting', {
          key: 'updates.auto_check',
          defaultValue: 'true',
        }),
      ]);

      // Flat-contour defaults — loaded in a separate batch to keep the
      // primary Promise.all destructuring readable. Reads inherit the same
      // get_setting fallback contract as the other settings here.
      const [fcRes, fcSig, fcCnt, fcGrad] = await Promise.all([
        api.invoke<string>('get_setting', {
          key: 'flat_contour.resolution_pct', defaultValue: '50',
        }),
        api.invoke<string>('get_setting', {
          key: 'flat_contour.sigma_px', defaultValue: '1.0',
        }),
        api.invoke<string>('get_setting', {
          key: 'flat_contour.contours', defaultValue: '15',
        }),
        api.invoke<string>('get_setting', {
          key: 'flat_contour.gradient_pct', defaultValue: '50',
        }),
      ]);
      setFlatContourResolution(fcRes);
      setFlatContourSigma(fcSig);
      setFlatContourCount(fcCnt);
      setFlatContourGradient(fcGrad);

      setThresholdValue(value);
      setThresholdUnit(unit as ThresholdUnit);
      setSessionGapHours(sessionGap);
      setQualityThumbnail(qThumbnail);
      setQualityPreview(qPreview);
      setQualityFull(qFull);
      setBlinkResolution(resolution);
      setBlinkThreads(blinkThreadsVal);
      setBlinkCacheSize(cacheSizeVal);
      setBlinkRetentionMinutes(retentionMin);
      setUseContentHash(contentHash.toLowerCase() === 'true');
      setContentHashRescanned(contentHashRescanned.toLowerCase() === 'true');
      setCheckBeta(checkBetaVal.toLowerCase() === 'true');
      setAutoCheck(autoCheckVal.toLowerCase() === 'true');

      // Load monitoring settings
      try {
        const [intervalVal, enabledVal, mergeButton, mergeMonitor] = await Promise.all([
          api.invoke<string>('get_setting', {
            key: 'monitoring.interval_minutes',
            defaultValue: '10',
          }),
          api.invoke<string>('get_setting', {
            key: 'monitoring.enabled_global',
            defaultValue: 'true',
          }),
          api.invoke<string>('get_setting', {
            key: 'auto_merge.on_button_click',
            defaultValue: 'false',
          }),
          api.invoke<string>('get_setting', {
            key: 'auto_merge.on_monitor_detect',
            defaultValue: 'false',
          }),
        ]);
        setMonitoringIntervalMinutes(intervalVal);
        setMonitoringEnabledGlobal(enabledVal.toLowerCase() === 'true');
        setAutoMergeOnButtonClick(mergeButton.toLowerCase() === 'true');
        setAutoMergeOnMonitorDetect(mergeMonitor.toLowerCase() === 'true');
      } catch {
        // ignore, use defaults
      }

      // Load annotation settings
      try {
        const annJson = await api.invoke<string>('get_setting', {
          key: 'blink.annotation_config',
          defaultValue: '',
        });
        if (annJson) {
          setAnnotationSettings({ ...DEFAULT_ANNOTATION_SETTINGS, ...JSON.parse(annJson) });
        }
      } catch {
        // ignore, use defaults
      }
    } catch (err) {
      setError(err as string);
      console.error('Failed to load settings:', err);
    } finally {
      setLoading(false);
    }
  };

  const handleSave = async () => {
    try {
      setSaving(true);
      setError(null);
      setSuccess(false);

      // Validate threshold value
      const numValue = parseFloat(thresholdValue);
      if (isNaN(numValue) || numValue <= 0) {
        setError('Threshold value must be a positive number');
        return;
      }

      // Validate session gap hours
      const sessionGapValue = parseFloat(sessionGapHours);
      if (isNaN(sessionGapValue) || sessionGapValue <= 0) {
        setError('Session gap threshold must be a positive number');
        return;
      }

      // Validate quality settings
      const qThumbnailValue = parseInt(qualityThumbnail);
      if (isNaN(qThumbnailValue) || qThumbnailValue < 1 || qThumbnailValue > 100) {
        setError('Thumbnail quality must be between 1 and 100');
        return;
      }

      const qPreviewValue = parseInt(qualityPreview);
      if (isNaN(qPreviewValue) || qPreviewValue < 1 || qPreviewValue > 100) {
        setError('Preview quality must be between 1 and 100');
        return;
      }

      const qFullValue = parseInt(qualityFull);
      if (isNaN(qFullValue) || qFullValue < 1 || qFullValue > 100) {
        setError('Full quality must be between 1 and 100');
        return;
      }

      // Save blink threads via dedicated command (rebuilds semaphore immediately)
      const blinkThreadsNum = parseInt(blinkThreads);
      if (isNaN(blinkThreadsNum) || blinkThreadsNum < 1 || blinkThreadsNum > blinkThreadsMax) {
        setError(`Concurrent threads must be between 1 and ${blinkThreadsMax}`);
        return;
      }
      await api.invoke('set_blink_threads', { threads: blinkThreadsNum });

      // Validate memory cache size
      const cacheSizeNum = parseInt(blinkCacheSize);
      if (isNaN(cacheSizeNum) || cacheSizeNum < 10 || cacheSizeNum > 5000) {
        setError('Memory cache size must be between 10 and 5000');
        return;
      }

      // Validate memory cache retention
      const retentionNum = parseInt(blinkRetentionMinutes);
      if (isNaN(retentionNum) || retentionNum < 1 || retentionNum > 1440) {
        setError('Memory cache retention must be between 1 and 1440 minutes');
        return;
      }

      await Promise.all([
        api.invoke('set_setting', {
          key: 'grouping.threshold.value',
          value: thresholdValue,
        }),
        api.invoke('set_setting', {
          key: 'grouping.threshold.unit',
          value: thresholdUnit,
        }),
        api.invoke('set_setting', {
          key: 'session_gap_threshold_hours',
          value: sessionGapHours,
        }),
        api.invoke('set_setting', {
          key: 'rustafits.quality.thumbnail',
          value: qualityThumbnail,
        }),
        api.invoke('set_setting', {
          key: 'rustafits.quality.preview',
          value: qualityPreview,
        }),
        api.invoke('set_setting', {
          key: 'rustafits.quality.full',
          value: qualityFull,
        }),
        api.invoke('set_setting', {
          key: 'blink.resolution',
          value: blinkResolution,
        }),
        api.invoke('set_setting', {
          key: 'blink.memory_cache_size',
          value: blinkCacheSize,
        }),
        api.invoke('set_setting', {
          key: 'blink.memory_retention_minutes',
          value: blinkRetentionMinutes,
        }),
        api.invoke('set_setting', {
          key: 'duplicates.use_content_hash',
          value: useContentHash ? 'true' : 'false',
        }),
        // Reset rescan flag when toggling content hash
        api.invoke('set_setting', {
          key: 'duplicates.content_hash_rescanned',
          value: useContentHash ? 'false' : 'false',
        }),
        api.invoke('set_setting', {
          key: 'updates.check_beta',
          value: checkBeta ? 'true' : 'false',
        }),
        api.invoke('set_setting', {
          key: 'updates.auto_check',
          value: autoCheck ? 'true' : 'false',
        }),
        api.invoke('set_setting', {
          key: 'blink.annotation_config',
          value: JSON.stringify(annotationSettings),
        }),
        api.invoke('set_setting', {
          key: 'monitoring.interval_minutes',
          value: monitoringIntervalMinutes,
        }),
        api.invoke('set_setting', {
          key: 'monitoring.enabled_global',
          value: monitoringEnabledGlobal ? 'true' : 'false',
        }),
        api.invoke('set_setting', {
          key: 'flat_contour.resolution_pct',
          value: flatContourResolution,
        }),
        api.invoke('set_setting', {
          key: 'flat_contour.sigma_px',
          value: flatContourSigma,
        }),
        api.invoke('set_setting', {
          key: 'flat_contour.contours',
          value: flatContourCount,
        }),
        api.invoke('set_setting', {
          key: 'flat_contour.gradient_pct',
          value: flatContourGradient,
        }),
        api.invoke('set_setting', {
          key: 'auto_merge.on_button_click',
          value: autoMergeOnButtonClick ? 'true' : 'false',
        }),
        api.invoke('set_setting', {
          key: 'auto_merge.on_monitor_detect',
          value: autoMergeOnMonitorDetect ? 'true' : 'false',
        }),
      ]);

      setSuccess(true);
      setTimeout(() => setSuccess(false), 3000);
    } catch (err) {
      setError(err as string);
      console.error('Failed to save settings:', err);
    } finally {
      setSaving(false);
    }
  };

  const getThresholdInDegrees = () => {
    const value = parseFloat(thresholdValue);
    if (isNaN(value)) return 'N/A';

    switch (thresholdUnit) {
      case 'arcsec':
        return (value / 3600).toFixed(4);
      case 'arcmin':
        return (value / 60).toFixed(4);
      case 'deg':
        return value.toFixed(4);
      default:
        return 'N/A';
    }
  };

  const handleRescanContentHash = async () => {
    try {
      setRescanningContentHash(true);
      setError(null);
      setRescanSuccess(null);

      const result = await api.invoke<{files_total: number, files_updated: number, files_skipped: number, files_missing: number, errors: string[]}>('rescan_all_for_content_hash');

      if (result.errors.length > 0) {
        setError(`Rescan completed with ${result.errors.length} errors. Check console for details.`);
        console.error('Rescan errors:', result.errors);
      }

      setRescanSuccess({ updated: result.files_updated, total: result.files_total });

      // Update the rescanned flag in state to hide warning immediately
      if (result.files_updated > 0 || result.files_skipped > 0) {
        setContentHashRescanned(true);
      }

      setTimeout(() => setRescanSuccess(null), 5000);
    } catch (err) {
      setError(err as string);
      console.error('Failed to rescan content hashes:', err);
    } finally {
      setRescanningContentHash(false);
    }
  };

  if (loading) {
    return (
      <div className="p-6">
        <div className="text-center py-12 text-content-muted">
          <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-accent mx-auto"></div>
          <p className="mt-4">Loading settings...</p>
        </div>
      </div>
    );
  }

  return (
    <div className="p-6 max-w-4xl">
      <div className="mb-6">
        <h2 className="text-3xl font-bold">Settings</h2>
        <p className="text-content-muted">Configure application settings</p>
      </div>

      {/* Tab Navigation */}
      <div className="flex gap-1 mb-6 border-b border-border">
        <button
          onClick={() => setActiveTab('general')}
          className={`flex items-center gap-2 px-4 py-2 rounded-t-lg transition-colors ${
            activeTab === 'general'
              ? 'bg-surface-elevated text-white border-b-2 border-accent'
              : 'text-content-muted hover:text-content hover:bg-surface-elevated/50'
          }`}
        >
          <SettingsIcon size={18} />
          General
        </button>
        <button
          onClick={() => setActiveTab('calibration')}
          className={`flex items-center gap-2 px-4 py-2 rounded-t-lg transition-colors ${
            activeTab === 'calibration'
              ? 'bg-surface-elevated text-white border-b-2 border-accent'
              : 'text-content-muted hover:text-content hover:bg-surface-elevated/50'
          }`}
        >
          <Crosshair size={18} />
          Calibration
        </button>
        <button
          onClick={() => setActiveTab('analysis')}
          className={`flex items-center gap-2 px-4 py-2 rounded-t-lg transition-colors ${
            activeTab === 'analysis'
              ? 'bg-surface-elevated text-white border-b-2 border-accent'
              : 'text-content-muted hover:text-content hover:bg-surface-elevated/50'
          }`}
        >
          <BarChart3 size={18} />
          Analysis
        </button>
        <button
          onClick={() => setActiveTab('plate_solving')}
          className={`flex items-center gap-2 px-4 py-2 rounded-t-lg transition-colors ${
            activeTab === 'plate_solving'
              ? 'bg-surface-elevated text-white border-b-2 border-accent'
              : 'text-content-muted hover:text-content hover:bg-surface-elevated/50'
          }`}
        >
          <ScanSearch size={18} />
          Plate Solving
        </button>
      </div>

      {/* Calibration Tab. The calibration-folder picker lives in File
          Manager → Monitored Directories (CalibrationFolderSection), next to
          the Archive Folders section. */}
      {activeTab === 'calibration' && (
        <div className="bg-surface-elevated rounded-lg p-6 mt-6">
          <h3 className="text-xl font-semibold mb-4">Calibration Matching Configuration</h3>
          <p className="text-content-muted mb-6">
            Configure how calibration frames (Flats, Darks, Bias) are matched to source frames.
            Define which parameters must match exactly, warn on threshold, or be ignored.
          </p>
          <CalibrationMatchingConfig />
        </div>
      )}

      {/* Analysis Tab */}
      {activeTab === 'analysis' && (
        <div className="bg-surface-elevated rounded-lg p-6">
          <h3 className="text-xl font-semibold mb-4">Star Analysis Configuration</h3>
          <p className="text-content-muted mb-6">
            Configure star detection parameters and quality scoring weights for the Lights Analysis tab.
            Changes here affect new analyses — existing results keep their original settings until re-analyzed.
          </p>
          <AnalysisSettingsPanel />
        </div>
      )}

      {/* Plate Solving Tab */}
      {activeTab === 'plate_solving' && (
        <div className="bg-surface-elevated rounded-lg p-6">
          <h3 className="text-xl font-semibold mb-4">Plate Solving Configuration</h3>
          <p className="text-content-muted mb-6">
            Configure the astrometric plate solver used to determine sky coordinates for frames
            that are missing RA/Dec metadata. The solver matches detected stars against the
            downloadable Gaia DR3 density-tier catalog to compute a full WCS solution.
          </p>
          <PlateSolveSettingsPanel />
        </div>
      )}

      {/* General Tab */}
      {activeTab === 'general' && (
        <>
          {error && (
            <div className="mb-4 p-4 bg-error-muted border border-error/50 rounded-lg flex items-start gap-3">
              <AlertCircle className="text-error flex-shrink-0 mt-0.5" size={20} />
              <div className="flex-1">
                <p className="font-medium text-error">Error</p>
                <p className="text-sm text-error/80">{String(error)}</p>
              </div>
            </div>
          )}

          {success && (
            <div className="mb-4 p-4 bg-success-muted border border-success/50 rounded-lg flex items-start gap-3">
              <CheckCircle className="text-success flex-shrink-0 mt-0.5" size={20} />
              <div className="flex-1">
                <p className="font-medium text-success">Settings saved successfully</p>
              </div>
            </div>
          )}

          <div className="bg-surface-elevated rounded-lg p-6 space-y-6">

        {/* Updates section - desktop only */}
        {isTauri && (
        <div>
          <h3 className="text-lg font-semibold mb-4">Updates</h3>
          <div className="space-y-4">
            <label className="flex items-center gap-3 cursor-pointer">
              <input
                type="checkbox"
                checked={autoCheck}
                onChange={(e) => setAutoCheck(e.target.checked)}
                className="w-5 h-5 rounded border-border bg-surface-hover text-accent focus:ring-2 focus:ring-accent focus:ring-offset-0"
              />
              <div>
                <span className="block text-sm font-medium text-content-secondary">
                  Automatically check for updates on startup
                </span>
                <span className="block text-xs text-content-muted mt-1">
                  When enabled, Athenaeum checks for a newer version each time it starts and shows a notification if one is available. Disable to only check manually via the button on the About page.
                </span>
              </div>
            </label>
            <label className="flex items-center gap-3 cursor-pointer">
              <input
                type="checkbox"
                checked={checkBeta}
                onChange={(e) => setCheckBeta(e.target.checked)}
                className="w-5 h-5 rounded border-border bg-surface-hover text-accent focus:ring-2 focus:ring-accent focus:ring-offset-0"
              />
              <div>
                <span className="block text-sm font-medium text-content-secondary">
                  Check for beta updates
                </span>
                <span className="block text-xs text-content-muted mt-1">
                  When enabled, the update checker will also look for pre-release (beta) versions. Beta builds may contain new features that are still being tested.
                </span>
              </div>
            </label>
          </div>
        </div>
        )}

        <div>
          <h3 className="text-lg font-semibold mb-4">Clustering Parameters</h3>

          <div className="space-y-4">
            {/* Threshold Value and Unit */}
            <div>
              <label className="block text-sm font-medium text-content-secondary mb-2">
                Grouping Threshold
              </label>
              <div className="flex gap-3">
                <input
                  type="number"
                  value={thresholdValue}
                  onChange={(e) => setThresholdValue(e.target.value)}
                  step="0.1"
                  min="0"
                  className="flex-1 bg-surface-hover border border-border rounded-lg px-4 py-2 text-content focus:outline-none focus:border-accent"
                />
                <select
                  value={thresholdUnit}
                  onChange={(e) => setThresholdUnit(e.target.value as ThresholdUnit)}
                  className="bg-surface-hover border border-border rounded-lg px-4 py-2 text-content focus:outline-none focus:border-accent"
                >
                  <option value="arcsec">arcseconds</option>
                  <option value="arcmin">arcminutes</option>
                  <option value="deg">degrees</option>
                </select>
              </div>
              <p className="text-xs text-content-muted mt-2">
                Frames within this angular distance will be grouped together.
                Current value: {getThresholdInDegrees()}° (decimal degrees)
              </p>
            </div>
          </div>
        </div>

        <div>
          <h3 className="text-lg font-semibold mb-4">Session Detection</h3>

          <div className="space-y-4">
            {/* Session Gap Threshold */}
            <div>
              <label className="block text-sm font-medium text-content-secondary mb-2">
                Session Gap Threshold (hours)
              </label>
              <input
                type="number"
                value={sessionGapHours}
                onChange={(e) => setSessionGapHours(e.target.value)}
                step="0.5"
                min="0"
                className="w-full bg-surface-hover border border-border rounded-lg px-4 py-2 text-content focus:outline-none focus:border-accent"
              />
              <p className="text-xs text-content-muted mt-2">
                Time gap to detect imaging night boundaries. If more than this many hours pass
                between frames, they will be grouped into separate imaging nights. Typical night
                sessions can span midnight (e.g., 19:00 Day 1 → 03:00 Day 2 = one night). Default
                is 6 hours.
              </p>
            </div>
          </div>
        </div>

        <div>
          <h3 className="text-lg font-semibold mb-4">Flat Contour Plot</h3>
          <p className="text-xs text-content-muted mb-4">
            Defaults for the per-flat contour plot rendered in Blink (toolbar
            mountain icon). Values match PixInsight's <span className="font-mono">FlatContourPlot</span> v1.3.1
            so the visual matches that script at default settings.
          </p>
          <div className="grid grid-cols-2 gap-4">
            <div>
              <label className="block text-sm font-medium text-content-secondary mb-2">
                Resolution (%)
              </label>
              <input
                type="number"
                value={flatContourResolution}
                onChange={(e) => setFlatContourResolution(e.target.value)}
                step="1"
                min="5"
                max="100"
                className="w-full bg-surface-hover border border-border rounded-lg px-4 py-2 text-content focus:outline-none focus:border-accent"
              />
              <p className="text-xs text-content-muted mt-1">
                Resampling factor. Lower = faster, less detail. PI default: 50.
              </p>
            </div>
            <div>
              <label className="block text-sm font-medium text-content-secondary mb-2">
                Sigma (px)
              </label>
              <input
                type="number"
                value={flatContourSigma}
                onChange={(e) => setFlatContourSigma(e.target.value)}
                step="0.1"
                min="0"
                max="10"
                className="w-full bg-surface-hover border border-border rounded-lg px-4 py-2 text-content focus:outline-none focus:border-accent"
              />
              <p className="text-xs text-content-muted mt-1">
                Gaussian noise-reduction sigma. PI default: 1.0.
              </p>
            </div>
            <div>
              <label className="block text-sm font-medium text-content-secondary mb-2">
                Contours
              </label>
              <input
                type="number"
                value={flatContourCount}
                onChange={(e) => setFlatContourCount(e.target.value)}
                step="1"
                min="4"
                max="20"
                className="w-full bg-surface-hover border border-border rounded-lg px-4 py-2 text-content focus:outline-none focus:border-accent"
              />
              <p className="text-xs text-content-muted mt-1">
                Number of discrete bands. PI default: 15.
              </p>
            </div>
            <div>
              <label className="block text-sm font-medium text-content-secondary mb-2">
                Gradient (%)
              </label>
              <input
                type="number"
                value={flatContourGradient}
                onChange={(e) => setFlatContourGradient(e.target.value)}
                step="1"
                min="0"
                max="200"
                className="w-full bg-surface-hover border border-border rounded-lg px-4 py-2 text-content focus:outline-none focus:border-accent"
              />
              <p className="text-xs text-content-muted mt-1">
                Boundary-emphasis strength. Higher darkens band edges more. PI default: 50.
              </p>
            </div>
          </div>
        </div>

        <div>
          <h3 className="text-lg font-semibold mb-4">Monitoring</h3>

          <div className="space-y-4">
            {/* Global enable switch */}
            <div>
              <label className="flex items-center gap-2 cursor-pointer">
                <input
                  type="checkbox"
                  checked={monitoringEnabledGlobal}
                  onChange={(e) => setMonitoringEnabledGlobal(e.target.checked)}
                  className="w-4 h-4 rounded border-border text-accent focus:ring-2 focus:ring-accent focus:ring-offset-0 bg-surface-hover"
                />
                <span className="text-sm font-medium text-content-secondary">
                  Enable background monitoring
                </span>
              </label>
              <p className="text-xs text-content-muted mt-2">
                Master switch. When off, no scan roots are polled even if individually marked
                as "Monitor". New files are still picked up on manual scan.
              </p>
            </div>

            {/* Polling interval */}
            <div>
              <label className="block text-sm font-medium text-content-secondary mb-2">
                Polling interval (minutes)
              </label>
              <input
                type="number"
                min="1"
                max="1440"
                step="1"
                value={monitoringIntervalMinutes}
                onChange={(e) => setMonitoringIntervalMinutes(e.target.value)}
                disabled={!monitoringEnabledGlobal}
                className="w-full bg-surface-hover border border-border rounded-lg px-4 py-2 text-content focus:outline-none focus:border-accent disabled:opacity-50"
              />
              <p className="text-xs text-content-muted mt-2">
                How often to re-scan each monitor-enabled folder for new files. The scanner
                is idempotent, so short intervals are fine on local drives but may be costly
                for large NAS directories. Default is 10 minutes.
              </p>
            </div>
          </div>
        </div>

        <div>
          <h3 className="text-lg font-semibold mb-4">Auto-merge</h3>
          <p className="text-xs text-content-muted mb-3">
            When enabled, new unclustered light frames that fall within the grouping
            threshold of an existing frame set are automatically attached to that set.
            Every merge is recorded in the frame set's History tab so you can audit what
            the algorithm did. Both settings default to off.
          </p>
          <div className="space-y-3">
            <label className="flex items-start gap-2 cursor-pointer">
              <input
                type="checkbox"
                checked={autoMergeOnButtonClick}
                onChange={(e) => setAutoMergeOnButtonClick(e.target.checked)}
                className="mt-0.5 w-4 h-4 rounded border-border text-accent focus:ring-2 focus:ring-accent focus:ring-offset-0 bg-surface-hover"
              />
              <div>
                <span className="text-sm font-medium text-content-secondary">
                  Skip confirmation on "Find new images"
                </span>
                <p className="text-xs text-content-muted mt-0.5">
                  When on, clicking the button merges all candidates immediately without
                  showing a preview dialog.
                </p>
              </div>
            </label>
            <label className="flex items-start gap-2 cursor-pointer">
              <input
                type="checkbox"
                checked={autoMergeOnMonitorDetect}
                onChange={(e) => setAutoMergeOnMonitorDetect(e.target.checked)}
                className="mt-0.5 w-4 h-4 rounded border-border text-accent focus:ring-2 focus:ring-accent focus:ring-offset-0 bg-surface-hover"
              />
              <div>
                <span className="text-sm font-medium text-content-secondary">
                  Auto-attach during background monitoring
                </span>
                <p className="text-xs text-content-muted mt-0.5">
                  When on, background scans that discover new lights automatically attach
                  them to the nearest matching frame set (within the grouping threshold)
                  without user intervention. You'll see a toast + notification bell entry
                  for each auto-merge.
                </p>
              </div>
            </label>
          </div>
        </div>

        <div>
          <h3 className="text-lg font-semibold mb-4">Blink Viewer</h3>

          <div className="space-y-4">
            {/* Resolution */}
            <div>
              <label className="block text-sm font-medium text-content-secondary mb-2">
                Image Resolution
              </label>
              <select
                value={blinkResolution}
                onChange={(e) => setBlinkResolution(e.target.value)}
                className="w-full bg-surface-hover border border-border rounded-lg px-4 py-2 text-content focus:outline-none focus:border-accent"
              >
                <option value="thumbnail">Thumbnail (4x downscale)</option>
                <option value="preview">Preview (2x2 binning)</option>
                <option value="full">Full Resolution</option>
              </select>
              <p className="text-xs text-content-muted mt-2">
                Resolution for blink viewer images. Thumbnail is fastest, Preview balances speed and quality, Full shows maximum detail. Note: Changing this will cache images separately for each resolution.
              </p>
            </div>

            {/* JPEG Quality - shows only the slider matching the selected resolution */}
            {blinkResolution === 'thumbnail' && (
            <div>
              <label className="block text-sm font-medium text-content-secondary mb-2">
                Thumbnail JPEG Quality ({qualityThumbnail})
              </label>
              <input
                type="range"
                value={qualityThumbnail}
                onChange={(e) => setQualityThumbnail(e.target.value)}
                min="1"
                max="100"
                step="1"
                className="w-full"
              />
              <div className="flex justify-between text-xs text-content-muted mt-1">
                <span>1 (Smallest)</span>
                <span>100 (Highest Quality)</span>
              </div>
              <p className="text-xs text-content-muted mt-2">
                JPEG quality for thumbnail images. Default: 70. Lower values = smaller files, faster loading.
              </p>
            </div>
            )}

            {blinkResolution === 'preview' && (
            <div>
              <label className="block text-sm font-medium text-content-secondary mb-2">
                Preview JPEG Quality ({qualityPreview})
              </label>
              <input
                type="range"
                value={qualityPreview}
                onChange={(e) => setQualityPreview(e.target.value)}
                min="1"
                max="100"
                step="1"
                className="w-full"
              />
              <div className="flex justify-between text-xs text-content-muted mt-1">
                <span>1 (Smallest)</span>
                <span>100 (Highest Quality)</span>
              </div>
              <p className="text-xs text-content-muted mt-2">
                JPEG quality for preview/blink viewer images. Default: 85. Good balance of quality and file size.
              </p>
            </div>
            )}

            {blinkResolution === 'full' && (
            <div>
              <label className="block text-sm font-medium text-content-secondary mb-2">
                Full Resolution JPEG Quality ({qualityFull})
              </label>
              <input
                type="range"
                value={qualityFull}
                onChange={(e) => setQualityFull(e.target.value)}
                min="1"
                max="100"
                step="1"
                className="w-full"
              />
              <div className="flex justify-between text-xs text-content-muted mt-1">
                <span>1 (Smallest)</span>
                <span>100 (Highest Quality)</span>
              </div>
              <p className="text-xs text-content-muted mt-2">
                JPEG quality for full resolution images. Default: 95. Highest quality for detailed viewing.
              </p>
            </div>
            )}

            {/* Concurrent Threads */}
            <div>
              <label className="block text-sm font-medium text-content-secondary mb-2">
                Concurrent Processing Threads ({blinkThreads})
              </label>
              <input
                type="number"
                value={blinkThreads}
                onChange={(e) => setBlinkThreads(e.target.value)}
                min="1"
                max={blinkThreadsMax}
                step="1"
                className="w-full bg-surface-hover border border-border rounded-lg px-4 py-2 text-content focus:outline-none focus:border-accent"
              />
              <p className="text-xs text-content-muted mt-2">
                Number of concurrent image processing threads (1–{blinkThreadsMax}). Lower values use less memory, higher values process faster.
              </p>
            </div>

            {/* Memory Cache Size */}
            <div>
              <label className="block text-sm font-medium text-content-secondary mb-2">
                Memory Cache Size (images)
              </label>
              <input
                type="number"
                value={blinkCacheSize}
                onChange={(e) => setBlinkCacheSize(e.target.value)}
                min="10"
                max="5000"
                step="10"
                className="w-full bg-surface-hover border border-border rounded-lg px-4 py-2 text-content focus:outline-none focus:border-accent"
              />
              <p className="text-xs text-content-muted mt-2">
                Maximum number of images kept in the memory cache. Default: 200. For large frame sets, increase this to avoid cache thrashing. Each image uses ~1-2 MB of RAM.
              </p>
            </div>

            {/* Memory Cache Retention */}
            <div>
              <label className="block text-sm font-medium text-content-secondary mb-2">
                Memory Cache Retention (minutes)
              </label>
              <input
                type="number"
                value={blinkRetentionMinutes}
                onChange={(e) => setBlinkRetentionMinutes(e.target.value)}
                min="1"
                max="1440"
                step="1"
                className="w-full bg-surface-hover border border-border rounded-lg px-4 py-2 text-content focus:outline-none focus:border-accent"
              />
              <p className="text-xs text-content-muted mt-2">
                Cached images are automatically evicted after this many minutes of inactivity. Default: 30. Max: 1440 (24 hours). Accessing an image resets its timer.
              </p>
            </div>

          </div>
        </div>

        <div>
          <h3 className="text-lg font-semibold mb-4">Star Annotation Display</h3>
          <p className="text-xs text-content-muted mb-4">
            Configure how star annotations appear when toggled on in the Blink Viewer.
          </p>
          <div className="space-y-4">
            <div className="grid grid-cols-2 gap-4">
              <div>
                <label className="block text-xs text-content-secondary mb-1">Color Scheme</label>
                <select
                  value={annotationSettings.color_scheme}
                  onChange={e => setAnnotationSettings(prev => ({ ...prev, color_scheme: e.target.value }))}
                  className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
                >
                  <option value="eccentricity">Eccentricity</option>
                  <option value="fwhm">FWHM</option>
                  <option value="uniform">Uniform (green)</option>
                </select>
              </div>
              <div>
                <label className="block text-xs text-content-secondary mb-1">Line Width</label>
                <select
                  value={annotationSettings.line_width}
                  onChange={e => setAnnotationSettings(prev => ({ ...prev, line_width: parseInt(e.target.value) }))}
                  className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
                >
                  <option value="1">1 (thin)</option>
                  <option value="2">2 (medium)</option>
                  <option value="3">3 (thick)</option>
                </select>
              </div>
            </div>
            <label className="flex items-center gap-3 cursor-pointer">
              <input
                type="checkbox"
                checked={annotationSettings.show_direction_tick}
                onChange={e => setAnnotationSettings(prev => ({ ...prev, show_direction_tick: e.target.checked }))}
                className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent"
              />
              <span className="text-sm text-content-secondary">Show direction tick on elongated stars</span>
            </label>
            <div className="grid grid-cols-2 gap-4">
              <div>
                <label className="block text-xs text-content-secondary mb-1">Eccentricity Good (&lt;)</label>
                <input
                  type="number"
                  value={annotationSettings.ecc_good}
                  onChange={e => setAnnotationSettings(prev => ({ ...prev, ecc_good: parseFloat(e.target.value) || 0.5 }))}
                  min="0" max="1" step="0.05"
                  className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
                />
              </div>
              <div>
                <label className="block text-xs text-content-secondary mb-1">Eccentricity Warn (&gt;)</label>
                <input
                  type="number"
                  value={annotationSettings.ecc_warn}
                  onChange={e => setAnnotationSettings(prev => ({ ...prev, ecc_warn: parseFloat(e.target.value) || 0.6 }))}
                  min="0" max="1" step="0.05"
                  className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
                />
              </div>
              <div>
                <label className="block text-xs text-content-secondary mb-1">FWHM Good (ratio &lt;)</label>
                <input
                  type="number"
                  value={annotationSettings.fwhm_good}
                  onChange={e => setAnnotationSettings(prev => ({ ...prev, fwhm_good: parseFloat(e.target.value) || 1.3 }))}
                  min="0.5" max="5" step="0.1"
                  className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
                />
              </div>
              <div>
                <label className="block text-xs text-content-secondary mb-1">FWHM Warn (ratio &gt;)</label>
                <input
                  type="number"
                  value={annotationSettings.fwhm_warn}
                  onChange={e => setAnnotationSettings(prev => ({ ...prev, fwhm_warn: parseFloat(e.target.value) || 2.0 }))}
                  min="0.5" max="10" step="0.1"
                  className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
                />
              </div>
              <div>
                <label className="block text-xs text-content-secondary mb-1">Ellipse Scale (&times;FWHM)</label>
                <input
                  type="number"
                  value={annotationSettings.ellipse_scale}
                  onChange={e => setAnnotationSettings(prev => ({ ...prev, ellipse_scale: parseFloat(e.target.value) || 1.2 }))}
                  min="0.5" max="4" step="0.1"
                  className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
                />
              </div>
              <div>
                <label className="block text-xs text-content-secondary mb-1">Min Radius (px)</label>
                <input
                  type="number"
                  value={annotationSettings.min_radius}
                  onChange={e => setAnnotationSettings(prev => ({ ...prev, min_radius: parseFloat(e.target.value) || 6 }))}
                  min="1" max="30" step="1"
                  className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
                />
              </div>
              <div>
                <label className="block text-xs text-content-secondary mb-1">Max Radius (px)</label>
                <input
                  type="number"
                  value={annotationSettings.max_radius}
                  onChange={e => setAnnotationSettings(prev => ({ ...prev, max_radius: parseFloat(e.target.value) || 60 }))}
                  min="10" max="200" step="5"
                  className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
                />
              </div>
            </div>
          </div>
        </div>

        <div>
          <h3 className="text-lg font-semibold mb-4">Duplicate Detection</h3>

          <div className="space-y-4">
            {/* Content Hash Toggle */}
            <div>
              <label className="flex items-center gap-3 cursor-pointer">
                <input
                  type="checkbox"
                  checked={useContentHash}
                  onChange={(e) => setUseContentHash(e.target.checked)}
                  className="w-5 h-5 rounded border-border bg-surface-hover text-accent focus:ring-2 focus:ring-accent focus:ring-offset-0"
                />
                <div>
                  <span className="block text-sm font-medium text-content-secondary">
                    Use File Content Hash (XXHash)
                  </span>
                  <span className="block text-xs text-content-muted mt-1">
                    When enabled, duplicate detection will use XXHash sampling (1.5MB per file) instead of metadata-based hashing. More accurate for finding true duplicates.
                    <br />
                    <span className="text-warning font-medium">⚠️ Not recommended for NAS or slow network storage</span> - hash computation requires reading file data which may be slow over network.
                  </span>
                </div>
              </label>
            </div>

            {/* Rescan Warning */}
            {useContentHash && !contentHashRescanned && (
              <div className="p-4 bg-warning-muted border border-warning/50 rounded-lg">
                <p className="text-sm text-warning/80">
                  ⚠️ After enabling content hash, you must rescan all files to compute their hashes. New files scanned after enabling this will automatically have their content hashes computed.
                </p>
                <button
                  onClick={handleRescanContentHash}
                  disabled={rescanningContentHash}
                  className="mt-3 flex items-center gap-2 px-4 py-2 bg-warning hover:brightness-90 disabled:bg-surface-hover disabled:cursor-not-allowed text-white rounded-lg transition-colors"
                >
                  <RefreshCw size={18} className={rescanningContentHash ? 'animate-spin' : ''} />
                  {rescanningContentHash ? 'Computing Hashes...' : 'Rescan All Files for Content Hash'}
                </button>
                {rescanSuccess !== null && (
                  <div className="mt-2 flex items-center gap-2 text-success">
                    <CheckCircle size={18} />
                    <span>Updated {rescanSuccess.updated} of {rescanSuccess.total} files</span>
                  </div>
                )}
              </div>
            )}
          </div>
        </div>

        <div className="pt-4 border-t border-border">
          <div className="flex items-center gap-4">
            <button
              onClick={handleSave}
              disabled={saving}
              className="flex items-center gap-2 px-6 py-2 bg-accent hover:bg-accent-hover disabled:bg-surface-hover disabled:cursor-not-allowed text-surface rounded-lg transition-colors"
            >
              <Save size={18} />
              {saving ? 'Saving...' : 'Save Settings'}
            </button>

          </div>
        </div>
      </div>

      {/* Archive Section — folder list moved to File Manager → Archive Folders.
          Only compression (a global preference) lives here now. */}
      <div className="mt-6 bg-surface-elevated rounded-lg p-6">
        <h3 className="text-lg font-semibold mb-4 flex items-center gap-2">
          <ArchiveIcon size={20} />
          Archive
        </h3>
        <p className="text-xs text-content-muted mb-4">
          Manage destination folders for archives in <span className="text-content">File Manager → Archive Folders</span>.
        </p>
        <div>
          <label className="block text-sm font-medium text-content-secondary mb-2">
            Compression
          </label>
          <select
            value={archiveCompression}
            onChange={(e) => handleArchiveCompressionChange(e.target.value as ArchiveCompression)}
            disabled={archiveSaving}
            className="px-3 py-2 bg-surface-hover border border-border rounded text-sm"
          >
            <option value="store">Store (no compression — fastest, archive size ≈ source size)</option>
            <option value="deflate">Deflate (smaller, slower — marginal savings on raw FITS)</option>
          </select>
          <p className="text-xs text-content-muted mt-2">
            FITS files compress poorly; Store is the recommended default.
          </p>
        </div>
      </div>

      {/* Data file locations (desktop only) */}
      {isTauri && (
        <div className="mt-6 bg-surface-elevated rounded-lg p-6">
          <h3 className="text-lg font-semibold mb-4 flex items-center gap-2">
            <Info size={20} />
            Data file locations
          </h3>
          <p className="text-sm text-content-muted mb-4">
            Where Athenaeum stores its catalog database and log files on disk. Click the
            folder icon to reveal the database in your file manager, or to open the log
            folder directly.
          </p>
          <div className="bg-surface-secondary rounded p-4 text-sm font-mono space-y-3">
            <div className="flex items-center gap-3">
              <span className="text-content-muted min-w-[80px]">Database:</span>
              <span className="text-content truncate flex-1">{dbPath || '—'}</span>
              {dbPath && (
                <button
                  onClick={() => revealItemInDir(dbPath)}
                  className="text-content-muted hover:text-content transition flex-shrink-0"
                  title="Reveal in file manager"
                >
                  <FolderOpen size={16} />
                </button>
              )}
            </div>
            <div className="flex items-center gap-3">
              <span className="text-content-muted min-w-[80px]">Log folder:</span>
              <span className="text-content truncate flex-1">{logDir || '—'}</span>
              {logDir && (
                <button
                  onClick={() => openPath(logDir)}
                  className="text-content-muted hover:text-content transition flex-shrink-0"
                  title="Open log folder"
                >
                  <FolderOpen size={16} />
                </button>
              )}
            </div>
          </div>
        </div>
      )}

      {/* Logging — base level + per-module overrides, live-applied by the backend. */}
      <div className="mt-6 bg-surface-elevated rounded-lg p-6">
        <h3 className="text-lg font-semibold mb-4 flex items-center gap-2">
          <ScrollText size={20} />
          Logging
        </h3>
        <p className="text-xs text-content-muted mb-4">
          Controls what gets written to the JSONL log file{isTauri ? ' shown above' : ''}. Debug is
          verbose — useful while diagnosing an issue, not recommended to leave on permanently.
        </p>
        <LoggingSettings />
      </div>

      <div className="mt-6 bg-surface-elevated rounded-lg p-6">
        <h3 className="text-lg font-semibold mb-3">About Frame Set Grouping</h3>
        <div className="text-sm text-content-muted space-y-2">
          <p>
            Frame sets are automatically created by clustering LIGHT frames based on their sky
            coordinates (RA/Dec).
          </p>
          <p>
            The algorithm uses a <strong>seed-and-grow</strong> approach with deterministic
            sorting (RA → Dec → DATE-OBS) to ensure stable results.
          </p>
          <p>
            Only LIGHT frames with valid coordinates are included. Frames missing RA/Dec or
            OBJCTRA/OBJCTDEC are excluded with detailed reasons.
          </p>
        </div>
      </div>
        </>
      )}
    </div>
  );
}
