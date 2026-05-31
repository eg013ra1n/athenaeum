// TypeScript interfaces for the plate solving feature.
// These mirror the Rust structs on the backend (snake_case preserved for IPC boundary).

// PlateSolveConfig fields are snake_case: the Rust struct has no rename_all attribute,
// so field names cross the IPC boundary unchanged. This is a partial mirror —
// only the fields surfaced in the settings UI are typed here; the solver's
// confidence-gate thresholds and bright-cache path round-trip untouched because
// the panel loads and re-saves the whole config object.
export interface PlateSolveConfig {
  sip_order: number;                 // default: 3 (SIP distortion order passed to solvemyastro)
  autofind_tolerance_deg: number;    // default: 0.5
  /** Base verification tolerance in arcseconds. Per-frame pixel
   * tolerance = base_arcsec / pixel_scale_arcsec, clamped [4, 20] px.
   * Default: 8.0. */
  base_verification_tolerance_arcsec?: number;
  /** Per-camera XPIXSZ defaults (`INSTRUME` or `TELESCOP` → µm). Consulted
   * when a frame's FITS header lacks XPIXSZ — without a default, focallen
   * cannot be derived from arcsec/px alone. Default: empty (behaviour
   * unchanged for frames that have XPIXSZ in their headers). */
  camera_defaults?: Record<string, number>;
}

export interface PlateSolveRecord {
  id: number | null;
  frame_id: number;
  crpix1: number;
  crpix2: number;
  crval1: number; // RA degrees
  crval2: number; // Dec degrees
  cd1_1: number;
  cd1_2: number;
  cd2_1: number;
  cd2_2: number;
  sip_order: number | null;
  sip_a_coeffs: string | null;
  sip_b_coeffs: string | null;
  sip_ap_coeffs: string | null;
  sip_bp_coeffs: string | null;
  matched_stars: number;
  total_detected: number;
  rms_residual_px: number;
  rms_residual_arcsec: number;
  pixel_scale_arcsec: number;
  field_rotation_deg: number;
  solve_time_ms: number;
  catalog_used: string;
  algorithm_used: string;
  solved_at: string; // ISO 8601 datetime (YYYY-MM-DD HH:MM:SS)
  /** Catalog stars inside the solved FOV (null for pre-density-aware solves). */
  expected_catalog_stars_in_fov: number | null;
  /** matched_stars / expected_catalog_stars_in_fov, confidence signal. Null for pre-density-aware solves. */
  inlier_ratio: number | null;
}

export interface PlateSolveProgressEvent {
  frame_id: number;
  current: number;
  total: number;
  status: "solving" | "solved" | "failed";
  matched_stars?: number;
  rms_arcsec?: number;
  error?: string;
  /** Machine code for a failure (solvemyastro FailureClass, e.g. "VERIFY_GAP",
   * or "REJECTED_LOW_CONFIDENCE" / "PANIC"). Lets the UI group/style reasons. */
  failure_code?: string;
  /** Frame filename, for labelling per-frame rows without a separate lookup. */
  filename?: string;
}

export interface PlateSolveCompleteEvent {
  solved: number;
  failed: number;
  total: number;
  total_time_ms: number;
}

export interface CatalogStatusInfo {
  name: string;
  installed: boolean;
  epoch: number;
  star_count_approx: number;
  mag_limit: number;
}

export interface CatalogDownloadProgress {
  phase:
    | "downloading"
    | "verifying"
    | "extracting"
    | "converting"
    | "complete"
    | "error";
  current: number;
  total: number;
  percent: number;
}

// Autofind object from coordinates — snake_case wire format, no rename_all on the Rust side.
export type AutofindStatus =
  | "processing"
  | "labeled"
  | "no_match"
  | "already_labeled"
  | "missing_coords"
  | "error";

export interface AutofindProgressEvent {
  frame_id: number;
  current: number;
  total: number;
  status: AutofindStatus;
  designation: string | null;
  distance_deg: number | null;
  /** "contains" or "nearest" when status === "labeled"; null otherwise. */
  reason: "contains" | "nearest" | null;
  /** Frame's RA/Dec at the moment of the lookup, if available. */
  frame_ra: number | null;
  frame_dec: number | null;
  /** On status === "no_match", the designation of the nearest DSO regardless
   *  of tolerance. Populated so the UI can explain why the match failed
   *  ("closest was M 31 at 0.38°, outside 0.2° tolerance"). */
  closest_designation: string | null;
  closest_distance_deg: number | null;
}

export interface AutofindCompleteEvent {
  total: number;
  labeled: number;
  no_match: number;
  already_labeled: number;
  missing_coords: number;
  errors: number;
  cancelled: boolean;
  total_time_ms: number;
}
