# Calibration matching


## Detail moved from CLAUDE.md (2026-10-20) - Calibration matching

### Calibration Matching

Fully configurable via UI (Settings → Calibration Matching). Stored as a single `CalibrationMatchingConfig` JSON in `settings` under key `calibration.matching_config`.

**Components**: parameter-matching rules, clustering settings (max age, time-cluster window per type), scoring weights, warning thresholds, master preferences.

**Source → calibration links**:

- Lights → Flat, Dark, Bias
- Flats → DarkFlat, Dark, Bias (fallback chain DarkFlat → Dark → Bias)
- Darks → Bias (when "BIAS for Dark Optimization" is on)

**Per-pair parameters** (each `Exact` / `Warning` / `Ignore`): `instrume`, `binning`, `gain`, `offset`, `exptime`, `focallen`, `filter` (Lights→Flat only), `ccd_temp`. Defaults reproduce the original hardcoded behavior — see the `Default` impls in `calibration/config.rs` for the matrix.

**Geometry is not a parameter** (2026-10-20): a set of another frame size (NAXIS1×NAXIS2, after binning) never calibrates — auto-link, the manual list, manual assignment, the readiness and stacking-plan gates all ask `calibration::geometry::fit`. Another sub-frame origin at the same size (`XORGSUBF`/`YORGSUBF` from the stored header, compared only when both record it): auto-link never, by hand with a warning (owner, G-7). In-app masters stamp their members' agreed origin (G-5). Reference: `docs/catalog/README.md` → Calibration geometry.

**A manual dark, bias or dark-flat at another or unknown gain/offset is accepted with one `warn!`** (2026-11-12): `manual_assign_calibration` and `manual_assign_subcalibration`, after the geometry guard, ask `calibration::manual::level_check` whether the set declares no gain or offset, or declares one more than 0.01 from the lights' (the source set's, for a sub-calibration), and whether its offset is the higher. The log line `manual calibration accepted with another gain or offset` carries `set_id`, `count`, `calibration_type`, `light_offset`, `light_gain`, `set_offset`, `set_gain`. The link is written as a manual override and nothing is refused; auto-link and the matching config are untouched, and the check holds whatever mode the config gives gain and offset (a flat set is never checked). Header values only: no pixel is read.

**Key files**:

- `crates/athenaeum-core/src/calibration/config.rs` — `CalibrationMatchingConfig`, `ParameterConfig`, `MatchMode`.
- `crates/athenaeum-core/src/calibration/configurable_matcher.rs` — `find_calibration_sets`, `load_config`.
- `crates/athenaeum-core/src/calibration/hierarchy.rs` — hierarchy builder (uses configurable matcher).
- `src/types/calibration-config.ts`, `src/components/calibration/`.

**Tauri commands**: `get_calibration_matching_config`, `set_calibration_matching_config`, `reset_calibration_matching_config`.


**FITS keyword fallbacks regroup raw calibration frames** (0.7.2, issue #21; the Apply side of `docs/catalog/README.md` "FITS keyword fallbacks", code `header_fallbacks/calibration.rs`). A set keeps its own copy of the grouping values (`exptime`, `filter`, `ccd_temp`, `gain`, `offset`, `instrume`, `telescop`, `temp_min` / `temp_max`), and the matcher reads those copies, so when Apply changes a field a raw dark, flat, bias or dark-flat is grouped by it must regroup the frame, or the catalog column and the set would disagree:

- The frames' columns are written in Apply's last transaction, not in the page; the frames are planned again there from the catalog as it is then (R7), so a value typed or a file re-parsed during Apply wins. Each regrouped frame leaves its set (`calibration_set_frames`) and is grouped again through the scanner's own path (`create_calibration_sets_from_scan_with_masters`), as if newly scanned. The `calibration_set_empty_prune` trigger removes the sets that become empty; Apply does not call the whole-table prune (R6), so a concurrent scan's new empty set is never pruned by it. Consumer links from lights to a removed set go with it (`calibration_set_to_frames.calibration_set_id` cascades), as after a bulk edit, and **nothing links those lights again by itself**: the user runs Find calibration on their frame sets (`find_calibration_for_frame_set` has no other caller). Apply keeps no set ids (the way `refresh_calibration_library_inner` does) and does not re-match (owner O8). Instead the preview and Apply count the links that will go, `light_links_dropped` and, of them, `manual_links_dropped` (a manual link to a raw set is lost too): a light link points at a movable raw set that every member of it leaves, counted from the same plan as `sets_regrouped`, and the confirm shows both numbers. A flat's links to a dark-flat or bias set (`source_type = 'calibration_set'`, removed with the source set by `calibration_set_subcal_cleanup`) are lost the same way and are not counted.
- **A frozen-lineage set keeps its frames (D11).** A raw frame whose set is superseded (`superseded_by_set_id IS NOT NULL`) or the source of a `master_provenance` row keeps its membership, its columns are still written, and Apply logs `lineage_kept`. Taking it out would make the regroup mint a duplicate of a lineage a master already replaced.
- **A master-library set takes its master's new values** (`is_master_library = 1`, one master per set): the values a fresh registration would write, so a built master keeps rows identical to scanner ingestion.
- **Lights keep their frame sets, nights and links** (their nights are re-derived once per frame set only when `instrume` or `exptime` changed). Their new values are seen by the next matching run; their links to sets that survive the regroup stay.
- Known, pre-existing, not fixed here (`docs/superpowers/open-items.md` "FITS keyword fallbacks, what it leaves"): a set that keeps other members after a regroup keeps its stored `temp_min` / `temp_max` / `ccd_temp` (`frame_count` and the triggers are right, as after a bulk edit); the dark / dark-flat set reuse query ignores temperature (S4); a dark set's `focallen` is never written by the scanner, so it is not stale.
