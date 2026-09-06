import { useState, useEffect } from 'react';
import { useSearchParams } from 'react-router-dom';
import { api } from '../api';
import { Save, AlertCircle, CheckCircle, RefreshCw, Settings as SettingsIcon, Crosshair, BarChart3, ScanSearch, Archive as ArchiveIcon, FolderOpen, Info, ScrollText, UserCircle, ArrowLeftRight } from 'lucide-react';
import { revealItemInDir, openPath } from '../api/desktop';
import { CalibrationMatchingConfig } from '../components/calibration';
import LoggingSettings from '../components/settings/LoggingSettings';
import AccountSection from '../components/settings/AccountSection';
import SyncSection from '../components/settings/SyncSection';
import TransfersSection from '../components/settings/TransfersSection';
import { AnalysisSettingsPanel } from '../components/analysis/AnalysisSettingsPanel';
import { PlateSolveSettingsPanel } from '../components/plate-solve';
import { isTauri } from '../utils/platform';
import { useContentIndex } from '../hooks/useContentIndex';
import { getArchiveSettings, setArchiveCompression as apiSetArchiveCompression } from '../api/archive';
import type { ArchiveCompression } from '../types/archive';
import type { AnnotationSettings } from '../types/analysis-config';
import { DEFAULT_ANNOTATION_SETTINGS } from '../types/helpers';
import type { IntegrationBudgetInfo } from '../types/models';

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
  const [blinkCacheMaxMb, setBlinkCacheMaxMb] = useState('512');
  const [blinkRetentionMinutes, setBlinkRetentionMinutes] = useState('30');
  const [useContentHash, setUseContentHash] = useState(false);
  const [checkBeta, setCheckBeta] = useState(false);
  const [autoCheck, setAutoCheck] = useState(true);
  const [annotationSettings, setAnnotationSettings] = useState<AnnotationSettings>(DEFAULT_ANNOTATION_SETTINGS);
  const [monitoringIntervalMinutes, setMonitoringIntervalMinutes] = useState('10');
  const [monitoringEnabledGlobal, setMonitoringEnabledGlobal] = useState(true);
  const [autoMergeOnButtonClick, setAutoMergeOnButtonClick] = useState(false);
  const [autoMergeOnMonitorDetect, setAutoMergeOnMonitorDetect] = useState(false);

  // Master-build (banded integration) memory budget — Calibration tab.
  // `integrationBudgetMb` is the editable input (string, "0" = auto);
  // `budgetInfo` is the last snapshot loaded from/after saving to the
  // backend, used to show the resolved effective/auto/RAM figures.
  const [integrationBudgetMb, setIntegrationBudgetMb] = useState('0');
  const [budgetInfo, setBudgetInfo] = useState<IntegrationBudgetInfo | null>(null);
  const [budgetSaving, setBudgetSaving] = useState(false);
  const [budgetError, setBudgetError] = useState<string | null>(null);

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

  // Content index — status + manual start for the Content Index card below.
  const contentIndex = useContentIndex();

  // Tab state — initial value comes from `?tab=…` so deep-links (e.g. the
  // "Open Plate-Solve Settings" CTA on the index-missing modal) land on the
  // right tab without an extra click.
  const [searchParams, setSearchParams] = useSearchParams();
  type SettingsTab = 'general' | 'transfers' | 'calibration' | 'analysis' | 'plate_solving';
  const tabFromUrl = (searchParams.get('tab') ?? '') as SettingsTab | '';
  const validTabs: readonly SettingsTab[] = ['general', 'transfers', 'calibration', 'analysis', 'plate_solving'];
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
    loadIntegrationBudget();
  }, []);

  const loadIntegrationBudget = async () => {
    try {
      const info = await api.invoke<IntegrationBudgetInfo>('get_integration_band_budget');
      setBudgetInfo(info);
      setIntegrationBudgetMb(String(info.configuredMb));
    } catch (err) {
      console.error('Failed to load integration memory budget:', err);
    }
  };

  const handleSaveIntegrationBudget = async () => {
    const raw = integrationBudgetMb.trim();
    const mb = Number(raw);
    if (raw === '' || !Number.isFinite(mb) || !Number.isInteger(mb) || mb < 0) {
      setBudgetError('Enter 0 for automatic, or a whole number of megabytes.');
      return;
    }
    setBudgetError(null);
    setBudgetSaving(true);
    try {
      await api.invoke('set_integration_band_budget', { mb });
      await loadIntegrationBudget();
    } catch (err) {
      console.error('Failed to save integration memory budget:', err);
      setBudgetError(err as string);
    } finally {
      setBudgetSaving(false);
    }
  };

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
        value, unit, sessionGap, qThumbnail, qPreview, qFull, resolution, blinkThreadsVal, cacheSizeVal, cacheMaxMbVal, retentionMin, contentHash, checkBetaVal, autoCheckVal
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
          key: 'blink.memory_cache_max_mb',
          defaultValue: '512',
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
      setBlinkCacheMaxMb(cacheMaxMbVal);
      setBlinkRetentionMinutes(retentionMin);
      setUseContentHash(contentHash.toLowerCase() === 'true');
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

      // Validate memory cache byte budget
      const cacheMaxMbNum = parseInt(blinkCacheMaxMb);
      if (isNaN(cacheMaxMbNum) || cacheMaxMbNum < 64 || cacheMaxMbNum > 16384) {
        setError('Memory cache limit must be between 64 and 16384 MB');
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
          key: 'blink.memory_cache_max_mb',
          value: blinkCacheMaxMb,
        }),
        api.invoke('set_setting', {
          key: 'blink.memory_retention_minutes',
          value: blinkRetentionMinutes,
        }),
        api.invoke('set_setting', {
          key: 'duplicates.use_content_hash',
          value: useContentHash ? 'true' : 'false',
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

  // Explains a gap between what the operator typed for the master-build
  // memory budget and what is actually applied — either the 256-16384 MB
  // configured range clamped it, more than one heavy job is admitted at
  // once and splits it, or both. `null` means the applied value matches
  // what was configured (or auto matches what auto alone would give).
  let budgetNote: string | null = null;
  if (budgetInfo && budgetInfo.configuredMb === 0) {
    if (budgetInfo.effectiveMb < budgetInfo.autoMb) {
      budgetNote = `Applying ${budgetInfo.effectiveMb} MB, below the automatic ${budgetInfo.autoMb} MB, because more than one heavy job (master build, analysis, …) may run at once and this budget is split between them.`;
    }
  } else if (budgetInfo && budgetInfo.effectiveMb !== budgetInfo.configuredMb) {
    const clamped = Math.min(16384, Math.max(256, budgetInfo.configuredMb));
    const wasClamped = clamped !== budgetInfo.configuredMb;
    const alsoSplitByConcurrency = budgetInfo.effectiveMb < clamped;
    if (wasClamped && alsoSplitByConcurrency) {
      budgetNote = `Applying ${budgetInfo.effectiveMb} MB: the value was clamped to ${clamped} MB (allowed range 256-16384 MB) and then split further because more than one heavy job may run at once.`;
    } else if (wasClamped) {
      budgetNote = `Clamped to ${clamped} MB — configured values are limited to the 256-16384 MB range.`;
    } else if (alsoSplitByConcurrency) {
      budgetNote = `Applying ${budgetInfo.effectiveMb} MB, not the ${budgetInfo.configuredMb} MB entered, because more than one heavy job may run at once and this budget is split between them.`;
    }
  }

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
          onClick={() => setActiveTab('transfers')}
          className={`flex items-center gap-2 px-4 py-2 rounded-t-lg transition-colors ${
            activeTab === 'transfers'
              ? 'bg-surface-elevated text-white border-b-2 border-accent'
              : 'text-content-muted hover:text-content hover:bg-surface-elevated/50'
          }`}
        >
          <ArrowLeftRight size={18} />
          Transfers
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

      {/* Transfers Tab — the two configurable transfer folders plus the
          bandwidth / receiving / storage knobs (moved out of General → Sync). */}
      {activeTab === 'transfers' && (
        <div className="mb-6 bg-surface-elevated rounded-lg p-6">
          <h3 className="text-lg font-semibold mb-4 flex items-center gap-2">
            <ArrowLeftRight size={20} />
            Transfers
          </h3>
          <p className="text-xs text-content-muted mb-4">
            Where transfers keep their working data, how fast they may upload, how many may arrive at once.
          </p>
          <TransfersSection />
        </div>
      )}

      {/* Calibration Tab. The calibration-folder picker lives in File
          Manager → Folders, on the Calibration Library rail entry. */}
      {activeTab === 'calibration' && (
        <div className="bg-surface-elevated rounded-lg p-6 mt-6">
          <h3 className="text-xl font-semibold mb-4">Calibration Matching Configuration</h3>
          <p className="text-content-muted mb-6">
            Configure how calibration frames (Flats, Darks, Bias) are matched to source frames.
            Define which parameters must match exactly, warn on threshold, or be ignored.
          </p>
          <CalibrationMatchingConfig />

          <div className="mt-6 pt-6 border-t border-border">
            <h3 className="text-xl font-semibold mb-4">Master Build Memory</h3>
            <div>
              <label className="block text-sm font-medium text-content-secondary mb-2">
                Integration memory budget ({integrationBudgetMb === '0' ? 'automatic' : `${integrationBudgetMb} MB`})
              </label>
              <input
                type="number"
                value={integrationBudgetMb}
                onChange={(e) => setIntegrationBudgetMb(e.target.value)}
                min="0"
                max="16384"
                step="64"
                className="w-full bg-surface-hover border border-border rounded-lg px-4 py-2 text-content focus:outline-none focus:border-accent"
              />
              <p className="text-xs text-content-muted mt-2">
                Working memory one master build may use for reading frames. 0 = automatic
                ({budgetInfo ? budgetInfo.autoMb : '…'} MB on this machine, from{' '}
                {budgetInfo ? budgetInfo.totalRamMb : '…'} MB of RAM). Larger values read the disk in
                fewer, longer sweeps; smaller values use less memory and take longer.
              </p>
              {budgetNote && (
                <p className="text-xs text-content-muted mt-1">{budgetNote}</p>
              )}
              {budgetError && (
                <p className="text-xs text-error mt-1">{budgetError}</p>
              )}
              <button
                onClick={handleSaveIntegrationBudget}
                disabled={budgetSaving}
                className="mt-3 flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover disabled:bg-surface-hover disabled:cursor-not-allowed text-surface rounded-lg transition-colors"
              >
                <Save size={16} />
                {budgetSaving ? 'Saving...' : 'Save'}
              </button>
            </div>
          </div>
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

          {/* Account — identity-level, kept at the top. Fully self-contained
              (see AccountSection / useAccount); the app runs signed-out. */}
          <div className="mb-6 bg-surface-elevated rounded-lg p-6">
            <h3 className="text-lg font-semibold mb-4 flex items-center gap-2">
              <UserCircle size={20} />
              Account
            </h3>
            <p className="text-xs text-content-muted mb-4">
              Sign in to link this machine to your account for syncing frames between devices.
              Optional — every feature works without an account.
            </p>
            <AccountSection />
          </div>

          {/* Sync — machine role, auto-send, receiver status. Placed right after
              Account (task M2b); self-contained like AccountSection, works
              signed-out (quiet empty state). */}
          <div className="mb-6 bg-surface-elevated rounded-lg p-6">
            <h3 className="text-lg font-semibold mb-4 flex items-center gap-2">
              <RefreshCw size={20} />
              Sync
            </h3>
            <p className="text-xs text-content-muted mb-4">
              Send frames between your machines. A Capture device queues its frames to a paired
              Primary; the Primary receives and ingests them. Transfer folders, bandwidth and
              storage live on the <span className="text-content-secondary">Transfers</span> tab.
            </p>
            <SyncSection />
          </div>

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
              {blinkResolution === 'full' && (
                <p className="text-xs text-warning mt-2">
                  Full resolution debayers one-shot-colour frames at their native resolution with
                  gradient interpolation — around ten times slower per frame than Preview, and four
                  times the pixels. Buffering a whole set will take noticeably longer.
                </p>
              )}
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
                Maximum number of images kept in the memory cache. Default: 200. For large frame sets, increase this to avoid cache thrashing. A preview image is well under a megabyte; a full-resolution colour one is much larger, which is what the limit below is for.
              </p>
            </div>

            {/* Memory Cache Limit (MB) */}
            <div>
              <label className="block text-sm font-medium text-content-secondary mb-2">
                Memory Cache Limit (MB)
              </label>
              <input
                type="number"
                value={blinkCacheMaxMb}
                onChange={(e) => setBlinkCacheMaxMb(e.target.value)}
                min="64"
                max="16384"
                step="64"
                className="w-full bg-surface-hover border border-border rounded-lg px-4 py-2 text-content focus:outline-none focus:border-accent"
              />
              <p className="text-xs text-content-muted mt-2">
                Total memory the image cache may use, whichever limit is reached first. Default: 512 MB. Image size varies enormously with resolution, so the image count alone is not a memory limit.
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
          <h3 className="text-lg font-semibold mb-4">Content index</h3>
          <div className="p-4 bg-surface rounded-lg border border-border space-y-3">
            <p className="text-xs text-content-muted">
              A sampled content hash of every catalogued file — the first, middle and last
              512 KB, about 1.5 MB of reading per file. It has two uses: skipping files the
              other device already has when transferring, and grouping the Duplicates view by
              content when the option below is on. It is built in the background as a job of
              its own, never during a scan: automatically after each scan when sync is set up
              or content grouping is on, and by hand from the button here or on the Folders
              page. A running job can be stopped from the job card in the sidebar.
            </p>

            {contentIndex.status && (
              <p className="text-sm text-content-secondary">
                {contentIndex.status.total === 0
                  ? 'No files catalogued yet.'
                  : contentIndex.status.pending === 0
                    ? `All ${contentIndex.status.total} files indexed.`
                    : `${contentIndex.status.pending} of ${contentIndex.status.total} files not indexed yet.`}
              </p>
            )}

            {contentIndex.status && contentIndex.status.pending > 0 && (
              <p className="text-xs text-content-muted">
                The count only reaches zero for files the app can read. Files on storage that is
                offline, files that changed since the last scan, and files archived into a ZIP
                are skipped and stay counted — running the job again will not clear them. Bring
                the storage back online, restore an archive, or rescan the files that changed,
                and the next run picks them up.
              </p>
            )}

            {/* Hidden while a pass runs: the previous run's counts beside a
                spinning "Indexing…" button would read as this run's result. */}
            {!contentIndex.running && contentIndex.lastFinished && (
              <p className={`text-xs ${contentIndex.lastFinished.failed ? 'text-warning' : 'text-content-muted'}`}>
                {contentIndex.lastFinished.failed
                  ? 'The last run could not read the catalog and indexed nothing. See the log for details.'
                  : `Last run${contentIndex.lastFinished.cancelled ? ' (cancelled)' : ''}: ${contentIndex.lastFinished.updated} indexed${
                      contentIndex.lastFinished.skipped > 0
                        ? `, ${contentIndex.lastFinished.skipped} skipped`
                        : ''
                    }.`}
              </p>
            )}

            <label className="flex items-start gap-3 cursor-pointer pt-1">
              <input
                type="checkbox"
                checked={useContentHash}
                onChange={(e) => setUseContentHash(e.target.checked)}
                className="mt-0.5 w-5 h-5 shrink-0 rounded border-border bg-surface-hover text-accent focus:ring-2 focus:ring-accent focus:ring-offset-0"
              />
              <div>
                <span className="block text-sm font-medium text-content-secondary">
                  Group the Duplicates view by content
                </span>
                <span className="block text-xs text-content-muted mt-1">
                  <span className="font-semibold text-content-secondary">Off</span> — raw
                  sub-frames are grouped by their stored FITS/XISF header, which every scan
                  already records: no extra reading, and copies still match after a move
                  between drives changed their timestamps. Masters and processed files are
                  compared by their full contents.
                </span>
                <span className="block text-xs text-content-muted mt-1">
                  <span className="font-semibold text-content-secondary">On</span> —
                  everything, masters included, is grouped by the sampled hash: 1.5 MB of each
                  file, so two masters that differ only outside the sampled regions look
                  identical. Run a deep verify before deleting masters in this mode.
                </span>
                <span className="block text-xs text-content-muted mt-1">
                  New files get their hash from the index job after each scan, not during it.
                </span>
              </div>
            </label>

            {/* Also gated on pending: with nothing to index the button is
                disabled, and inviting a build it refuses would read as broken. */}
            {contentIndex.status && !contentIndex.status.syncConfigured && !useContentHash && contentIndex.status.pending > 0 && (
              <p className="text-xs text-content-muted">
                Sync is not set up on this device and content grouping is off, so the index is
                not built automatically. You can still build it now.
              </p>
            )}

            <button
              onClick={contentIndex.start}
              disabled={
                contentIndex.starting ||
                !contentIndex.status ||
                contentIndex.running ||
                contentIndex.status.pending === 0
              }
              className="flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover disabled:bg-surface-hover disabled:text-content-muted disabled:cursor-not-allowed text-surface rounded-lg transition-colors"
            >
              <RefreshCw size={18} className={contentIndex.running ? 'animate-spin' : ''} />
              {contentIndex.running ? 'Indexing…' : 'Build index now'}
            </button>
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
              <span className="text-content truncate flex-1" title={dbPath || undefined}>{dbPath || '—'}</span>
              {dbPath && (
                <button
                  onClick={() => revealItemInDir(dbPath)}
                  className="text-content-muted hover:text-content transition flex-shrink-0"
                  title="Reveal in file manager"
                  aria-label="Reveal in file manager"
                >
                  <FolderOpen size={16} />
                </button>
              )}
            </div>
            <div className="flex items-center gap-3">
              <span className="text-content-muted min-w-[80px]">Log folder:</span>
              <span className="text-content truncate flex-1" title={logDir || undefined}>{logDir || '—'}</span>
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
