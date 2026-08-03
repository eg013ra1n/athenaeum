# Calibration Functionality Audit — 2026-08-02

Full-depth audit of the calibration subsystem: matching engine, master
calibration library (Phase 2), in-app light calibration (B5),
archive-of-originals, DB layer, both backends, and the frontend. Six parallel
review passes (integration math / master build+registration / light calibration /
matching / archive+DB / boundary+frontend), with every Critical and the top
Important findings re-verified by hand against the code before this document was
written.

**Companion fix plan:** `docs/superpowers/plans/2026-08-02-calibration-supersede-hardening.md`

## Overall verdict

The modules are individually well-engineered: `register_master` is one real
transaction pinned by a scanner-parity column-diff test; the banded integration
engine genuinely streams N-frames-per-band; sigma-clip/median/winsorized math is
correct and regression-tested; the light-cal preflight handshake
(wait-before-acquire) is implemented exactly as designed; `derive_status`,
scanner reconcile-adopt, the 12-step schema rebuild, and restore are clean; the
Tauri↔Axum command surface is in full parity (39/39 calibration/masters/lights
functions, verified by name-set diff).

**The systemic hole is one seam: the supersede lifecycle exists only inside the
Phase 2 module.** Five adjacent, older subsystems — the auto-matcher, the flats
auto-path, the Black Hole delete, the frame-set archive planner, and the
`unique_camera` recluster cascade — do not know masters or
`superseded_by_set_id` exist. As a result the master library has neither a safe
forward path (a routine re-match silently reverts links off the master) nor a
safe reverse path (deleting a master is impossible; deleting its file locks the
lineage forever).

## Critical findings

### C1 — Auto re-match silently reverts lights from a master onto a freshly-minted duplicate raw set

*Verified end-to-end by hand.*

- **Dark/Bias/DarkFlat:** default `master_preferences = NoPreference`
  (`calibration/config.rs:472-476`) + the `OnlyCompatible` filter
  `compatible.retain(|c| !c.is_master)`
  (`calibration/configurable_matcher.rs:496-498`) removes every master from
  auto-link candidates; the raw predecessor is excluded as superseded
  (`configurable_matcher.rs:364`). Zero candidates → `try_create_dark_for_frame`
  / `try_create_bias_for_frame` (`calibration/hierarchy.rs:712/809`) re-cluster
  the *same raw frames* (grouping never excludes members of superseded sets) into
  a **new duplicate raw set** (`check_for_existing_dark_set` at
  `dark_bias_groups.rs:1061` skips superseded sets, so the original is never
  reused) → `insert_calibration_link`'s `ON CONFLICT DO UPDATE`
  (`db/calibration_links.rs:53-63`) overwrites the master link, because the
  relink at registration deliberately preserved `is_manual_override = 0`
  (`calibration_library/register.rs:196-216`).
- **Flats (worse, structural):** the auto path never touches the configurable
  matcher at all — `build_complete_hierarchy` resolves flats exclusively via
  `find_flat_groups_for_light_frame` (`hierarchy.rs:337`), which groups raw
  `frames` rows and has no concept of `calibration_set`/`is_master_library`.
  A Master Flat is reachable only through the manual modal; `master_preferences`
  cannot fix it.
- **User impact:** build a master, add one more night, press *Find
  Calibration* — every light silently moves back to a duplicate raw set. The
  flagship Phase 2 feature is undone by routine use, and duplicate sets
  accumulate.

### C2 — A single NaN/Inf pixel in any source frame breaks or silently poisons a master build

*Verified: clamp-panic path and Auto-recipe default confirmed by hand; silent-NaN
path traced in review.*

- No finiteness check exists anywhere in `integration/`
  (`banded.rs::read_band` decodes raw bytes unvalidated; `write_fits_f32` writes
  unvalidated). A legal `BITPIX=-32` FITS with NaN (foreign frame, driver
  glitch) hits:
  - **WinsorizedSigma** (the Auto default for N≥15, `api/masters.rs:216-220`):
    NaN mean/σ → NaN clamp bounds → `f64::clamp` **panics**
    (`combine.rs:322`) — caught upstream, but the build dies with a useless
    message.
  - **SigmaClip / LinearFitClip / None / Median:** NaN survives into the
    combine and the master gets a **silent NaN**, reported as "0 rejected",
    which then propagates into every light calibrated with it.
