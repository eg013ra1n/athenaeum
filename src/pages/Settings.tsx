import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Save, AlertCircle, CheckCircle, Trash2 } from 'lucide-react';

type ThresholdUnit = 'arcsec' | 'arcmin' | 'deg';

export default function Settings() {
  const [thresholdValue, setThresholdValue] = useState('5.0');
  const [thresholdUnit, setThresholdUnit] = useState<ThresholdUnit>('arcmin');
  const [coordFrame, setCoordFrame] = useState('ICRS');
  const [nameMode, setNameMode] = useState('majority-object');
  const [sessionGapHours, setSessionGapHours] = useState('6.0');
  const [blinkCacheSize, setBlinkCacheSize] = useState('15');
  const [qualityThumbnail, setQualityThumbnail] = useState('70');
  const [qualityPreview, setQualityPreview] = useState('85');
  const [qualityFull, setQualityFull] = useState('95');
  const [blinkResolution, setBlinkResolution] = useState('preview');
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [clearingCache, setClearingCache] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);
  const [cacheSuccess, setCacheSuccess] = useState(false);

  useEffect(() => {
    loadSettings();
  }, []);

  const loadSettings = async () => {
    try {
      setLoading(true);
      setError(null);

      const [value, unit, frame, mode, sessionGap, cacheSize, qThumbnail, qPreview, qFull, resolution] = await Promise.all([
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
          key: 'blink_cache_size',
          defaultValue: '15',
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
      ]);

      setThresholdValue(value);
      setThresholdUnit(unit as ThresholdUnit);
      setCoordFrame(frame);
      setNameMode(mode);
      setSessionGapHours(sessionGap);
      setBlinkCacheSize(cacheSize);
      setQualityThumbnail(qThumbnail);
      setQualityPreview(qPreview);
      setQualityFull(qFull);
      setBlinkResolution(resolution);
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

      // Validate cache size
      const cacheSizeValue = parseInt(blinkCacheSize);
      if (isNaN(cacheSizeValue) || cacheSizeValue < 5 || cacheSizeValue > 30) {
        setError('Cache size must be between 5 and 30');
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
          key: 'blink_cache_size',
          value: blinkCacheSize,
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

  const handleClearCache = async () => {
    try {
      setClearingCache(true);
      setError(null);
      setCacheSuccess(false);

      await invoke('clear_image_cache');

      setCacheSuccess(true);
      setTimeout(() => setCacheSuccess(false), 3000);
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

            {/* Cache Size */}
            <div>
              <label className="block text-sm font-medium text-gray-300 mb-2">
                Image Cache Size ({blinkCacheSize} images)
              </label>
              <input
                type="range"
                value={blinkCacheSize}
                onChange={(e) => setBlinkCacheSize(e.target.value)}
                min="5"
                max="30"
                step="1"
                className="w-full"
              />
              <div className="flex justify-between text-xs text-gray-500 mt-1">
                <span>5 (Less Memory)</span>
                <span>30 (More Memory)</span>
              </div>
              <p className="text-xs text-gray-500 mt-2">
                Number of processed images to keep in memory cache. Cached images load instantly
                when revisiting them. Higher values use more memory (~20-40 MB per image depending
                on size and quality). Default is 15 images.
              </p>
            </div>
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
              {clearingCache ? 'Clearing...' : 'Clear Image Cache'}
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
