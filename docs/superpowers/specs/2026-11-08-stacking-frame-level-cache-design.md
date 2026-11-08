# Stacking cache: a frame's own work belongs to the frame — design

Date: 2026-11-08. Status: draft for the owner. Items 1–7 are the owner's direction from the 2026-11-08 discussion;
item 8 (`maxStars` default 9 000) is the owner's ruling of 2026-11-08: it ships without a truth bar (§5.8).

## 1. Intent

Re-stacking the same frames with another grouping must reuse every result that does not depend on the grouping. Today
it reuses nothing: a frame's calibrated file, its star measurement and its registration are filed under the group's
key, and the calibration hash carries a grouping field. Every stage downstream chains on that hash, so one grouping
change recomputes the whole run.

The principle: **the key says whose result it is, the hash says what it was made from.** A result that is a function
of one frame (and the run-wide reference) is keyed on the frame. Only a result that is a function of the group's
membership is keyed on the group.

## 2. Evidence: frame set 270 (Iris, ASI2600MC, 551 lights), prod catalog, 2026-11-07

| Run | Grouping | Group keys | What happened |
| ---- | ---- | ---- | ---- |
| 9 | `seconds`, tolerance 2 s | `osc__NoFilter__bin1__180s` (361), `…__300s` (190) | full run, 80 min |
| 10 | `stops` | `…__st180` (551) | every frame missed by key; 3.5 GB recalibrated in 13 s, then cancelled |
| 11 | `seconds`, tolerance 200 s | `…__180s` (551) | same key as run 9's 180 s group, still every frame missed: Calibrate 683 s (172 GB written), Measure pass 1 1423 s, pass 2 823 s, Register again |

- Run 10 missed by key: `stacking_artifacts_subject_key` is `(subject, group_key, kind, frame)`
  (`db/stacking.rs::find_artifact`), and the files sit under `calibrated/<group_key>/`, `measured/<group_key>/`,
  `fits/<group_key>/`, `registered/<group_key>/`.
- Run 11 missed by hash: `config.rs::calibration_subtree` folds `grouping.exposureToleranceSec`. Every stage hash
  chains on the calibration hash (`plan.rs::measurement_hash_for`, `pass1_hash_for`, `registration_hash_for`,
  `normalization_hash_for`).
- The group β was 1.5 in all three groups (run 9 both groups, run 11 the merged one), so even Measure pass 2 would
  have been a cache hit under a frame key.
- Disk after run 11: the 300 s frames are stored twice (`calibrated/…300s/` 55 GB, `registered/…300s/` 57 GB,
  `measured/…300s/` 1.9 GB, plus their copies under `…180s/`), and `calibrated/…st180/` holds 3.5 GB that no row
  points to.