- FITS context: for float data the standard defines IEEE NaN as the *undefined
  pixel* marker (BLANK is forbidden for BITPIX −32/−64) — NaN inputs are
  legal data that must be handled, not corruption.

### C3 — A master cannot be deleted, and the spec's "un-supersede by deleting the master" does not exist

*Verified by exhaustive grep + FK check.*

- No `delete_master`/un-supersede command exists in any crate or the frontend.
  Nothing ever clears `superseded_by_set_id` (plain `REFERENCES
  calibration_set(id)`, no `SET NULL`).
- The only file-delete path is the generic Black Hole
  (`db/operations_blackhole.rs:199` `send_to_void`), which knows nothing about
  `calibration_set`: the master's set row is immortal (both prune paths exempt
  `is_master_library = 1`, `schema.rs:34/75`), the raw set stays superseded
  forever, `validate_buildable_set` refuses a rebuild ("already superseded"),
  `rebuild_master` fails ("no file on record").
- **User impact:** one mistakenly-built master permanently locks its calibration
  lineage; recovery requires manual SQLite surgery.

### C4 — Frame-set archive with disposition Move can pull a master's file into a project zip and delete it from the library

*Verified: collector query confirmed by hand.*

- `collect_calibration_files` (`archive/planner.rs:602-632`) collects
  calibration files by consumer links with **no `is_master_library` filter**.
  The shared-calibration guard is client-side only
  (`ArchiveDispositionDialog.tsx:70-84`) and blind to masters: a just-built
  master with a single consumer is "not shared" → Move permitted → the
  master's file is zipped into the frame set's archive and deleted from the
  Calibration Library, while `files.path` still claims the library location.
  Matching/rebuild/light-cal silently break; the operation reports success.

### C5 — The `unique_camera` recluster cascade either fails every scan of the root or silently destroys archive audit rows

*Verified: unguarded DELETE confirmed by hand; the sibling path already has the
guard AND a regression test for exactly this class of bug.*

- `delete_calibration_sets_for_root` (`db/operations.rs:409-487`, unguarded
  `DELETE FROM calibration_set` at `:470`) has none of the exemptions that
  `prune_orphaned_calibration_sets` (`schema.rs:72-85`) applies
  (`is_master_library`, superseded, `master_provenance.source_set_id`).
  Reached on every scan after toggling `unique_camera`
  (`scanner/mod.rs:2323` → `reconcile_unique_camera_instrume`,
  `operations.rs:491-577`).
- With a superseded set or master under the root: `FOREIGN KEY constraint
  failed`, the SavepointGuard rolls back the rename too, so **every subsequent
  scan repeats the failure**. Where the FK doesn't fire,
  `archive_operations.calibration_set_id ON DELETE CASCADE` silently deletes
  the archive operation's audit/step rows (the zip survives orphaned).

## Important findings

- **I1 — Flat division by ~zero pixels** (`calibration_library/light_cal.rs:174-176`,
  *verified by hand*): a dead/negative flat pixel produces Inf/NaN/sign-flip in
  the calibrated light, silently; `CALSTAT` reports success. Reference behavior
  (see research below): established tools floor the denominator
  (`max(0.00002, flat)` in normalized units) and warn.
- **I2 — Hardcoded output scale divisor** (`light_cal.rs:54`, *verified*):
  `OUTPUT_SCALE_DIVISOR = 65535.0` regardless of the source's BITPIX; 8-bit /
  32-bit-int / float sources are mis-scaled silently. The spec promises
  "divide by the source bit-depth maximum".
- **I3 — `skip_matching` hides candidates from the manual "Show All" list**
  (`configurable_matcher.rs:413-415`): the unconditional `continue` fires before
  the mode branch, so a set with a NULL Exact+required parameter (e.g. no GAIN
  header — common on CCDs) is unselectable even manually.
