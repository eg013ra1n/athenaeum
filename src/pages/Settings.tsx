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

      const [value, unit, frame, mode, sessionGap] = await Promise.all([
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
      ]);

      setThresholdValue(value);
      setThresholdUnit(unit as ThresholdUnit);
      setCoordFrame(frame);
      setNameMode(mode);
      setSessionGapHours(sessionGap);
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
