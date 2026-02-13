import { useSearchParams } from 'react-router-dom';
import { useCallback } from 'react';

export interface MapViewState {
  zoom: number | null;
  ra: number | null;
  dec: number | null;
  rotation: number | null;
}

/**
 * Custom hook to manage celestial map view state via URL parameters with sessionStorage fallback.
 * This enables preserving zoom and position when navigating away and back.
 *
 * Persistence strategy:
 * 1. Primary: URL parameters (for sharing, history, refreshes)
 * 2. Fallback: sessionStorage (for sidebar navigation within session)
 *
 * @param storageKey - sessionStorage key for this map instance (default: 'skyatlas_view_state')
 */
export function useMapViewState(storageKey: string = 'skyatlas_view_state') {
  const [searchParams, setSearchParams] = useSearchParams();

  /**
   * Load view state from sessionStorage
   */
  const loadFromStorage = useCallback((): MapViewState | null => {
    try {
      const stored = sessionStorage.getItem(storageKey);
      if (stored) {
        return JSON.parse(stored) as MapViewState;
      }
    } catch (e) {
      console.warn('Failed to load view state from sessionStorage:', e);
    }
    return null;
  }, []);

  /**
   * Save view state to sessionStorage
   */
  const saveToStorage = useCallback((state: MapViewState) => {
    try {
      sessionStorage.setItem(storageKey, JSON.stringify(state));
    } catch (e) {
      console.warn('Failed to save view state to sessionStorage:', e);
    }
  }, []);

  /**
   * Get current view state from URL parameters, with sessionStorage fallback
   */
  const getViewState = useCallback((): MapViewState => {
    const zoom = searchParams.get('zoom');
    const ra = searchParams.get('ra');
    const dec = searchParams.get('dec');
    const rotation = searchParams.get('rotation');

    // If URL has params, use them
    if (zoom || ra || dec) {
      return {
        zoom: zoom ? parseFloat(zoom) : null,
        ra: ra ? parseFloat(ra) : null,
        dec: dec ? parseFloat(dec) : null,
        rotation: rotation ? parseFloat(rotation) : null,
      };
    }

    // Otherwise, try sessionStorage as fallback
    const stored = loadFromStorage();
    if (stored) {
      console.log('📦 Restored view state from sessionStorage:', stored);
      return stored;
    }

    // No state available
    return {
      zoom: null,
      ra: null,
      dec: null,
      rotation: null,
    };
  }, [searchParams, loadFromStorage]);

  /**
   * Validate view state values
   */
  const validateViewState = useCallback((state: MapViewState): boolean => {
    // Zoom should be positive and reasonable for projection.scale()
    // projection.scale() = Bt.scale * containerWidth / 1024 * zoomFactor
    // Stereographic base scale is 500; at 1920px wide, max x20 zoom ≈ 500*1920/1024*20 ≈ 18750
    if (state.zoom !== null && (state.zoom <= 0 || state.zoom > 20000)) return false;

    // RA should be 0-360 degrees
    if (state.ra !== null && (state.ra < 0 || state.ra > 360)) return false;

    // Dec should be -90 to 90 degrees
    if (state.dec !== null && (state.dec < -90 || state.dec > 90)) return false;

    // Rotation should be within reasonable bounds (typically -180 to 180)
    if (state.rotation !== null && (state.rotation < -360 || state.rotation > 360)) return false;

    return true;
  }, []);

  /**
   * Save view state to URL parameters and sessionStorage
   * @param state - The map view state to save
   * @param replace - If true, replaces current history entry instead of creating new one
   */
  const saveViewState = useCallback((state: MapViewState, replace: boolean = true) => {
    // Normalize RA to 0-360 range (d3-celestial can return negative or >360 values)
    let normalizedRa = state.ra;
    if (normalizedRa !== null) {
      normalizedRa = ((normalizedRa % 360) + 360) % 360;
    }

    const normalizedState = {
      ...state,
      ra: normalizedRa
    };

    // Validate before saving
    if (!validateViewState(normalizedState)) {
      console.warn('Invalid view state, not saving:', normalizedState);
      return;
    }

    // Save to sessionStorage for persistence across sidebar navigation
    saveToStorage(normalizedState);

    const params = new URLSearchParams(searchParams);

    if (normalizedState.zoom !== null) {
      params.set('zoom', normalizedState.zoom.toFixed(6));
    } else {
      params.delete('zoom');
    }

    if (normalizedState.ra !== null && normalizedState.dec !== null) {
      params.set('ra', normalizedState.ra.toFixed(6));
      params.set('dec', normalizedState.dec.toFixed(6));
    } else {
      params.delete('ra');
      params.delete('dec');
    }

    if (normalizedState.rotation !== null) {
      params.set('rotation', normalizedState.rotation.toFixed(6));
    } else {
      params.delete('rotation');
    }

    // Use replace to avoid cluttering browser history
    setSearchParams(params, { replace });
  }, [searchParams, setSearchParams, validateViewState, saveToStorage]);

  /**
   * Clear view state from URL
   */
  const clearViewState = useCallback(() => {
    const params = new URLSearchParams(searchParams);
    params.delete('zoom');
    params.delete('ra');
    params.delete('dec');
    params.delete('rotation');
    setSearchParams(params, { replace: true });
  }, [searchParams, setSearchParams]);

  return {
    getViewState,
    saveViewState,
    clearViewState,
    validateViewState
  };
}