- **I4 — `archive_after` chain is not panic-safe** (`api/masters.rs:1122-1140`):
  its neighbor `run_build` is wrapped in `catch_unwind` precisely to protect
  handle removal and `master-build-complete`; the archive chain call is not. A
  panic there = progress stuck forever + set locked ("build already in
  progress") until restart.
- **I5 — Band budget floor breaks the memory bound** (`banded.rs:216-219`):
  the `.max(16)` row floor overrides the 256 MiB budget once
  `frame_count` is large; ~440+ full-frame subs exceed the budget, ~2000 reach
  ~1.2 GB per band.
- **I6 — Manual Calibration modal deselect is a silent no-op**
  (`ManualCalibrationModal.tsx` `handleApply` null-forwarding +
  `CalibrationHierarchyView.tsx` `!== null` gates): clicking a selected set to
  clear it, then Apply, looks successful but changes nothing; no per-type clear
  path exists in the UI at all.
- **I7 — Filename-claim TOCTOU between concurrent builds**
  (`calibration_library/paths.rs:145-159` + unconditional `rename_replace`):
  at `compute.max_concurrent > 1`, two builds resolving the same target name
  race check-then-write; the loser's bytes silently land under the winner's
  catalog metadata.

## Minor findings

- `alert()` instead of `notify()` — `CalibrationSetTable.tsx:136`.
- Fully silent `.catch(() => {})` — `CalibrationHierarchyView.tsx:96`.
- Raw Tailwind colors (rose/amber/yellow/numbered orange/purple shades) in
  `MatchingMatrixTable.tsx`, `CalibrationSetsTable.tsx`,
  `LightsAnalysisTable.tsx`, `BlackholedFramesSection.tsx`,
  `CameraFilterTree.tsx`, `ManualCalibrationModal.tsx` — should be design
  tokens.
- Calibration-matching config travels snake_case (documented deviation in
  `src/types/helpers.ts`; not a functional bug).
- FITS Logical cards (`PLTSOLVD`) round-trip as quoted strings in light-cal
  copy-through (`api/lights.rs:662-671`).
- Re-calibration to a recorded `output_path` recreates a user-deleted directory
  instead of detecting a manual move (`api/lights.rs:968-985`).
- `BITPIX=32` loses precision above 2²⁴ (cast to f32 before BZERO/BSCALE,
  `banded.rs:186-189`) — practically unreachable with consumer cameras.
- `reject_percentile` deviation blow-up for small-but-nonzero medians — only
  reachable via a deliberate manual recipe override.
- 0×0 image through the spill fallback panics with an unhelpful
  "step != 0" message (caught; cosmetic).
- A crash between master file write and DB commit leaves an orphan file that
  self-heals as an "imported" master on the next scan — silent, but nothing is
  lost.

## Verified clean

Registration transaction + relink of both link source types (pinned by tests);
combiner math and band boundary/seek arithmetic; central-third normalization and
Auto-recipe thresholds exactly per spec; `recipe_json` records the resolved
recipe; ComputeQueue lock hierarchy/cancel/wake + preflight wait-before-acquire;
`derive_status` full staleness matrix; scanner reconcile-adopt all four branches
+ catalog-injection gate; honest CALSTAT ladder in both places; 12-step
`archive_operations` rebuild (atomic, idempotent, FK-reverified, tested);
archive subject dichotomy (`frames_set_id` vs `calibration_set_id`)
Option-guarded everywhere; path sanitization incl. Windows reserved names and
traversal; full Tauri↔Axum parity; StrictMode-safe listener pattern and
`notify()` usage in all calibration hooks/contexts.

## Industry research: bad pixels and NaN (basis for the C2/I1 policy)

Researched 2026-08-02 against the FITS standard, PixInsight (GPL-era
ImageIntegration sources + ImageCalibration documentation), Siril docs, and DSS
(already covered in `2026-07-04-calibration-math-research.md` §9):

- **FITS standard:** for BITPIX −32/−64, IEEE NaN *is* the undefined-pixel
  marker (BLANK forbidden); readers must treat NaN as missing data.
  (STScI FITS standard §6.3 / users-guide "IEEE Floating Point Data".)
- **PixInsight ImageIntegration** (verified in GPL-era source:
  `ImageIntegrationInstance.cpp`, `IntegrationRejectionEngine.cpp`,
  `ImageIntegrationParameters.cpp`): **range rejection runs before statistical
  rejection**, per pixel, excluding out-of-range samples from the stack and
  counting them into rejection maps and the console report. `rangeClipLow` is
  **enabled by default with `rangeLow = 0.0`** — i.e. ≤0 samples are excluded
  per pixel out of the box. Integration never aborts on a bad sample.
- **PixInsight ImageCalibration:** the flat denominator is floored —
  `max(0.00002, flat)` in normalized [0,1] units — so division by zero is
  structurally impossible; invalid output pixels produce a counted warning, not
  an abort. Optional output pedestal guards against clipping negatives.
- **Siril:** rejection maps (normal per-pixel rejection rate 0.1–0.5%),
  median/winsorized master stacking — same philosophy: bad values are the
  rejection stage's job.
- **DSS:** hot pixels detected from darks (> median + 16σ) and repaired by
  neighbor interpolation **after** calibration (cosmetic correction is a
  separate, post-calibration stage).

**Conclusion:** no mainstream tool fails a build over a bad pixel. The
standard-conformant behavior is per-pixel exclusion + accounting + a warning,
with an epsilon floor at the flat division. Cosmetic correction
(defect-map/neighbor repair) is a distinct post-calibration feature — noted as a
future enhancement, not part of this fix cycle.

## Ratified decisions (owner, 2026-08-02)

1. **C1:** masters are *always* auto-link candidates; `master_preferences`
   affects ordering only (PreferMaster default for fresh configs). Fixes all
   installs without config migration.
2. **C3:** new `delete_master` command (un-supersede + reverse relink + file
   delete) **and** Black Hole interception — voiding/black-holing a master file
   performs the same un-supersede cleanup.
3. **C4:** master files are always disposition **Copy** in frame-set archives,
   enforced server-side in the planner.
4. **C2/I1:** research-backed policy — per-pixel exclusion of non-finite
   samples at integration (+ per-frame counters, `warn!`, notification detail,
   finite-output invariant; all-samples-bad pixel → 0 with its own counter);
   epsilon floor `max(2e-5, flat/ATH_FNRM)` at the light-cal flat division
   (+ counter + `warn!`). Cosmetic correction deferred as a future feature.
5. **C1 log fields:** `covered` / `uncovered` (both frame counts) are added to
   the canonical field dictionary for the superseded-guard partial-coverage
   `warn!` — a group only partly covered by a superseded lineage still reuses
   the master, and these fields make the orphaned remainder visible instead of
   silent.
6. **I1 log fields:** `floored_flat_pixels` (pixel count, on the per-frame
   `light calibrated` / `light calibration recorded` debug events) and `total`
   (the frame's pixel count, paired with `count` on the flat-denominator floor
   `warn!` so the count reads as a fraction) are added to the canonical field
   dictionary. The count is not comparable across flat-normalization modes —
   normalization ON compares the floor against `flat/ATH_FNRM`, OFF against the
   raw flat value — so `total` is always the emitting frame's own denominator,
   never a cross-frame or cross-mode rate.
7. **OSC/CFA cycle log fields** added to the canonical field dictionary.
   Genuinely new names: `light_pattern` / `master_pattern` — one CFA layout
   label each (`RGGB`, `RGGB at (1, 0)`, `mono`, `unrecognized 'XTRANS'`); at
   the `api::lights` advisory sites that string is the one the readiness
   dialog shows, so log and UI cannot disagree, but the `light_cal`
   flat-vs-light warn reuses `light_pattern` (with `flat_pattern`) for a BARE
   pattern name and is log-only — a reader must not assume the dialog
   spelling. `bayerpat`: a raw pattern string the parser could not vouch for.
   `field` (which keyword or column a note is about) with `value` (the
   offending value) on the Bayer-consensus, offset-range and channel-constant
   warns. `cfa_warnings` (a count) on the readiness debug event.
   `light_patterns`, plural, for the set-level note — deliberately distinct
   from the singular, but it does **not** yet carry one shape: the
   `kind == "lights"` log branch serves BOTH set-level advisories, the
   multi-layout one (a joined list, `"RGGB (3), mono (1)"`) and the
   unrecognized-consensus one (a single label), so a reader must still
   tolerate both. Splitting them is in the deferred list below.

   **Two of these names COLLIDE with pre-existing fields of an unrelated
   meaning, and the collision is live.** `pattern` already means the flat
   *selection* pattern — `FlatPattern { Automatic, LongTerm, Manual }`
   (`calibration/flat_matcher.rs:48-55`; the legacy `before_session`-style
   strings are migration inputs that all parse to `Automatic`) — emitted as
   the parsed enum at `calibration/hierarchy.rs:447` and as raw setting
   strings (`pattern=Some("long_term")`) at `hierarchy.rs:403/422`; and
   `flat_pattern` already means the same concept at `api/calibration.rs:146`
   (`flat_pattern=Some("automatic")`). So `pattern=RGGB` now shares a field
   name with `pattern=Automatic`, and `flat_pattern=RGGB` with
   `flat_pattern=Some("automatic")` — a log query on either name returns two
   unrelated populations. Registered as-is rather than renamed under review;
   the rename is in the deferred list below. (The `= ?` Debug form is why an
   exact-string sweep of the cycle diff missed the pre-existing pair. On
   re-sweep against `7a84c8c9~1`, `pattern` and `flat_pattern` are the only
   collisions within athenaeum-core/tauri/web: `bayerpat`, `light_pattern`,
   `master_pattern`, `light_patterns`, `cfa_warnings` and `field` have no
   prior tracing-field use there — their pre-cycle hits are all local
   bindings, SQL fragments or struct-literal fields. `value` has prior
   tracing use in the Perseus batcher (`crates/perseus/src/batcher.rs:1092,
   1165`) in a compatible "offending value + error" sense, in a separate
   binary's log stream.)

## Deferred follow-ups (not in the fix plan)

- Cosmetic correction / defect maps (post-calibration hot-pixel repair).
- FITS Logical card type in light-cal copy-through.
- `BITPIX=32` >2²⁴ precision (no real-world exposure).
- Moved-output detection for re-calibration without a rescan.
- Percentile-clip near-zero-median normalization (manual-recipe-only).
- snake_case calibration-config wire format (documented deviation).
- Scan-root deletion leaves master/superseded shells with dangling supersede
  pointers when the deleted root held master files — un-supersede-on-root-delete
  semantics need an owner decision (re-added roots re-ingest masters as NEW sets).

From the 2026-08-02 OSC/CFA hardening cycle:

- **`pixinsightTrimmed` per-channel variant.** That statistic is a whole-frame
  two-sided trimmed mean, so per-channel scaling is skipped in it and the run
  falls back to the global scalar. A per-channel version needs three trimmed
  means over three interleaved sample sets — a real design step, not a flag.
- **Flat Analysis contour plot on CFA data** block-averages mixed mosaic
  pixels, so its surface reads flatter than the sensor is. Display-only: no
  calibration path consumes those numbers.
- **Matcher-level `bayerpat` parameter.** The configurable matcher compares
  set-level columns, and `calibration_set` carries no Bayer column — a real
  matching rule would need the pattern denormalized onto the set first.
- **Mono-flat-on-OSC is advisory, not blocked.** The readiness dialog says so
  and the batch logs it, then calibrates anyway. Whether that should ever hard
  block is an owner decision, not an engineering one.
- **Per-batch `(flat_path, mode, geometry) → divisor` memo** (perf, no
  behaviour change): the divisor is resolved per FRAME, so a flat with no
  stamped `ATH_FNR/G/B` — every master built before this cycle — is read in
  full once for the constants and again by the band stream, for every light in
  the batch. The flat-vs-light phase-disagreement path likewise re-emits its
  `warn!` once per frame instead of once per batch.
- **Phase-class canonicalization.** `GRBG` at offset (0, 0) and `RGGB` at
  (1, 0) are the same mosaic, but compare unequal everywhere — so a legitimate
  pairing can raise a compatibility advisory and push a perfectly good flat
  onto the recompute path. There is exactly one place to land the fix:
  `CfaGeometry::same_phase`, which the flat-card check and the advisory
  comparison already share.
- **Rename the CFA sites off the colliding `pattern` / `flat_pattern` names**
  (ratified decision 7): `cfa_pattern` for the XISF `<ColorFilterArray>`
  adopt/reject pair, `cfa_flat_pattern` for the `light_cal` flat-vs-light
  warn. Mechanical, but it touches the field dictionary, so it is a decision
  rather than a cleanup — the pre-existing `FlatPattern` sites keep the plain
  names, since they are the older claim on them.
- **Split the two set-level CFA advisories** so `light_patterns` carries one
  shape. `collect_cfa_advisories` gives both the multi-layout note (joined
  list) and the unrecognized-consensus note (single label) `kind: "lights"`,
  so the batch-start log branch cannot tell them apart: the unrecognized case
  is logged under the plural field AND under the multi-layout message,
  *"cfa layouts disagree among light frames"*, which is wrong for it. The fix
  is a third `kind` (a wire change — `kind` rides the readiness payload) or a
  discriminator on `CfaAdvisory`; message text and field name both move with
  it, so it wants doing in one go.

## Post-cycle follow-ups (from the 2026-08-02 final whole-branch review)

Owner decisions queued:

- **Deselecting an AUTO-matched link in the Manual Calibration modal** now shows
  an honest "Link not cleared — auto-matched" notice instead of a fake success,
  but truly removing an auto link needs a manual-block concept (auto-find would
  otherwise re-add it) — new feature, owner call.
- **`frames.is_master = 1` with `is_master_library = 0`** (a master frame not in
  a library set) is NOT force-Copied by frame-set archives — the ratified
  predicate keys on the set flag. Confirm this shape is acceptable or widen.

Engineering follow-ups (none block a release):

- XISF / decode-and-spill float lights still take the 65535 scale-divisor
  fallback (`probe_bitpix` → None) — bit-depth probe for the spill path.
- Stranded 0-byte claim placeholder after a hard crash re-reports a parse error
  on every library scan until swept — claim-sweeper at startup.
- Promote a shared `unregister_master_for_file` helper (the interception loop
  is duplicated at the relinking door).
- Archive planner: cross-role dedup could theoretically out-vote the master
  force-Copy (no production path); zip self-containment untested via
  `zip_reader`; "N master(s) copied, not moved" dialog line per the (N of M)
  convention.
- `calibration_set.instrume` drift on spared superseded sets after a
  unique_camera rename (cosmetic; matcher excludes superseded sets).
- Show All: parameters after a skipping param render as unmatched without
  having been compared (`check_calibration_match` early-return).
- delete_master dialog enrichment staleness race (no per-target guard);
  ConfirmDialog lacks a disabled prop; LIMIT 1 multi-raw-set lookup.
- NFS: `O_EXCL` claim reliability assumption if network library roots ever
  become supported.
- `send_all_to_void` swallows per-file results (`let _`) — pre-existing,
  now hides more.
- Log-level/naming polish: guard lookup-failure `error!`→`warn!`; guard event
  `set_id` vs `master_set_id` ambiguity; duplicate per-frame light-cal debug
  events; warn-emission not log-asserted (docs/logging pattern exists).

Release-note lines owed (next `RELEASE_NOTES.md` rewrite):

- Masters are now always auto-link candidates; fresh default preference is
  PreferMaster; routine re-matching keeps lights on their masters.
- New: Delete Master (Equipment) — un-supersedes and restores the raw set;
  deleting a master's file via Black Hole does the same.
- Light-calibration engine v2: BITPIX-aware output scaling + dead-flat-pixel
  floor — existing calibrated lights show as *stale* and can be re-run once.
- Master builds now exclude undefined (NaN/Inf) pixels with a per-build
  warning instead of failing or silently poisoning the master.
- A failed disk delete of a master is reported honestly (the file would
  otherwise re-ingest as an imported master on the next scan).

From the OSC/CFA hardening cycle:

- New: **per-channel flat scaling for colour (OSC) cameras** — the master flat
  is now normalized separately for red, green and blue instead of by one
  mixed number, removing the colour cast a single scale factor leaves behind.
  On by default for lights that declare a Bayer pattern; mono is unchanged.
- **Bayer metadata you can trust** on master frames and calibrated lights:
  the real `XBAYROFF`/`YBAYROFF` phase (never a fabricated `0`), `ROWORDER`,
  and — on masters — the pattern agreed by the member frames rather than
  whichever member happened to be read first.
- **XISF files that declare their colour filter array the native way** (a
  `<ColorFilterArray>` element instead of a `BAYERPAT` keyword) are now
  recognized as colour throughout calibration.
- Colour lights calibrated before this release show as *stale* once and can be
  re-run to pick up per-channel scaling. Mono frames are untouched.
- Existing master files are not rewritten: their Bayer cards stay as they were
  built. Rebuilding a master that Athenaeum built itself refreshes them.

Owner smoke list (consolidated): the 7 scenarios in the fix plan's final gates,
plus: delete an auto-matched link → "Link not cleared" notice; delete_master
on a real library (3 scenarios in task-8 report); junk flat in a real library
(degenerate-flat per-frame failure); Black-Hole restore of an ex-master
re-ingests as imported; Windows pass for the claim/rename path.
