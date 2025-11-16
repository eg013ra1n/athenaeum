import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Save, AlertCircle, CheckCircle, Trash2, Database, RefreshCw } from 'lucide-react';

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
  const [thresholdValue, setThresholdValue] = useState('5.0');
  const [thresholdUnit, setThresholdUnit] = useState<ThresholdUnit>('arcmin');
  const [coordFrame, setCoordFrame] = useState('ICRS');
  const [nameMode, setNameMode] = useState('majority-object');
  const [sessionGapHours, setSessionGapHours] = useState('6.0');
  const [qualityThumbnail, setQualityThumbnail] = useState('70');
  const [qualityPreview, setQualityPreview] = useState('85');
  const [qualityFull, setQualityFull] = useState('95');
  const [blinkResolution, setBlinkResolution] = useState('preview');
  const [useContentHash, setUseContentHash] = useState(false);
  const [contentHashRescanned, setContentHashRescanned] = useState(false);

  // Calibration settings
  const [calibTempDelta, setCalibTempDelta] = useState('2.0');
  const [calibFlatDateWarning, setCalibFlatDateWarning] = useState('30');
  const [calibDarkDateWarning, setCalibDarkDateWarning] = useState('365');
  const [flatsMaxAge, setFlatsMaxAge] = useState('30');
  const [flatsTimeCluster, setFlatsTimeCluster] = useState('30');
  const [tempMatchWeight, setTempMatchWeight] = useState('0.3');
  const [darksMaxAge, setDarksMaxAge] = useState('30');
  const [darksTimeCluster, setDarksTimeCluster] = useState('30');
  const [biasMaxAge, setBiasMaxAge] = useState('30');
  const [biasTimeCluster, setBiasTimeCluster] = useState('30');

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

  useEffect(() => {
    loadSettings();
    loadCacheStats();
  }, []);

  const loadSettings = async () => {
    try {
      setLoading(true);
      setError(null);

      const [
        value, unit, frame, mode, sessionGap, qThumbnail, qPreview, qFull, resolution, contentHash, contentHashRescanned,
        tempDelta, flatDateWarn, darkDateWarn, flatsAge, flatsCluster, tempWeight, darksAge, darksCluster, biasAge, biasCluster
      ] = await Promise.all([
        invoke<string>('get_setting', {
          key: 'grouping.threshold.value',
          defaultValue: '3.0',
        }),
        invoke<string>('get_setting', {
          key: 'grouping.threshold.unit',
          defaultValue: 'deg',
        }),
        invoke<string>('get_setting', {
          key: 'grouping.coord.frame',
          defaultValue: 'ICRS',
        }),
        invoke<string>('get_setting', {
          key: 'ui.objects.auto_name_mode',
          defaultValue: 'majority-object',
        }),
        invoke<string>('get_setting', {
          key: 'session_gap_threshold_hours',
          defaultValue: '6.0',
        }),
        invoke<string>('get_setting', {
          key: 'rustafits.quality.thumbnail',
          defaultValue: '70',
        }),
        invoke<string>('get_setting', {
          key: 'rustafits.quality.preview',
          defaultValue: '85',
        }),
        invoke<string>('get_setting', {
          key: 'rustafits.quality.full',
          defaultValue: '95',
        }),
        invoke<string>('get_setting', {
          key: 'blink.resolution',
          defaultValue: 'preview',
        }),
        invoke<string>('get_setting', {
          key: 'duplicates.use_content_hash',
          defaultValue: 'false',
        }),
        invoke<string>('get_setting', {
          key: 'duplicates.content_hash_rescanned',
          defaultValue: 'false',
        }),
        // Calibration settings
        invoke<string>('get_setting', {
          key: 'calibration.temp_delta_celsius',
          defaultValue: '2.0',
        }),
        invoke<string>('get_setting', {
          key: 'calibration.flat_date_warning_days',
          defaultValue: '30',
        }),
        invoke<string>('get_setting', {
          key: 'calibration.dark_date_warning_days',
          defaultValue: '365',
        }),
        invoke<string>('get_setting', {
          key: 'flats.max_age_days',
          defaultValue: '30',
        }),
        invoke<string>('get_setting', {
          key: 'flats.time_cluster_minutes',
          defaultValue: '30',
        }),
        invoke<string>('get_setting', {
          key: 'temperature.match_weight',
          defaultValue: '0.3',
        }),
        invoke<string>('get_setting', {
          key: 'darks.max_age_days',
          defaultValue: '30',
        }),
        invoke<string>('get_setting', {
          key: 'darks.time_cluster_minutes',
          defaultValue: '30',
        }),
        invoke<string>('get_setting', {
          key: 'bias.max_age_days',
          defaultValue: '30',
        }),
        invoke<string>('get_setting', {
          key: 'bias.time_cluster_minutes',
          defaultValue: '30',
        }),
      ]);

      setThresholdValue(value);
      setThresholdUnit(unit as ThresholdUnit);
      setCoordFrame(frame);
      setNameMode(mode);
      setSessionGapHours(sessionGap);
      setQualityThumbnail(qThumbnail);
      setQualityPreview(qPreview);
      setQualityFull(qFull);
      setBlinkResolution(resolution);
      setUseContentHash(contentHash.toLowerCase() === 'true');
      setContentHashRescanned(contentHashRescanned.toLowerCase() === 'true');

      // Calibration settings
      setCalibTempDelta(tempDelta);
      setCalibFlatDateWarning(flatDateWarn);
      setCalibDarkDateWarning(darkDateWarn);
      setFlatsMaxAge(flatsAge);
      setFlatsTimeCluster(flatsCluster);
      setTempMatchWeight(tempWeight);
      setDarksMaxAge(darksAge);
      setDarksTimeCluster(darksCluster);
      setBiasMaxAge(biasAge);
      setBiasTimeCluster(biasCluster);
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

      // Validate calibration settings
      const tempDeltaValue = parseFloat(calibTempDelta);
      if (isNaN(tempDeltaValue) || tempDeltaValue <= 0) {
        setError('Temperature delta must be a positive number');
        return;
      }

      const flatDateWarnValue = parseInt(calibFlatDateWarning);
      if (isNaN(flatDateWarnValue) || flatDateWarnValue <= 0) {
        setError('Flat date warning must be a positive number');
        return;
      }

      const darkDateWarnValue = parseInt(calibDarkDateWarning);
      if (isNaN(darkDateWarnValue) || darkDateWarnValue <= 0) {
        setError('Dark date warning must be a positive number');
        return;
      }

      const flatsMaxAgeValue = parseInt(flatsMaxAge);
      if (isNaN(flatsMaxAgeValue) || flatsMaxAgeValue <= 0) {
        setError('Flats maximum age must be a positive number');
        return;
      }

      const flatsTimeClusterValue = parseInt(flatsTimeCluster);
      if (isNaN(flatsTimeClusterValue) || flatsTimeClusterValue <= 0) {
        setError('Flats time clustering must be a positive number');
        return;
      }

      const tempWeightValue = parseFloat(tempMatchWeight);
      if (isNaN(tempWeightValue) || tempWeightValue < 0 || tempWeightValue > 1) {
        setError('Temperature match weight must be between 0.0 and 1.0');
        return;
      }

      const darksMaxAgeValue = parseInt(darksMaxAge);
      if (isNaN(darksMaxAgeValue) || darksMaxAgeValue <= 0) {
        setError('Darks maximum age must be a positive number');
        return;
      }

      const darksTimeClusterValue = parseInt(darksTimeCluster);
      if (isNaN(darksTimeClusterValue) || darksTimeClusterValue <= 0) {
        setError('Darks time clustering must be a positive number');
        return;
      }

      const biasMaxAgeValue = parseInt(biasMaxAge);
      if (isNaN(biasMaxAgeValue) || biasMaxAgeValue <= 0) {
        setError('Bias maximum age must be a positive number');
        return;
      }

      const biasTimeClusterValue = parseInt(biasTimeCluster);
      if (isNaN(biasTimeClusterValue) || biasTimeClusterValue <= 0) {
        setError('Bias time clustering must be a positive number');
        return;
      }

      await Promise.all([
        invoke('set_setting', {
          key: 'grouping.threshold.value',
          value: thresholdValue,
        }),
        invoke('set_setting', {
          key: 'grouping.threshold.unit',
          value: thresholdUnit,
        }),
        invoke('set_setting', {
          key: 'grouping.coord.frame',
          value: coordFrame,
        }),
        invoke('set_setting', {
          key: 'ui.objects.auto_name_mode',
          value: nameMode,
        }),
        invoke('set_setting', {
          key: 'session_gap_threshold_hours',
          value: sessionGapHours,
        }),
        invoke('set_setting', {
          key: 'rustafits.quality.thumbnail',
          value: qualityThumbnail,
        }),
        invoke('set_setting', {
          key: 'rustafits.quality.preview',
          value: qualityPreview,
        }),
        invoke('set_setting', {
          key: 'rustafits.quality.full',
          value: qualityFull,
        }),
        invoke('set_setting', {
          key: 'blink.resolution',
          value: blinkResolution,
        }),
        invoke('set_setting', {
          key: 'duplicates.use_content_hash',
          value: useContentHash ? 'true' : 'false',
        }),
        // Reset rescan flag when toggling content hash
        invoke('set_setting', {
          key: 'duplicates.content_hash_rescanned',
          value: useContentHash ? 'false' : 'false',
        }),
        // Calibration settings
        invoke('set_setting', {
          key: 'calibration.temp_delta_celsius',
          value: calibTempDelta,
        }),
        invoke('set_setting', {
          key: 'calibration.flat_date_warning_days',
          value: calibFlatDateWarning,
        }),
        invoke('set_setting', {
          key: 'calibration.dark_date_warning_days',
          value: calibDarkDateWarning,
        }),
        invoke('set_setting', {
          key: 'flats.max_age_days',
          value: flatsMaxAge,
        }),
        invoke('set_setting', {
          key: 'flats.time_cluster_minutes',
          value: flatsTimeCluster,
        }),
        invoke('set_setting', {
          key: 'temperature.match_weight',
          value: tempMatchWeight,
        }),
        invoke('set_setting', {
          key: 'darks.max_age_days',
          value: darksMaxAge,
        }),
        invoke('set_setting', {
          key: 'darks.time_cluster_minutes',
          value: darksTimeCluster,
        }),
        invoke('set_setting', {
          key: 'bias.max_age_days',
          value: biasMaxAge,
        }),
        invoke('set_setting', {
          key: 'bias.time_cluster_minutes',
          value: biasTimeCluster,
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
      const stats = await invoke<CacheStats>('get_cache_stats');
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

      await invoke('clear_image_cache');

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

      const count = await invoke<number>('backfill_header_fingerprints');
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

      const result = await invoke<{files_total: number, files_updated: number, files_skipped: number, files_missing: number, errors: string[]}>('rescan_all_for_content_hash');

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
        <div className="text-center py-12 text-gray-400">
          <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-blue-500 mx-auto"></div>
          <p className="mt-4">Loading settings...</p>
        </div>
      </div>
    );
  }

  return (
    <div className="p-6 max-w-4xl">
      <div className="mb-6">
        <h2 className="text-3xl font-bold">Settings</h2>
        <p className="text-gray-400">Configure frame set grouping parameters</p>
      </div>

      {error && (
        <div className="mb-4 p-4 bg-red-900/20 border border-red-800 rounded-lg flex items-start gap-3">
          <AlertCircle className="text-red-500 flex-shrink-0 mt-0.5" size={20} />
          <div className="flex-1">
            <p className="font-medium text-red-400">Error</p>
            <p className="text-sm text-red-300">{String(error)}</p>
          </div>
        </div>
      )}

      {success && (
        <div className="mb-4 p-4 bg-green-900/20 border border-green-800 rounded-lg flex items-start gap-3">
          <CheckCircle className="text-green-500 flex-shrink-0 mt-0.5" size={20} />
          <div className="flex-1">
            <p className="font-medium text-green-400">Settings saved successfully</p>
          </div>
        </div>
      )}

      <div className="bg-gray-800 rounded-lg p-6 space-y-6">
        <div>
          <h3 className="text-lg font-semibold mb-4">Clustering Parameters</h3>

          <div className="space-y-4">
            {/* Threshold Value and Unit */}
            <div>
              <label className="block text-sm font-medium text-gray-300 mb-2">
                Grouping Threshold
              </label>
              <div className="flex gap-3">
                <input
                  type="number"
                  value={thresholdValue}
                  onChange={(e) => setThresholdValue(e.target.value)}
                  step="0.1"
                  min="0"
                  className="flex-1 bg-gray-700 border border-gray-600 rounded-lg px-4 py-2 text-gray-100 focus:outline-none focus:border-blue-500"
                />
                <select
                  value={thresholdUnit}
                  onChange={(e) => setThresholdUnit(e.target.value as ThresholdUnit)}
                  className="bg-gray-700 border border-gray-600 rounded-lg px-4 py-2 text-gray-100 focus:outline-none focus:border-blue-500"
                >
                  <option value="arcsec">arcseconds</option>
                  <option value="arcmin">arcminutes</option>
                  <option value="deg">degrees</option>
                </select>
              </div>
              <p className="text-xs text-gray-500 mt-2">
                Frames within this angular distance will be grouped together.
                Current value: {getThresholdInDegrees()}° (decimal degrees)
              </p>
            </div>

            {/* Coordinate Frame */}
            <div>
              <label className="block text-sm font-medium text-gray-300 mb-2">
                Coordinate Frame
              </label>
              <select
                value={coordFrame}
                onChange={(e) => setCoordFrame(e.target.value)}
                className="w-full bg-gray-700 border border-gray-600 rounded-lg px-4 py-2 text-gray-100 focus:outline-none focus:border-blue-500"
              >
                <option value="ICRS">ICRS (J2000)</option>
                <option value="FK5">FK5</option>
                <option value="FK4">FK4</option>
              </select>
              <p className="text-xs text-gray-500 mt-2">
                Reference frame for coordinate normalization. ICRS (J2000) is recommended.
              </p>
            </div>

            {/* Naming Mode */}
            <div>
              <label className="block text-sm font-medium text-gray-300 mb-2">
                Auto-Naming Mode
              </label>
              <select
                value={nameMode}
                onChange={(e) => setNameMode(e.target.value)}
                className="w-full bg-gray-700 border border-gray-600 rounded-lg px-4 py-2 text-gray-100 focus:outline-none focus:border-blue-500"
              >
                <option value="majority-object">Majority OBJECT value</option>
                <option value="ra-dec">RA/Dec coordinates</option>
              </select>
              <p className="text-xs text-gray-500 mt-2">
                How to name auto-generated frame sets. "Majority OBJECT" uses the most common
                OBJECT value; falls back to RA/Dec if no majority exists.
              </p>
            </div>
          </div>
        </div>

        <div>
          <h3 className="text-lg font-semibold mb-4">Session Detection</h3>

          <div className="space-y-4">
            {/* Session Gap Threshold */}
            <div>
              <label className="block text-sm font-medium text-gray-300 mb-2">
                Session Gap Threshold (hours)
              </label>
              <input
                type="number"
                value={sessionGapHours}
                onChange={(e) => setSessionGapHours(e.target.value)}
                step="0.5"
                min="0"
                className="w-full bg-gray-700 border border-gray-600 rounded-lg px-4 py-2 text-gray-100 focus:outline-none focus:border-blue-500"
              />
              <p className="text-xs text-gray-500 mt-2">
                Time gap to detect imaging night boundaries. If more than this many hours pass
                between frames, they will be grouped into separate imaging nights. Typical night
                sessions can span midnight (e.g., 19:00 Day 1 → 03:00 Day 2 = one night). Default
                is 6 hours.
              </p>
            </div>
          </div>
        </div>

        <div>
          <h3 className="text-lg font-semibold mb-4">Calibration</h3>

          <div className="space-y-6">
            {/* Warning Thresholds */}
            <div>
              <h4 className="text-md font-medium text-gray-200 mb-3">Warning Thresholds</h4>
              <div className="space-y-4">
                <div>
                  <label className="block text-sm font-medium text-gray-300 mb-2">
                    Temperature Warning Threshold (°C)
                  </label>
                  <input
                    type="number"
                    value={calibTempDelta}
                    onChange={(e) => setCalibTempDelta(e.target.value)}
                    step="0.1"
                    min="0"
                    className="w-full bg-gray-700 border border-gray-600 rounded-lg px-4 py-2 text-gray-100 focus:outline-none focus:border-blue-500"
                  />
                  <p className="text-xs text-gray-500 mt-2">
                    Maximum allowed temperature difference (±) between light frames and calibration frames before issuing a warning. Lower values are stricter.
                  </p>
                </div>

                <div>
                  <label className="block text-sm font-medium text-gray-300 mb-2">
                    Flat Date Warning Threshold (days)
                  </label>
                  <input
                    type="number"
                    value={calibFlatDateWarning}
                    onChange={(e) => setCalibFlatDateWarning(e.target.value)}
                    step="1"
                    min="1"
                    className="w-full bg-gray-700 border border-gray-600 rounded-lg px-4 py-2 text-gray-100 focus:outline-none focus:border-blue-500"
                  />
                  <p className="text-xs text-gray-500 mt-2">
                    Issue a warning if flat calibration frames are older than this many days. Flats should typically be captured frequently (weekly or per session).
                  </p>
                </div>

                <div>
                  <label className="block text-sm font-medium text-gray-300 mb-2">
                    Dark Date Warning Threshold (days)
                  </label>
                  <input
                    type="number"
                    value={calibDarkDateWarning}
                    onChange={(e) => setCalibDarkDateWarning(e.target.value)}
                    step="1"
                    min="1"
                    className="w-full bg-gray-700 border border-gray-600 rounded-lg px-4 py-2 text-gray-100 focus:outline-none focus:border-blue-500"
                  />
                  <p className="text-xs text-gray-500 mt-2">
                    Issue a warning if dark/bias calibration frames are older than this many days. Darks can typically be reused for longer periods than flats.
                  </p>
                </div>
              </div>
            </div>

            {/* Flat Calibration */}
            <div>
              <h4 className="text-md font-medium text-gray-200 mb-3">Flat Calibration</h4>
              <div className="space-y-4">
                <div>
                  <label className="block text-sm font-medium text-gray-300 mb-2">
                    Maximum Age (days)
                  </label>
                  <input
                    type="number"
                    value={flatsMaxAge}
                    onChange={(e) => setFlatsMaxAge(e.target.value)}
                    step="1"
                    min="1"
                    className="w-full bg-gray-700 border border-gray-600 rounded-lg px-4 py-2 text-gray-100 focus:outline-none focus:border-blue-500"
                  />
                  <p className="text-xs text-gray-500 mt-2">
                    Only consider flat frames captured within this many days when searching for calibration. Older flats will be excluded from matching.
                  </p>
                </div>

                <div>
                  <label className="block text-sm font-medium text-gray-300 mb-2">
                    Time Clustering (minutes)
                  </label>
                  <input
                    type="number"
                    value={flatsTimeCluster}
                    onChange={(e) => setFlatsTimeCluster(e.target.value)}
                    step="1"
                    min="1"
                    className="w-full bg-gray-700 border border-gray-600 rounded-lg px-4 py-2 text-gray-100 focus:outline-none focus:border-blue-500"
                  />
                  <p className="text-xs text-gray-500 mt-2">
                    Group flat frames into sets if they are within this time window. Flats are typically shot in bursts during a session.
                  </p>
                </div>

                <div>
                  <label className="block text-sm font-medium text-gray-300 mb-2">
                    Temperature Match Weight (0.0-1.0)
                  </label>
                  <input
                    type="number"
                    value={tempMatchWeight}
                    onChange={(e) => setTempMatchWeight(e.target.value)}
                    step="0.1"
                    min="0"
                    max="1"
                    className="w-full bg-gray-700 border border-gray-600 rounded-lg px-4 py-2 text-gray-100 focus:outline-none focus:border-blue-500"
                  />
                  <p className="text-xs text-gray-500 mt-2">
                    Weight given to temperature matching when scoring flat candidates (0.0 = ignore temperature, 1.0 = only temperature matters). Typical value is 0.3.
                  </p>
                </div>
              </div>
            </div>

            {/* Dark Calibration */}
            <div>
              <h4 className="text-md font-medium text-gray-200 mb-3">Dark Calibration</h4>
              <div className="space-y-4">
                <div>
                  <label className="block text-sm font-medium text-gray-300 mb-2">
                    Maximum Age (days)
                  </label>
                  <input
                    type="number"
                    value={darksMaxAge}
                    onChange={(e) => setDarksMaxAge(e.target.value)}
                    step="1"
                    min="1"
                    className="w-full bg-gray-700 border border-gray-600 rounded-lg px-4 py-2 text-gray-100 focus:outline-none focus:border-blue-500"
                  />
                  <p className="text-xs text-gray-500 mt-2">
                    Only consider dark frames captured within this many days. Darks can typically be reused for longer than flats.
                  </p>
                </div>

                <div>
                  <label className="block text-sm font-medium text-gray-300 mb-2">
                    Time Clustering (minutes)
                  </label>
                  <input
                    type="number"
                    value={darksTimeCluster}
                    onChange={(e) => setDarksTimeCluster(e.target.value)}
                    step="1"
                    min="1"
                    className="w-full bg-gray-700 border border-gray-600 rounded-lg px-4 py-2 text-gray-100 focus:outline-none focus:border-blue-500"
                  />
                  <p className="text-xs text-gray-500 mt-2">
                    Group dark frames into sets if they are within this time window.
                  </p>
                </div>
              </div>
            </div>

            {/* Bias Calibration */}
            <div>
              <h4 className="text-md font-medium text-gray-200 mb-3">Bias Calibration</h4>
              <div className="space-y-4">
                <div>
                  <label className="block text-sm font-medium text-gray-300 mb-2">
                    Maximum Age (days)
                  </label>
                  <input
                    type="number"
                    value={biasMaxAge}
                    onChange={(e) => setBiasMaxAge(e.target.value)}
                    step="1"
                    min="1"
                    className="w-full bg-gray-700 border border-gray-600 rounded-lg px-4 py-2 text-gray-100 focus:outline-none focus:border-blue-500"
                  />
                  <p className="text-xs text-gray-500 mt-2">
                    Only consider bias frames captured within this many days. Bias frames are very stable and can be reused for extended periods.
                  </p>
                </div>

                <div>
                  <label className="block text-sm font-medium text-gray-300 mb-2">
                    Time Clustering (minutes)
                  </label>
                  <input
                    type="number"
                    value={biasTimeCluster}
                    onChange={(e) => setBiasTimeCluster(e.target.value)}
                    step="1"
                    min="1"
                    className="w-full bg-gray-700 border border-gray-600 rounded-lg px-4 py-2 text-gray-100 focus:outline-none focus:border-blue-500"
                  />
                  <p className="text-xs text-gray-500 mt-2">
                    Group bias frames into sets if they are within this time window.
                  </p>
                </div>
              </div>
            </div>
          </div>
        </div>

        <div>
          <h3 className="text-lg font-semibold mb-4">Blink Viewer</h3>

          <div className="space-y-4">
            {/* Resolution */}
            <div>
              <label className="block text-sm font-medium text-gray-300 mb-2">
                Image Resolution
              </label>
              <select
                value={blinkResolution}
                onChange={(e) => setBlinkResolution(e.target.value)}
                className="w-full bg-gray-700 border border-gray-600 rounded-lg px-4 py-2 text-gray-100 focus:outline-none focus:border-blue-500"
              >
                <option value="thumbnail">Thumbnail (4x downscale)</option>
                <option value="preview">Preview (2x2 binning)</option>
                <option value="full">Full Resolution</option>
              </select>
              <p className="text-xs text-gray-500 mt-2">
                Resolution for blink viewer images. Thumbnail is fastest, Preview balances speed and quality, Full shows maximum detail. Note: Changing this will cache images separately for each resolution.
              </p>
            </div>

            {/* Thumbnail JPEG Quality */}
            <div>
              <label className="block text-sm font-medium text-gray-300 mb-2">
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
              <div className="flex justify-between text-xs text-gray-500 mt-1">
                <span>1 (Smallest)</span>
                <span>100 (Highest Quality)</span>
              </div>
              <p className="text-xs text-gray-500 mt-2">
                JPEG quality for thumbnail images. Default: 70. Lower values = smaller files, faster loading.
              </p>
            </div>

            {/* Preview JPEG Quality */}
            <div>
              <label className="block text-sm font-medium text-gray-300 mb-2">
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
              <div className="flex justify-between text-xs text-gray-500 mt-1">
                <span>1 (Smallest)</span>
                <span>100 (Highest Quality)</span>
              </div>
              <p className="text-xs text-gray-500 mt-2">
                JPEG quality for preview/blink viewer images. Default: 85. Good balance of quality and file size.
              </p>
            </div>

            {/* Full JPEG Quality */}
            <div>
              <label className="block text-sm font-medium text-gray-300 mb-2">
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
              <div className="flex justify-between text-xs text-gray-500 mt-1">
                <span>1 (Smallest)</span>
                <span>100 (Highest Quality)</span>
              </div>
              <p className="text-xs text-gray-500 mt-2">
                JPEG quality for full resolution images. Default: 95. Highest quality for detailed viewing.
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
                  className="w-5 h-5 rounded border-gray-600 bg-gray-700 text-blue-600 focus:ring-2 focus:ring-blue-500 focus:ring-offset-0"
                />
                <div>
                  <span className="block text-sm font-medium text-gray-300">
                    Use File Content Hash (XXHash)
                  </span>
                  <span className="block text-xs text-gray-500 mt-1">
                    When enabled, duplicate detection will use XXHash sampling (1.5MB per file) instead of metadata-based hashing. More accurate for finding true duplicates.
                    <br />
                    <span className="text-yellow-400 font-medium">⚠️ Not recommended for NAS or slow network storage</span> - hash computation requires reading file data which may be slow over network.
                  </span>
                </div>
              </label>
            </div>

            {/* Rescan Warning */}
            {useContentHash && !contentHashRescanned && (
              <div className="p-4 bg-yellow-900/20 border border-yellow-800 rounded-lg">
                <p className="text-sm text-yellow-300">
                  ⚠️ After enabling content hash, you must rescan all files to compute their hashes. New files scanned after enabling this will automatically have their content hashes computed.
                </p>
                <button
                  onClick={handleRescanContentHash}
                  disabled={rescanningContentHash}
                  className="mt-3 flex items-center gap-2 px-4 py-2 bg-yellow-600 hover:bg-yellow-700 disabled:bg-gray-600 disabled:cursor-not-allowed text-white rounded-lg transition-colors"
                >
                  <RefreshCw size={18} className={rescanningContentHash ? 'animate-spin' : ''} />
                  {rescanningContentHash ? 'Computing Hashes...' : 'Rescan All Files for Content Hash'}
                </button>
                {rescanSuccess !== null && (
                  <div className="mt-2 flex items-center gap-2 text-green-400">
                    <CheckCircle size={18} />
                    <span>Updated {rescanSuccess.updated} of {rescanSuccess.total} files</span>
                  </div>
                )}
              </div>
            )}
          </div>
        </div>

        <div className="pt-4 border-t border-gray-700">
          <div className="flex items-center gap-4">
            <button
              onClick={handleSave}
              disabled={saving}
              className="flex items-center gap-2 px-6 py-2 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-600 disabled:cursor-not-allowed text-white rounded-lg transition-colors"
            >
              <Save size={18} />
              {saving ? 'Saving...' : 'Save Settings'}
            </button>

            <button
              onClick={handleClearCache}
              disabled={clearingCache}
              className="flex items-center gap-2 px-6 py-2 bg-red-600 hover:bg-red-700 disabled:bg-gray-600 disabled:cursor-not-allowed text-white rounded-lg transition-colors"
            >
              <Trash2 size={18} />
              {clearingCache
                ? 'Clearing...'
                : `Clear Image Cache${cacheStats ? ` (${formatBytes(cacheStats.total_size_bytes)})` : ''}`
              }
            </button>

            {cacheSuccess && (
              <div className="flex items-center gap-2 text-green-400">
                <CheckCircle size={18} />
                <span>Cache cleared!</span>
              </div>
            )}
          </div>
        </div>
      </div>

      {/* Database Maintenance Section */}
      <div className="mt-6 bg-gray-800 rounded-lg p-6">
        <h3 className="text-lg font-semibold mb-4 flex items-center gap-2">
          <Database size={20} />
          Database Maintenance
        </h3>
        <div className="space-y-4">
          <div>
            <p className="text-sm text-gray-400 mb-3">
              Backfill header fingerprints for existing FITS files. This is required for file relinking when directories are moved.
            </p>
            <button
              onClick={handleBackfillFingerprints}
              disabled={backfillingFingerprints}
              className="flex items-center gap-2 px-4 py-2 bg-indigo-600 hover:bg-indigo-700 disabled:bg-gray-600 disabled:cursor-not-allowed text-white rounded-lg transition-colors"
            >
              <RefreshCw size={18} className={backfillingFingerprints ? 'animate-spin' : ''} />
              {backfillingFingerprints ? 'Computing...' : 'Backfill Header Fingerprints'}
            </button>
            {backfillSuccess !== null && (
              <div className="mt-2 flex items-center gap-2 text-green-400">
                <CheckCircle size={18} />
                <span>Processed {backfillSuccess} headers</span>
              </div>
            )}
          </div>
        </div>
      </div>

      <div className="mt-6 bg-gray-800 rounded-lg p-6">
        <h3 className="text-lg font-semibold mb-3">About Frame Set Grouping</h3>
        <div className="text-sm text-gray-400 space-y-2">
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
    </div>
  );
}
