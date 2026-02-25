import { useState, useEffect } from 'react';
import { api } from '../api';
import { Save, AlertCircle, CheckCircle, Trash2, Database, RefreshCw, Settings as SettingsIcon, Crosshair } from 'lucide-react';
import { CalibrationMatchingConfig } from '../components/calibration';
import { isTauri } from '../utils/platform';

type ThresholdUnit = 'arcsec' | 'arcmin' | 'deg';

interface CacheStats {
  total_entries: number;
  total_size_bytes: number;
  cache_hits: number;
  cache_misses: number;
  hit_rate: number;
  max_size_bytes: number;
}

export default function Settings() {
  // Defaults should match backend: settings/mod.rs defaults
  const [thresholdValue, setThresholdValue] = useState('3.0');
  const [thresholdUnit, setThresholdUnit] = useState<ThresholdUnit>('deg');
  const [sessionGapHours, setSessionGapHours] = useState('6.0');
  const [qualityThumbnail, setQualityThumbnail] = useState('70');
  const [qualityPreview, setQualityPreview] = useState('85');
  const [qualityFull, setQualityFull] = useState('95');
  const [blinkResolution, setBlinkResolution] = useState('preview');
  const [blinkCacheMode, setBlinkCacheMode] = useState('file');
  const [blinkThreads, setBlinkThreads] = useState('4');
  const [blinkThreadsMax, setBlinkThreadsMax] = useState(4);
  const [blinkCacheSize, setBlinkCacheSize] = useState('200');
  const [blinkRetentionMinutes, setBlinkRetentionMinutes] = useState('30');
  const [useContentHash, setUseContentHash] = useState(false);
  const [contentHashRescanned, setContentHashRescanned] = useState(false);

  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [clearingCache, setClearingCache] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);
  const [cacheSuccess, setCacheSuccess] = useState(false);
  const [cacheStats, setCacheStats] = useState<CacheStats | null>(null);

  // Backfill fingerprints state
  const [backfillingFingerprints, setBackfillingFingerprints] = useState(false);
  const [backfillSuccess, setBackfillSuccess] = useState<number | null>(null);

  // Content hash rescan state
  const [rescanningContentHash, setRescanningContentHash] = useState(false);
  const [rescanSuccess, setRescanSuccess] = useState<{updated: number, total: number} | null>(null);

  // Tab state
  const [activeTab, setActiveTab] = useState<'general' | 'calibration'>('general');

  useEffect(() => {
    loadSettings();
    if (isTauri) loadCacheStats();
    api.invoke<number>('get_blink_threads_max').then(setBlinkThreadsMax).catch(console.error);
  }, []);

  const loadSettings = async () => {
    try {
      setLoading(true);
      setError(null);

      const [
        value, unit, sessionGap, qThumbnail, qPreview, qFull, resolution, cacheMode, blinkThreadsVal, cacheSizeVal, retentionMin, contentHash, contentHashRescanned
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
          key: 'blink.cache_mode',
          defaultValue: 'memory',
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
      ]);

      setThresholdValue(value);
      setThresholdUnit(unit as ThresholdUnit);
      setSessionGapHours(sessionGap);
      setQualityThumbnail(qThumbnail);
      setQualityPreview(qPreview);
      setQualityFull(qFull);
      setBlinkResolution(resolution);
      setBlinkCacheMode(cacheMode);
      setBlinkThreads(blinkThreadsVal);
      setBlinkCacheSize(cacheSizeVal);
      setBlinkRetentionMinutes(retentionMin);
      setUseContentHash(contentHash.toLowerCase() === 'true');
      setContentHashRescanned(contentHashRescanned.toLowerCase() === 'true');
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
        ...(isTauri ? [api.invoke('set_setting', {
          key: 'blink.cache_mode',
          value: blinkCacheMode,
        })] : []),
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

  const loadCacheStats = async () => {
    try {
      const stats = await api.invoke<CacheStats>('get_cache_stats');
      setCacheStats(stats);
    } catch (err) {
      console.error('Failed to load cache stats:', err);
      // Don't show error to user, just fail silently
      setCacheStats(null);
    }
  };

  const formatBytes = (bytes: number): string => {
    const KB = 1024;
    const MB = KB * 1024;
    const GB = MB * 1024;

    if (bytes >= GB) {
      return `${(bytes / GB).toFixed(2)} GB`;
    } else if (bytes >= MB) {
      return `${(bytes / MB).toFixed(2)} MB`;
    } else if (bytes >= KB) {
      return `${(bytes / KB).toFixed(2)} KB`;
    } else {
      return `${bytes} bytes`;
    }
  };

  const handleClearCache = async () => {
    try {
      setClearingCache(true);
      setError(null);
      setCacheSuccess(false);

      await api.invoke('clear_image_cache');

      setCacheSuccess(true);
      setTimeout(() => setCacheSuccess(false), 3000);

      // Reload cache stats after clearing
      await loadCacheStats();
    } catch (err) {
      setError(err as string);
      console.error('Failed to clear cache:', err);
    } finally {
      setClearingCache(false);
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

  const handleBackfillFingerprints = async () => {
    try {
      setBackfillingFingerprints(true);
      setError(null);
      setBackfillSuccess(null);

      const count = await api.invoke<number>('backfill_header_fingerprints');
      setBackfillSuccess(count);
      setTimeout(() => setBackfillSuccess(null), 5000);
    } catch (err) {
      setError(err as string);
      console.error('Failed to backfill fingerprints:', err);
    } finally {
      setBackfillingFingerprints(false);
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
          Calibration Matching
        </button>
      </div>

      {/* Calibration Matching Tab */}
      {activeTab === 'calibration' && (
        <div className="bg-surface-elevated rounded-lg p-6">
          <h3 className="text-xl font-semibold mb-4">Calibration Matching Configuration</h3>
          <p className="text-content-muted mb-6">
            Configure how calibration frames (Flats, Darks, Bias) are matched to source frames.
            Define which parameters must match exactly, warn on threshold, or be ignored.
          </p>
          <CalibrationMatchingConfig />
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

            {/* Cache Mode - desktop only */}
            {isTauri && (
            <div>
              <label className="block text-sm font-medium text-content-secondary mb-2">
                Cache Mode
              </label>
              <select
                value={blinkCacheMode}
                onChange={(e) => setBlinkCacheMode(e.target.value)}
                className="w-full bg-surface-hover border border-border rounded-lg px-4 py-2 text-content focus:outline-none focus:border-accent"
              >
                <option value="file">File (disk JPEG)</option>
                <option value="memory">Memory (in-memory JPEG)</option>
              </select>
              <p className="text-xs text-content-muted mt-2">
                File mode caches JPEGs on disk (persistent). Memory mode keeps up to 200 images in RAM for instant switching (~60MB).
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
              className="flex items-center gap-2 px-6 py-2 bg-accent hover:bg-accent-hover disabled:bg-surface-hover disabled:cursor-not-allowed text-white rounded-lg transition-colors"
            >
              <Save size={18} />
              {saving ? 'Saving...' : 'Save Settings'}
            </button>

            {isTauri && (
            <button
              onClick={handleClearCache}
              disabled={clearingCache}
              className="flex items-center gap-2 px-6 py-2 bg-error hover:brightness-90 disabled:bg-surface-hover disabled:cursor-not-allowed text-white rounded-lg transition-colors"
            >
              <Trash2 size={18} />
              {clearingCache
                ? 'Clearing...'
                : `Clear Image Cache${cacheStats ? ` (${formatBytes(cacheStats.total_size_bytes)})` : ''}`
              }
            </button>
            )}

            {isTauri && cacheSuccess && (
              <div className="flex items-center gap-2 text-success">
                <CheckCircle size={18} />
                <span>Cache cleared!</span>
              </div>
            )}
          </div>
        </div>
      </div>

      {/* Database Maintenance Section */}
      <div className="mt-6 bg-surface-elevated rounded-lg p-6">
        <h3 className="text-lg font-semibold mb-4 flex items-center gap-2">
          <Database size={20} />
          Database Maintenance
        </h3>
        <div className="space-y-4">
          <div>
            <p className="text-sm text-content-muted mb-3">
              Backfill header fingerprints for existing FITS files. This is required for file relinking when directories are moved.
            </p>
            <button
              onClick={handleBackfillFingerprints}
              disabled={backfillingFingerprints}
              className="flex items-center gap-2 px-4 py-2 bg-purple hover:brightness-90 disabled:bg-surface-hover disabled:cursor-not-allowed text-white rounded-lg transition-colors"
            >
              <RefreshCw size={18} className={backfillingFingerprints ? 'animate-spin' : ''} />
              {backfillingFingerprints ? 'Computing...' : 'Backfill Header Fingerprints'}
            </button>
            {backfillSuccess !== null && (
              <div className="mt-2 flex items-center gap-2 text-success">
                <CheckCircle size={18} />
                <span>Processed {backfillSuccess} headers</span>
              </div>
            )}
          </div>
        </div>
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
