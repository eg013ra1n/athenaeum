import { useSearchParams } from 'react-router-dom';
import { useCallback } from 'react';

export interface MapViewState {
  zoom: number | null;
  ra: number | null;
  dec: number | null;
}

/**
 * Custom hook to manage celestial map view state via URL parameters.
 * This enables preserving zoom and position when navigating away and back.
 */
export function useMapViewState() {
  const [searchParams, setSearchParams] = useSearchParams();

  /**
   * Get current view state from URL parameters
   */
  const getViewState = useCallback((): MapViewState => {
    const zoom = searchParams.get('zoom');
    const ra = searchParams.get('ra');
    const dec = searchParams.get('dec');

    return {
      zoom: zoom ? parseFloat(zoom) : null,
      ra: ra ? parseFloat(ra) : null,
      dec: dec ? parseFloat(dec) : null,
    };
  }, [searchParams]);

  /**
   * Validate view state values
   */
  const validateViewState = useCallback((state: MapViewState): boolean => {
    // Zoom should be positive and reasonable for projection.scale()
    // projection.scale() typically ranges from ~150 (default) to ~2000 (max zoom)
    // This is different from d3-celestial's zoomlevel (0-10)
    if (state.zoom !== null && (state.zoom <= 0 || state.zoom > 2000)) return false;

    // RA should be 0-360 degrees
    if (state.ra !== null && (state.ra < 0 || state.ra > 360)) return false;

    // Dec should be -90 to 90 degrees
    if (state.dec !== null && (state.dec < -90 || state.dec > 90)) return false;

    return true;
  }, []);

  /**
   * Save view state to URL parameters
   * @param state - The map view state to save
   * @param replace - If true, replaces current history entry instead of creating new one
   */
  const saveViewState = useCallback((state: MapViewState, replace: boolean = true) => {
    // Validate before saving
    if (!validateViewState(state)) {
      console.warn('Invalid view state, not saving:', state);
      return;
    }

    const params = new URLSearchParams(searchParams);

    if (state.zoom !== null) {
      params.set('zoom', state.zoom.toFixed(6));
    } else {
      params.delete('zoom');
    }

    if (state.ra !== null && state.dec !== null) {
      params.set('ra', state.ra.toFixed(6));
      params.set('dec', state.dec.toFixed(6));
    } else {
      params.delete('ra');
      params.delete('dec');
    }

    // Use replace to avoid cluttering browser history
    setSearchParams(params, { replace });
  }, [searchParams, setSearchParams, validateViewState]);

  /**
   * Clear view state from URL
   */
  const clearViewState = useCallback(() => {
    const params = new URLSearchParams(searchParams);
    params.delete('zoom');
    params.delete('ra');
    params.delete('dec');
    setSearchParams(params, { replace: true });
  }, [searchParams, setSearchParams]);

  return {
    getViewState,
    saveViewState,
    clearViewState,
    validateViewState
  };
}
