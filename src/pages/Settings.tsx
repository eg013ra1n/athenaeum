import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Save, AlertCircle, CheckCircle } from 'lucide-react';

type ThresholdUnit = 'arcsec' | 'arcmin' | 'deg';

export default function Settings() {
  const [thresholdValue, setThresholdValue] = useState('5.0');
  const [thresholdUnit, setThresholdUnit] = useState<ThresholdUnit>('arcmin');
  const [coordFrame, setCoordFrame] = useState('ICRS');
  const [nameMode, setNameMode] = useState('majority-object');
  const [sessionGapHours, setSessionGapHours] = useState('6.0');
  const [blinkJpegQuality, setBlinkJpegQuality] = useState('50');
  const [blinkCacheSize, setBlinkCacheSize] = useState('15');
  const [blinkBlackPoint, setBlinkBlackPoint] = useState('850');
  const [blinkWhitePoint, setBlinkWhitePoint] = useState('64000');
  const [blinkMidtones, setBlinkMidtones] = useState('0.25');
  const [autostfShadowsClipping, setAutostfShadowsClipping] = useState('-1.5');
  const [autostfTargetMedian, setAutostfTargetMedian] = useState('0.35');
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);

  useEffect(() => {
    loadSettings();
  }, []);

  const loadSettings = async () => {
    try {
      setLoading(true);
      setError(null);

      const [value, unit, frame, mode, sessionGap, jpegQuality, cacheSize, blackPoint, whitePoint, midtones, shadowsClipping, targetMedian] = await Promise.all([
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
          key: 'blink_jpeg_quality',
          defaultValue: '50',
        }),
        invoke<string>('get_setting', {
          key: 'blink_cache_size',
          defaultValue: '15',
        }),
        invoke<string>('get_setting', {
          key: 'blink_black_point',
          defaultValue: '850',
        }),
        invoke<string>('get_setting', {
          key: 'blink_white_point',
          defaultValue: '64000',
        }),
        invoke<string>('get_setting', {
          key: 'blink_midtones',
          defaultValue: '0.25',
        }),
        invoke<string>('get_setting', {
          key: 'autostf.shadows_clipping',
          defaultValue: '-1.5',
        }),
        invoke<string>('get_setting', {
          key: 'autostf.target_median',
          defaultValue: '0.35',
        }),
      ]);

      setThresholdValue(value);
      setThresholdUnit(unit as ThresholdUnit);
      setCoordFrame(frame);
      setNameMode(mode);
      setSessionGapHours(sessionGap);
      setBlinkJpegQuality(jpegQuality);
      setBlinkCacheSize(cacheSize);
      setBlinkBlackPoint(blackPoint);
      setBlinkWhitePoint(whitePoint);
      setBlinkMidtones(midtones);
      setAutostfShadowsClipping(shadowsClipping);
      setAutostfTargetMedian(targetMedian);
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

      // Validate JPEG quality
      const jpegQualityValue = parseInt(blinkJpegQuality);
      if (isNaN(jpegQualityValue) || jpegQualityValue < 30 || jpegQualityValue > 100) {
        setError('JPEG quality must be between 30 and 100');
        return;
      }

      // Validate cache size
      const cacheSizeValue = parseInt(blinkCacheSize);
      if (isNaN(cacheSizeValue) || cacheSizeValue < 5 || cacheSizeValue > 30) {
        setError('Cache size must be between 5 and 30');
        return;
      }

      // Validate black point
      const blackPointValue = parseInt(blinkBlackPoint);
      if (isNaN(blackPointValue) || blackPointValue < 0 || blackPointValue > 65535) {
        setError('Black point must be between 0 and 65535');
        return;
      }

      // Validate white point
      const whitePointValue = parseInt(blinkWhitePoint);
      if (isNaN(whitePointValue) || whitePointValue < 0 || whitePointValue > 65535) {
        setError('White point must be between 0 and 65535');
        return;
      }

      // Validate black < white
      if (blackPointValue >= whitePointValue) {
        setError('Black point must be less than white point');
        return;
      }

      // Validate midtones
      const midtonesValue = parseFloat(blinkMidtones);
      if (isNaN(midtonesValue) || midtonesValue <= 0 || midtonesValue >= 1) {
        setError('Midtones must be between 0 and 1 (typical: 0.25)');
        return;
      }

      // Validate AutoSTF shadows clipping
      const shadowsClippingValue = parseFloat(autostfShadowsClipping);
      if (isNaN(shadowsClippingValue) || shadowsClippingValue < -3.0 || shadowsClippingValue > 0.0) {
        setError('AutoSTF Shadows Clipping must be between -3.0 and 0.0');
        return;
      }

      // Validate AutoSTF target median
      const targetMedianValue = parseFloat(autostfTargetMedian);
      if (isNaN(targetMedianValue) || targetMedianValue < 0.1 || targetMedianValue > 0.6) {
        setError('AutoSTF Target Median must be between 0.1 and 0.6');
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
          key: 'blink_jpeg_quality',
          value: blinkJpegQuality,
        }),
        invoke('set_setting', {
          key: 'blink_cache_size',
          value: blinkCacheSize,
        }),
        invoke('set_setting', {
          key: 'blink_black_point',
          value: blinkBlackPoint,
        }),
        invoke('set_setting', {
          key: 'blink_white_point',
          value: blinkWhitePoint,
        }),
        invoke('set_setting', {
          key: 'blink_midtones',
          value: blinkMidtones,
        }),
        invoke('set_setting', {
          key: 'autostf.shadows_clipping',
          value: autostfShadowsClipping,
        }),
        invoke('set_setting', {
          key: 'autostf.target_median',
          value: autostfTargetMedian,
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
            {/* JPEG Quality */}
            <div>
              <label className="block text-sm font-medium text-gray-300 mb-2">
                JPEG Quality ({blinkJpegQuality})
              </label>
              <input
                type="range"
                value={blinkJpegQuality}
                onChange={(e) => setBlinkJpegQuality(e.target.value)}
                min="30"
                max="100"
                step="1"
                className="w-full"
              />
              <div className="flex justify-between text-xs text-gray-500 mt-1">
                <span>30 (Fastest)</span>
                <span>100 (Highest Quality)</span>
              </div>
              <p className="text-xs text-gray-500 mt-2">
                JPEG quality for blink viewer. Lower quality = faster encoding. 50 (default) provides
                fast encoding (~1-1.5s) with acceptable quality for visual inspection. Quality 30-40
                for maximum speed, 60-70 for better quality, 80+ for highest quality but slower.
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

            {/* Black Point */}
            <div>
              <label className="block text-sm font-medium text-gray-300 mb-2">
                Black Point (16-bit value)
              </label>
              <input
                type="number"
                value={blinkBlackPoint}
                onChange={(e) => setBlinkBlackPoint(e.target.value)}
                min="0"
                max="65535"
                step="100"
                className="w-full bg-gray-700 border border-gray-600 rounded-lg px-4 py-2 text-gray-100 focus:outline-none focus:border-blue-500"
              />
              <p className="text-xs text-gray-500 mt-2">
                Percentile clipping black point for 16-bit images. Pixels below this value are
                clipped to black. Default: 850 (typical 1st percentile for astrophotography).
                Adjust for your camera's bias level.
              </p>
            </div>

            {/* White Point */}
            <div>
              <label className="block text-sm font-medium text-gray-300 mb-2">
                White Point (16-bit value)
              </label>
              <input
                type="number"
                value={blinkWhitePoint}
                onChange={(e) => setBlinkWhitePoint(e.target.value)}
                min="0"
                max="65535"
                step="100"
                className="w-full bg-gray-700 border border-gray-600 rounded-lg px-4 py-2 text-gray-100 focus:outline-none focus:border-blue-500"
              />
              <p className="text-xs text-gray-500 mt-2">
                Percentile clipping white point for 16-bit images. Pixels above this value are
                clipped to white. Default: 64000 (typical 99th percentile). This approach is
                10-20x faster than scanning for min/max and provides better visual results by
                rejecting outliers. Industry standard for DS9, PixInsight, Astropy.
              </p>
            </div>

            {/* Midtones Balance (MTF) */}
            <div>
              <label className="block text-sm font-medium text-gray-300 mb-2">
                Midtones Balance (STF)
              </label>
              <input
                type="number"
                value={blinkMidtones}
                onChange={(e) => setBlinkMidtones(e.target.value)}
                min="0.001"
                max="0.999"
                step="0.01"
                className="w-full bg-gray-700 border border-gray-600 rounded-lg px-4 py-2 text-gray-100 focus:outline-none focus:border-blue-500"
              />
              <p className="text-xs text-gray-500 mt-2">
                Midtones Transfer Function (MTF) balance for Screen Transfer Function (STF) stretching.
                Controls brightness of the stretch. Lower values (0.1-0.2) = darker/more contrast.
                Higher values (0.3-0.5) = brighter. Default: 0.25 (typical for linear astronomical data).
                Similar to PixInsight's auto-stretch midtones parameter.
              </p>
            </div>

            {/* AutoSTF Shadows Clipping */}
            <div>
              <label className="block text-sm font-medium text-gray-300 mb-2">
                AutoSTF Shadows Clipping ({autostfShadowsClipping})
              </label>
              <input
                type="range"
                value={autostfShadowsClipping}
                onChange={(e) => setAutostfShadowsClipping(e.target.value)}
                min="-3.0"
                max="0.0"
                step="0.1"
                className="w-full"
              />
              <div className="flex justify-between text-xs text-gray-500 mt-1">
                <span>-3.0 (Darker Shadows)</span>
                <span>0.0 (Brighter Shadows)</span>
              </div>
              <p className="text-xs text-gray-500 mt-2">
                Controls black point calculation for AutoSTF. More negative values = darker shadows
                (e.g. -2.8), values closer to 0 = brighter shadows (e.g. -0.5). Default: -1.5.
                This parameter uses MAD (Median Absolute Deviation) for robust histogram analysis.
              </p>
            </div>

            {/* AutoSTF Target Median */}
            <div>
              <label className="block text-sm font-medium text-gray-300 mb-2">
                AutoSTF Target Median ({autostfTargetMedian})
              </label>
              <input
                type="range"
                value={autostfTargetMedian}
                onChange={(e) => setAutostfTargetMedian(e.target.value)}
                min="0.1"
                max="0.6"
                step="0.05"
                className="w-full"
              />
              <div className="flex justify-between text-xs text-gray-500 mt-1">
                <span>0.1 (Darker/More Contrast)</span>
                <span>0.6 (Brighter)</span>
              </div>
              <p className="text-xs text-gray-500 mt-2">
                Controls overall brightness after AutoSTF stretch. Lower values = darker with more
                contrast (e.g. 0.25), higher values = brighter (e.g. 0.50). Default: 0.35.
                This determines where the image median will be mapped to in the output.
              </p>
            </div>
          </div>
        </div>

        <div className="pt-4 border-t border-gray-700">
          <button
            onClick={handleSave}
            disabled={saving}
            className="flex items-center gap-2 px-6 py-2 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-600 disabled:cursor-not-allowed text-white rounded-lg transition-colors"
          >
            <Save size={18} />
            {saving ? 'Saving...' : 'Save Settings'}
          </button>
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
