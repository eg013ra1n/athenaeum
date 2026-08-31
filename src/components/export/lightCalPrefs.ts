// Light-calibration preferences, persisted per browser/profile in
// localStorage. They used to live in CalibrateLightsDialog — the dialog that
// pre-generated calibrated artifacts. Calibration is part of the EXPORT now
// (calibrated-export v2), so the preferences live with the export tree and the
// dialog re-exports them until it is removed.
//
// Every reader is total: an unset, unparsable or out-of-range value resolves to
// the documented default rather than throwing, because localStorage can be
// absent (private mode, quota) or hold a blob written by an older build.

import type { FlatNormMode, LightCalParams, BiasFallback } from '../../types/models';

/** localStorage key for the "Normalize master flat" preference (default ON). */
export const LIGHTCAL_FLATNORM_KEY = 'athenaeum.lightcal.flatNorm';

/** localStorage key for the flat-normalization statistic (default centralThird). */
export const LIGHTCAL_FLATNORM_MODE_KEY = 'athenaeum.lightcal.flatNormMode';

/** localStorage key for the Advanced calibration parameters (JSON). */
export const LIGHTCAL_PARAMS_KEY = 'athenaeum.lightcal.params';

/** localStorage key for the hot-pixel correction toggle (default ON). */
export const LIGHTCAL_HOT_PIXEL_KEY = 'athenaeum.lightcal.hotPixel';

/** localStorage key for the OSC debayer toggle (default ON). */
export const LIGHTCAL_DEBAYER_KEY = 'athenaeum.lightcal.debayer';

/** Advanced-parameter defaults — these reproduce the engine's current behavior
 *  (see `LightCalParams::default` in `calibration_library/light_cal.rs`). */
export const DEFAULT_LIGHTCAL_PARAMS: LightCalParams = {
  trimFraction: 0.05,
  pedestalDn: 0,
  biasFallback: 'subtractBias',
  cfaFlatScaling: true,
};

/** Read the persisted Advanced parameters, coercing every field into range and
 *  falling back to {@link DEFAULT_LIGHTCAL_PARAMS} when unset/corrupt. Exported so
 *  the badge/details readiness fetches in FrameSetDetail resolve staleness against
 *  the exact params the dialog would submit. */
export function readLightCalParamsPref(): LightCalParams {
  try {
    const raw = localStorage.getItem(LIGHTCAL_PARAMS_KEY);
    if (!raw) return { ...DEFAULT_LIGHTCAL_PARAMS };
    const parsed = JSON.parse(raw) as Partial<LightCalParams>;
    const trimFraction =
      typeof parsed.trimFraction === 'number' && Number.isFinite(parsed.trimFraction)
        ? Math.min(0.25, Math.max(0, parsed.trimFraction))
        : DEFAULT_LIGHTCAL_PARAMS.trimFraction;
    const pedestalDn =
      typeof parsed.pedestalDn === 'number' && Number.isFinite(parsed.pedestalDn)
        ? Math.max(0, parsed.pedestalDn)
        : DEFAULT_LIGHTCAL_PARAMS.pedestalDn;
    const biasFallback: BiasFallback =
      parsed.biasFallback === 'skipFrame' ? 'skipFrame' : 'subtractBias';
    // Default ON, so a preference blob written before this option existed keeps
    // the recommended behavior instead of silently opting out of it.
    const cfaFlatScaling = parsed.cfaFlatScaling !== false;
    return { trimFraction, pedestalDn, biasFallback, cfaFlatScaling };
  } catch {
    return { ...DEFAULT_LIGHTCAL_PARAMS };
  }
}

/** Read the persisted flat-norm preference (default ON when unset/corrupt). */
export function readFlatNormPref(): boolean {
  try {
    return localStorage.getItem(LIGHTCAL_FLATNORM_KEY) !== 'false';
  } catch {
    return true;
  }
}

/** Read the persisted flat-normalization statistic (default centralThird when
 *  unset/corrupt — any value other than the exact 'pixinsightTrimmed' token
 *  falls back to the default). */
export function readFlatNormModePref(): FlatNormMode {
  try {
    return localStorage.getItem(LIGHTCAL_FLATNORM_MODE_KEY) === 'pixinsightTrimmed'
      ? 'pixinsightTrimmed'
      : 'centralThird';
  } catch {
    return 'centralThird';
  }
}

/** Read the hot-pixel correction preference (default ON when unset/corrupt —
 *  same 'anything but the literal false' rule as the flat-norm toggle). */
export function readHotPixelPref(): boolean {
  try {
    return localStorage.getItem(LIGHTCAL_HOT_PIXEL_KEY) !== 'false';
  } catch {
    return true;
  }
}

/** Read the OSC debayer preference (default ON when unset/corrupt). */
export function readDebayerPref(): boolean {
  try {
    return localStorage.getItem(LIGHTCAL_DEBAYER_KEY) !== 'false';
  } catch {
    return true;
  }
}

/** Persist the Advanced parameters. Best-effort: a storage failure loses the
 *  memory of the choice, never the choice itself (the caller already holds it). */
export function writeLightCalParamsPref(params: LightCalParams): void {
  try {
    localStorage.setItem(LIGHTCAL_PARAMS_KEY, JSON.stringify(params));
  } catch {
    /* ignore — localStorage unavailable (private mode / quota) */
  }
}

/** Persist the "Normalize master flat" preference. */
export function writeFlatNormPref(on: boolean): void {
  try {
    localStorage.setItem(LIGHTCAL_FLATNORM_KEY, String(on));
  } catch {
    /* ignore */
  }
}

/** Persist the flat-normalization statistic. */
export function writeFlatNormModePref(mode: FlatNormMode): void {
  try {
    localStorage.setItem(LIGHTCAL_FLATNORM_MODE_KEY, mode);
  } catch {
    /* ignore */
  }
}

/** Persist the hot-pixel correction preference. */
export function writeHotPixelPref(on: boolean): void {
  try {
    localStorage.setItem(LIGHTCAL_HOT_PIXEL_KEY, String(on));
  } catch {
    /* ignore */
  }
}

/** Persist the OSC debayer preference. */
export function writeDebayerPref(on: boolean): void {
  try {
    localStorage.setItem(LIGHTCAL_DEBAYER_KEY, String(on));
  } catch {
    /* ignore */
  }
}
