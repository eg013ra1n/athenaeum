import { useState, useEffect, useCallback } from 'react';
import { Save, RotateCw } from 'lucide-react';
import { api } from '../../api';
import type { AnalysisConfig, ScoringWeights } from '../../types/analysis-config';

export function AnalysisSettingsPanel() {
  const [config, setConfig] = useState<AnalysisConfig | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    loadConfig();
  }, []);

  const loadConfig = async () => {
    try {
      setLoading(true);
      const result = await api.invoke<AnalysisConfig>('get_analysis_config');
      setConfig(result);
    } catch (err) {
      setError(String(err));
      console.error('Failed to load analysis config:', err);
    } finally {
      setLoading(false);
    }
  };

  const handleSave = useCallback(async () => {
    if (!config) return;
    try {
      setSaving(true);
      setError(null);
      await api.invoke('set_analysis_config', { config });
      setSaved(true);
      setTimeout(() => setSaved(false), 3000);
    } catch (err) {
      setError(String(err));
      console.error('Failed to save analysis config:', err);
    } finally {
      setSaving(false);
    }
  }, [config]);

  const handleReset = useCallback(async () => {
    try {
      setError(null);
      const result = await api.invoke<AnalysisConfig>('reset_analysis_config');
      setConfig(result);
      setSaved(true);
      setTimeout(() => setSaved(false), 3000);
    } catch (err) {
      setError(String(err));
      console.error('Failed to reset analysis config:', err);
    }
  }, []);

  const updateField = useCallback(<K extends keyof AnalysisConfig>(field: K, value: AnalysisConfig[K]) => {
    setConfig(prev => prev ? { ...prev, [field]: value } : prev);
  }, []);

  const updateWeight = useCallback(<K extends keyof ScoringWeights>(field: K, value: number) => {
    setConfig(prev => prev ? {
      ...prev,
      scoring_weights: { ...prev.scoring_weights, [field]: value },
    } : prev);
  }, []);

  if (loading || !config) {
    return (
      <div className="text-center py-8 text-content-muted">
        Loading analysis settings...
      </div>
    );
  }

  const weightsSum = config.scoring_weights.fwhm + config.scoring_weights.eccentricity
    + config.scoring_weights.snr_weight + config.scoring_weights.star_count;

  return (
    <div className="space-y-6">
      {error && (
        <div className="p-3 bg-error-muted border border-error/50 rounded-lg text-sm text-error">
          {error}
        </div>
      )}

      {saved && (
        <div className="p-3 bg-success-muted border border-success/50 rounded-lg text-sm text-success">
          Settings saved successfully
        </div>
      )}

      {/* Star Detection Parameters */}
      <div>
        <h4 className="text-sm font-semibold text-content mb-3">Star Detection Parameters</h4>
        <div className="grid grid-cols-2 gap-4">
          <div>
            <label className="block text-xs text-content-secondary mb-1">Detection Sigma</label>
            <input
              type="number"
              value={config.detection_sigma}
              onChange={e => updateField('detection_sigma', parseFloat(e.target.value) || 5.0)}
              min="1" max="20" step="0.5"
              className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
            />
            <p className="text-xs text-content-muted mt-1">Threshold in sigma above background (1.0-20.0)</p>
          </div>
          <div>
            <label className="block text-xs text-content-secondary mb-1">Max Stars</label>
            <input
              type="number"
              value={config.max_stars}
              onChange={e => updateField('max_stars', parseInt(e.target.value) || 200)}
              min="10" max="2000" step="10"
              className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
            />
            <p className="text-xs text-content-muted mt-1">Keep brightest N stars (10-2000)</p>
          </div>
          <div>
            <label className="block text-xs text-content-secondary mb-1">Min Star Area (px)</label>
            <input
              type="number"
              value={config.min_star_area}
              onChange={e => updateField('min_star_area', parseInt(e.target.value) || 5)}
              min="1" step="1"
              className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
            />
          </div>
          <div>
            <label className="block text-xs text-content-secondary mb-1">Max Star Area (px)</label>
            <input
              type="number"
              value={config.max_star_area}
              onChange={e => updateField('max_star_area', parseInt(e.target.value) || 2000)}
              min="1" step="10"
              className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
            />
          </div>
          <div>
            <label className="block text-xs text-content-secondary mb-1">Saturation Fraction</label>
            <input
              type="number"
              value={config.saturation_fraction}
              onChange={e => updateField('saturation_fraction', parseFloat(e.target.value) || 0.95)}
              min="0.5" max="1.0" step="0.01"
              className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
            />
            <p className="text-xs text-content-muted mt-1">Reject saturated stars (0.5-1.0)</p>
          </div>
        </div>
      </div>

      {/* Measurement Method */}
      <div>
        <h4 className="text-sm font-semibold text-content mb-3">Measurement Method</h4>
        <div className="space-y-3">
          <label className="flex items-center gap-3 cursor-pointer">
            <input
              type="checkbox"
              checked={config.use_gaussian_fit}
              onChange={e => updateField('use_gaussian_fit', e.target.checked)}
              className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent"
            />
            <div>
              <span className="text-sm text-content-secondary">Use Gaussian Fit</span>
              <p className="text-xs text-content-muted">More accurate FWHM/eccentricity but slower. Disable for fast approximation.</p>
            </div>
          </label>
          <div>
            <label className="block text-xs text-content-secondary mb-1">Background Mesh Size (optional)</label>
            <input
              type="number"
              value={config.background_mesh_size ?? ''}
              onChange={e => {
                const val = e.target.value ? parseInt(e.target.value) : null;
                updateField('background_mesh_size', val && val >= 16 ? val : null);
              }}
              min="16" step="8"
              placeholder="Global (disabled)"
              className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
            />
            <p className="text-xs text-content-muted mt-1">Grid cell size for spatially varying background. Leave empty for global estimation.</p>
          </div>
        </div>
      </div>

      {/* Scoring Weights */}
      <div>
        <h4 className="text-sm font-semibold text-content mb-3">
          Scoring Weights
          <span className="ml-2 text-xs font-normal text-content-muted">
            (sum: {weightsSum.toFixed(2)}, auto-normalized to 1.0)
          </span>
        </h4>
        <div className="space-y-3">
          {([
            { key: 'fwhm' as const, label: 'FWHM Weight', desc: 'Focus quality (lower FWHM = better)' },
            { key: 'eccentricity' as const, label: 'Eccentricity Weight', desc: 'Tracking/guiding (lower = better)' },
            { key: 'snr_weight' as const, label: 'SNR Weight', desc: 'Signal quality (higher = better)' },
            { key: 'star_count' as const, label: 'Star Count Weight', desc: 'Transparency proxy (more = better)' },
          ]).map(({ key, label, desc }) => (
            <div key={key}>
              <div className="flex items-center justify-between mb-1">
                <label className="text-xs text-content-secondary">{label}</label>
                <span className="text-xs text-content-muted">{config.scoring_weights[key].toFixed(2)}</span>
              </div>
              <input
                type="range"
                value={config.scoring_weights[key]}
                onChange={e => updateWeight(key, parseFloat(e.target.value))}
                min="0" max="1" step="0.05"
                className="w-full"
              />
              <p className="text-xs text-content-muted">{desc}</p>
            </div>
          ))}
        </div>
      </div>

      {/* Actions */}
      <div className="flex items-center gap-3 pt-4 border-t border-border">
        <button
          onClick={handleSave}
          disabled={saving}
          className="inline-flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover disabled:bg-surface-hover text-white text-sm rounded-lg transition-colors"
        >
          <Save size={16} />
          {saving ? 'Saving...' : 'Save'}
        </button>
        <button
          onClick={handleReset}
          className="inline-flex items-center gap-2 px-4 py-2 bg-surface-hover hover:bg-surface-hover text-content-secondary text-sm rounded-lg transition-colors"
        >
          <RotateCw size={16} />
          Reset to Defaults
        </button>
      </div>
    </div>
  );
}