This was recorded as accepted behaviour in the wave-6 spec (2026-10-02 §8: "the group-keyed artifacts are recomputed
under the new keys; that is the existing behaviour for any regrouping") and noted as broader than intended in research
6b1 §0.2. The `solve` kind already escaped it with `group_key = ""` (research 6b2), as did the project star map
(`registration`, D2). This design supersedes the wave-6 §8 sentence.

## 3. Goals

- A regroup (exposure mode, tolerance, stops width) recomputes only group-level results: the LN reference, its
  calibration, the LN sidecars, rejection and the master. This holds for co-registered geometry (the default); under
  Native geometry each group has its own reference, so a regroup also re-registers, by design.
- Measure pass 1 is computed once per (calibrated content, measurement options), and survives a regroup, a
  recalibration with an identical result, and "Delete intermediates".
- Changing the frame weighting (mode, formula, keyword) re-weighs in seconds and re-measures nothing.
- An OSC frame does not re-register because its group's β moved.
- The plan gate reports the same hits the run will make.
- Items 1–7 recompute no set after the update unless a field that is actually part of its result differs from the
  default. Item 8 changes no stored document, so it recomputes only a set that has no stored config.

## 4. Non-goals

- Pass 2 results kept per β (several variants per frame). Not needed until β moves in practice; on set 270 it is 1.5
  everywhere.
- Content-addressed storage in general (several variants of any kind per frame).
- Any change to what a stage computes, except item 8. Items 1–7 move no pixel and no number; item 8 does.

## 5. Design

### 5.1 Frame-level and group-level kinds (item 1)

| Class | Kinds | Key |
| ---- | ---- | ---- |
| Frame-level | `calibrated`, `calibrated_mosaic` (Prepare writes these too), `measured`, `metrics`, `fits.<plane>`, `registered`, `registration`, `solve` | `group_key = ""` |
| Group-level | `ln`, `ln_reference`, `ln_calibration`, `ln_global`, `two_pass` (new, §5.6), rejection maps | the group key, as today |

- `metrics` and `fits.<plane>` depend on the group β under `Auto`. β is a value: it stays in the hash
  (`measure_beta:{:.6}`), never in the key. Same β → hit; another β → pass 2 only.
- `registered` and `registration` depend on the run-wide reference, already folded as `ref:<id>` plus the reference's
  calibration hash.
- One constant list of frame-level kinds in `stacking/paths.rs`, and one helper that returns the key for a kind
  (`""` or the group key). Every `find_artifact` / `upsert_artifact` call site goes through it: the run
  (`run.rs`, the calibrate, prepare, measure, register and normalize stages) and the plan gate (`plan.rs` at the
  `g.key` lookups: metrics, calibrated, calibrated_mosaic, registered, ln, ln_reference, ln_calibration).
- Paths: frame-level files go to `calibrated/`, `measured/`, `fits/`, `registered/` with no group sub-folder.
  `prepared/` likewise.
- File stem: a function of the frame alone. Today `run.rs::calibrated_file_stem` adds `_f<id>` only when another
  member of the group shares the stem, so a merge can rename a frame's file. New rule: `<stem>_f<frame_id>` always
  (the registered name follows through the same function). Existing rows keep their stored `path`; only new writes use
  the new name, so no file is moved.
- Projects (`StackSubject::Project`) follow the same rule on `project_frame`.

### 5.2 The calibration hash no longer carries the grouping (item 2)

`calibration_subtree` writes `"grouping": {"exposureToleranceSec": 2.0}` as a constant (the shipped default), not the
config's value. Every set at the default tolerance keeps a byte-identical hash, and the golden pins
(`stage_hashes_are_pinned_to_golden_literals`) stay. A set at another tolerance recomputes once. The doc comment says
why the constant is there: it holds the hash shape, it says nothing about the run.

### 5.3 Measure pass 1 survives a recalibration and "Delete intermediates" (item 3)

- `measured` leaves `paths::INTERMEDIATE_ARTIFACT_KINDS`: only `CleanupWhat::All` drops it, as for `metrics` and
  `solve`. A pass-1 file is about 13 MB per frame; on set 270 it costs 24–34 min to rebuild.
- Pass 1's validity follows the calibration hash (the key already folds it), not the calibrated file's size and mtime
  (R11c-4). Precondition: calibration is reproducible to the bit on one machine. Today's evidence is
  `calibrate_fan_out_matches_the_sequential_stage_byte_for_byte` (sequential vs parallel, one run); the plan adds a
  pin that calibrates the same frame twice in two runs and compares the planes. If that pin cannot be made to pass,
  this sub-item is dropped and the mtime stamp stays.
- Pass 2 still reads the pixels. With the calibrated frames deleted and β moved, the run recalibrates and reruns pass 2;
  pass 1 is not rerun.

### 5.4 Weight settings leave the Measure hash (item 4)

`measurement_subtree` writes `weightMode`, `formula` and `keyword` as the shipped defaults (`psfSignalWeight`,
`FormulaWeights::default()`, `"SSWEIGHT"`) instead of the config's values. The weights are computed in every run from
the stored measurements (`run.rs` stage 3, `compute_weights`), and no fit reads these fields (`measure.rs`,
`common_set.rs`). A set with default weights keeps its hashes; a set with custom weights re-measures once. The
reference is still re-picked by weight, and a changed reference re-registers through the existing `ref:<id>` fold.

### 5.5 OSC registration does not fold the measurement hash (item 5)

`registration_hash_for` folds `measure:<hash>` only for a frame whose registration reads Measure's fits: a
1-plane calibrated frame (mono, ruling C-6, and also an OSC frame set calibrated with `debayerOsc` off, whose mosaic
reads `fits.0`), decided by `stage_one_planes(cfg, group, prepare_path) == 1` in the run and the gate alike. A
debayered OSC frame keeps its luminance detection, which reads only `cfg.registration` (already in
`registration_subtree`), so it folds nothing in its place. This reverses ruling C-18's "uniform across mono and OSC"
for debayered OSC; the cost C-18 avoided (a second hash-construction rule) is paid once, in one predicate shared by
the run and the gate, with all three cases pinned.

### 5.6 The two-pass dry pass reuses fresh registrations (item 6)

With `reference.twoPass`, an Auto reference registers its own group in a dry pass before the real one. It is uncached
and runs every time: on set 270 it is 101 s for 551 frames, and it is why run 11's Register shows 1 102 items
(551 dry + 551 real). The stored registration rows cannot stand in for it: they are against the FINAL reference,
which after a switch is not the candidate the dry pass registers against. What is cached instead is the dry pass's
decision: a group-level payload row (kind `two_pass`, `{"switchTo": <frame id> | null}`) keyed on everything the
decision reads — the candidate reference, its calibration hash, the top-weight candidate ids, and every included
member's expected registration hash against the candidate. Registration is deterministic (RQ-1), so equal inputs give
the equal decision; a hit skips the dry pass and builds the switch exactly as today. The row is group-level (it depends
on membership), survives "Delete intermediates" like `metrics`, and drops with "All". The Register progress carries a
message naming the pass ("checking the reference", "aligning"), and a cache hit removes the dry pass from the total.

### 5.7 Migration and disk (item 7)

One schema migration:

1. For the frame-level kinds, set `group_key = ''`. Where a frame has rows under several group keys, keep the newest
   (`created_at`) and delete the others.
2. The rows' `path` is kept, so nothing is moved. Every reader takes a frame-level file's path from its row, never
   from the layout (today the pass-1 spill is recomputed from the layout: `run.rs` `measure_two_pass_prepare`).
3. A writer that replaces a frame-level row whose stored path differs from the new one removes the old file, unless
   another row still points to it.
4. Files no row points to (the deleted duplicates, folders like `calibrated/…st180/`) are removed by "Delete
   intermediates": `calibrated/`, `prepared/`, `registered/` and `ln/` are removed whole, as today, and `measured/`
   and `fits/` (which survive it) get a sweep of the files no row of the subject points to. The migration deletes
   no file. No new `WorkUsage` field.

### 5.8 `measurement.maxStars` default 24 576 → 9 000 (item 8, owner ruling 2026-11-08)

The owner's ruling: 9 000 stars is enough, and the default changes now, without a truth bar. On set 270 the median
plane has 22 138 detected and 20 328 fitted stars, 29 % of planes hit the 24 576 cap, and every plane but two has more
than 9 000. So 9 000 fits about 2.3× fewer stars per plane, and Measure's fit time drops roughly in proportion.

The known risk, recorded so a later reading of the numbers has it: the cap keeps the first N seeds in detection order,
brightest first (`rustafits star_measure::seeds`, `kept.truncate(max_stars)`). That is the selection
`measurement.fitStars = K` makes, and K = 4 000 / 5 000 / 8 000 failed the fast-measurement truth run (R-OSP-14: K1
estimator tilt, K3 families worse than All, K4 14–21 of 32 injected planes moved more than 1 %, worst −20.8 %). No
truth bar or master-level check is run before shipping. `maxStars` also sets LN's fit budget (`normalization_subtree`),
so LN moves too.

Where the default changes:

- `MeasurementConfig::default` (`stacking/config.rs`) and its pins (`config.rs` tests that assert 24 576, the
  `resolve_config` cases, `measure.rs` serde pins); `fitStars` still canonicalizes to `None` at or above `maxStars`,
  and its floor `max(MIN_FIT_STARS, registration.maxStars)` stays below 9 000.
- The probe defaults (`StarPopulation::probe_default` in `stacking/measure.rs`, `examples/ln_scale_probe.rs`) follow,
  so the lab measures what the app runs.
- **Stored documents are not migrated (owner ruling 2026-11-08: "for new ones it will be 9 000").** A stored per-set
  config IS the run's config (whole-config precedence) and carries `maxStars` explicitly, so every set, project,
  preset or global default stored before the update keeps its value (on the prod catalog sets 109, 182 and 270 keep
  24 576), and its caches stay valid. 9 000 reaches a document only when it is created from the built-in default: a
  new set, a new project, or a set with no stored config and no stored global default.

## 6. Hash compatibility, in one table

| Set | Recomputes once after the update |
| ---- | ---- |
| Default tolerance, default weights | nothing (items 1–7) |
| Non-default tolerance | Calibrate and everything after it |
| Custom weights | Measure and everything after it |
| A set with no stored config and no stored global default (item 8) | Measure, Register, LN |

## 7. Tests

- Regroup reuse: a fixture set stacked under `seconds`, then `stops`, then a wide tolerance: the second and third runs
  calibrate 0 frames, run pass 1 on 0 frames, and register 0 frames; LN and Integrate run. The plan gate reports the
  same before each run.
- Golden pins: `stage_hashes_are_pinned_to_golden_literals` unchanged; new pins for the tolerance-blind calibration
  hash, the weight-blind measurement hash, and the OSC / mono registration branches.
- Stem: a frame's calibrated and registered names do not change when the group's membership changes.
- Migration: duplicate rows collapse to the newest; paths are kept; unreferenced files are counted and removed only by
  the cleanup.
- Pass 1 (item 3): the cross-run calibration identity pin; `measured` survives "Delete intermediates".
- Dry pass (item 6): with fresh rows the dry pass aligns 0 frames and picks the same reference as an uncached dry pass.
- Item 8: the default pins at 9 000; a stored document holding 24 576 still resolves to 24 576.

## 8. Open questions for the owner

1. Item 3: if a cross-run calibration identity pin fails on some platform, drop the sub-item there (recommended) or
   keep the mtime stamp everywhere?
