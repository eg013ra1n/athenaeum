import { useCallback, useEffect, useState } from 'react';
import { api } from '../api';
import type { HipsSurveyInfo } from '../types/models';
import { setSkyBackdrop } from '../components/skychart/celestialHost';
import {
  HipsLayerStatus, setHipsOpacity, setHipsStatusListener, setHipsSurvey,
} from '../components/skychart/hipsLayer';

export const BACKGROUND_SETTING_KEY = 'skychart.background';
export const BACKGROUND_OPACITY_SETTING_KEY = 'skychart.background_opacity';

/**
 * The Sky Chart's survey background: the survey list, the chosen survey
 * (`'none'` or an id) and its opacity, both persisted as settings, and the
 * tile layer's loading status. The layer itself lives in the persistent
 * chart host, so it survives navigation like the chart.
 */
export function useSkyBackground(mapReady: boolean) {
  const [surveys, setSurveys] = useState<HipsSurveyInfo[]>([]);
  const [background, setBackground] = useState<string>('none');
  const [opacity, setOpacity] = useState(1);
  const [loaded, setLoaded] = useState(false);
  const [status, setStatus] = useState<HipsLayerStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    Promise.all([
      api.invoke<HipsSurveyInfo[]>('get_hips_surveys'),
      api.invoke<string>('get_setting', { key: BACKGROUND_SETTING_KEY, defaultValue: 'none' }),
      api.invoke<string>('get_setting', { key: BACKGROUND_OPACITY_SETTING_KEY, defaultValue: '1' }),
    ])
      .then(([list, bg, op]) => {
        if (cancelled) return;
        setSurveys(list);
        setBackground(list.some((s) => s.id === bg) ? bg : 'none');
        const o = parseFloat(op);
        setOpacity(Number.isFinite(o) ? Math.min(1, Math.max(0.1, o)) : 1);
        setLoaded(true);
      })
      .catch((err) => {
        console.error('[SkyChart] loading the sky background failed:', err);
        if (!cancelled) {
          setError(String(err));
          setLoaded(true);
        }
      });
    return () => { cancelled = true; };
  }, []);

  const survey = surveys.find((s) => s.id === background) ?? null;

  // Drive the layer and the chart backdrop from the chosen survey.
  useEffect(() => {
    if (!mapReady || !loaded) return;
    setHipsSurvey(survey ? { id: survey.id, mime: survey.mime, maxOrder: survey.maxOrder, tileWidth: survey.tileWidth } : null);
    setSkyBackdrop(!!survey);
    if (!survey) setStatus(null);
  }, [mapReady, loaded, survey]);

  useEffect(() => { setHipsOpacity(opacity); }, [opacity]);

  useEffect(() => {
    setHipsStatusListener(setStatus);
    return () => setHipsStatusListener(null);
  }, []);

  const chooseBackground = useCallback((id: string) => {
    setBackground(id);
    api.invoke('set_setting', { key: BACKGROUND_SETTING_KEY, value: id })
      .catch((err) => console.error('[SkyChart] saving the sky background failed:', err));
  }, []);

  const chooseOpacity = useCallback((value: number) => {
    setOpacity(value);
    api.invoke('set_setting', { key: BACKGROUND_OPACITY_SETTING_KEY, value: String(value) })
      .catch((err) => console.error('[SkyChart] saving the background opacity failed:', err));
  }, []);

  return { surveys, survey, background, opacity, status, error, chooseBackground, chooseOpacity };
}
