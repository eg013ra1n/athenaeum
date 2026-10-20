# Calibration geometry — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: `lean-execution` (it overrides superpowers:subagent-driven-development wherever the two conflict), or superpowers:executing-plans for inline execution. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A calibration set of another frame size can never calibrate a frame: not in auto-link, not in the manual list, not by a manual assignment, not through the gates. A set of the same size at another sub-frame origin (both sides record one) is never auto-linked, but a manual choice of it is allowed with a visible warning (owner, G-7). Masters built in the app carry the origin their members agree on (G-5), so the origin rule can judge them. A link that already mismatches in size is refused up front by the readiness and stacking-plan gates, naming the set and both sizes, instead of killing a stacking run minutes in.

**Architecture:**
- **One owner of the rule:** a new core module `calibration::geometry` (ungated, so the headless build keeps it). It reads a frame's size from `frames.naxis1/naxis2`, a set's size as its members' agreed size, and the sub-frame origin `XORGSUBF`/`YORGSUBF` lazily from the stored header blob (`fits_header.header`, via the existing `parse_stored_header_keys`). `fit(source, calibration)` answers `Fits | Unknown | Refused(Size | Origin | Mixed)`. Every other site asks it; none re-implements it. `Refused(Size | Mixed)` is hard everywhere; `Refused(Origin)` is hard for automatic choices only (G-7).
- **Masters (G-5):** `calibration_library::headers` stamps `XORGSUBF`/`YORGSUBF` into a built master when every member records the same origin (Task 1b).
- **Candidate choice:** `configurable_matcher::find_calibration_candidates` (auto-link AND the manual list) aggregates set sizes in its own query and refuses any geometry misfit as a hard filter (so auto-link never takes one) with a `geometry` verdict: `mismatch` for size, `warning` for origin; raw-frame grouping (`detect_flat_groups` / `detect_dark_groups` / `detect_bias_groups`) takes the source size; the flat pattern path and the on-demand dark/bias path judge each GROUP before any set row is written; a manual assignment refuses a size misfit and accepts an origin misfit with a warning; `select_flat_precal` does the same; Find Calibration removes automatic links that no longer fit (size or origin).
- **Gates:** `api::lights::readiness_from_data` computes `ExportReadiness.geometry_mismatches` (size, a blocker) and `geometry_warnings` (origin, advisory) once, reusing its own first-loop links; `check_mode_ready` (calibrated-lights), the stacking plan (`links` blocker, plus a plan warning for origin) and the collab verdict all read those fields, so the features cannot disagree.
- **UI:** the picker card shows the set's size (+ origin), cannot select a size misfit (but can still deselect the one linked now), and shows an origin misfit as selectable with a warning badge; the calibration hierarchy's flat/dark/bias tables get a Size column badged against the lights, warning on another origin; the Export tab mirror, its warning list and the Calibrate panel read the new fields.

**Tech Stack:** Rust (`athenaeum-core`: rusqlite, serde, ts-rs via `tests/ts_contract.rs`, tracing). React/TS (vitest, @testing-library/react). No new dependency.

**Spec:** `docs/superpowers/open-items.md` → "Crucial bugs" → "Calibration matching ignores frame geometry — stacking dies on a cropped dark (2026-10-01)" (symptom, root cause, fix direction), plus the owner and controller rulings restated below. Read the entry before Task 1.

---

## Rulings

**Owner (2026-10-20, both "да").**
- **O1.** Geometry (NAXIS1 × NAXIS2) is a HARD requirement for every source → calibration pair (lights → flat/dark/bias, flats → darkflat/dark/bias, darks → bias), in auto-link (`configurable_matcher`) AND in the manual-selection candidate list. A different-sized frame can never calibrate. It is not a `MatchMode` parameter the user can relax.
- **O2.** ROI origin (`XORGSUBF`/`YORGSUBF`) is read from the stored raw header, compared ONLY when both sides carry it; absent on either side → no block.

**Controller (kept, with what the exploration found).**
- **R1.** NULL `naxis1`/`naxis2` on either side → no block, a `warn!`. *Counted on a read-only copy of the dev catalog (`sqlite3 "file:…/com.vsharifov.athenaeum.dev/athenaeum.db?immutable=1"`, 2026-10-20): 0 of 40,456 frames have a NULL size (every image type, masters included); 0 of 1,079 calibration sets have members that disagree on size.* The warn is emitted once per call with a `count`, never per candidate.
- **R2.** Existing mismatched links are not silently rewritten: the stacking plan gate blocks under `links` naming the set ids and both sizes; the same check covers a flat set against its pre-calibration links; the check lives in `readiness_from_data`, which the export gate shares. *The dev catalog holds 50 such light links today, all manual Dark overrides of frame set 182 onto 1242/1315; 0 mismatched set → set links.* (R9 below narrows R2 for AUTOMATIC links only, with its reason.)
- **R3.** The manual-pick dialog and the calibration hierarchy show each set's geometry (W×H, plus origin when known).
- **R4.** Geometry is compared after binning. *Verified, not assumed:* the FITS parser stores `NAXIS1/NAXIS2` (the array as written) and the XISF parser the `NAXIS` keywords or the `<Image geometry="w:h:c">` attribute (`fits_parser/mod.rs:785`) — both the stored, binned array. In the dev catalog every 2×2 ASI6200MM frame is 4784×3194 (9576/2 = 4788, stored 4784). Binning stays its own matcher parameter; sizes compare as stored.
- **R5.** Built masters are `frames` rows registered like scanned files. *Verified:* registration parses the written file (direct-registration invariant), and in the dev catalog all 11 in-app masters (`master_provenance`) have NAXIS filled and equal to their source sets' members. Task 1 pins the registration side with a test. From this plan on a built master also carries `XORGSUBF/YORGSUBF` when every member records the same origin (G-5, Task 1b — the header builder wrote none before, `grep ORGSUBF` over `crates/` and `rustafits/` was empty on 2026-10-20). A master built before this change, or from members that disagree or record none, records no origin; the origin is then not compared for it (O2), and a rebuild stamps it.

**Plan rulings (new; the controller may veto each with a reason).**
- **R6. The raw export modes are not blocked.** `rawWithMasters` / `rawWithCalibrationSets` hand files to another tool and calibrate nothing in-app; only `calibratedLights` (and the stacking plan) refuse a misfit. Confirmed by the controller (G-6).
- **R7. `select_flat_precal` refuses a size misfit, it does not skip it.** A linked pre-calibration master of another size (or of mixed sizes) returns `ApiError::Invalid` naming both ids and sizes. Skipping it (the exposure precedent in the same function) would silently build a flat with a different pre-calibration than the one linked; the engine already refuses the same build, only minutes later after reading every flat. A master of the same size at another recorded origin is used as linked, with a `warn!` and a line in the selection's warnings (G-7: such a link is a manual choice — auto-link never makes it, Find Calibration removes a stale automatic one).
- **R8. What the readiness pass checks.** A light's Dark and Flat links, and its Bias link only when no dark RESOLVED for it — the same `dark.is_none()` fact the missing-master-file pass right above already keys on (review fix #1: a Dark link to a set with no built master leaves the engine reading the bias), never the bare existence of a Dark link (review I-4). For every flat set the run builds (a raw Flat linked by an in-scope light) or rebuilds (the provenance source of a missing master flat): EVERY DarkFlat / Dark / Bias pre-calibration link, not only the one the chain would pick, because which one wins depends on what stage 0.5 builds first. The pass reuses the links the first loop already read (review I-3) and reads a light's origin only when a linked set of the same size records one. A size or mixed refusal lands in `geometry_mismatches` (a blocker); an origin refusal in `geometry_warnings` (advisory, G-7).
- **R9. Find Calibration removes an AUTOMATIC link that no longer fits** — size, mixed or origin (G-2, G-7). At the start of `store_calibration_hierarchy`, every `is_manual_override = 0` link of the light — and of the flat and dark sets it now uses — whose set fails `fit` is deleted and `warn!`-logged. Reason: an automatic light link cannot be cleared from the UI (the picker's deselect clears manual links only — `CalibrationHierarchyView` says "This link was auto-matched — deselect only affects manual assignments"), and an upsert only replaces a link of the SAME type when a NEW candidate is found; without this, a geometry-stale automatic link (e.g. a flat's old Dark sub-link when the fallback now lands on Bias) would block the gate forever. Manual links are never touched (R2): the gate names a size misfit, warns on an origin one, and the picker's Reset clears them.
- **R10. Origin from the blob, no new column.** A `frames` column would be a `Frame` field (56 literal constructions in 29 files), a scanner write and a backfill. The blob is read lazily: the matcher reads a set's origin only when the sizes agree, or — in the manual list — for a candidate that passes the parameter filter (without Show all the picker shows no refused set but the linked one, and a parameter-refused set's card still shows its origin on the facts line, from the set detail — review M-3); a frame's origin at most once per call. A picker open therefore parses at most one stored header per same-type set that passes the parameters — acceptable for hundreds of sets. A set's origin is its lowest-frame-id member's (the `calibration_library::headers` stored-header precedent).
- **R11. A manual assignment refuses a size misfit** (`manual_assign_calibration`, `manual_assign_subcalibration`), for every frame, before anything is written; it ACCEPTS a same-size set at another recorded origin, with one `warn!` (G-7 — the dialog showed the warning before the user chose it, the hierarchy and the gates show it after). The picker greys a size-misfit card out, except the one linked now, which stays clickable so the user can deselect it; an origin-misfit card is never greyed out — it carries a warning badge. Reason: O1 — size is not a parameter the user can relax; the origin is, by hand only (G-7).
- **R12. A set whose members disagree on size is "mixed" and fits nothing.** Set formation (scan grouping keys, `check_for_existing_*_set`) is NOT changed: 0 mixed sets in the dev catalog, the integrator already refuses mixed members, and a refused set is visible, never silently wrong. Splitting is a follow-up only if one ever appears.
- **R13. Logging field names.** New: `source_geometry` / `set_geometry` (string, `Geometry::label`), `geometry_mismatches` / `geometry_warnings` (usize, on `"export readiness computed"`); registered (already used by existing events, missing from the dictionary — review M-4): `calibration_type`, `source_set_id` (the set being calibrated on a set → set link event), `master_set_id` (a master calibration set's id where the event also names another set), `age_days` (a flat group's age on the existing `"pattern selected flat match"` debug, which Task 3 re-emits). Sizes on the grouping debugs reuse the registered `width` / `height`. Task 8 adds them to the logging spec's dictionary.

## Controller rulings after the plan (G-1…G-8)

Binding; source `.superpowers/sdd/2026-10-20-calib-geometry/rulings.md`.
- **G-1 (acceptance catalog).** Task 9 makes its own copy of the production catalog the allowed way — `sqlite3 "file:<prod>?immutable=1" ".backup <copy>"`, run by the controller after the owner's word, never writable — repoints every folder root (stacking working/output, calibration library, transfers, collaboration) to scratch BEFORE the app starts, and STOPS if the copy does not show the described state. → Global Constraints, Task 9.
- **G-2 (R9 accepted).** Find Calibration deletes AUTOMATIC links that no longer fit; manual links are never touched (the gate names a size misfit). → R9, Task 3.
- **G-3 (tests).** A fixture that registers masters at another size than its lights is fixed; the gate is never weakened. → Global Constraints, Task 6 (baseline + proven fixture list, review I-5).
- **G-4 (the hierarchy view).** A failed manual apply raises `notify()` (kind `generic`, review M-1), never console only. → Task 7.
- **G-5 (Q2: in this plan).** An in-app master stamps `XORGSUBF`/`YORGSUBF` when every member records the same origin; mixed or absent → no stamp and a `warn!`. Read here as: members that disagree, or SOME that record none, warn (an origin is lost); when NO member records one there is nothing to carry and the event is a `debug!` (otherwise every master of a camera without sub-frame readout would warn). → R5, new Task 1b, Task 8 docs.
- **G-6 (Q3, R6 accepted).** Raw export modes stay ungated; the calibrated-lights mode and stacking are gated through the shared readiness check. → R6, Task 6.
- **G-7 (OWNER 2026-10-20: "Авто нет, вручную можно" — auto no, manual yes).** Same NAXIS, different recorded origin: auto-link never links the pair and Find Calibration deletes such automatic links; a manual assignment is allowed with a visible warning in the manual-pick dialog (a selectable card with a warning badge, never a locked one), the hierarchy and the stacking plan / export readiness (a warning, never a `links` blocker). NAXIS stays hard everywhere (auto, manual, gates). We never crop: calibration only when sizes match. → R7, R8, R9, R11; Tasks 2, 3, 4, 5, 6, 7, 8; Review Focus 4.
- **G-8 (review accepted in full).** C-1 → Task 1b; C-2 → Task 9 Steps 1–2 + Global Constraints; I-1 → this block + Open questions closed; I-2 → Task 3 (every group judged before any `create_*_calibration_set`); I-3, I-4 → Task 6 `geometry_tally` + R8; I-5 → Task 6 Steps 0 and 4; M-1 → Task 7; M-2 → Task 9 Step 5; M-3 → Task 2 + R10; M-4 → R13 + Task 8; M-5 → Task 6 test comment; M-6 → Task 2 `rule_weight` doc; M-7 → Task 2 origin test.

## Open questions for the owner — all answered

- **Q1 → G-7.** The dev catalog's master dark set 1518 (`masterDark_CROP-BIN-1_6384x4258_EXPOSURE-300.00s.xisf`, built outside Athenaeum) records `XORGSUBF = 0 / YORGSUBF = 0`, while every 6384×4258 sub in that catalog records 1584 / 1064. Auto-link never takes it for those ROI lights; the user may still choose it by hand, with a warning.
- **Q2 → G-5.** In-app masters carry the members' agreed origin — in this plan (Task 1b), not a follow-up.
- **Q3 → G-6.** R6 confirmed: the raw export modes stay unblocked.

## Global Constraints

- **Worktree / branch:** `/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/.claude/worktrees/calib-geometry`, branch `calib-geometry` (from `main` 068cf2d6). Work only there. Never push; merge only on the owner's word.
- **Never read or write the production catalog** (`~/Library/Application Support/com.vsharifov.athenaeum/athenaeum.db`) — with ONE exception, Task 9 Step 1 (ruling G-1): the controller, after the owner's word, reads it once with `immutable=1` into a `.backup` that becomes the `.dev` copy. Nothing ever opens it writable, and no implementer task touches it. Real-data reads go to the `.dev` sibling, opened `immutable=1` unless a step says otherwise.
- **Copy rule:** never name a third-party astro program or project anywhere — code, comments, docs, commit messages.
- **Commits** as the configured user (`eg013ra1n` / `vilen.sharifov@gmail.com`; pass no `-c user.*`), by explicit `git add <files>` after `git status`, each message ending with exactly:
  ```
  Co-Authored-By: Claude <your model name> <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01NDfRyApE6tYi9ZMD7b4g8m
  ```
- **Build discipline (this Mac):** every cargo call runs with
  ```bash
  export CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/.superpowers/target-geom
  export CARGO_BUILD_JOBS=4
  ```
  and tests with `-- --test-threads=4`. One compile at a time. No `cargo fmt`. Use `rustfmt <file>` only on a file that is rustfmt-clean before your edit (`rustfmt --check <file>` prints nothing); otherwise format your hunks by hand. **Never rustfmt** `stacking/run.rs`, `stacking/plan.rs`, `calibration_library/light_cal.rs`, `stacking/measure.rs`. `cargo check -p athenaeum-core --lib` stays warning-free; clippy is not a gate.
- **Headless build stays green:** `cargo check -p athenaeum-core --no-default-features`. `calibration::*` and `api::calibration` are ungated; `api::lights`, `api::masters`, `calibration_library` are `render`; `stacking` is `render + solver`. `calibration::geometry` names no gated type.
- **Errors and logging:** `tracing` only, short stable message, snake_case fields from the dictionary (+ R13's names). Never swallow an error: log at `warn!`/`error!` before degrading; every refusal is logged before it returns.
- **Two backends:** no command is added, removed or renamed. New fields ride existing payloads (`ExportReadiness`, `CalibrationSetDetail`, `LightFrameWithCalibration`, `CalibrationSetWithScore.parameters`); both hosts return the same core structs, so neither host changes beyond what the compiler asks for (it should ask for nothing).
- **TypeScript types:** `src/types/` changes only in Tasks 5 and 6, regenerated (`TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`), never hand-edited. Every other task ends with `git diff --exit-code src/types`.
- **UI:** design tokens only (`text-warning`, `text-content-muted`, …), no raw colours; `notify()` for failures; no `@tauri-apps/*` outside `src/api/`.
- **Tests: never loosen an assertion to make a test pass** (G-3). A pre-existing test that fails because its fixture links a master of another size than its lights (and is not about geometry) gets its fixture sizes aligned — only after the mismatch is PROVEN (the light's `width/height` beside the master size the fixture passes) and only against a baseline recorded before the change (Task 6 Steps 0 and 4); every such test is named in the commit message. A test whose intent IS a size mismatch, and anything else that fails, is stop-and-report. The gate is never weakened.
- **Gate per task** (the controller re-runs it):
  ```bash
  cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/.claude/worktrees/calib-geometry
  cargo check -p athenaeum-core --lib
  cargo check -p athenaeum-core --no-default-features
  cargo test -p athenaeum-core --lib <the task's filter> -- --test-threads=4
  ```

## Review Focus

The inputs the rulings imply that a person will meet first; each line names the test that pins it.

1. **The owner's 151 lights manually linked to a ROI master.** The picker shows that set as "Linked now" and refused on size; it must stay clickable so the user can deselect it (clearing the override), while every other set of another size cannot be chosen. → Task 7, `CandidateCard.test.tsx` "the linked-now refused set stays deselectable".
2. **A geometry-stale AUTOMATIC link with no fitting replacement** (a flat's old ROI Dark sub-link after the fallback lands on Bias). Find Calibration removes it; a manual link beside it survives and the gate names it. → Task 3 `store_prunes_unfit_auto_links_and_keeps_manual_ones`; Task 6 `readiness_names_a_manual_misfit`.
3. **A manual selection of lights with mixed sizes.** Candidates are judged against the most common size; the assignment refuses if ANY frame misfits and writes nothing. → Task 2 `synthesized_lights_carry_the_most_common_size`; Task 4 `manual_assign_refuses_when_one_frame_misfits_and_writes_nothing`.
4. **An externally built master that records origin 0,0 for ROI lights at 1584,1064** (dev set 1518; G-7). Auto-link never takes it (a light that records no origin is not refused); the manual list shows it (Show all, or Linked now) as a selectable card with a warning badge; a manual assignment of it is accepted with one `warn!`; the hierarchy, the export readiness and the stacking plan warn, never block. → Task 2 `geometry_origin_compared_only_when_both_sides_record_it` (the landing spot, review M-7); Task 3 `store_prunes_an_auto_link_at_another_origin_and_keeps_a_manual_one`; Task 4 `manual_assign_accepts_another_origin_with_a_warning`; Task 5 `hierarchy_view_warns_on_another_origin`; Task 6 `readiness_reports_an_origin_misfit_as_a_warning`, `plan_warns_on_masters_at_another_origin`; Task 7 `CandidateCard.test.tsx` "another sub-frame origin is selectable, with a warning badge".
5. **A set whose members disagree on size.** Refused everywhere with the label "mixed sizes", never averaged into a size. → Task 1 `set_geometry_uniform_mixed_and_unrecorded`; Task 2 `geometry_unrecorded_blocks_nothing_and_mixed_sizes_fit_nothing`.
6. **A master built in the app from ROI subs** (G-5). It records the origin its members agree on; members that disagree leave it unstamped with a `warn!`. → Task 1b `master_cards_carry_the_members_agreed_origin`, `master_origin_is_not_stamped_when_members_disagree_or_some_record_none`, `registered_master_records_its_stamped_origin`.
7. **A raw group the rule refuses during Find Calibration** (review I-2). No `calibration_set` row is written for it — the flat pattern path and the on-demand dark/bias path judge the group first. → Task 3 `flat_pattern_path_takes_the_nearest_group_that_fits`, `on_demand_dark_skips_a_group_at_another_origin_without_writing_it`.

## File map

| File | Responsibility | Task |
| ---- | ---- | ---- |
| `crates/athenaeum-core/src/calibration/geometry.rs` (new) | the rule: `Geometry`, `fit`, size/origin readers, `geometry_for_frames`, `LazyFrameGeometry`, `test_support::count_warns`; later `prune_unfit_auto_links`, `fill_set_origin`, `origin_mismatch_for_lights`, `GeometryMismatch`, `mismatch_sentence`, `origin_sentence` | 1, 3, 5, 6 |
| `crates/athenaeum-core/src/calibration/mod.rs` | `pub mod geometry;` | 1 |
| `crates/athenaeum-core/src/fits_parser/mod.rs` | test: XISF `<Image geometry>` fills NAXIS | 1 |
| `crates/athenaeum-core/src/calibration_library/register.rs` | tests: a registered master's NAXIS = its file's; its stamped origin reaches the stored header | 1, 1b |
| `crates/athenaeum-core/src/calibration_library/headers.rs` | `MasterHeaderInputs.origin`, members' agreed origin, `XORGSUBF`/`YORGSUBF` cards (G-5) | 1b |
| `crates/athenaeum-core/src/calibration/finder.rs` | `CandidateMatchDetails.geometry` | 2 |
| `crates/athenaeum-core/src/calibration/configurable_matcher.rs` | size aggregate in the candidate query, geometry hard filter (origin = warning verdict), near-miss weight | 2 |
| `crates/athenaeum-core/src/calibration/manual.rs` | `geometry` verdict; synthesized frames carry size + representative file | 2, 5 |
| `crates/athenaeum-core/src/calibration/{flat_groups,dark_bias_groups,flat_matcher}.rs` | size filter in raw-frame grouping | 3 |
| `crates/athenaeum-core/src/calibration/hierarchy.rs` | on-demand sizes, every group judged before a set is written, prune at store; set origin in its detail | 3, 5 |
| `crates/athenaeum-core/src/api/calibration.rs` | manual assignment: size refused, origin accepted with a warning | 4 |
| `crates/athenaeum-core/src/models.rs` | `CalibrationSetDetail.xorgsubf/yorgsubf`, `LightFrameWithCalibration.naxis1/naxis2`, `ParameterVerdict.name` doc, `CalibrationWarning.warning_type` comment | 5 |
| `crates/athenaeum-core/src/db/{calibration_links,equipment}.rs`, `calibration/processor.rs` | fill the new fields; the hierarchy's origin warning | 5 |
| `crates/athenaeum-core/src/api/lights.rs` | `geometry_mismatches` / `geometry_warnings` + `check_mode_ready` arm | 6 |
| `crates/athenaeum-core/src/api/masters.rs` | `select_flat_precal`: size refused, origin warned | 6 |
| `crates/athenaeum-core/src/stacking/plan.rs` | `links` blocker, origin plan warning | 6 |
| `crates/athenaeum-core/src/collab/gate.rs` | classify the new sentence | 6 |
| `crates/athenaeum-core/src/ts_export.rs`, `src/types/models.ts` | register / regenerate | 5, 6 |
| `src/components/calibration/{pickerModel.ts,CandidateCard.tsx,CalibrationTableView.tsx}` | geometry label, locked size-misfit card, origin warning badge, Size column + origin warning | 7 |
| `src/components/export/{modeBlocker.ts,ExportTab.tsx}`, `src/components/stacking/panels/CalibratePanel.tsx`, `src/components/CalibrationHierarchyView.tsx` | gate mirror, origin warning line, readout, failure notify | 7 |
| `docs/catalog/README.md`, `docs/export/calibrated-lights.md`, `docs/stacking/README.md`, `docs/masters/README.md`, logging spec, `CLAUDE.md`, `docs/superpowers/open-items.md` | the reference | 8 |

| # | Task | Kind |
| ---- | ---- | ---- |
| 1 | `calibration::geometry` core | Rust |
| 1b | In-app masters carry the members' agreed sub-frame origin (G-5) | Rust |
| 2 | Candidate engine: geometry as a hard filter + manual verdict (origin = warning) | Rust |
| 3 | Auto-link paths: grouping sizes, groups judged before any set is written, prune stale auto links | Rust |
| 4 | Manual assignment: size refused, origin accepted with a warning | Rust |
| 5 | Geometry in the set/light models, the hierarchy's origin warning + TS | Rust + TS types |
| 6 | Readiness (blockers + origin warnings), plan gate, pre-calibration refusal, collab verdict | Rust + TS types |
| 7 | Frontend | TS/React |
| 8 | Docs + full gate | Docs + gate |
| 9 | Real-data acceptance on a `.dev` copy of the owner's catalog | Controller/owner |

---

### Task 1: `calibration::geometry` core

**Env (every cargo call in this task):** `export CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/.superpowers/target-geom CARGO_BUILD_JOBS=4`, run from the worktree root, tests always `-- --test-threads=4`, one compile at a time (Global Constraints).

**Files:**
- Create: `crates/athenaeum-core/src/calibration/geometry.rs`
- Modify: `crates/athenaeum-core/src/calibration/mod.rs` (add `pub mod geometry;` after `pub mod finder;`)
- Test: `geometry.rs` (`mod tests`), `crates/athenaeum-core/src/fits_parser/mod.rs` (existing test module), `crates/athenaeum-core/src/calibration_library/register.rs` (existing test module)

**Interfaces:**
- Consumes: `crate::fits_parser::stored_header::parse_stored_header_keys(FileFormat, &str) -> HashMap<String, String>` (keys UPPERCASE); `crate::models::{FileFormat, Frame}`.
- Produces (all later tasks rely on these exact names):
  - `pub struct Geometry { pub size: Option<(i32, i32)>, pub origin: Option<(i32, i32)>, pub mixed: bool }` (`Copy`, `Default`, `PartialEq`); `Geometry::label(&self) -> String`.
  - `pub enum Refusal { Size, Origin, Mixed }`; `pub enum Fit { Fits, Unknown, Refused(Refusal) }`; `Fit::refused(self) -> bool`.
  - `pub fn fit(source: &Geometry, calibration: &Geometry) -> Fit`
  - `pub fn size_of(frame: &Frame) -> Option<(i32, i32)>`
  - `pub(crate) fn size_from_aggregate(w_min: Option<i32>, w_max: Option<i32>, h_min: Option<i32>, h_max: Option<i32>) -> (Option<(i32, i32)>, bool)` — `(size, mixed)`
  - `pub fn origin_from_keys(&HashMap<String, String>) -> Option<(i32, i32)>`; `pub fn origin_from_header(FileFormat, &str) -> Option<(i32, i32)>`
  - `pub fn origin_for_file(conn, file_id: i64) -> rusqlite::Result<Option<(i32, i32)>>`
  - `pub fn geometry_for_frame(conn, frame_id: i64) -> rusqlite::Result<Geometry>`
  - `pub fn set_origin(conn, set_id: i64) -> rusqlite::Result<Option<(i32, i32)>>`
  - `pub fn geometry_for_set(conn, set_id: i64) -> rusqlite::Result<Geometry>`
  - `pub fn geometry_for_frames(conn, frame_ids: &[i64]) -> rusqlite::Result<Geometry>` — what a set made of exactly those frames would record (Task 3 judges a raw group with it BEFORE any set row is written, review I-2).
  - `pub struct LazyFrameGeometry` with `load(conn, frame_id)`, `fit(&mut self, conn, &Geometry) -> rusqlite::Result<Fit>`, `geometry(&self) -> Geometry`, `report_label(&self, Refusal) -> String` — a frame whose stored header is parsed only when a same-size set records an origin (Tasks 5 and 6, review I-3).
  - `#[cfg(test)] pub(crate) mod test_support` with `fits_header_text(origin) -> String`, `seed_file(conn, tag, origin) -> i64` (files.id), `seed_frame(conn, tag, imagetyp, size, origin) -> i64` (frames.id), `seed_set(conn, imagetyp) -> i64`, `add_members(conn, set_id, &[i64])`, `file_id_of(conn, frame_id) -> i64`, `count_warns(message, f) -> (R, usize)` (WARN events with exactly that message while `f` runs — Tasks 1b and 4).
  - Every DB function returns `rusqlite::Result` so it composes with `?` in anyhow, rusqlite and `ApiError` callers alike.

- [ ] **Step 1: Write the module skeleton and its failing tests**

Create `geometry.rs` holding only these two test modules (Step 3 adds the code above them):

```rust
#[cfg(test)]
pub(crate) mod test_support {
    use rusqlite::{params, Connection};

    /// A FITS stored header in the scanner's shape (80-column cards joined by
    /// `\n`, `FitsHeader::to_header_text`) that records `origin` when given.
    pub(crate) fn fits_header_text(origin: Option<(i32, i32)>) -> String {
        let mut cards = vec![format!("{:<80}", "SIMPLE  =                    T")];
        if let Some((x, y)) = origin {
            cards.push(format!("{:<80}", format!("XORGSUBF= {x:>20} / Subframe X position")));
            cards.push(format!("{:<80}", format!("YORGSUBF= {y:>20} / Subframe Y position")));
        }
        cards.push(format!("{:<80}", "END"));
        cards.join("\n")
    }

    /// One FITS `files` row (`/geometry/<tag>.fits`, tags must be unique per
    /// database) with a stored header. Returns `files.id`.
    pub(crate) fn seed_file(conn: &Connection, tag: &str, origin: Option<(i32, i32)>) -> i64 {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format)
             VALUES (?1, ?2, 0, '2025-01-01T00:00:00Z', 'FITS')",
            params![format!("/geometry/{tag}.fits"), format!("{tag}.fits")],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO fits_header (file_id, header) VALUES (?1, ?2)",
            params![file_id, fits_header_text(origin)],
        )
        .unwrap();
        file_id
    }

    /// One `frames` row of `imagetyp` on a fresh file. Returns `frames.id`.
    pub(crate) fn seed_frame(
        conn: &Connection,
        tag: &str,
        imagetyp: &str,
        size: Option<(i32, i32)>,
        origin: Option<(i32, i32)>,
    ) -> i64 {
        let file_id = seed_file(conn, tag, origin);
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp, naxis1, naxis2) VALUES (?1, ?2, ?3, ?4)",
            params![file_id, imagetyp, size.map(|s| s.0), size.map(|s| s.1)],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// A bare `calibration_set` row of `imagetyp`. Returns its id.
    pub(crate) fn seed_set(conn: &Connection, imagetyp: &str) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date) VALUES (?1, '2025-01-01')",
            params![imagetyp],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// Join `frame_ids` into calibration set `set_id`.
    pub(crate) fn add_members(conn: &Connection, set_id: i64, frame_ids: &[i64]) {
        for &frame_id in frame_ids {
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                params![set_id, frame_id],
            )
            .unwrap();
        }
    }

    pub(crate) fn file_id_of(conn: &Connection, frame_id: i64) -> i64 {
        conn.query_row("SELECT file_id FROM frames WHERE id = ?1", [frame_id], |r| r.get(0))
            .unwrap()
    }

    /// Run `f` under a thread-local subscriber and count the WARN events
    /// whose message is exactly `message` — the in-process pattern of
    /// `stacking::config::tests::resolve_counting_warns`, narrowed to one
    /// event so an unrelated warn cannot pass or fail the count.
    pub(crate) fn count_warns<R>(message: &'static str, f: impl FnOnce() -> R) -> (R, usize) {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        use tracing_subscriber::layer::SubscriberExt;
        struct Message(Option<String>);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = Some(format!("{value:?}"));
                }
            }
        }
        struct Count(Arc<AtomicUsize>, &'static str);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Count {
            fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
                if *event.metadata().level() != tracing::Level::WARN {
                    return;
                }
                let mut m = Message(None);
                event.record(&mut m);
                if m.0.as_deref() == Some(self.1) {
                    self.0.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
        let n = Arc::new(AtomicUsize::new(0));
        let subscriber = tracing_subscriber::registry().with(Count(n.clone(), message));
        let out = tracing::subscriber::with_default(subscriber, || {
            tracing::callsite::rebuild_interest_cache();
            f()
        });
        (out, n.load(Ordering::SeqCst))
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::db::schema::init_db;

    const FULL: (i32, i32) = (9576, 6388);
    const ROI: (i32, i32) = (6384, 4258);

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    fn g(size: Option<(i32, i32)>, origin: Option<(i32, i32)>) -> Geometry {
        Geometry { size, origin, mixed: false }
    }

    #[test]
    fn fit_truth_table() {
        let full = g(Some(FULL), None);
        assert_eq!(fit(&full, &full), Fit::Fits);
        assert_eq!(fit(&full, &g(Some(ROI), None)), Fit::Refused(Refusal::Size));
        // Origin only when BOTH record it (O2).
        assert_eq!(fit(&g(Some(ROI), Some((1584, 1064))), &g(Some(ROI), Some((0, 0)))), Fit::Refused(Refusal::Origin));
        assert_eq!(fit(&g(Some(ROI), Some((1584, 1064))), &g(Some(ROI), None)), Fit::Fits);
        assert_eq!(fit(&g(Some(ROI), None), &g(Some(ROI), Some((0, 0)))), Fit::Fits);
        // An unrecorded size blocks nothing (R1).
        assert_eq!(fit(&g(None, None), &full), Fit::Unknown);
        assert_eq!(fit(&full, &g(None, None)), Fit::Unknown);
        // A mixed set fits nothing, whatever the source (R12).
        let mixed = Geometry { size: None, origin: None, mixed: true };
        assert_eq!(fit(&full, &mixed), Fit::Refused(Refusal::Mixed));
        assert_eq!(fit(&g(None, None), &mixed), Fit::Refused(Refusal::Mixed));
        assert!(Fit::Refused(Refusal::Size).refused() && !Fit::Unknown.refused() && !Fit::Fits.refused());
    }

    #[test]
    fn label_spelling() {
        assert_eq!(g(Some(FULL), None).label(), "9576×6388");
        assert_eq!(g(Some(ROI), Some((1584, 1064))).label(), "6384×4258 @ 1584,1064");
        assert_eq!(g(None, None).label(), "size not recorded");
        assert_eq!(Geometry { size: None, origin: None, mixed: true }.label(), "mixed sizes");
    }

    #[test]
    fn origin_from_fits_cards_xisf_xml_and_keyword_dump() {
        assert_eq!(origin_from_header(FileFormat::FITS, &fits_header_text(Some((1584, 1064)))), Some((1584, 1064)));
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<xisf version="1.0"><Image geometry="6384:4258:1">
<FITSKeyword name="XORGSUBF" value="1584" comment="Subframe X position"/>
<FITSKeyword name="YORGSUBF" value="1064" comment="Subframe Y position"/>
</Image></xisf>"#;
        assert_eq!(origin_from_header(FileFormat::XISF, xml), Some((1584, 1064)));
        // The plain "KEY = value" dump some writers leave (seen on XISF rows of
        // the dev catalog) goes through the dispatcher's fallback.
        let dump = "XISF FITS Keywords:\n==================\n\nXORGSUBF = 0\nYORGSUBF = 0\n";
        assert_eq!(origin_from_header(FileFormat::XISF, dump), Some((0, 0)));
    }

    #[test]
    fn origin_needs_both_keywords() {
        let mut keys = HashMap::new();
        keys.insert("XORGSUBF".to_string(), "1584".to_string());
        assert_eq!(origin_from_keys(&keys), None);
        keys.insert("YORGSUBF".to_string(), "1064.".to_string());
        assert_eq!(origin_from_keys(&keys), Some((1584, 1064)), "an integral real is an integer");
        keys.insert("YORGSUBF".to_string(), "abc".to_string());
        assert_eq!(origin_from_keys(&keys), None);
        assert_eq!(origin_from_header(FileFormat::FITS, &fits_header_text(None)), None);
    }

    #[test]
    fn set_geometry_uniform_mixed_and_unrecorded() {
        let conn = db();
        let uniform = seed_set(&conn, "Dark");
        let a = seed_frame(&conn, "u1", "Dark", Some(FULL), Some((0, 0)));
        let b = seed_frame(&conn, "u2", "Dark", Some(FULL), None);
        add_members(&conn, uniform, &[a, b]);
        assert_eq!(geometry_for_set(&conn, uniform).unwrap(), g(Some(FULL), Some((0, 0))));

        let mixed = seed_set(&conn, "Dark");
        let c = seed_frame(&conn, "m1", "Dark", Some(FULL), None);
        let d = seed_frame(&conn, "m2", "Dark", Some(ROI), None);
        add_members(&conn, mixed, &[c, d]);
        let m = geometry_for_set(&conn, mixed).unwrap();
        assert!(m.mixed && m.size.is_none(), "{m:?}");

        let unrecorded = seed_set(&conn, "Dark");
        let e = seed_frame(&conn, "n1", "Dark", None, None);
        add_members(&conn, unrecorded, &[e]);
        assert_eq!(geometry_for_set(&conn, unrecorded).unwrap(), g(None, None));

        let empty = seed_set(&conn, "Dark");
        assert_eq!(geometry_for_set(&conn, empty).unwrap(), g(None, None));
    }

    #[test]
    fn set_origin_is_the_lowest_frame_id_members() {
        let conn = db();
        let set = seed_set(&conn, "Flat");
        let first = seed_frame(&conn, "o1", "Flat", Some(ROI), Some((1584, 1064)));
        let second = seed_frame(&conn, "o2", "Flat", Some(ROI), Some((0, 0)));
        add_members(&conn, set, &[second, first]);
        assert_eq!(set_origin(&conn, set).unwrap(), Some((1584, 1064)));
    }

    #[test]
    fn frame_geometry_reads_size_and_origin() {
        let conn = db();
        let id = seed_frame(&conn, "light", "Light", Some(ROI), Some((1584, 1064)));
        assert_eq!(geometry_for_frame(&conn, id).unwrap(), g(Some(ROI), Some((1584, 1064))));
        // No stored header at all: size alone.
        conn.execute("DELETE FROM fits_header WHERE file_id = ?1", [file_id_of(&conn, id)]).unwrap();
        assert_eq!(geometry_for_frame(&conn, id).unwrap(), g(Some(ROI), None));
        assert_eq!(origin_for_file(&conn, 999_999).unwrap(), None, "an unknown file records nothing");
    }

    /// Review I-2: a raw group is judged as the set it would become — agreed
    /// size, `mixed`, the lowest frame id's origin — before any row exists.
    #[test]
    fn group_geometry_is_what_its_set_would_record() {
        let conn = db();
        let a = seed_frame(&conn, "g1", "Flat", Some(ROI), Some((1584, 1064)));
        let b = seed_frame(&conn, "g2", "Flat", Some(ROI), Some((0, 0)));
        let c = seed_frame(&conn, "g3", "Flat", Some(FULL), None);
        assert_eq!(geometry_for_frames(&conn, &[b, a]).unwrap(), g(Some(ROI), Some((1584, 1064))));
        let m = geometry_for_frames(&conn, &[a, c]).unwrap();
        assert!(m.mixed && m.size.is_none(), "{m:?}");
        assert_eq!(geometry_for_frames(&conn, &[]).unwrap(), g(None, None));
    }

    /// Review I-3 / ruling R10: the stored header is parsed only when a
    /// same-size set records an origin, and the report label carries the
    /// origin only when the origin decided the refusal.
    #[test]
    fn lazy_frame_reads_the_origin_only_when_it_can_decide() {
        let conn = db();
        let id = seed_frame(&conn, "lazy", "Light", Some(ROI), Some((1584, 1064)));
        let mut light = LazyFrameGeometry::load(&conn, id).unwrap();
        assert_eq!(light.fit(&conn, &g(Some(FULL), Some((0, 0)))).unwrap(), Fit::Refused(Refusal::Size));
        assert_eq!(light.geometry(), g(Some(ROI), None), "a size refusal reads no header");
        assert_eq!(light.fit(&conn, &g(Some(ROI), None)).unwrap(), Fit::Fits);
        assert_eq!(light.geometry(), g(Some(ROI), None), "a set without an origin reads no header");
        assert_eq!(light.fit(&conn, &g(Some(ROI), Some((0, 0)))).unwrap(), Fit::Refused(Refusal::Origin));
        assert_eq!(light.geometry(), g(Some(ROI), Some((1584, 1064))));
        assert_eq!(light.report_label(Refusal::Origin), "6384×4258 @ 1584,1064");
        assert_eq!(light.report_label(Refusal::Size), "6384×4258");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p athenaeum-core --lib calibration::geometry -- --test-threads=4`
Expected: compile errors (`Geometry`, `fit`, … not defined) until Step 3.

- [ ] **Step 3: Implement the module**

Put this above the two test modules (module doc comment first):

```rust
//! Frame geometry for calibration matching (owner rulings 2026-10-20).
//!
//! A calibration frame of a different size can never calibrate a source
//! frame. That holds for every pair the matcher knows — lights → flat / dark /
//! bias, flats → dark flat / dark / bias, darks → bias — in auto-link, in the
//! manual list and in a manual assignment, and it is not a `MatchMode` a user
//! can relax.
//!
//! - **Size** is `frames.naxis1` × `frames.naxis2`: the array as stored, so
//!   after binning (a 2×2 capture of a 9576×6388 sensor is 4784×3194). Binning
//!   itself stays the matcher's own parameter.
//! - **Origin** is the sub-frame origin `XORGSUBF` / `YORGSUBF`, read from the
//!   stored header blob (`fits_header.header`) — the catalog has no column for
//!   it. It is compared only when BOTH sides record it, and an origin refusal
//!   binds automatic choices only (owner ruling G-7, "auto no, manual yes"):
//!   auto-link never takes such a set, a person may — with a warning. Callers
//!   that accept a manual choice tell `Refused(Origin)` from the other two.
//! - A size one side never recorded blocks nothing; the caller logs it.
//! - A calibration set's size is its members' agreed size; members that
//!   disagree make the set `mixed`, and a mixed set fits nothing. Its origin is
//!   its lowest-frame-id member's (the stored-header precedent of
//!   `calibration_library::headers`).

use std::collections::HashMap;

use rusqlite::{params, Connection, OptionalExtension};

use crate::fits_parser::stored_header::parse_stored_header_keys;
use crate::models::{FileFormat, Frame};

/// What a frame or a calibration set records about its pixel grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Geometry {
    /// `(NAXIS1, NAXIS2)` as stored. `None` when not recorded — or, for a set,
    /// when its members disagree (then `mixed` is set).
    pub size: Option<(i32, i32)>,
    /// `(XORGSUBF, YORGSUBF)`; `None` unless the stored header records both.
    pub origin: Option<(i32, i32)>,
    /// A calibration set whose members are not all one size.
    pub mixed: bool,
}

impl Geometry {
    /// `9576×6388`, `6384×4258 @ 1584,1064`, `mixed sizes` or
    /// `size not recorded` — the one spelling every message, log field and
    /// card uses.
    pub fn label(&self) -> String {
        if self.mixed {
            return "mixed sizes".to_string();
        }
        let size = match self.size {
            Some((w, h)) => format!("{w}×{h}"),
            None => "size not recorded".to_string(),
        };
        match self.origin {
            Some((x, y)) => format!("{size} @ {x},{y}"),
            None => size,
        }
    }
}

/// Why a calibration set cannot calibrate a frame. `Size` and `Mixed` bind
/// everywhere; `Origin` binds automatic choices only (G-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Size,
    Origin,
    Mixed,
}

/// The answer to "can this set calibrate that frame?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    Fits,
    /// One side records no size: nothing is blocked (ruling R1); the caller
    /// counts it and logs once.
    Unknown,
    Refused(Refusal),
}

impl Fit {
    pub fn refused(self) -> bool {
        matches!(self, Fit::Refused(_))
    }
}

/// The rule. `source` is the frame (or the flat set) being calibrated.
pub fn fit(source: &Geometry, calibration: &Geometry) -> Fit {
    if source.mixed || calibration.mixed {
        return Fit::Refused(Refusal::Mixed);
    }
    if let (Some(a), Some(b)) = (source.size, calibration.size) {
        if a != b {
            return Fit::Refused(Refusal::Size);
        }
    }
    if let (Some(a), Some(b)) = (source.origin, calibration.origin) {
        if a != b {
            return Fit::Refused(Refusal::Origin);
        }
    }
    if source.size.is_none() || calibration.size.is_none() {
        Fit::Unknown
    } else {
        Fit::Fits
    }
}

/// A `Frame`'s recorded size.
pub fn size_of(frame: &Frame) -> Option<(i32, i32)> {
    frame.naxis1.zip(frame.naxis2)
}

/// A set's `(size, mixed)` from `MIN`/`MAX` of its members' `naxis1`/`naxis2`
/// (SQL aggregates skip NULLs, so members that record no size do not vote).
pub(crate) fn size_from_aggregate(
    w_min: Option<i32>,
    w_max: Option<i32>,
    h_min: Option<i32>,
    h_max: Option<i32>,
) -> (Option<(i32, i32)>, bool) {
    match (w_min, w_max, h_min, h_max) {
        (Some(a), Some(b), Some(c), Some(d)) if a == b && c == d => (Some((a, c)), false),
        (Some(_), Some(_), Some(_), Some(_)) => (None, true),
        _ => (None, false),
    }
}

/// `(XORGSUBF, YORGSUBF)` from a parsed keyword map (UPPERCASE keys) — both
/// or nothing.
pub fn origin_from_keys(keys: &HashMap<String, String>) -> Option<(i32, i32)> {
    let read = |key: &str| keys.get(key).and_then(|v| card_int(v));
    read("XORGSUBF").zip(read("YORGSUBF"))
}

/// An integer card value; an integral real (`1064.`) counts, anything else
/// does not.
fn card_int(value: &str) -> Option<i32> {
    let v = value.trim();
    v.parse::<i32>().ok().or_else(|| {
        v.parse::<f64>()
            .ok()
            .filter(|f| f.is_finite() && f.fract() == 0.0 && f.abs() <= i32::MAX as f64)
            .map(|f| f as i32)
    })
}

/// The origin a stored header blob records, in any of the shapes the scanner
/// stores (FITS cards, XISF XML, a plain keyword dump).
pub fn origin_from_header(format: FileFormat, header: &str) -> Option<(i32, i32)> {
    origin_from_keys(&parse_stored_header_keys(format, header))
}

/// The origin `file_id`'s newest stored header records. `Ok(None)` when the
/// file has no stored header or it records no origin.
pub fn origin_for_file(conn: &Connection, file_id: i64) -> rusqlite::Result<Option<(i32, i32)>> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT fi.format, fh.header FROM files fi
             JOIN fits_header fh ON fh.file_id = fi.id
             WHERE fi.id = ?1
             ORDER BY fh.id DESC
             LIMIT 1",
            params![file_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((format, header)) = row else {
        return Ok(None);
    };
    // `files.format` is CHECK-constrained to these two.
    let format = if format == "XISF" { FileFormat::XISF } else { FileFormat::FITS };
    Ok(origin_from_header(format, &header))
}

/// One frame's size and origin.
pub fn geometry_for_frame(conn: &Connection, frame_id: i64) -> rusqlite::Result<Geometry> {
    let (file_id, w, h): (i64, Option<i32>, Option<i32>) = conn.query_row(
        "SELECT file_id, naxis1, naxis2 FROM frames WHERE id = ?1",
        params![frame_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok(Geometry {
        size: w.zip(h),
        origin: origin_for_file(conn, file_id)?,
        mixed: false,
    })
}

/// A set's origin: its lowest-frame-id member's.
pub fn set_origin(conn: &Connection, set_id: i64) -> rusqlite::Result<Option<(i32, i32)>> {
    let file_id: Option<i64> = conn
        .query_row(
            "SELECT f.file_id FROM calibration_set_frames csf
             JOIN frames f ON f.id = csf.frame_id
             WHERE csf.set_id = ?1
             ORDER BY csf.frame_id
             LIMIT 1",
            params![set_id],
            |r| r.get(0),
        )
        .optional()?;
    match file_id {
        Some(id) => origin_for_file(conn, id),
        None => Ok(None),
    }
}

/// A set's agreed size, origin, and whether its members disagree.
pub fn geometry_for_set(conn: &Connection, set_id: i64) -> rusqlite::Result<Geometry> {
    let (w_min, w_max, h_min, h_max): (Option<i32>, Option<i32>, Option<i32>, Option<i32>) = conn
        .query_row(
            "SELECT MIN(f.naxis1), MAX(f.naxis1), MIN(f.naxis2), MAX(f.naxis2)
             FROM calibration_set_frames csf
             JOIN frames f ON f.id = csf.frame_id
             WHERE csf.set_id = ?1",
            params![set_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
    let (size, mixed) = size_from_aggregate(w_min, w_max, h_min, h_max);
    let origin = if mixed { None } else { set_origin(conn, set_id)? };
    Ok(Geometry { size, origin, mixed })
}

/// What a set made of exactly `frame_ids` would record — the same rules as
/// [`geometry_for_set`] (agreed size, `mixed`, the lowest frame id's origin)
/// — so a raw group is judged BEFORE any set row is written for it (ruling
/// G-8, review I-2). Empty → nothing recorded.
pub fn geometry_for_frames(conn: &Connection, frame_ids: &[i64]) -> rusqlite::Result<Geometry> {
    let Some(&lowest) = frame_ids.iter().min() else {
        return Ok(Geometry::default());
    };
    let placeholders = vec!["?"; frame_ids.len()].join(",");
    let (w_min, w_max, h_min, h_max): (Option<i32>, Option<i32>, Option<i32>, Option<i32>) = conn
        .query_row(
            &format!(
                "SELECT MIN(naxis1), MAX(naxis1), MIN(naxis2), MAX(naxis2)
                 FROM frames WHERE id IN ({placeholders})"
            ),
            rusqlite::params_from_iter(frame_ids.iter()),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
    let (size, mixed) = size_from_aggregate(w_min, w_max, h_min, h_max);
    let origin = if mixed {
        None
    } else {
        let file_id: i64 =
            conn.query_row("SELECT file_id FROM frames WHERE id = ?1", params![lowest], |r| r.get(0))?;
        origin_for_file(conn, file_id)?
    };
    Ok(Geometry { size, origin, mixed })
}

/// A frame whose stored header is parsed only when it can decide a verdict
/// (ruling R10, review I-3): the size comes from `frames`; the origin is read
/// at most once, and only when a set of the same size records one.
#[derive(Debug)]
pub struct LazyFrameGeometry {
    file_id: i64,
    size: Option<(i32, i32)>,
    /// `None` = not read yet; `Some(None)` = read, records no origin.
    origin: Option<Option<(i32, i32)>>,
}

impl LazyFrameGeometry {
    pub fn load(conn: &Connection, frame_id: i64) -> rusqlite::Result<Self> {
        let (file_id, w, h): (i64, Option<i32>, Option<i32>) = conn.query_row(
            "SELECT file_id, naxis1, naxis2 FROM frames WHERE id = ?1",
            params![frame_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(Self { file_id, size: w.zip(h), origin: None })
    }

    /// [`fit`] against `set`, reading this frame's origin first only when the
    /// sizes agree and `set` records an origin.
    pub fn fit(&mut self, conn: &Connection, set: &Geometry) -> rusqlite::Result<Fit> {
        if self.origin.is_none()
            && !set.mixed
            && set.origin.is_some()
            && set.size.is_some()
            && set.size == self.size
        {
            self.origin = Some(origin_for_file(conn, self.file_id)?);
        }
        Ok(fit(&self.geometry(), set))
    }

    /// What has been read so far (the origin stays `None` until a fit needed it).
    pub fn geometry(&self) -> Geometry {
        Geometry { size: self.size, origin: self.origin.flatten(), mixed: false }
    }

    /// The label a report shows for `refusal`: with the origin only when the
    /// origin decided it, so lights of one size share one report row however
    /// many origins were read for their other links.
    pub fn report_label(&self, refusal: Refusal) -> String {
        let g = self.geometry();
        if refusal == Refusal::Origin {
            g.label()
        } else {
            Geometry { origin: None, ..g }.label()
        }
    }
}
```

- [ ] **Step 4: The two pinning tests outside the module (R4, R5)**

In `fits_parser/mod.rs`'s test module (it already has `write_xisf_with_keywords` and `parse_xisf`), add:

```rust
    /// R4 (calibration geometry, 2026-10-20): an XISF whose keywords carry no
    /// NAXIS takes its size from `<Image geometry="w:h:c">` — the stored
    /// image, the same post-binning array a FITS NAXIS describes.
    #[test]
    fn xisf_image_geometry_fills_naxis_without_keywords() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("no_naxis.xisf");
        write_xisf_with_keywords(&path, &[("IMAGETYP", "'Dark'")]);
        let frame = parse_xisf(&path, 1).unwrap();
        assert_eq!((frame.naxis1, frame.naxis2), (Some(100), Some(100)));
    }
```

In `calibration_library/register.rs`'s test module, next to `matcher_excludes_superseded_sets` (reuse its `seed_source_set` / `write_master` helpers), add:

```rust
    /// R5 (calibration geometry, 2026-10-20): a registered master's size is
    /// the size of the file it registers — the same parse the scanner runs —
    /// so a master never loses a match its raw set had.
    #[test]
    fn registered_master_records_its_file_size() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        let raw = seed_source_set(&conn, dir.path());
        let master_path = write_master(dir.path());
        let reg = register_master(&conn, raw, &master_path, "{}").unwrap();
        let parsed = crate::fits_parser::parse_fits(&master_path, 0).unwrap();
        let g = crate::calibration::geometry::geometry_for_set(&conn, reg.master_set_id).unwrap();
        assert_eq!(g.size, parsed.naxis1.zip(parsed.naxis2));
        assert!(g.size.is_some(), "a registered master must record its size");
    }
```

(`write_master` writes an 8×8 FITS master through `write_master_as(dir, OutputFormat::Fits)`.)

- [ ] **Step 5: Run the tests to verify they pass**

Run:
```bash
cargo test -p athenaeum-core --lib calibration::geometry -- --test-threads=4
cargo test -p athenaeum-core --lib xisf_image_geometry_fills_naxis_without_keywords registered_master_records_its_file_size -- --test-threads=4
```
(Two filters need two runs if your cargo rejects a second positional filter.) Expected: all PASS. Then the task gate (filter `calibration::`).

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/calibration/geometry.rs crates/athenaeum-core/src/calibration/mod.rs crates/athenaeum-core/src/fits_parser/mod.rs crates/athenaeum-core/src/calibration_library/register.rs
git commit -m "feat(calibration): calibration::geometry - frame size and sub-frame origin, the fit rule (owner ruling 2026-10-20)

Co-Authored-By: Claude <your model name> <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01NDfRyApE6tYi9ZMD7b4g8m"
```

---

### Task 1b: In-app masters carry the members' agreed sub-frame origin (G-5)

Without this the origin rule is blind for every master the app builds: the header builder never wrote `XORGSUBF`/`YORGSUBF` (`grep ORGSUBF` over `crates/` and `rustafits/` was empty on 2026-10-20). Master LIGHTS (`stacking/master_cards.rs`) are never calibration candidates and stay out of scope.

**Env (every cargo call in this task):** `export CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/.superpowers/target-geom CARGO_BUILD_JOBS=4`, run from the worktree root, tests always `-- --test-threads=4`, one compile at a time (Global Constraints).

**Files:**
- Modify: `crates/athenaeum-core/src/calibration_library/headers.rs` (`MasterHeaderInputs.origin`, new private `members_origin`, `load_header_inputs`, `build_master_cards`)
- Test: `headers.rs` (existing `mod tests`), `crates/athenaeum-core/src/calibration_library/register.rs` (existing `mod tests`)

**Interfaces:**
- Consumes (Task 1): `geometry::origin_for_file`, `geometry::geometry_for_set`, `test_support::{fits_header_text, count_warns}`; `crate::db::insert_fits_header`.
- Produces: `MasterHeaderInputs.origin: Option<(i32, i32)>`; a built master's header carries `XORGSUBF` / `YORGSUBF` (FITS cards, or `<FITSKeyword>` elements in an XISF master — the XISF writer emits every non-structural card) exactly when every member records the same origin. Registration then stores that header, so `geometry_for_set(master).origin` reads it with no other change. `MasterHeaderInputs` is built in exactly one place (`load_header_inputs`); `cfa_flat_inputs` in the tests calls it.

- [ ] **Step 1: Write the failing tests**

In `headers.rs`'s `mod tests` (reuse `seed`, `cards_for`, `card_int`, `has_card`):

```rust
    /// One stored header per member of `set_id`, in frame-id order, recording
    /// `origins[i]` (G-5 fixtures; `seed` writes no `fits_header` rows).
    fn stamp_member_origins(conn: &Connection, set_id: i64, origins: &[Option<(i32, i32)>]) {
        let file_ids: Vec<i64> = conn
            .prepare(
                "SELECT f.file_id FROM calibration_set_frames csf
                 JOIN frames f ON f.id = csf.frame_id
                 WHERE csf.set_id = ?1 ORDER BY csf.frame_id",
            )
            .unwrap()
            .query_map([set_id], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(file_ids.len(), origins.len(), "fixture must cover every member");
        for (file_id, origin) in file_ids.into_iter().zip(origins) {
            crate::db::insert_fits_header(
                conn,
                file_id,
                &crate::calibration::geometry::test_support::fits_header_text(*origin),
            )
            .unwrap();
        }
    }

    const NOT_STAMPED: &str = "master origin not stamped: members disagree or some record none";

    /// G-5: a master carries the sub-frame origin every member records.
    #[test]
    fn master_cards_carry_the_members_agreed_origin() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        stamp_member_origins(&conn, set_id, &[Some((1584, 1064)), Some((1584, 1064))]);
        let cards = cards_for(&conn, set_id);
        assert_eq!(card_int(&cards, "XORGSUBF"), Some(1584));
        assert_eq!(card_int(&cards, "YORGSUBF"), Some(1064));
    }

    /// G-5: members that disagree, or some that record none — no stamp, one warn.
    #[test]
    fn master_origin_is_not_stamped_when_members_disagree_or_some_record_none() {
        for origins in [[Some((1584, 1064)), Some((0, 0))], [Some((1584, 1064)), None]] {
            let conn = Connection::open_in_memory().unwrap();
            let set_id = seed(&conn);
            stamp_member_origins(&conn, set_id, &origins);
            let (inputs, warns) = crate::calibration::geometry::test_support::count_warns(
                NOT_STAMPED,
                || load_header_inputs(&conn, set_id).unwrap(),
            );
            assert_eq!(inputs.origin, None, "{origins:?}");
            assert_eq!(warns, 1, "{origins:?}");
            let cards = build_master_cards(&inputs, "0.2.5", "median n=2", "cafe", None, None).unwrap();
            assert!(!has_card(&cards, "XORGSUBF") && !has_card(&cards, "YORGSUBF"), "{origins:?}");
        }
    }

    /// No member records an origin (a camera without sub-frame readout): nothing
    /// is lost, so no warn — a debug line only.
    #[test]
    fn master_origin_absent_when_no_member_records_one() {
        let conn = Connection::open_in_memory().unwrap();
        let set_id = seed(&conn);
        let (inputs, warns) = crate::calibration::geometry::test_support::count_warns(
            NOT_STAMPED,
            || load_header_inputs(&conn, set_id).unwrap(),
        );
        assert_eq!(inputs.origin, None);
        assert_eq!(warns, 0);
        assert!(!has_card(&cards_for(&conn, set_id), "XORGSUBF"));
    }
```

In `register.rs`'s `mod tests`, next to `registered_master_records_its_file_size` (Task 1):

```rust
    /// G-5: a stamped origin survives write → registration → the stored
    /// header, so the origin rule judges an in-app master like a raw set —
    /// for both containers the library writes.
    #[test]
    fn registered_master_records_its_stamped_origin() {
        use crate::fits_writer::{Card, CardValue};
        for format in [OutputFormat::Fits, OutputFormat::Xisf] {
            let dir = tempfile::tempdir().unwrap();
            let conn = Connection::open_in_memory().unwrap();
            let raw = seed_source_set(&conn, dir.path());
            let path = dir.path().join(format!("master_dark.{}", format.extension()));
            let cards = HeaderBuilder::new(FrameKind::MasterDark)
                .instrume("TestCam").exptime(300.0).gain(100).offset(50)
                .binning(1, 1).ccd_temp(-10.0)
                .custom(Card::new("XORGSUBF", CardValue::Integer(1584)).unwrap())
                .custom(Card::new("YORGSUBF", CardValue::Integer(1064)).unwrap())
                .build().unwrap();
            crate::fits_writer::write_image_f32(&path, 8, 8, 1, &vec![100.0; 64], &cards, format, XisfBounds::Adu16)
                .unwrap();
            let reg = register_master(&conn, raw, &path, "{}").unwrap();
            let g = crate::calibration::geometry::geometry_for_set(&conn, reg.master_set_id).unwrap();
            assert_eq!(g.origin, Some((1584, 1064)), "{format:?}");
        }
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p athenaeum-core --lib calibration_library::headers -- --test-threads=4`, then `cargo test -p athenaeum-core --lib registered_master_records_its_stamped_origin -- --test-threads=4`.
Expected: compile error (`inputs.origin` does not exist), then the headers tests FAIL (no cards). The register test may already PASS (it writes the cards by hand — it pins the round trip the stamp relies on); that is fine, it is the half of G-5 that needs no code.

- [ ] **Step 3: Implement**

`MasterHeaderInputs`, after `source_set_uuid`:

```rust
    /// The sub-frame origin (`XORGSUBF`, `YORGSUBF`) EVERY member records
    /// (ruling G-5): stamped into the master so the calibration-geometry
    /// origin rule can judge it. `None` when members disagree, when some
    /// record none, or when none records one.
    pub origin: Option<(i32, i32)>,
```

New private fn above `load_header_inputs`:

```rust
/// The origin every member of `set_id` records (ruling G-5), read per member
/// from its stored header (`calibration::geometry::origin_for_file` — the
/// same reader the matcher uses). Members that disagree, or some that record
/// none, lose the origin: `None` and one `warn!`. None recording one is the
/// ordinary shape for a camera without sub-frame readout: `None`, `debug!`.
fn members_origin(conn: &Connection, set_id: i64) -> Result<Option<(i32, i32)>> {
    let file_ids: Vec<i64> = conn
        .prepare(
            "SELECT f.file_id FROM calibration_set_frames csf
             JOIN frames f ON f.id = csf.frame_id
             WHERE csf.set_id = ?1 ORDER BY csf.frame_id",
        )?
        .query_map([set_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut origins = Vec::with_capacity(file_ids.len());
    for &file_id in &file_ids {
        origins.push(crate::calibration::geometry::origin_for_file(conn, file_id)?);
    }
    let recorded: Vec<(i32, i32)> = origins.iter().flatten().copied().collect();
    if recorded.is_empty() {
        debug!(set_id, count = file_ids.len(), "no member records a sub-frame origin; master origin not stamped");
        return Ok(None);
    }
    let first = recorded[0];
    if recorded.len() == origins.len() && recorded.iter().all(|&o| o == first) {
        return Ok(Some(first));
    }
    warn!(
        set_id,
        count = file_ids.len(),
        "master origin not stamped: members disagree or some record none"
    );
    Ok(None)
}
```

`load_header_inputs`: after `let bayer = load_bayer_consensus(conn, source_set_id)?;` add `let origin = members_origin(conn, source_set_id)?;` and `origin,` as the last field of the returned `MasterHeaderInputs`.

`build_master_cards`, right before `b = b.ath_src(&inputs.source_set_uuid)`:

```rust
    // Sub-frame origin (ruling G-5): only when every member agreed, so the
    // calibration-geometry origin rule can judge this master like a raw set.
    if let Some((x, y)) = inputs.origin {
        b = b.custom(
            Card::new("XORGSUBF", crate::fits_writer::CardValue::Integer(i64::from(x)))?
                .with_comment("Subframe X position"),
        );
        b = b.custom(
            Card::new("YORGSUBF", crate::fits_writer::CardValue::Integer(i64::from(y)))?
                .with_comment("Subframe Y position"),
        );
    }
```

(`CardValue`'s integer variant is `Integer(i64)`; `Card::new` returns `Result<Card, FitsWriteError>`, which `build_master_cards` already propagates with `?`.)

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p athenaeum-core --lib calibration_library:: -- --test-threads=4
cargo test -p athenaeum-core --lib api::masters -- --test-threads=4
```
Expected: PASS — every pre-existing header and master-build test still passes (their members have no stored header, so nothing is stamped and no warn fires). Then the task gate (filter `calibration_library::`).

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/calibration_library/headers.rs crates/athenaeum-core/src/calibration_library/register.rs
git commit -m "feat(masters): an in-app master carries the sub-frame origin its members agree on (XORGSUBF/YORGSUBF), so the origin rule can judge it (G-5)

Co-Authored-By: Claude <your model name> <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01NDfRyApE6tYi9ZMD7b4g8m"
```

---

### Task 2: Candidate engine — geometry as a hard filter + the manual verdict

**Env (every cargo call in this task):** `export CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/.superpowers/target-geom CARGO_BUILD_JOBS=4`, run from the worktree root, tests always `-- --test-threads=4`, one compile at a time (Global Constraints).

**Files:**
- Modify: `crates/athenaeum-core/src/calibration/finder.rs` (`CandidateMatchDetails`)
- Modify: `crates/athenaeum-core/src/calibration/configurable_matcher.rs` (`find_calibration_candidates`, `rule_weight`, `near_miss_penalty`, new `geometry_detail`)
- Modify: `crates/athenaeum-core/src/calibration/manual.rs` (`verdicts_from_candidate`, `synthesize_frame_for_lights`, `synthesize_frame_for_set`)
- Test: `configurable_matcher.rs` (existing `mod tests`), `manual.rs` (new `#[cfg(test)] mod geometry_tests`)

**Interfaces:**
- Consumes (Task 1): `geometry::{Geometry, Fit, fit, size_of, size_from_aggregate, set_origin, origin_for_file, geometry_for_set}`, `geometry::test_support::*`.
- Produces: `CandidateMatchDetails.geometry: ParameterMatch` (`mode = Exact`; a size or mixed refusal → `matched = false` → verdict `mismatch`; an origin refusal → `matched = true, warning = true` → verdict `warning` (G-7: the card stays selectable); `unknown = (fit == Unknown)`, `frame_value = source label when its size is known`, `set_value = set label when its size is known or mixed`); `passed_hard_filter = false` for EVERY geometry refusal, origin included, so auto-link (`OnlyCompatible`) never takes one; a `ParameterVerdict` named `"geometry"` in `CalibrationSetWithScore.parameters` (Task 7 labels it); synthesized frames whose `naxis1/naxis2` and `file_id` carry the selection's geometry (Task 4 relies on its own check, not on these).
- In the manual list an origin-only refusal is a refused candidate like any other (`compatible = passed_hard_filter = false`): it shows with Show all, or as Linked now (`candidate_is_visible`, unchanged), and its card is selectable with a warning badge (Task 7). The visibility rule itself is not changed.

- [ ] **Step 1: Write the failing tests**

Append to `configurable_matcher.rs`'s `mod tests`:

```rust
    // ── Geometry (owner ruling 2026-10-20) ──────────────────────────────
    use crate::calibration::geometry::test_support::{add_members, seed_file, seed_frame};

    const FULL: (i32, i32) = (9576, 6388);
    const ROI: (i32, i32) = (6384, 4258);

    /// A set that ties with every other one this helper makes on every
    /// matcher parameter (ASI6200MM, 1x1, gain 100, offset 50, 120 s, −10 °C)
    /// and has one member of `size` / `origin`.
    #[allow(clippy::too_many_arguments)]
    fn tied_set(
        conn: &Connection,
        id: i64,
        imagetyp: &str,
        filter: Option<&str>,
        date_start: &str,
        size: Option<(i32, i32)>,
        origin: Option<(i32, i32)>,
        is_master: i32,
    ) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set
             (id, imagetyp, exptime, ccd_temp, temp_min, temp_max, gain, offset, binning,
              instrume, filter, date, date_start, date_end, frame_count, is_master_library)
             VALUES (?1, ?2, 120.0, -10.0, -10.0, -10.0, 100.0, 50.0, '1x1',
                     'ASI6200MM', ?3, ?4, ?4, ?4, 1, ?5)",
            params![id, imagetyp, filter, date_start, is_master],
        )
        .unwrap();
        let member = seed_frame(conn, &format!("member-{id}"), imagetyp, size, origin);
        add_members(conn, id, &[member]);
        id
    }

    /// The source those sets are judged against, on a file whose stored
    /// header records `origin`.
    fn tied_frame(
        conn: &Connection,
        tag: &str,
        imagetyp: ImageType,
        filter: Option<&str>,
        size: Option<(i32, i32)>,
        origin: Option<(i32, i32)>,
    ) -> Frame {
        Frame {
            file_id: seed_file(conn, tag, origin),
            instrume: Some("ASI6200MM".to_string()),
            binning: Some("1x1".to_string()),
            gain: Some(100.0),
            offset: Some(50.0),
            exptime: Some(120.0),
            ccd_temp: Some(-10.0),
            filter: filter.map(String::from),
            imagetyp: Some(imagetyp),
            focallen: None,
            telescop: None,
            naxis1: size.map(|s| s.0),
            naxis2: size.map(|s| s.1),
            date_obs: Some(
                chrono::DateTime::parse_from_rfc3339("2023-08-01T00:00:00+00:00")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            ..light_frame_for_dark()
        }
    }

    fn ids(c: &[CalibrationCandidate]) -> Vec<i64> {
        let mut v: Vec<i64> = c.iter().map(|c| c.set_id).collect();
        v.sort();
        v
    }

    /// The bug of 2026-10-01 (frame set 182 "Horsehead C11"): dark libraries
    /// that tie on every parameter and differ only in size; the ROI one is a
    /// day newer, so it out-scored the full frame and auto-link took it.
    #[test]
    fn geometry_dark_sets_tied_on_every_parameter_but_size() {
        let conn = engine_test_db();
        let full = tied_set(&conn, 1, "Dark", None, "2023-07-27T19:55:06+00:00", Some(FULL), Some((0, 0)), 0);
        let roi = tied_set(&conn, 2, "Dark", None, "2023-07-28T17:24:40+00:00", Some(ROI), Some((1584, 1064)), 0);
        let light = tied_frame(&conn, "light", ImageType::Light, None, Some(FULL), Some((0, 0)));
        let config = CalibrationMatchingConfig::default();

        let all = find_calibration_candidates(&conn, &light, "lights", "dark", &config, CandidateMode::IncludeIncompatible).unwrap();
        let by_id = |id: i64| all.iter().find(|c| c.set_id == id).unwrap();
        assert!(by_id(roi).match_score > by_id(full).match_score, "precondition: the ROI set is the closer one");

        let auto = find_calibration_candidates(&conn, &light, "lights", "dark", &config, CandidateMode::OnlyCompatible).unwrap();
        assert_eq!(ids(&auto), vec![full], "auto-link must never take a set of another size");

        let refused = by_id(roi);
        assert!(!refused.passed_hard_filter);
        let d = &refused.details;
        assert!(!d.geometry.matched && !d.geometry.unknown);
        assert_eq!(d.geometry.frame_value.as_deref(), Some("9576×6388"));
        assert_eq!(d.geometry.set_value.as_deref(), Some("6384×4258 @ 1584,1064"));
        assert!(d.instrume.matched && d.binning.matched && d.gain.matched && d.offset.matched && d.exptime.matched,
            "geometry is the only reason");
        let verdicts = crate::calibration::manual::verdicts_from_candidate(d);
        let v = verdicts.iter().find(|v| v.name == "geometry").expect("a geometry verdict");
        assert_eq!(v.status, crate::models::ParameterStatus::Mismatch);
        assert!(v.enforced);
    }

    /// O1: every source → calibration pair the matcher knows.
    #[test]
    fn geometry_is_required_for_every_source_calibration_pair() {
        let pairs: [(&str, &str, &str, ImageType, Option<&str>); 7] = [
            ("lights", "flat", "Flat", ImageType::Light, Some("L")),
            ("lights", "dark", "Dark", ImageType::Light, None),
            ("lights", "bias", "Bias", ImageType::Light, None),
            ("flats", "darkflat", "DarkFlat", ImageType::Flat, None),
            ("flats", "dark", "Dark", ImageType::Flat, None),
            ("flats", "bias", "Bias", ImageType::Flat, None),
            ("darks", "bias", "Bias", ImageType::Dark, None),
        ];
        for (source_type, cal_type, imagetyp, kind, filter) in pairs {
            let conn = engine_test_db();
            let full = tied_set(&conn, 1, imagetyp, filter, "2023-07-27T00:00:00+00:00", Some(FULL), None, 0);
            tied_set(&conn, 2, imagetyp, filter, "2023-07-28T00:00:00+00:00", Some(ROI), None, 0);
            let source = tied_frame(&conn, "source", kind, filter, Some(FULL), None);
            let got = find_calibration_candidates(&conn, &source, source_type, cal_type,
                &CalibrationMatchingConfig::default(), CandidateMode::OnlyCompatible).unwrap();
            assert_eq!(ids(&got), vec![full], "{source_type} → {cal_type}");
        }
    }

    /// O2: same size, another origin — refused only when both sides record
    /// one. The externally built master recording 0,0 is the dev catalog's
    /// set 1518 shape. G-7 (owner 2026-10-20, "auto no, manual yes"): auto-link
    /// never takes it, but its verdict is a WARNING so the manual card stays
    /// selectable (review M-7: this is the test that pins the owner's answer).
    #[test]
    fn geometry_origin_compared_only_when_both_sides_record_it() {
        let conn = engine_test_db();
        let centred = tied_set(&conn, 1, "Dark", None, "2023-07-27T00:00:00+00:00", Some(ROI), Some((1584, 1064)), 0);
        let corner = tied_set(&conn, 2, "MasterDark", None, "2023-07-28T00:00:00+00:00", Some(ROI), Some((0, 0)), 1);
        let silent = tied_set(&conn, 3, "Dark", None, "2023-07-26T00:00:00+00:00", Some(ROI), None, 0);
        let config = CalibrationMatchingConfig::default();

        let light = tied_frame(&conn, "roi-light", ImageType::Light, None, Some(ROI), Some((1584, 1064)));
        let got = find_calibration_candidates(&conn, &light, "lights", "dark", &config, CandidateMode::OnlyCompatible).unwrap();
        assert_eq!(ids(&got), vec![centred, silent], "auto-link never takes another origin");

        let all = find_calibration_candidates(&conn, &light, "lights", "dark", &config, CandidateMode::IncludeIncompatible).unwrap();
        let c = all.iter().find(|c| c.set_id == corner).unwrap();
        assert!(!c.passed_hard_filter);
        let d = &c.details.geometry;
        assert!(d.matched && d.warning && !d.unknown, "{d:?}");
        assert_eq!(d.frame_value.as_deref(), Some("6384×4258 @ 1584,1064"));
        assert_eq!(d.set_value.as_deref(), Some("6384×4258 @ 0,0"));
        let verdicts = crate::calibration::manual::verdicts_from_candidate(&c.details);
        let v = verdicts.iter().find(|v| v.name == "geometry").unwrap();
        assert_eq!(v.status, crate::models::ParameterStatus::Warning, "the manual card stays selectable");

        let quiet = tied_frame(&conn, "quiet-light", ImageType::Light, None, Some(ROI), None);
        let got = find_calibration_candidates(&conn, &quiet, "lights", "dark", &config, CandidateMode::OnlyCompatible).unwrap();
        assert_eq!(ids(&got), vec![centred, corner, silent]);
    }

    /// R1 + R12.
    #[test]
    fn geometry_unrecorded_blocks_nothing_and_mixed_sizes_fit_nothing() {
        let conn = engine_test_db();
        let unrecorded = tied_set(&conn, 1, "Dark", None, "2023-07-27T00:00:00+00:00", None, None, 0);
        let mixed = tied_set(&conn, 2, "Dark", None, "2023-07-28T00:00:00+00:00", Some(FULL), None, 0);
        let roi_member = seed_frame(&conn, "mixed-roi", "Dark", Some(ROI), None);
        add_members(&conn, mixed, &[roi_member]);
        let config = CalibrationMatchingConfig::default();

        let light = tied_frame(&conn, "light", ImageType::Light, None, Some(FULL), None);
        let all = find_calibration_candidates(&conn, &light, "lights", "dark", &config, CandidateMode::IncludeIncompatible).unwrap();
        let u = all.iter().find(|c| c.set_id == unrecorded).unwrap();
        assert!(u.passed_hard_filter && u.details.geometry.matched && u.details.geometry.unknown);
        let m = all.iter().find(|c| c.set_id == mixed).unwrap();
        assert!(!m.passed_hard_filter);
        assert_eq!(m.details.geometry.set_value.as_deref(), Some("mixed sizes"));

        let blind = tied_frame(&conn, "blind", ImageType::Light, None, None, None);
        let got = find_calibration_candidates(&conn, &blind, "lights", "dark", &config, CandidateMode::OnlyCompatible).unwrap();
        assert_eq!(ids(&got), vec![unrecorded], "a frame of unknown size still never takes a mixed set");
    }
```

Create, at the end of `manual.rs`:

```rust
#[cfg(test)]
mod geometry_tests {
    use super::*;
    use crate::calibration::geometry::test_support::{add_members, file_id_of, seed_frame, seed_set};
    use crate::db::schema::init_db;

    const FULL: (i32, i32) = (9576, 6388);
    const ROI: (i32, i32) = (6384, 4258);

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    /// Review Focus 3: a selection that mixes sizes is judged against its
    /// most common size; the representative file is that size's lowest frame
    /// id, whose stored header supplies the origin.
    #[test]
    fn synthesized_lights_carry_the_most_common_size() {
        let conn = db();
        let a = seed_frame(&conn, "a", "Light", Some(FULL), Some((0, 0)));
        let b = seed_frame(&conn, "b", "Light", Some(FULL), None);
        let c = seed_frame(&conn, "c", "Light", Some(ROI), None);
        let frame = synthesize_frame_for_lights(&conn, &[c, b, a]).unwrap();
        assert_eq!((frame.naxis1, frame.naxis2), (Some(9576), Some(6388)));
        assert_eq!(frame.file_id, file_id_of(&conn, a));
    }

    #[test]
    fn synthesized_set_frame_carries_the_set_geometry() {
        let conn = db();
        let set = seed_set(&conn, "Flat");
        let m1 = seed_frame(&conn, "f1", "Flat", Some(ROI), Some((1584, 1064)));
        let m2 = seed_frame(&conn, "f2", "Flat", Some(ROI), None);
        add_members(&conn, set, &[m2, m1]);
        let (frame, source_type) = synthesize_frame_for_set(&conn, set).unwrap();
        assert_eq!(source_type, "flats");
        assert_eq!((frame.naxis1, frame.naxis2), (Some(6384), Some(4258)));
        assert_eq!(frame.file_id, file_id_of(&conn, m1), "the lowest frame id's file");
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p athenaeum-core --lib calibration:: -- --test-threads=4`
Expected: compile error (`details.geometry` does not exist), then — once Step 3's finder field exists — assertion failures (`auto` holds both sets; synthesized `naxis1` is `None`).

- [ ] **Step 3: Implement**

`finder.rs`, in `CandidateMatchDetails` after `ccd_temp`:

```rust
    /// Frame size and sub-frame origin (`calibration::geometry`). Always
    /// `MatchMode::Exact`: the owner's rule (2026-10-20), not a config
    /// parameter — no setting relaxes it.
    pub geometry: ParameterMatch,
```

`configurable_matcher.rs`:

1. Imports: `use crate::calibration::geometry::{self, Fit, Geometry, Refusal};`.
2. Replace the candidate query (the `let query = format!(…)` block) with:

```rust
    // Geometry (owner ruling 2026-10-20): a set's size is its members' agreed
    // NAXIS1 × NAXIS2, aggregated in this same statement so the loop issues
    // no per-set size query. The origin is read lazily below, only where it
    // can decide something (plan ruling R10).
    let query = format!(
        "WITH cand AS (
             SELECT id, gain, offset, binning, instrume, exptime, focallen, filter,
                    ccd_temp, temp_min, temp_max, date_start, date_end, telescop, is_master_library
             FROM calibration_set
             WHERE imagetyp IN ('{}', '{}')
               AND superseded_by_set_id IS NULL
         ),
         geom AS (
             SELECT csf.set_id,
                    MIN(f.naxis1) AS w_min, MAX(f.naxis1) AS w_max,
                    MIN(f.naxis2) AS h_min, MAX(f.naxis2) AS h_max
             FROM calibration_set_frames csf
             JOIN frames f ON f.id = csf.frame_id
             WHERE csf.set_id IN (SELECT id FROM cand)
             GROUP BY csf.set_id
         )
         SELECT cand.id, cand.gain, cand.offset, cand.binning, cand.instrume, cand.exptime,
                cand.focallen, cand.filter, cand.ccd_temp, cand.temp_min, cand.temp_max,
                cand.date_start, cand.date_end, cand.telescop, cand.is_master_library,
                geom.w_min, geom.w_max, geom.h_min, geom.h_max
         FROM cand
         LEFT JOIN geom ON geom.set_id = cand.id
         ORDER BY cand.date_start DESC",
        imagetyp_str, master_imagetyp_str
    );
```

3. In the `query_map` tuple append, after `is_master_library`:

```rust
            row.get::<_, Option<i32>>(15)?,     // w_min
            row.get::<_, Option<i32>>(16)?,     // w_max
            row.get::<_, Option<i32>>(17)?,     // h_min
            row.get::<_, Option<i32>>(18)?,     // h_max
```
and append `w_min, w_max, h_min, h_max,` to the destructuring `let (…) = row_result?;`.

4. Before `for row_result in rows`:

```rust
    let source_size = geometry::size_of(frame);
    // Read at most once per call, and only when a candidate needs it.
    let mut frame_origin: Option<Option<(i32, i32)>> = None;
    let mut geometry_unknown = 0usize;
```

5. Replace

```rust
        let passed_hard_filter = match_result.matches && !match_result.skip_matching;

        if mode == CandidateMode::OnlyCompatible && !passed_hard_filter {
            continue;
        }
```
with

```rust
        let passed_parameters = match_result.matches && !match_result.skip_matching;
        if mode == CandidateMode::OnlyCompatible && !passed_parameters {
            continue;
        }

        // Geometry: a hard requirement for every pair, never a MatchMode
        // (owner ruling 2026-10-20). After the parameters, so auto-link reads
        // no origin for a set it drops anyway. The set's origin is read when it
        // can decide the fit (sizes agree), or — in the manual list — for a
        // candidate that passes the parameters, so its verdict can show it; a
        // parameter-refused set is shown only with Show all and its card's
        // facts line carries the origin from the set detail (R10, review M-3).
        let (set_size, set_mixed) = geometry::size_from_aggregate(w_min, w_max, h_min, h_max);
        let sizes_agree = set_size.is_some() && set_size == source_size;
        let set_origin = if !set_mixed
            && set_size.is_some()
            && (sizes_agree || (mode == CandidateMode::IncludeIncompatible && passed_parameters))
        {
            geometry::set_origin(conn, set_id)?
        } else {
            None
        };
        let source_origin = if sizes_agree && set_origin.is_some() {
            match frame_origin {
                Some(origin) => origin,
                None => {
                    let origin = geometry::origin_for_file(conn, frame.file_id)?;
                    frame_origin = Some(origin);
                    origin
                }
            }
        } else {
            None
        };
        let source_geometry = Geometry { size: source_size, origin: source_origin, mixed: false };
        let set_geometry = Geometry { size: set_size, origin: set_origin, mixed: set_mixed };
        let geometry_fit = geometry::fit(&source_geometry, &set_geometry);
        if geometry_fit == Fit::Unknown {
            geometry_unknown += 1;
        }
        details.geometry = geometry_detail(&source_geometry, &set_geometry, geometry_fit);
        // Every geometry refusal fails the hard filter — an origin one too:
        // auto-link never takes it (G-7). The manual card reads the verdict
        // instead, where an origin refusal is only a warning.
        let passed_hard_filter = passed_parameters && !geometry_fit.refused();
        if mode == CandidateMode::OnlyCompatible && !passed_hard_filter {
            continue;
        }
```

6. After the loop (before the sort):

```rust
    if geometry_unknown > 0 {
        // Ruling R1: an unrecorded size blocks nothing, but never silently.
        tracing::warn!(
            frame_id = frame.id.unwrap_or(-1),
            count = geometry_unknown,
            "calibration geometry not recorded on one side; size not compared"
        );
    }
```

7. New private fn below `near_miss_penalty`:

```rust
/// The geometry verdict as the manual cards render it (`name = "geometry"`).
/// A size or mixed refusal is a MISMATCH — the card locks (O1). An origin
/// refusal is a WARNING — the owner's "auto no, manual yes" (G-7): the card
/// stays selectable with a badge, while `passed_hard_filter` (set by the
/// caller) still keeps auto-link away from it.
fn geometry_detail(source: &Geometry, set: &Geometry, fit: Fit) -> ParameterMatch {
    let origin_only = fit == Fit::Refused(Refusal::Origin);
    ParameterMatch {
        matched: !fit.refused() || origin_only,
        warning: origin_only,
        mode: MatchMode::Exact,
        frame_value: source.size.map(|_| source.label()),
        set_value: (set.size.is_some() || set.mixed).then(|| set.label()),
        unknown: fit == Fit::Unknown,
        ..Default::default()
    }
}
```

8. `rule_weight`: `"binning"` becomes `"binning" | "geometry"` in all three arms (flats `W_SERIOUS`, bias `W_MAJOR`, dark `W_MAJOR`); in its doc comment, "geometry" now reads "geometry (binning and frame size)", and add the sentence (review M-6): "The frame-size weight only ORDERS candidates the manual list shows as refused — geometry never scores a compatible one, and an origin-only refusal (a warning verdict, `matched = true`) costs nothing here, so it ranks first among the refused." `near_miss_penalty`: add `("geometry", &d.geometry),` after the binning entry (it already counts only `!matched` parameters).

`manual.rs`:

1. `verdicts_from_candidate`: add `verdict("geometry", &details.geometry),` after the binning line.
2. `synthesize_frame_for_lights`: change the SELECT to
   `"SELECT instrume, binning, gain, offset, filter, ccd_temp, exptime, focallen, telescop, date_obs, id, file_id, naxis1, naxis2 FROM frames WHERE id IN ({}) ORDER BY id"`;
   inside the row loop collect `sized.push((row.get::<_, i64>(10)?, row.get::<_, i64>(11)?, row.get::<_, Option<i32>>(12)?.zip(row.get::<_, Option<i32>>(13)?)));` (declare `let mut sized: Vec<(i64, i64, Option<(i32, i32)>)> = Vec::new();` with the other vectors); after the loop:

```rust
    // Geometry: the selection's most common size (ties → the size whose first
    // frame has the lowest id), and that frame's file as the representative
    // whose stored header supplies the origin. `manual_assign_calibration`
    // re-checks every frame, so a minority size is refused there, not here.
    let mut by_size: HashMap<(i32, i32), (usize, i64, i64)> = HashMap::new(); // count, first frame id, its file id
    for &(frame_id, file_id, size) in &sized {
        if let Some(size) = size {
            let entry = by_size.entry(size).or_insert((0, frame_id, file_id));
            entry.0 += 1;
        }
    }
    if by_size.len() > 1 {
        tracing::warn!(
            count = by_size.len(),
            "lights in the manual selection differ in frame size; candidates are judged against the most common size"
        );
    }
    let chosen = by_size
        .into_iter()
        .max_by(|a, b| a.1 .0.cmp(&b.1 .0).then(b.1 .1.cmp(&a.1 .1)))
        .map(|(size, (_, _, file_id))| (size, file_id));
```
   and in the returned `Frame`: `file_id: chosen.map(|c| c.1).unwrap_or(0)`, `naxis1: chosen.map(|c| c.0 .0)`, `naxis2: chosen.map(|c| c.0 .1)`. Update the fn doc comment: the frame carries the most common size and a representative file.
3. `synthesize_frame_for_set`: after the set row is read,

```rust
    let set_geometry = crate::calibration::geometry::geometry_for_set(conn, set_id)?;
    if set_geometry.mixed {
        tracing::warn!(set_id, "calibration set members differ in frame size; its sub-calibration list cannot judge geometry");
    }
    let representative_file_id: i64 = conn
        .query_row(
            "SELECT f.file_id FROM calibration_set_frames csf
             JOIN frames f ON f.id = csf.frame_id
             WHERE csf.set_id = ?1 ORDER BY csf.frame_id LIMIT 1",
            [set_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
```
   (add `use rusqlite::OptionalExtension;`) and in the `Frame`: `file_id: representative_file_id`, `naxis1: set_geometry.size.map(|s| s.0)`, `naxis2: set_geometry.size.map(|s| s.1)`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p athenaeum-core --lib calibration:: -- --test-threads=4` — every new test PASSES and every pre-existing `calibration::` test still passes (their sets have no members, so geometry is `Unknown` and blocks nothing). Then `cargo test -p athenaeum-core --lib api::calibration -- --test-threads=4` and the task gate.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/calibration/finder.rs crates/athenaeum-core/src/calibration/configurable_matcher.rs crates/athenaeum-core/src/calibration/manual.rs
git commit -m "fix(calibration): the candidate engine refuses a set of another frame size (or, for auto-link, another sub-frame origin) - every source pair; a geometry verdict on every candidate, an origin difference only a warning for a manual choice (G-7)

Co-Authored-By: Claude <your model name> <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01NDfRyApE6tYi9ZMD7b4g8m"
```

---

### Task 3: Auto-link paths — grouping sizes, every group judged before a set is written, stale automatic links

**Env (every cargo call in this task):** `export CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/.superpowers/target-geom CARGO_BUILD_JOBS=4`, run from the worktree root, tests always `-- --test-threads=4`, one compile at a time (Global Constraints).

**Files:**
- Modify: `crates/athenaeum-core/src/calibration/flat_groups.rs` (`detect_flat_groups` + its test call)
- Modify: `crates/athenaeum-core/src/calibration/dark_bias_groups.rs` (`detect_dark_groups`, `detect_bias_groups`, `execute_dark_query`, `execute_bias_query` + their test calls)
- Modify: `crates/athenaeum-core/src/calibration/flat_matcher.rs` (pass the light's size)
- Modify: `crates/athenaeum-core/src/calibration/hierarchy.rs` (`build_complete_hierarchy` — the flat pattern selection and the light's dark path —, `find_calibration_for_flat_set`, `find_calibration_for_dark_set`, `try_create_dark_for_frame`, `try_create_bias_for_frame`, `store_calibration_hierarchy`)
- Modify: `crates/athenaeum-core/src/calibration/geometry.rs` (`LinkSource`, `prune_unfit_auto_links`)
- Test: `dark_bias_groups.rs` + `hierarchy.rs` (new `#[cfg(test)] mod geometry_tests` at each file's end)

**Interfaces:**
- Consumes (Task 1): `geometry::{fit, size_of, geometry_for_frame, geometry_for_frames, geometry_for_set, Geometry, Fit}`, `test_support::fits_header_text`.
- Produces:
  - `detect_flat_groups(…, date_range, size: Option<(i32, i32)>)`, `detect_dark_groups(…, temp_threshold, size: Option<(i32, i32)>)`, `detect_bias_groups(…, temp_threshold, size: Option<(i32, i32)>)` — a new LAST parameter; `None` = no filter.
  - `try_create_dark_for_frame(conn, frame, source: &Geometry)` / `try_create_bias_for_frame(conn, frame, source: &Geometry)` (private) — the first group, newest first, that fits `source`; no set row for a refused group (ruling G-8, review I-2).
  - `pub enum LinkSource { Frame(i64), Set(i64) }` and `pub fn prune_unfit_auto_links(conn, source: LinkSource, geometry: &Geometry) -> rusqlite::Result<usize>` in `calibration::geometry` — removes size, mixed AND origin misfits (G-2, G-7).

- [ ] **Step 1: Write the failing tests**

End of `dark_bias_groups.rs`:

```rust
#[cfg(test)]
mod geometry_tests {
    use super::*;
    use crate::db::schema::init_db;
    use rusqlite::params;

    fn raw(conn: &Connection, id: i64, imagetyp: &str, date_obs: &str, size: Option<(i32, i32)>) {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2025-01-01T00:00:00Z', 'FITS')",
            params![id, format!("/raw/{id}.fits"), format!("{id}.fits")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, instrume, binning, xbinning, ybinning,
                                 gain, offset, exptime, ccd_temp, date_obs, naxis1, naxis2)
             VALUES (?1, ?1, ?2, 'ASI6200MM', '1x1', 1, 1, 100.0, 50.0, 120.0, -10.0, ?3, ?4, ?5)",
            params![id, imagetyp, date_obs, size.map(|s| s.0), size.map(|s| s.1)],
        )
        .unwrap();
    }

    /// Owner ruling 2026-10-20: on-demand grouping only gathers frames of the
    /// source's size; a frame that records no size is not excluded (R1).
    #[test]
    fn grouping_keeps_only_frames_of_the_source_size() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        for (imagetyp, base) in [("Dark", 10), ("Bias", 20)] {
            raw(&conn, base, imagetyp, "2023-07-27T20:00:00+00:00", Some((9576, 6388)));
            raw(&conn, base + 1, imagetyp, "2023-07-27T20:01:00+00:00", Some((6384, 4258)));
            raw(&conn, base + 2, imagetyp, "2023-07-27T20:02:00+00:00", None);
        }
        let ids = |g: &[Vec<i64>]| { let mut v: Vec<i64> = g.concat(); v.sort(); v };

        let darks = detect_dark_groups(&conn, "ASI6200MM", "1x1", Some(100.0), Some(50.0), Some(120.0),
            None, 30, None, None, Some((9576, 6388))).unwrap();
        assert_eq!(ids(&darks.iter().map(|g| g.frame_ids.clone()).collect::<Vec<_>>()), vec![10, 12]);
        let all = detect_dark_groups(&conn, "ASI6200MM", "1x1", Some(100.0), Some(50.0), Some(120.0),
            None, 30, None, None, None).unwrap();
        assert_eq!(ids(&all.iter().map(|g| g.frame_ids.clone()).collect::<Vec<_>>()), vec![10, 11, 12]);

        let bias = detect_bias_groups(&conn, "ASI6200MM", "1x1", Some(100.0), Some(50.0),
            None, 30, None, None, Some((6384, 4258))).unwrap();
        assert_eq!(ids(&bias.iter().map(|g| g.frame_ids.clone()).collect::<Vec<_>>()), vec![21, 22]);
    }
}
```

End of `hierarchy.rs`:

```rust
#[cfg(test)]
mod geometry_tests {
    use super::*;
    use crate::calibration::geometry::test_support::fits_header_text;
    use crate::db::schema::init_db;
    use rusqlite::params;

    const FULL: (i32, i32) = (9576, 6388);
    const ROI: (i32, i32) = (6384, 4258);

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    /// One file + frame (`id` = file id) with every parameter the matcher and
    /// the groupers compare, a size, and a stored header recording `origin`.
    fn raw_frame(conn: &Connection, id: i64, imagetyp: &str, date_obs: &str, size: (i32, i32), origin: Option<(i32, i32)>) {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 1024, '2025-09-25T00:00:00+00:00', 'FITS')",
            params![id, format!("/geom/{id}.fits"), format!("{id}.fits")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, date_obs, instrume, exptime, filter, gain, offset,
                                 binning, xbinning, ybinning, ccd_temp, focallen, imagetyp, is_master,
                                 naxis1, naxis2)
             VALUES (?1, ?1, ?2, 'ASI6200MM', 2.0, 'Ha', 100.0, 30.0, '1x1', 1, 1, -10.0, 448.0,
                     ?3, 0, ?4, ?5)",
            params![id, date_obs, imagetyp, size.0, size.1],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO fits_header (file_id, header) VALUES (?1, ?2)",
            params![id, fits_header_text(origin)],
        )
        .unwrap();
    }

    fn members(conn: &Connection, set_id: i64) -> Vec<i64> {
        let mut stmt = conn.prepare("SELECT frame_id FROM calibration_set_frames WHERE set_id = ?1 ORDER BY frame_id").unwrap();
        stmt.query_map([set_id], |r| r.get(0)).unwrap().collect::<rusqlite::Result<Vec<i64>>>().unwrap()
    }

    fn hierarchy_for(conn: &Connection, light_id: i64) -> CalibrationHierarchy {
        let light = get_frame_by_id(conn, light_id).unwrap();
        build_complete_hierarchy(conn, &light, &CalibrationTolerance::default(), Some("automatic"),
            None, None, 30, 30, 0.3).unwrap()
    }

    /// How many `calibration_set_frames` rows hold any of `frame_ids` — 0 means
    /// no set was ever written for them (ruling G-8, review I-2).
    fn set_rows_holding(conn: &Connection, frame_ids: &[i64]) -> i64 {
        let list = frame_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
        conn.query_row(
            &format!("SELECT COUNT(*) FROM calibration_set_frames WHERE frame_id IN ({list})"),
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// The flat pattern path: the nearest flats of another size never become
    /// candidates (grouping filter), and the nearest flats at another origin
    /// are judged as a GROUP and refused before any set is written for them —
    /// the next group is taken (review I-2).
    #[test]
    fn flat_pattern_path_takes_the_nearest_group_that_fits() {
        let conn = db();
        raw_frame(&conn, 1, "Light", "2025-10-01T00:00:00+00:00", ROI, Some((1584, 1064)));
        raw_frame(&conn, 30, "Flat", "2025-09-30T23:30:00+00:00", FULL, Some((0, 0)));
        raw_frame(&conn, 31, "Flat", "2025-09-30T23:31:00+00:00", FULL, Some((0, 0)));
        raw_frame(&conn, 10, "Flat", "2025-09-30T23:00:00+00:00", ROI, Some((0, 0)));
        raw_frame(&conn, 11, "Flat", "2025-09-30T23:01:00+00:00", ROI, Some((0, 0)));
        raw_frame(&conn, 20, "Flat", "2025-09-29T23:00:00+00:00", ROI, Some((1584, 1064)));
        raw_frame(&conn, 21, "Flat", "2025-09-29T23:01:00+00:00", ROI, Some((1584, 1064)));

        let h = hierarchy_for(&conn, 1);
        assert_eq!(h.flat_sets.len(), 1, "{:?}", h.missing_calibration);
        assert_eq!(members(&conn, h.flat_sets[0].set.id.unwrap()), vec![20, 21]);
        assert_eq!(set_rows_holding(&conn, &[10, 11, 30, 31]), 0, "no set row for a refused group");
    }

    /// On-demand dark creation groups only frames of the light's size, so the
    /// newer ROI darks never become the light's dark.
    #[test]
    fn on_demand_dark_is_built_from_frames_of_the_lights_size() {
        let conn = db();
        raw_frame(&conn, 1, "Light", "2025-10-01T00:00:00+00:00", FULL, None);
        raw_frame(&conn, 40, "Dark", "2025-09-25T22:00:00+00:00", FULL, None);
        raw_frame(&conn, 41, "Dark", "2025-09-25T22:01:00+00:00", FULL, None);
        raw_frame(&conn, 50, "Dark", "2025-09-28T22:00:00+00:00", ROI, None);
        raw_frame(&conn, 51, "Dark", "2025-09-28T22:01:00+00:00", ROI, None);

        let h = hierarchy_for(&conn, 1);
        assert_eq!(h.dark_sets.len(), 1, "{:?}", h.missing_calibration);
        assert_eq!(members(&conn, h.dark_sets[0].set.id.unwrap()), vec![40, 41]);
    }

    /// Review I-2 on the on-demand path: the newest dark group is the light's
    /// size but at another recorded origin — it is judged first and skipped,
    /// and no set row is ever written for it; the older group that fits is built.
    #[test]
    fn on_demand_dark_skips_a_group_at_another_origin_without_writing_it() {
        let conn = db();
        raw_frame(&conn, 1, "Light", "2025-10-01T00:00:00+00:00", ROI, Some((1584, 1064)));
        raw_frame(&conn, 60, "Dark", "2025-09-25T22:00:00+00:00", ROI, Some((1584, 1064)));
        raw_frame(&conn, 61, "Dark", "2025-09-25T22:01:00+00:00", ROI, Some((1584, 1064)));
        raw_frame(&conn, 70, "Dark", "2025-09-28T22:00:00+00:00", ROI, Some((0, 0)));
        raw_frame(&conn, 71, "Dark", "2025-09-28T22:01:00+00:00", ROI, Some((0, 0)));

        let h = hierarchy_for(&conn, 1);
        assert_eq!(h.dark_sets.len(), 1, "{:?}", h.missing_calibration);
        assert_eq!(members(&conn, h.dark_sets[0].set.id.unwrap()), vec![60, 61]);
        assert_eq!(set_rows_holding(&conn, &[70, 71]), 0, "no set row for a refused group");
    }

    fn set_with_member_at(
        conn: &Connection,
        set_id: i64,
        imagetyp: &str,
        member: i64,
        size: (i32, i32),
        origin: Option<(i32, i32)>,
    ) {
        conn.execute("INSERT INTO calibration_set (id, imagetyp, date) VALUES (?1, ?2, '2025-09-25')", params![set_id, imagetyp]).unwrap();
        raw_frame(conn, member, imagetyp, "2025-09-25T22:00:00+00:00", size, origin);
        conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)", params![set_id, member]).unwrap();
    }

    fn set_with_member(conn: &Connection, set_id: i64, imagetyp: &str, member: i64, size: (i32, i32)) {
        set_with_member_at(conn, set_id, imagetyp, member, size, None);
    }

    fn link(conn: &Connection, source_type: &str, source_id: i64, set_id: i64, cal_type: &str, manual: bool) {
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at, is_manual_override)
             VALUES (?1, ?2, ?3, ?4, '2025-09-25T00:00:00Z', ?5)",
            params![source_id, source_type, set_id, cal_type, manual as i32],
        )
        .unwrap();
    }

    fn links_of(conn: &Connection, source_type: &str, source_id: i64) -> Vec<(String, i64, bool)> {
        let mut stmt = conn.prepare(
            "SELECT calibration_type, calibration_set_id, is_manual_override FROM calibration_set_to_frames
             WHERE source_type = ?1 AND source_id = ?2 ORDER BY calibration_type").unwrap();
        stmt.query_map(params![source_type, source_id], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i32>(2)? == 1)))
            .unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap()
    }

    /// Plan ruling R9 (Review Focus 2): storing a hierarchy drops every
    /// AUTOMATIC link of the light — and of the flat set it now uses — whose
    /// set does not fit; manual links stay for the gate to name.
    #[test]
    fn store_prunes_unfit_auto_links_and_keeps_manual_ones() {
        let conn = db();
        raw_frame(&conn, 1, "Light", "2025-10-01T00:00:00+00:00", FULL, None);
        set_with_member(&conn, 200, "Dark", 60, ROI);
        set_with_member(&conn, 201, "Flat", 61, ROI);
        set_with_member(&conn, 202, "Bias", 62, FULL);
        set_with_member(&conn, 300, "Flat", 63, FULL);
        link(&conn, "frame", 1, 200, "Dark", false);
        link(&conn, "frame", 1, 201, "Flat", true);
        link(&conn, "frame", 1, 202, "Bias", false);
        link(&conn, "calibration_set", 300, 200, "Dark", false);

        let h = CalibrationHierarchy {
            light_frame_id: 1,
            flat_sets: vec![CalibrationSetWithLinks { set: get_calibration_set_by_id(&conn, 300).unwrap(), sub_calibration: Vec::new() }],
            dark_sets: Vec::new(),
            missing_calibration: Vec::new(),
            warnings: Vec::new(),
        };
        store_calibration_hierarchy(&conn, &h).unwrap();

        assert_eq!(links_of(&conn, "frame", 1), vec![
            ("Bias".to_string(), 202, false),
            ("Flat".to_string(), 201, true),
        ], "the auto ROI dark is gone; the manual ROI flat stays (and wins over the auto flat 300)");
        assert!(links_of(&conn, "calibration_set", 300).is_empty(), "flat 300's auto ROI dark sub-link is gone");
    }

    /// G-2 + G-7: the same size at another recorded origin — an AUTOMATIC link
    /// to it is removed (auto never keeps one), a MANUAL one stays.
    #[test]
    fn store_prunes_an_auto_link_at_another_origin_and_keeps_a_manual_one() {
        let conn = db();
        raw_frame(&conn, 1, "Light", "2025-10-01T00:00:00+00:00", ROI, Some((1584, 1064)));
        set_with_member_at(&conn, 210, "Dark", 64, ROI, Some((0, 0)));
        set_with_member_at(&conn, 211, "Flat", 65, ROI, Some((0, 0)));
        link(&conn, "frame", 1, 210, "Dark", false);
        link(&conn, "frame", 1, 211, "Flat", true);

        let h = CalibrationHierarchy {
            light_frame_id: 1,
            flat_sets: Vec::new(),
            dark_sets: Vec::new(),
            missing_calibration: Vec::new(),
            warnings: Vec::new(),
        };
        store_calibration_hierarchy(&conn, &h).unwrap();
        assert_eq!(links_of(&conn, "frame", 1), vec![("Flat".to_string(), 211, true)]);
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p athenaeum-core --lib calibration::dark_bias_groups::geometry_tests calibration::hierarchy::geometry_tests -- --test-threads=4` (one filter per run if needed).
Expected: compile errors (the new `size` argument), then failures: the flat path links 10/11 (and writes their set), the on-demand dark is 50/51 or 70/71 (after Task 2 the re-query refuses it, so `dark_sets` is empty — and the refused group's set row was written), and the stale auto links survive the store.

- [ ] **Step 3: Implement**

`flat_groups.rs::detect_flat_groups` — new last parameter `size: Option<(i32, i32)>` (document it in the `# Arguments` list: "`size` — the light's NAXIS1×NAXIS2; only flats of that size (or of no recorded size, ruling R1) are gathered"). In the query, right after the gain block:

```rust
    // Frame geometry (owner ruling 2026-10-20): only flats the size of the
    // light are candidates. A frame that records no size is not excluded
    // (ruling R1); the matcher re-checks the set it lands in.
    if size.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND (naxis1 IS NULL OR naxis1 = ?{})", param_count));
        param_count += 1;
        query.push_str(&format!(" AND (naxis2 IS NULL OR naxis2 = ?{})", param_count));
    }
```
and in the binding, right after the gain binding:

```rust
    if let Some((w, h)) = size {
        stmt.raw_bind_parameter(param_idx, w)?;
        param_idx += 1;
        stmt.raw_bind_parameter(param_idx, h)?;
        param_idx += 1;
    }
```
Add `width = ?size.map(|s| s.0), height = ?size.map(|s| s.1)` to the `"executing flat search query"` debug (the registered `width` / `height` names, R13 — not a new `size` field). Update the test call in `detect_flat_groups_executes_with_the_shared_column_list` to pass `None` last.

`dark_bias_groups.rs`: the same last parameter on `detect_dark_groups` and `detect_bias_groups`, and a `size: Option<(i32, i32)>` parameter on `execute_dark_query` / `execute_bias_query`. Dark: the filter block goes after the exptime block (before the date range), its binding after the exptime binding. Bias: after the offset (`param_count = 4`) and before the date range, binding after the offset binding. Same SQL text as the flat block, same comment ("only frames the size of the source can form its dark/bias"). Pass `size` through from `detect_*` to `execute_*`. Update `detect_dark_and_bias_groups_execute_with_the_shared_column_list` to pass `None`.

`flat_matcher.rs::find_flat_groups_for_light_frame`: pass `crate::calibration::geometry::size_of(light_frame)` as the new argument; add `width = ?size.map(|s| s.0), height = ?size.map(|s| s.1)` (computed from that same `size`) to its `"finding flats for light frame"` debug and the error log.

`hierarchy.rs` — every group is judged BEFORE any `create_*_calibration_set` call, so no set row is ever written for a group the rule refuses (ruling G-8, review I-2). `create_*_calibration_set` writes a row only in its "create new" branch, from exactly the group's frames, so a group that fits gives a set that fits; its two other branches (an existing set, or the master that superseded the lineage) write nothing and are re-checked after the call.
1. `use crate::calibration::geometry::{self, Geometry, LinkSource};` (add `Fit` only if what you write uses it — the file must compile warning-free).
2. `build_complete_hierarchy`: right after `let frame_id = light_frame.id.context("Frame must have an ID")?;` add `let light_geometry = geometry::geometry_for_frame(conn, frame_id)?;` — one stored-header read per light per Find Calibration, used by the flat path and the dark path below.
3. The pattern selection (`let selected_match = apply_pattern_selection(…); if let Some(flat_match) = selected_match { … let set_id = create_flat_calibration_set(…)?; … Some(set_id) } else { warn; None }`) becomes a loop that judges each chosen group before writing, and re-checks a reused set. Replace from `let selected_match = apply_pattern_selection(` down to (not including) the line `// Add age warning if needed` with:

```rust
            // Owner ruling 2026-10-20: the grouping already gathered only flats
            // of the light's size. The chosen GROUP is judged before any set row
            // is written (ruling G-8): another sub-frame origin makes way for the
            // next group and leaves nothing behind. A reused set (an existing one,
            // or the master that superseded this lineage) can differ from its
            // group, so it is re-checked too — its call wrote nothing.
            let mut remaining = flat_matches;
            let selected = loop {
                let Some(flat_match) = apply_pattern_selection(remaining.clone(), &pattern, light_frame_date) else {
                    break None;
                };
                let group_geometry = geometry::geometry_for_frames(conn, &flat_match.group.frame_ids)?;
                let fitting_set = if geometry::fit(&light_geometry, &group_geometry).refused() {
                    tracing::warn!(
                        frame_id,
                        count = flat_match.group.frame_count,
                        source_geometry = %light_geometry.label(),
                        set_geometry = %group_geometry.label(),
                        "flat group does not fit the light's frame geometry; trying the next"
                    );
                    None
                } else {
                    // Find/reuse calibration set from the flat group (don't modify existing sets)
                    let set_id = create_flat_calibration_set(conn, &flat_match.group, false, focallen_tolerance)?;
                    let set_geometry = geometry::geometry_for_set(conn, set_id)?;
                    if geometry::fit(&light_geometry, &set_geometry).refused() {
                        tracing::warn!(
                            frame_id,
                            set_id,
                            source_geometry = %light_geometry.label(),
                            set_geometry = %set_geometry.label(),
                            "reused flat set does not fit the light's frame geometry; trying the next"
                        );
                        None
                    } else {
                        Some(set_id)
                    }
                };
                match fitting_set {
                    Some(set_id) => break Some((flat_match, set_id)),
                    None => remaining.retain(|m| m.group.frame_ids != flat_match.group.frame_ids),
                }
            };

            if let Some((flat_match, set_id)) = selected {
                tracing::debug!(
                    frame_id,
                    age_days = flat_match.age_days,
                    timing = ?flat_match.timing,
                    count = flat_match.group.frame_count,
                    "pattern selected flat match"
                );
```
   and delete the old `// Find/reuse calibration set …` comment and `let set_id = create_flat_calibration_set(…)?;` line inside the block (the loop made it). The rest of the block (age and temperature warnings, `Some(set_id)`, the `else { warn!(…, "flat pattern selection returned no match"); None }`) is unchanged. `FlatGroupMatch` is `Clone`.
4. The on-demand dark / bias path. `try_create_dark_for_frame(conn, frame, source: &Geometry)` and `try_create_bias_for_frame(conn, frame, source: &Geometry)` gain the source geometry. Each passes `geometry::size_of(frame)` as the grouper's new last argument, then — instead of `let best_group = &dark_groups[0];` (resp. `&bias_groups[0]`) — takes the first group, newest first, that fits:

```rust
    // Ruling G-8 (review I-2): judge each group before any set row is written
    // — newest first, the first one that fits the source's geometry. The
    // grouping already filtered by size; this catches another sub-frame
    // origin, which auto-link never takes (G-7).
    let mut chosen = None;
    for group in &dark_groups {
        let group_geometry = geometry::geometry_for_frames(conn, &group.frame_ids)?;
        if geometry::fit(source, &group_geometry).refused() {
            tracing::warn!(
                frame_id = frame.id.unwrap_or(-1),
                count = group.frame_count,
                source_geometry = %source.label(),
                set_geometry = %group_geometry.label(),
                "dark group does not fit the frame geometry; trying the next"
            );
            continue;
        }
        chosen = Some(group);
        break;
    }
    let Some(best_group) = chosen else {
        tracing::warn!(
            frame_id = frame.id.unwrap_or(-1),
            count = dark_groups.len(),
            "no dark group fits the frame geometry; none created on demand"
        );
        return Ok(None);
    };
```
   (the bias twin says "bias group" / `bias_groups`). The existing `"selected best dark group for on-demand creation"` debug and the `create_dark_calibration_set(conn, best_group, false)?` call follow unchanged; the matcher's re-query after the call still judges the set it returns (a reused one included). Callers:
   - `build_complete_hierarchy` (the light's dark): `try_create_dark_for_frame(conn, light_frame, &light_geometry)?`.
   - `find_calibration_for_flat_set` and `find_calibration_for_dark_set`: their representative-frame query `SELECT frame_id FROM calibration_set_frames WHERE set_id = ?1 LIMIT 1` gains `ORDER BY frame_id` (so the frame the matcher judges is the member whose origin is the set's — ruling R10; for a set whose members agree nothing changes), then `let frame_geometry = geometry::geometry_for_frame(conn, frame_id)?;` after `get_frame_by_id`, passed to each `try_create_*_for_frame(conn, &frame, &frame_geometry)?`.
5. `store_calibration_hierarchy`, at its very top:

```rust
    // Plan ruling R9 (G-2, G-7): before writing this run's links, drop every
    // AUTOMATIC link of the light — and of the flat and dark sets it now uses —
    // whose set does not fit: another frame size, mixed sizes, or another
    // recorded sub-frame origin (auto never keeps one). Find Calibration is the only path
    // that can clear an automatic link (the picker's deselect clears manual
    // ones only), and an upsert replaces a link only when a new candidate of
    // the SAME type is found; a geometry-stale automatic link would otherwise
    // block the gate forever. Manual links are the user's and stay (R2).
    let light_geometry = geometry::geometry_for_frame(conn, hierarchy.light_frame_id)?;
    geometry::prune_unfit_auto_links(conn, LinkSource::Frame(hierarchy.light_frame_id), &light_geometry)?;
    for set_id in hierarchy.flat_sets.iter().chain(hierarchy.dark_sets.iter()).filter_map(|s| s.set.id) {
        let set_geometry = geometry::geometry_for_set(conn, set_id)?;
        geometry::prune_unfit_auto_links(conn, LinkSource::Set(set_id), &set_geometry)?;
    }
```
   (`Fit` may end up unused in this file — import only what compiles warning-free.)

`geometry.rs`, after the `impl LazyFrameGeometry` block (Task 1):

```rust
/// Whose links [`prune_unfit_auto_links`] sweeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkSource {
    Frame(i64),
    Set(i64),
}

/// Delete every AUTOMATIC link of `source` whose set does not fit
/// `geometry` — size, mixed or origin (plan ruling R9, G-2, G-7); manual
/// links are never touched. Each removal is logged. Returns how many were
/// removed.
pub fn prune_unfit_auto_links(
    conn: &Connection,
    source: LinkSource,
    geometry: &Geometry,
) -> rusqlite::Result<usize> {
    let (source_type, source_id) = match source {
        LinkSource::Frame(id) => ("frame", id),
        LinkSource::Set(id) => ("calibration_set", id),
    };
    let links: Vec<(i64, i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, calibration_set_id, calibration_type FROM calibration_set_to_frames
             WHERE source_type = ?1 AND source_id = ?2 AND is_manual_override = 0
             ORDER BY id",
        )?;
        let rows = stmt.query_map(params![source_type, source_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut removed = 0;
    for (link_id, set_id, calibration_type) in links {
        let set = geometry_for_set(conn, set_id)?;
        if !fit(geometry, &set).refused() {
            continue;
        }
        conn.execute("DELETE FROM calibration_set_to_frames WHERE id = ?1", params![link_id])?;
        removed += 1;
        match source {
            LinkSource::Frame(frame_id) => tracing::warn!(
                frame_id,
                set_id,
                calibration_type = %calibration_type,
                source_geometry = %geometry.label(),
                set_geometry = %set.label(),
                "automatic calibration link removed: the set does not fit the frame geometry"
            ),
            LinkSource::Set(source_set_id) => tracing::warn!(
                source_set_id,
                set_id,
                calibration_type = %calibration_type,
                source_geometry = %geometry.label(),
                set_geometry = %set.label(),
                "automatic calibration link removed: the set does not fit the frame geometry"
            ),
        }
    }
    Ok(removed)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p athenaeum-core --lib calibration:: -- --test-threads=4` — the new tests PASS; every pre-existing calibration test passes (their frames record no size, so no filter applies; a test that depended on the unordered representative frame of `find_calibration_for_*_set` would be a stop-and-report, not a fixture edit). Then the task gate.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/calibration/
git commit -m "fix(calibration): auto-link paths honour frame geometry - grouping gathers the source's size, every flat and on-demand dark/bias group is judged before a set row is written (G-8), Find Calibration drops automatic links that no longer fit, another origin included (R9, G-7)

Co-Authored-By: Claude <your model name> <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01NDfRyApE6tYi9ZMD7b4g8m"
```

---

### Task 4: Manual assignment refuses a size misfit, accepts another origin with a warning

**Env (every cargo call in this task):** `export CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/.superpowers/target-geom CARGO_BUILD_JOBS=4`, run from the worktree root, tests always `-- --test-threads=4`, one compile at a time (Global Constraints).

**Files:**
- Modify: `crates/athenaeum-core/src/api/calibration.rs` (`manual_assign_calibration`, `manual_assign_subcalibration`, two new private fns)
- Test: `api/calibration.rs` existing `mod tests`

**Interfaces:**
- Consumes (Task 1): `geometry::{fit, Fit, Refusal, geometry_for_frame, geometry_for_set}`, `test_support::*` (incl. `count_warns`).
- Produces: `ApiError::Invalid` with the sentences below for a size or mixed refusal — the picker shows them through `notify()` (Task 7). A same-size set at another recorded origin is ACCEPTED (G-7) with one `warn!` (`"manual calibration accepted at another sub-frame origin"` / `"manual sub-calibration accepted at another sub-frame origin"`); the warning reaches the user in the dialog before the choice (Task 7 badge) and in the hierarchy and the gates after it (Tasks 5, 6) — the command's return type stays as it is. No command surface change.

- [ ] **Step 1: Write the failing tests**

In `api/calibration.rs`'s `mod tests`:

```rust
    // ── Geometry (owner ruling 2026-10-20, plan ruling R11) ─────────────
    use crate::calibration::geometry::test_support as geo;

    fn geo_ctx() -> (tempfile::TempDir, ServiceContext) {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        (tmp, ctx)
    }

    fn link_count(ctx: &ServiceContext) -> i64 {
        db(ctx).unwrap().conn().query_row("SELECT COUNT(*) FROM calibration_set_to_frames", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn manual_assign_refuses_a_set_of_another_size() {
        let (_tmp, ctx) = geo_ctx();
        let (a, b, set) = {
            let conn = db(&ctx).unwrap().conn();
            let a = geo::seed_frame(&conn, "a", "Light", Some((9576, 6388)), None);
            let b = geo::seed_frame(&conn, "b", "Light", Some((9576, 6388)), None);
            let set = geo::seed_set(&conn, "MasterDark");
            let m = geo::seed_frame(&conn, "m", "MasterDark", Some((6384, 4258)), Some((1584, 1064)));
            geo::add_members(&conn, set, &[m]);
            (a, b, set)
        };
        let err = manual_assign_calibration(&ctx, vec![a, b], set, "Dark".to_string()).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("Dark set #{set} is 6384×4258 @ 1584,1064, but 2 of the 2 frames are 9576×6388 — a calibration set must match the frame size")
        );
        assert_eq!(link_count(&ctx), 0, "nothing is written");
    }

    /// Review Focus 3.
    #[test]
    fn manual_assign_refuses_when_one_frame_misfits_and_writes_nothing() {
        let (_tmp, ctx) = geo_ctx();
        let (a, b, set) = {
            let conn = db(&ctx).unwrap().conn();
            let a = geo::seed_frame(&conn, "a", "Light", Some((9576, 6388)), None);
            let b = geo::seed_frame(&conn, "b", "Light", Some((6384, 4258)), None);
            let set = geo::seed_set(&conn, "Flat");
            let m = geo::seed_frame(&conn, "m", "Flat", Some((9576, 6388)), None);
            geo::add_members(&conn, set, &[m]);
            (a, b, set)
        };
        let err = manual_assign_calibration(&ctx, vec![a, b], set, "Flat".to_string()).unwrap_err();
        assert!(err.to_string().contains("1 of the 2 frames is 6384×4258"), "{err}");
        assert_eq!(link_count(&ctx), 0);
        assert_eq!(manual_assign_calibration(&ctx, vec![a], set, "Flat".to_string()).unwrap(), 1);
    }

    #[test]
    fn manual_subcalibration_refuses_a_set_of_another_size() {
        let (_tmp, ctx) = geo_ctx();
        let (flat, roi_dark, full_dark) = {
            let conn = db(&ctx).unwrap().conn();
            let flat = geo::seed_set(&conn, "Flat");
            let f = geo::seed_frame(&conn, "f", "Flat", Some((9576, 6388)), None);
            geo::add_members(&conn, flat, &[f]);
            let roi_dark = geo::seed_set(&conn, "Dark");
            let d1 = geo::seed_frame(&conn, "d1", "Dark", Some((6384, 4258)), None);
            geo::add_members(&conn, roi_dark, &[d1]);
            let full_dark = geo::seed_set(&conn, "Dark");
            let d2 = geo::seed_frame(&conn, "d2", "Dark", Some((9576, 6388)), None);
            geo::add_members(&conn, full_dark, &[d2]);
            (flat, roi_dark, full_dark)
        };
        let err = manual_assign_subcalibration(&ctx, flat, roi_dark, "Dark".to_string()).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("Dark set #{roi_dark} is 6384×4258, but flat set #{flat} is 9576×6388 — a calibration set must match the frame size")
        );
        assert_eq!(link_count(&ctx), 0);
        manual_assign_subcalibration(&ctx, flat, full_dark, "Dark".to_string()).unwrap();
        assert_eq!(link_count(&ctx), 1);
    }

    /// G-7 (owner 2026-10-20, "auto no, manual yes"): the same size at another
    /// recorded origin — a manual assignment is accepted, logged once.
    #[test]
    fn manual_assign_accepts_another_origin_with_a_warning() {
        let (_tmp, ctx) = geo_ctx();
        let (a, set) = {
            let conn = db(&ctx).unwrap().conn();
            let a = geo::seed_frame(&conn, "a", "Light", Some((6384, 4258)), Some((1584, 1064)));
            let set = geo::seed_set(&conn, "MasterDark");
            let m = geo::seed_frame(&conn, "m", "MasterDark", Some((6384, 4258)), Some((0, 0)));
            geo::add_members(&conn, set, &[m]);
            (a, set)
        };
        let (assigned, warns) = geo::count_warns("manual calibration accepted at another sub-frame origin", || {
            manual_assign_calibration(&ctx, vec![a], set, "Dark".to_string())
        });
        assert_eq!(assigned.unwrap(), 1);
        assert_eq!(warns, 1);
        let manual: i64 = db(&ctx).unwrap().conn().query_row(
            "SELECT is_manual_override FROM calibration_set_to_frames WHERE source_id = ?1 AND calibration_set_id = ?2",
            rusqlite::params![a, set],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(manual, 1);
    }

    #[test]
    fn manual_subcalibration_accepts_another_origin_with_a_warning() {
        let (_tmp, ctx) = geo_ctx();
        let (flat, dark) = {
            let conn = db(&ctx).unwrap().conn();
            let flat = geo::seed_set(&conn, "Flat");
            let f = geo::seed_frame(&conn, "f", "Flat", Some((6384, 4258)), Some((1584, 1064)));
            geo::add_members(&conn, flat, &[f]);
            let dark = geo::seed_set(&conn, "Dark");
            let d = geo::seed_frame(&conn, "d", "Dark", Some((6384, 4258)), Some((0, 0)));
            geo::add_members(&conn, dark, &[d]);
            (flat, dark)
        };
        let (done, warns) = geo::count_warns("manual sub-calibration accepted at another sub-frame origin", || {
            manual_assign_subcalibration(&ctx, flat, dark, "Dark".to_string())
        });
        done.unwrap();
        assert_eq!(warns, 1);
        assert_eq!(link_count(&ctx), 1);
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p athenaeum-core --lib api::calibration::tests::manual_ -- --test-threads=4`
Expected: FAIL — the size misfits are assigned; the origin cases are assigned but log no warning (`warns == 0`).

- [ ] **Step 3: Implement**

Private fns (near `candidate_is_visible`):

```rust
/// Refuse a manual pick whose set does not fit every frame's SIZE (owner
/// ruling 2026-10-20: frame size is not a parameter the user can relax; plan
/// ruling R11). Checked for every frame before anything is written. Another
/// recorded sub-frame origin at the same size is the user's call (G-7, "auto
/// no, manual yes"): accepted, with one warning.
fn refuse_unfit_assignment(
    conn: &rusqlite::Connection,
    frame_ids: &[i64],
    calibration_set_id: i64,
    calibration_type: &str,
) -> Result<(), ApiError> {
    use crate::calibration::geometry::{fit, geometry_for_frame, geometry_for_set, Fit, Refusal};
    let set = geometry_for_set(conn, calibration_set_id)?;
    let mut refused = Vec::new();
    let mut other_origin = Vec::new();
    for &frame_id in frame_ids {
        let frame = geometry_for_frame(conn, frame_id)?;
        match fit(&frame, &set) {
            Fit::Refused(Refusal::Origin) => other_origin.push(frame),
            Fit::Refused(_) => refused.push(frame),
            Fit::Fits | Fit::Unknown => {}
        }
    }
    let Some(first) = refused.first() else {
        if let Some(first) = other_origin.first() {
            tracing::warn!(
                set_id = calibration_set_id,
                count = other_origin.len(),
                calibration_type,
                source_geometry = %first.label(),
                set_geometry = %set.label(),
                "manual calibration accepted at another sub-frame origin"
            );
        }
        return Ok(());
    };
    tracing::warn!(
        set_id = calibration_set_id,
        count = refused.len(),
        calibration_type,
        source_geometry = %first.label(),
        set_geometry = %set.label(),
        "manual calibration refused: the set does not fit the frame size"
    );
    Err(ApiError::Invalid(format!(
        "{calibration_type} set #{calibration_set_id} is {}, but {} of the {} frames {} {} — a calibration set must match the frame size",
        set.label(),
        refused.len(),
        frame_ids.len(),
        if refused.len() == 1 { "is" } else { "are" },
        first.label()
    )))
}

/// [`refuse_unfit_assignment`] for a calibration set's own sub-calibration.
fn refuse_unfit_subcalibration(
    conn: &rusqlite::Connection,
    source_set_id: i64,
    calibration_set_id: i64,
    calibration_type: &str,
) -> Result<(), ApiError> {
    use crate::calibration::geometry::{fit, geometry_for_set, Fit, Refusal};
    let source = geometry_for_set(conn, source_set_id)?;
    let set = geometry_for_set(conn, calibration_set_id)?;
    match fit(&source, &set) {
        Fit::Fits | Fit::Unknown => return Ok(()),
        Fit::Refused(Refusal::Origin) => {
            // G-7: the user's call — accepted, with one warning.
            tracing::warn!(
                source_set_id,
                set_id = calibration_set_id,
                calibration_type,
                source_geometry = %source.label(),
                set_geometry = %set.label(),
                "manual sub-calibration accepted at another sub-frame origin"
            );
            return Ok(());
        }
        Fit::Refused(_) => {}
    }
    let source_imagetyp: String = conn.query_row(
        "SELECT imagetyp FROM calibration_set WHERE id = ?1",
        [source_set_id],
        |r| r.get(0),
    )?;
    tracing::warn!(
        source_set_id,
        set_id = calibration_set_id,
        calibration_type,
        source_geometry = %source.label(),
        set_geometry = %set.label(),
        "manual sub-calibration refused: the set does not fit the frame size"
    );
    Err(ApiError::Invalid(format!(
        "{calibration_type} set #{calibration_set_id} is {}, but {} set #{source_set_id} is {} — a calibration set must match the frame size",
        set.label(),
        source_imagetyp.to_lowercase(),
        source.label()
    )))
}
```

In `manual_assign_calibration`, after the `valid_types` check and before `let mut assigned_count = 0;`: `refuse_unfit_assignment(&conn, &frame_ids, calibration_set_id, &calibration_type)?;`. In `manual_assign_subcalibration`, after its `valid_types` check: `refuse_unfit_subcalibration(&conn, source_set_id, calibration_set_id, &calibration_type)?;`. Both functions' doc comments gain one line: "Refuses a set of another frame size (owner ruling 2026-10-20); a set of the same size at another recorded sub-frame origin is accepted with a warning (G-7)."

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p athenaeum-core --lib api::calibration -- --test-threads=4` — PASS (pre-existing tests seed frames with no size, so nothing is refused). Then the task gate.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/api/calibration.rs
git commit -m "fix(calibration): a manual assignment refuses a set of another frame size before writing anything, and accepts another sub-frame origin with a warning (R11, G-7)

Co-Authored-By: Claude <your model name> <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01NDfRyApE6tYi9ZMD7b4g8m"
```

---

### Task 5: Geometry in the set and light models, the hierarchy's origin warning + TypeScript

**Env (every cargo call in this task):** `export CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/.superpowers/target-geom CARGO_BUILD_JOBS=4`, run from the worktree root, tests always `-- --test-threads=4`, one compile at a time (Global Constraints).

**Files:**
- Modify: `crates/athenaeum-core/src/models.rs` (`CalibrationSetDetail`, `LightFrameWithCalibration`, `ParameterVerdict.name` doc, `CalibrationWarning.warning_type` comment)
- Modify: `crates/athenaeum-core/src/calibration/geometry.rs` (`fill_set_origin`, `origin_mismatch_for_lights`)
- Modify: every `CalibrationSetDetail` construction — `calibration/hierarchy.rs::get_calibration_set_by_id`, `calibration/manual.rs::load_set_with_score`, `db/calibration_links.rs::get_calibration_set_detail`, `db/equipment.rs::query_camera_calibration_sets`, `calibration/processor.rs` (2 test literals)
- Modify: `db/calibration_links.rs` (`RawFrame` + SELECT + the one `LightFrameWithCalibration` construction; new private `push_origin_warning`, called by the three set builders of `get_calibration_hierarchy_for_frame_set`)
- Regenerate: `src/types/models.ts`
- Modify: `src/components/calibration/LightsAnalysisTable.project-column.test.tsx` (fixture fields)
- Test: `manual.rs::geometry_tests`, `db/calibration_links.rs::night_grouping_tests`

**Interfaces:**
- Consumes (Task 1): `geometry::{set_origin, geometry_for_set, LazyFrameGeometry, Fit, Refusal}`, `test_support::*`.
- Produces (Task 7 reads them):
  - `CalibrationSetDetail.xorgsubf: Option<i32>`, `.yorgsubf: Option<i32>` → TS `xorgsubf: number | null`, `yorgsubf: number | null` (this struct is snake_case on the wire).
  - `LightFrameWithCalibration.naxis1: Option<i32>`, `.naxis2: Option<i32>` → TS `naxis1/naxis2: number | null`.
  - `pub fn fill_set_origin(conn, detail: &mut CalibrationSetDetail) -> rusqlite::Result<()>` in `calibration::geometry`.
  - `pub fn origin_mismatch_for_lights(conn, set_id, light_frame_ids: &[i64]) -> rusqlite::Result<Option<(Geometry, Geometry)>>` in `calibration::geometry` — `(set, first light)` when a light of the set's size records another origin; reads no light header when the set records no origin.
  - G-7 in the hierarchy: a `CalibrationSetWithFrameCount.warnings` entry with `warning_type = "geometry"` on a light → set link at another recorded origin (no model change — `warning_type` is a free string; its `//` comment is not exported to TS). Task 7 shows it in the set's Size cell.

- [ ] **Step 1: Write the failing tests**

In `manual.rs::geometry_tests`:

```rust
    /// R3: a candidate's set detail carries its origin when known.
    #[test]
    fn set_detail_carries_the_origin() {
        let conn = db();
        let set = seed_set(&conn, "Dark");
        let m = seed_frame(&conn, "o", "Dark", Some(ROI), Some((1584, 1064)));
        add_members(&conn, set, &[m]);
        let candidate = crate::calibration::finder::CalibrationCandidate {
            set_id: set,
            imagetyp: ImageType::Dark,
            match_score: 0.0,
            date_diff_days: 0,
            temp_diff: None,
            date_warning: false,
            temp_warning: false,
            is_master: false,
            passed_hard_filter: false,
            details: CandidateMatchDetails::default(),
            warnings: Vec::new(),
        };
        let swc = load_set_with_score(&conn, &candidate).unwrap().unwrap();
        assert_eq!((swc.set.naxis1, swc.set.naxis2), (Some(6384), Some(4258)));
        assert_eq!((swc.set.xorgsubf, swc.set.yorgsubf), (Some(1584), Some(1064)));
    }
```

In `db/calibration_links.rs::night_grouping_tests`:

```rust
    /// R3: the hierarchy view carries each light's size and each set's
    /// origin, so the tables can show and badge both.
    #[test]
    fn hierarchy_view_carries_light_size_and_set_origin() {
        use crate::calibration::geometry::test_support::{add_members, seed_frame};
        let conn = db();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'Horsehead')", []).unwrap();
        night_with_lights(&conn, 1, "2025-10-18T18:00:00Z", "2025-10-18T23:30:00Z", &[(10, "2025-10-18T18:00:00Z")]);
        conn.execute("UPDATE frames SET naxis1 = 9576, naxis2 = 6388 WHERE id = 10", []).unwrap();
        conn.execute("INSERT INTO calibration_set (id, imagetyp, date) VALUES (500, 'MasterDark', '2025-10-18')", []).unwrap();
        let m = seed_frame(&conn, "dark", "MasterDark", Some((6384, 4258)), Some((1584, 1064)));
        add_members(&conn, 500, &[m]);
        conn.execute(
            "INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (10, 'frame', 500, 'Dark', '2025-10-18T00:00:00Z')", []).unwrap();

        let view = get_calibration_hierarchy_for_frame_set(&conn, 1).unwrap();
        let fg = &view.date_groups[0].camera_groups[0].filter_groups[0];
        assert_eq!((fg.light_frames[0].naxis1, fg.light_frames[0].naxis2), (Some(9576), Some(6388)));
        assert_eq!((fg.dark_sets[0].set.xorgsubf, fg.dark_sets[0].set.yorgsubf), (Some(1584), Some(1064)));
        assert!(
            fg.dark_sets[0].warnings.iter().all(|w| w.warning_type != "geometry"),
            "a size misfit is the gate's (Task 6), not an origin warning"
        );
    }

    /// G-7: a light → set link at another recorded origin (same size) is a
    /// manual choice, and the hierarchy says so on the set.
    #[test]
    fn hierarchy_view_warns_on_another_origin() {
        use crate::calibration::geometry::test_support::{add_members, fits_header_text, seed_frame};
        let conn = db();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'Horsehead')", []).unwrap();
        night_with_lights(&conn, 1, "2025-10-18T18:00:00Z", "2025-10-18T23:30:00Z", &[(10, "2025-10-18T18:00:00Z")]);
        conn.execute("UPDATE frames SET naxis1 = 6384, naxis2 = 4258 WHERE id = 10", []).unwrap();
        // `night_with_lights` gives light 10 file id 10.
        crate::db::insert_fits_header(&conn, 10, &fits_header_text(Some((1584, 1064)))).unwrap();
        conn.execute("INSERT INTO calibration_set (id, imagetyp, date) VALUES (500, 'MasterDark', '2025-10-18')", []).unwrap();
        let m = seed_frame(&conn, "dark", "MasterDark", Some((6384, 4258)), Some((0, 0)));
        add_members(&conn, 500, &[m]);
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at, is_manual_override)
             VALUES (10, 'frame', 500, 'Dark', '2025-10-18T00:00:00Z', 1)", []).unwrap();

        let view = get_calibration_hierarchy_for_frame_set(&conn, 1).unwrap();
        let dark = &view.date_groups[0].camera_groups[0].filter_groups[0].dark_sets[0];
        let w = dark.warnings.iter().find(|w| w.warning_type == "geometry").expect("an origin warning");
        assert_eq!(
            w.message,
            "Dark set #500 is 6384×4258 @ 0,0, a light using it is 6384×4258 @ 1584,1064 — another sub-frame origin"
        );
        assert_eq!((w.calibration_type.as_str(), w.set_id), ("Dark", 500));
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p athenaeum-core --lib set_detail_carries_the_origin hierarchy_view_ -- --test-threads=4` (one filter per run if needed)
Expected: compile errors (no `xorgsubf`, no `naxis1` on the light), then `hierarchy_view_warns_on_another_origin` FAILS (no geometry warning).

- [ ] **Step 3: Implement**

`models.rs`, `CalibrationSetDetail`, after `naxis2`:

```rust
    /// Sub-frame origin (`XORGSUBF`, `YORGSUBF`) from the lowest-frame-id
    /// member's stored header; `None` unless that header records both
    /// (`calibration::geometry::set_origin`).
    pub xorgsubf: Option<i32>,
    pub yorgsubf: Option<i32>,
```
`LightFrameWithCalibration`, after `binning`:

```rust
    /// Stored frame size (NAXIS1 × NAXIS2, after binning) — what the
    /// calibration tables badge each set's size against.
    pub naxis1: Option<i32>,
    pub naxis2: Option<i32>,
```
`ParameterVerdict.name` doc: add `` `geometry` `` to the list and the sentence "(`geometry` is the frame size + sub-frame origin — always enforced, never a config parameter)".

`geometry.rs`:

```rust
/// Fill `detail.xorgsubf` / `yorgsubf` from [`set_origin`].
pub fn fill_set_origin(
    conn: &Connection,
    detail: &mut crate::models::CalibrationSetDetail,
) -> rusqlite::Result<()> {
    if let Some(id) = detail.id {
        let origin = set_origin(conn, id)?;
        detail.xorgsubf = origin.map(|o| o.0);
        detail.yorgsubf = origin.map(|o| o.1);
    }
    Ok(())
}
```

Each construction site: add `xorgsubf: None, yorgsubf: None,` inside the row closure, then fill after the query:
- `hierarchy.rs::get_calibration_set_by_id`: `let mut set = stmt.query_row(…)?; geometry::fill_set_origin(conn, &mut set)?; Ok(set)`.
- `manual.rs::load_set_with_score`: after `.ok()`: `let set_opt = match set_opt { Some(mut s) => { crate::calibration::geometry::fill_set_origin(conn, &mut s)?; Some(s) } None => None };`.
- `db/calibration_links.rs::get_calibration_set_detail`: `let mut detail = stmt.query_row(…)?; crate::calibration::geometry::fill_set_origin(conn, &mut detail)?; Ok(detail)` (this module's `Result` is rusqlite's — the helper returns the same).
- `db/equipment.rs::query_camera_calibration_sets`: change `let sets = …collect()?;` to `let mut sets = …;` then `for s in &mut sets { crate::calibration::geometry::fill_set_origin(conn, s)?; }`.
- `calibration/processor.rs` test literals: `xorgsubf: None, yorgsubf: None,`.

`db/calibration_links.rs` hierarchy view: append `, f.naxis1, f.naxis2` to the END of the SELECT list (after `n.end_time as night_end`), add `naxis1: Option<i32>, naxis2: Option<i32>` to `RawFrame`, read them at indices 25 and 26, and set `naxis1: raw_frame.naxis1, naxis2: raw_frame.naxis2` in the `LightFrameWithCalibration` construction.

The hierarchy's origin warning (G-7). `geometry.rs`, after `fill_set_origin`:

```rust
/// The calibration hierarchy's origin warning for one set (ruling G-7): the
/// set and the first of `light_frame_ids` whose size equals the set's and
/// whose recorded sub-frame origin differs. `None` when the set records no
/// origin (then no light header is read) or every light agrees. A size misfit
/// is not reported here — the readiness gate blocks it (Task 6).
pub fn origin_mismatch_for_lights(
    conn: &Connection,
    set_id: i64,
    light_frame_ids: &[i64],
) -> rusqlite::Result<Option<(Geometry, Geometry)>> {
    let set = geometry_for_set(conn, set_id)?;
    if set.mixed || set.size.is_none() || set.origin.is_none() {
        return Ok(None);
    }
    for &frame_id in light_frame_ids {
        let mut light = LazyFrameGeometry::load(conn, frame_id)?;
        if light.fit(conn, &set)? == Fit::Refused(Refusal::Origin) {
            return Ok(Some((set, light.geometry())));
        }
    }
    Ok(None)
}
```

`db/calibration_links.rs`, a private helper next to `get_warnings_for_calibration_set`:

```rust
/// Ruling G-7: a light → set link at another recorded sub-frame origin is
/// allowed only by hand (auto-link never makes one), and the hierarchy says
/// so on the set's row. A failed check is logged, never swallowed; the row
/// then simply shows no origin warning.
fn push_origin_warning(
    conn: &Connection,
    set_id: i64,
    frame_ids: &[i64],
    calibration_type: &str,
    warnings: &mut Vec<CalibrationWarning>,
) {
    match crate::calibration::geometry::origin_mismatch_for_lights(conn, set_id, frame_ids) {
        Ok(Some((set, light))) => warnings.push(CalibrationWarning {
            warning_type: "geometry".to_string(),
            message: format!(
                "{calibration_type} set #{set_id} is {}, a light using it is {} — another sub-frame origin",
                set.label(),
                light.label()
            ),
            calibration_type: calibration_type.to_string(),
            set_id,
        }),
        Ok(None) => {}
        Err(error) => tracing::warn!(
            set_id,
            %error,
            "sub-frame origin check failed; the hierarchy shows no origin warning for this set"
        ),
    }
}
```
In `get_calibration_hierarchy_for_frame_set`'s three set builders (flat / dark / bias), turn `let warnings = get_warnings_for_calibration_set(conn, set_id, &frame_ids, "Flat");` into `let mut warnings = …;` followed by `push_origin_warning(conn, set_id, &frame_ids, "Flat", &mut warnings);` (`"Dark"` / `"Bias"` in the other two). A set recording an origin costs at most one stored-header parse per light using it, once per view load (not a hot path); a set recording none costs nothing.

`models.rs`, `CalibrationWarning.warning_type`'s comment: `// 'date', 'temperature' or 'geometry' (another sub-frame origin, G-7)`.

TypeScript:

```bash
TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract
cargo test -p athenaeum-core --test ts_contract
git diff --stat src/types   # only src/types/models.ts: CalibrationSetDetail +2, LightFrameWithCalibration +2, ParameterVerdict doc
```
In `LightsAnalysisTable.project-column.test.tsx`'s light fixture add `naxis1: null, naxis2: null,` after `binning`.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p athenaeum-core --lib calibration:: db::calibration_links db::equipment -- --test-threads=4
npx tsc --noEmit
npx vitest run src/components/calibration
```
Expected: PASS. Then the task gate.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/models.rs crates/athenaeum-core/src/calibration/ crates/athenaeum-core/src/db/calibration_links.rs crates/athenaeum-core/src/db/equipment.rs src/types/models.ts src/components/calibration/LightsAnalysisTable.project-column.test.tsx
git commit -m "feat(calibration): set details carry the sub-frame origin, hierarchy lights carry their frame size, the hierarchy warns on a link at another sub-frame origin (R3, G-7)

Co-Authored-By: Claude <your model name> <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01NDfRyApE6tYi9ZMD7b4g8m"
```

---

### Task 6: Readiness (blockers + origin warnings), the stacking plan gate, the pre-calibration refusal, the collab verdict

**Env (every cargo call in this task):** `export CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/.superpowers/target-geom CARGO_BUILD_JOBS=4`, run from the worktree root, tests always `-- --test-threads=4`, one compile at a time (Global Constraints).

**Files:**
- Modify: `crates/athenaeum-core/src/calibration/geometry.rs` (`GeometryMismatch`, `mismatch_sentence`, `origin_sentence`)
- Modify: `crates/athenaeum-core/src/api/lights.rs` (`ExportReadiness.geometry_mismatches` + `.geometry_warnings`, `LinkedLight`, `GeometryTally`, `geometry_tally()`, `readiness_from_data`, `check_mode_ready`, test literal)
- Modify: `crates/athenaeum-core/src/api/masters.rs` (`select_flat_precal`)
- Modify: `crates/athenaeum-core/src/stacking/plan.rs` (`frame_set_inputs` — NO rustfmt)
- Modify: `crates/athenaeum-core/src/collab/gate.rs` (`is_calibration_reason` + its test)
- Modify: `crates/athenaeum-core/src/ts_export.rs`; regenerate `src/types/models.ts`
- Modify: `src/components/export/modeBlocker.test.ts` (fixture fields only)
- Possibly modify (Step 4, only what the baseline proves): `stacking/{plan,run}.rs`, `api/stacking.rs` test fixtures
- Test: `geometry.rs`, `api/lights.rs`, `api/masters.rs`, `stacking/plan.rs`, `collab/gate.rs` (existing test modules)

**Interfaces:**
- Consumes (Task 1): `geometry::{fit, Fit, Refusal, Geometry, LazyFrameGeometry, geometry_for_set}`; `test_support::{add_members, fits_header_text, seed_frame}`; `crate::db::insert_fits_header`.
- Produces:
  - `pub struct GeometryMismatch { calibration_type: String, calibration_set_id: i64, calibration_geometry: String, source_set_id: Option<i64>, source_geometry: String, light_count: i64 }` (serde camelCase, ts-rs) in `calibration::geometry` — one shape for both lists.
  - `pub fn mismatch_sentence(m: &[GeometryMismatch]) -> String` and `pub fn origin_sentence(m: &[GeometryMismatch]) -> String` — Task 7 mirrors both verbatim in TS.
  - `ExportReadiness.geometry_mismatches: Vec<GeometryMismatch>` (size / mixed — blocks the calibrated mode and the plan's `links`) and `ExportReadiness.geometry_warnings: Vec<GeometryMismatch>` (another recorded origin — advisory, G-7), both `#[serde(default)]` → TS `geometryMismatches` / `geometryWarnings: Array<GeometryMismatch>`. Both empty for an attested set.
  - The stacking plan carries `origin_sentence` in its `warnings` (frame sets only) and never a blocker for it.

- [ ] **Step 0: Baseline the stacking tests BEFORE any edit** (review I-5, G-3)

About 110 `add_master_dark_and_flat(` calls feed the stacking tests (23 in `stacking/plan.rs`, 85 in `stacking/run.rs`, 1 in `api/stacking.rs`; `grep -c`, 2026-10-20), several sized from constants (`SMALL_W`, `COARSE_W`/`FINE_W`, `odd_w`, `W`, `(&f, &[mono_id, osc_id], 32, 24)`). Once the gate lands, a fixture whose masters differ in size from its lights gets a `links` blocker and the run tests behind it fail — which a reader can mistake for "the gate is wrong". So first, on the unchanged tree:

```bash
cargo test -p athenaeum-core --lib stacking:: -- --test-threads=4 2>&1 | tee "$CARGO_TARGET_DIR/geom-stacking-baseline.txt" | tail -5
cargo test -p athenaeum-core --lib api::stacking -- --test-threads=4 2>&1 | tee -a "$CARGO_TARGET_DIR/geom-stacking-baseline.txt" | tail -5
grep -E "^test .* FAILED$|^failures:" "$CARGO_TARGET_DIR/geom-stacking-baseline.txt"
```
Record the failing set (expected: none, except the known wall-clock flakes named in Task 8 Step 2). Step 4 compares against it.

- [ ] **Step 1: Write the failing tests**

`geometry.rs` `mod tests` — the shared sentence vector (Task 7 asserts the identical string in TS):

```rust
    fn mm(t: &str, id: i64, cal: &str, src_set: Option<i64>, src: &str, n: i64) -> GeometryMismatch {
        GeometryMismatch {
            calibration_type: t.to_string(),
            calibration_set_id: id,
            calibration_geometry: cal.to_string(),
            source_set_id: src_set,
            source_geometry: src.to_string(),
            light_count: n,
        }
    }

    #[test]
    fn mismatch_sentence_vectors() {
        assert_eq!(
            mismatch_sentence(&[
                mm("Dark", 1843, "6384×4258", None, "9576×6388", 151),
                mm("Dark", 1267, "6384×4258 @ 1584,1064", Some(558), "9576×6388", 0),
            ]),
            "Calibration does not fit the frame size — dark set #1843 is 6384×4258, 151 lights are 9576×6388; flat set #558 is 9576×6388, its dark #1267 is 6384×4258 @ 1584,1064; re-link with Find Calibration"
        );
        assert_eq!(
            mismatch_sentence(&[
                mm("Flat", 1, "mixed sizes", None, "9576×6388", 1),
                mm("Bias", 2, "6384×4258", None, "9576×6388", 2),
                mm("DarkFlat", 3, "6384×4258", Some(9), "9576×6388", 0),
                mm("Dark", 4, "6384×4258", None, "9576×6388", 3),
            ]),
            "Calibration does not fit the frame size — flat set #1 is mixed sizes, 1 light is 9576×6388; bias set #2 is 6384×4258, 2 lights are 9576×6388; flat set #9 is 9576×6388, its dark flat #3 is 6384×4258; +1 more; re-link with Find Calibration"
        );
    }

    /// G-7: the advisory sentence for links at another recorded origin.
    #[test]
    fn origin_sentence_vectors() {
        assert_eq!(
            origin_sentence(&[
                mm("Dark", 1518, "6384×4258 @ 0,0", None, "6384×4258 @ 1584,1064", 151),
                mm("Bias", 7, "6384×4258 @ 0,0", Some(558), "6384×4258 @ 1584,1064", 0),
            ]),
            "Calibration at another sub-frame origin — dark set #1518 is 6384×4258 @ 0,0, 151 lights are 6384×4258 @ 1584,1064; flat set #558 is 6384×4258 @ 1584,1064, its bias #7 is 6384×4258 @ 0,0"
        );
    }
```

`api/lights.rs` `mod tests` (reuse `seed_db`, `seed_frame_set`, `seed_light` — its file id is `frame_id + 1_000_000` —, `seed_set`, `seed_masters`, `add_link`, `add_set_link`):

```rust
    // ── Geometry (owner ruling 2026-10-20, rulings R2/R8) ───────────────
    fn set_light_size(conn: &Connection, frame_id: i64, size: (i32, i32)) {
        conn.execute("UPDATE frames SET naxis1 = ?1, naxis2 = ?2 WHERE id = ?3", params![size.0, size.1, frame_id]).unwrap();
    }

    fn give_member(conn: &Connection, set_id: i64, tag: &str, imagetyp: &str, size: (i32, i32)) {
        use crate::calibration::geometry::test_support::{add_members, seed_frame};
        let m = seed_frame(conn, tag, imagetyp, Some(size), None);
        add_members(conn, set_id, &[m]);
    }

    /// The owner's case: 151 lights manually linked to a ROI master dark.
    /// Readiness names it, the calibrated mode refuses with the sentence, the
    /// raw modes do not (R6).
    #[test]
    fn readiness_names_a_manual_misfit() {
        use crate::calibration::geometry::GeometryMismatch;
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        set_light_size(&conn, 1, (9576, 6388));
        seed_masters(&conn);
        give_member(&conn, 100, "dark100", "MasterDark", (6384, 4258));
        give_member(&conn, 101, "flat101", "MasterFlat", (9576, 6388));
        add_link(&conn, 1, 100, "Dark");
        add_link(&conn, 1, 101, "Flat");
        conn.execute("UPDATE calibration_set_to_frames SET is_manual_override = 1 WHERE calibration_set_id = 100", []).unwrap();

        let r = compute_export_readiness(&conn, 1).unwrap();
        assert_eq!(r.geometry_mismatches, vec![GeometryMismatch {
            calibration_type: "Dark".to_string(),
            calibration_set_id: 100,
            calibration_geometry: "6384×4258".to_string(),
            source_set_id: None,
            source_geometry: "9576×6388".to_string(),
            light_count: 1,
        }]);
        assert!(r.geometry_warnings.is_empty());
        // The member files (`/geometry/dark100.fits`, …) are not on disk, so
        // `missing_master_files` is 2 and the rebuildability classifiers warn;
        // the geometry sentence still wins because its arm comes BEFORE the
        // missing-file arm (review M-5). A later reorder of the arms is not a
        // geometry regression — re-read this test before "fixing" it.
        assert_eq!(
            check_mode_ready(&r, ExportMode::CalibratedLights).unwrap_err(),
            "Calibration does not fit the frame size — dark set #100 is 6384×4258, 1 light is 9576×6388; re-link with Find Calibration"
        );
        for mode in [ExportMode::LightsOnly, ExportMode::RawWithCalibrationSets, ExportMode::RawWithMasters] {
            assert!(check_mode_ready(&r, mode).is_ok(), "{mode:?}");
        }
    }

    /// R8 (review I-4): the bias is judged exactly when no dark RESOLVED for
    /// the light — the fact the missing-master-file pass keys on — not when no
    /// Dark link exists; a set that records no size blocks nothing (R1).
    #[test]
    fn readiness_judges_the_bias_only_without_a_resolved_dark() {
        use crate::calibration::geometry::test_support::{add_members, seed_frame};
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        for id in 1..=3 {
            seed_light(&conn, id, session);
            set_light_size(&conn, id, (9576, 6388));
        }
        seed_masters(&conn); // 100 dark, 101 flat, 102 bias (all is_master_library = 1)
        // Dark 100 resolves (a member file row) but records no size: Unknown.
        let d = seed_frame(&conn, "dark100", "MasterDark", None, None);
        add_members(&conn, 100, &[d]);
        give_member(&conn, 102, "bias102", "MasterBias", (6384, 4258));
        seed_set(&conn, 103, "Dark", false); // raw, no master: the engine reads the bias
        add_link(&conn, 1, 100, "Dark");
        add_link(&conn, 1, 102, "Bias");
        add_link(&conn, 2, 102, "Bias");
        add_link(&conn, 3, 103, "Dark");
        add_link(&conn, 3, 102, "Bias");

        let r = compute_export_readiness(&conn, 1).unwrap();
        assert_eq!(r.geometry_mismatches.len(), 1, "{:?}", r.geometry_mismatches);
        assert_eq!(
            (r.geometry_mismatches[0].calibration_set_id, r.geometry_mismatches[0].light_count),
            (102, 2),
            "lights 2 (no dark) and 3 (a dark link that resolves to no master) read the bias; light 1's resolved dark skips it"
        );
    }

    /// G-7 (owner 2026-10-20, "auto no, manual yes"): a manual link at another
    /// recorded origin, same size, is a WARNING — never a blocker.
    #[test]
    fn readiness_reports_an_origin_misfit_as_a_warning() {
        use crate::calibration::geometry::test_support::{add_members, fits_header_text, seed_frame};
        use crate::calibration::geometry::GeometryMismatch;
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        set_light_size(&conn, 1, (6384, 4258));
        crate::db::insert_fits_header(&conn, 1_000_001, &fits_header_text(Some((1584, 1064)))).unwrap();
        seed_masters(&conn);
        let m = seed_frame(&conn, "dark100", "MasterDark", Some((6384, 4258)), Some((0, 0)));
        add_members(&conn, 100, &[m]);
        add_link(&conn, 1, 100, "Dark");
        conn.execute("UPDATE calibration_set_to_frames SET is_manual_override = 1 WHERE calibration_set_id = 100", []).unwrap();

        let r = compute_export_readiness(&conn, 1).unwrap();
        assert!(r.geometry_mismatches.is_empty(), "{:?}", r.geometry_mismatches);
        assert_eq!(r.geometry_warnings, vec![GeometryMismatch {
            calibration_type: "Dark".to_string(),
            calibration_set_id: 100,
            calibration_geometry: "6384×4258 @ 0,0".to_string(),
            source_set_id: None,
            source_geometry: "6384×4258 @ 1584,1064".to_string(),
            light_count: 1,
        }]);
        // Advisory: the gate's verdict is what it is without the warning (here
        // the missing master file answers — the member path is not on disk).
        let without = ExportReadiness { geometry_warnings: Vec::new(), ..r.clone() };
        assert_eq!(
            check_mode_ready(&r, ExportMode::CalibratedLights),
            check_mode_ready(&without, ExportMode::CalibratedLights)
        );
    }

    /// R2/R8: a raw flat the run will build is judged against EVERY
    /// pre-calibration link it carries.
    #[test]
    fn readiness_names_a_flat_precal_misfit() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        set_light_size(&conn, 1, (9576, 6388));
        seed_set(&conn, 200, "Flat", false);
        give_member(&conn, 200, "flat200", "Flat", (9576, 6388));
        seed_set(&conn, 300, "MasterDark", true);
        give_member(&conn, 300, "dark300", "MasterDark", (6384, 4258));
        seed_set(&conn, 301, "MasterBias", true);
        give_member(&conn, 301, "bias301", "MasterBias", (9576, 6388));
        add_link(&conn, 1, 200, "Flat");
        add_set_link(&conn, 200, 300, "Dark");
        add_set_link(&conn, 200, 301, "Bias");

        let r = compute_export_readiness(&conn, 1).unwrap();
        assert_eq!(r.geometry_mismatches.len(), 1, "{:?}", r.geometry_mismatches);
        let m = &r.geometry_mismatches[0];
        assert_eq!((m.calibration_type.as_str(), m.calibration_set_id, m.source_set_id, m.light_count), ("Dark", 300, Some(200), 0));
    }
```

`api/masters.rs` `mod tests` (reuse `test_conn`, `seed_flat_set`, `seed_precal_set`, `link`):

```rust
    /// Plan ruling R7: a linked pre-calibration master of another size is
    /// refused at selection — not skipped to the next link.
    #[test]
    fn precal_master_of_another_size_is_refused_not_skipped() {
        use crate::calibration::geometry::test_support::{add_members, seed_frame};
        let conn = test_conn();
        let flat = seed_flat_set(&conn, 1.0);
        let member = seed_frame(&conn, "flat-member", "Flat", Some((9576, 6388)), None);
        add_members(&conn, flat, &[member]);
        let dark = seed_precal_set(&conn, "MasterDark", 1, Some(1.0));
        conn.execute(
            "UPDATE frames SET naxis1 = 6384, naxis2 = 4258
             WHERE id IN (SELECT frame_id FROM calibration_set_frames WHERE set_id = ?1)",
            [dark],
        ).unwrap();
        link(&conn, flat, dark, "Dark");
        let bias = seed_precal_set(&conn, "MasterBias", 1, None);
        link(&conn, flat, bias, "Bias");

        let msg = match select_flat_precal(&conn, flat, Some(1.0), None) {
            Ok(_) => panic!("a misfit pre-calibration must be refused"),
            Err(e) => e.to_string(),
        };
        assert_eq!(msg, format!("linked dark master #{dark} is 6384×4258, the flats are 9576×6388 — re-link the flats' pre-calibration"));
    }

    /// G-7: a pre-calibration master of the same size at another recorded
    /// origin is used as linked, with a warning — never refused.
    #[test]
    fn precal_master_at_another_origin_is_used_with_a_warning() {
        use crate::calibration::geometry::test_support::{add_members, fits_header_text, seed_frame};
        let conn = test_conn();
        let flat = seed_flat_set(&conn, 1.0);
        let member = seed_frame(&conn, "flat-member", "Flat", Some((6384, 4258)), Some((1584, 1064)));
        add_members(&conn, flat, &[member]);
        let dark = seed_precal_set(&conn, "MasterDark", 1, Some(1.0));
        let dark_file: i64 = conn.query_row(
            "SELECT f.file_id FROM calibration_set_frames csf JOIN frames f ON f.id = csf.frame_id
             WHERE csf.set_id = ?1",
            [dark],
            |r| r.get(0),
        ).unwrap();
        conn.execute("UPDATE frames SET naxis1 = 6384, naxis2 = 4258 WHERE file_id = ?1", [dark_file]).unwrap();
        crate::db::insert_fits_header(&conn, dark_file, &fits_header_text(Some((0, 0)))).unwrap();
        link(&conn, flat, dark, "Dark");

        let sel = select_flat_precal(&conn, flat, Some(1.0), None).expect("another origin is not a refusal");
        assert!(matches!(&sel.choice, PrecalChoice::Master { set_id, .. } if *set_id == dark));
        assert!(sel.warnings.iter().any(|w| w.contains("another sub-frame origin")), "{:?}", sel.warnings);
    }
```

`stacking/plan.rs` `mod tests` (after `plan_blocks_without_masters`; hand-formatted):

```rust
    /// Owner ruling 2026-10-20 (calibration geometry, R2): lights linked to
    /// masters of another size are blocked up front under `links`, naming the
    /// set and both sizes — the run used to be admitted and die in Masters or
    /// Calibrate.
    #[test]
    fn plan_blocks_masters_of_another_size_under_links() {
        let f = test_fixtures::frame_set("Horsehead C11");
        let ids: Vec<i64> = THREE_TIMES
            .iter()
            .enumerate()
            .map(|(i, t)| test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t)).0)
            .collect();
        // `light_spec` lights are 64×48; these masters are 32×24.
        let (dark_set, flat_set) = test_fixtures::add_master_dark_and_flat(&f, &ids, 32, 24);
        let settings = SettingsManager::new();
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();
        let links: Vec<&PlanBlocker> = plan.blockers.iter().filter(|b| b.code == "links").collect();
        assert_eq!(links.len(), 1, "{:?}", plan.blockers);
        assert_eq!(
            links[0].message,
            format!(
                "Calibration does not fit the frame size — dark set #{dark_set} is 32×24, 3 lights are 64×48; \
                 flat set #{flat_set} is 32×24, 3 lights are 64×48; re-link with Find Calibration"
            )
        );
    }

    /// G-7: masters of the lights' size at another recorded sub-frame origin
    /// (a manual choice) are a plan WARNING, never a `links` blocker.
    #[test]
    fn plan_warns_on_masters_at_another_origin() {
        use crate::calibration::geometry::test_support::fits_header_text;
        let f = test_fixtures::frame_set("Horsehead C11");
        let ids: Vec<i64> = THREE_TIMES
            .iter()
            .enumerate()
            .map(|(i, t)| test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t)).0)
            .collect();
        let (dark_set, flat_set) = test_fixtures::add_master_dark_and_flat(&f, &ids, 64, 48);
        // A newer stored header wins (`origin_for_file` reads the newest row).
        let file_of = |sql: &str, id: i64| -> i64 { f.conn.query_row(sql, [id], |r| r.get(0)).unwrap() };
        for &id in &ids {
            let file_id = file_of("SELECT file_id FROM frames WHERE id = ?1", id);
            crate::db::insert_fits_header(&f.conn, file_id, &fits_header_text(Some((1584, 1064)))).unwrap();
        }
        for set in [dark_set, flat_set] {
            let file_id = file_of(
                "SELECT f.file_id FROM calibration_set_frames csf JOIN frames f ON f.id = csf.frame_id
                 WHERE csf.set_id = ?1",
                set,
            );
            crate::db::insert_fits_header(&f.conn, file_id, &fits_header_text(Some((0, 0)))).unwrap();
        }
        let settings = SettingsManager::new();
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();
        assert!(plan.blockers.iter().all(|b| b.code != "links"), "{:?}", plan.blockers);
        let expected = format!(
            "Calibration at another sub-frame origin — dark set #{dark_set} is 64×48 @ 0,0, 3 lights are 64×48 @ 1584,1064; \
             flat set #{flat_set} is 64×48 @ 0,0, 3 lights are 64×48 @ 1584,1064"
        );
        assert!(plan.warnings.contains(&expected), "{:?}", plan.warnings);
    }
```

`collab/gate.rs` test `every_calibrated_lights_readiness_sentence_is_a_calibration_blocker`: add `geometry_mismatches: Vec::new(), geometry_warnings: Vec::new(),` to the `ready` literal, and a fourth scenario (an origin WARNING is never a reason, so it gets no scenario: `check_mode_ready` never returns its sentence):

```rust
            (
                // Geometry (2026-10-20): a link problem, offered with attest.
                ExportReadiness {
                    geometry_mismatches: vec![crate::calibration::geometry::GeometryMismatch {
                        calibration_type: "Dark".to_string(),
                        calibration_set_id: 1843,
                        calibration_geometry: "6384×4258".to_string(),
                        source_set_id: None,
                        source_geometry: "9576×6388".to_string(),
                        light_count: 1,
                    }],
                    ..ready.clone()
                },
                "linkCalibration",
            ),
```
and change its comment "The three ways" → "The four ways".

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p athenaeum-core --lib geometry::tests:: api::lights::tests::readiness_ api::masters::tests::precal_master_ plan_blocks_masters_of_another_size plan_warns_on_masters_at_another_origin collab::gate -- --test-threads=4` (one filter per run if needed).
Expected: compile errors, then failures (no blocker, no refusal, no warning, `threshold` classification).

- [ ] **Step 3: Implement**

`geometry.rs` (add `use serde::{Deserialize, Serialize};`):

```rust
/// One calibration link whose set does not fit the frames it would
/// calibrate — the readiness gate's geometry tally (`ExportReadiness`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct GeometryMismatch {
    /// The link type: `Dark` / `Flat` / `Bias` for a light's link,
    /// `DarkFlat` / `Dark` / `Bias` for a flat set's pre-calibration link.
    pub calibration_type: String,
    pub calibration_set_id: i64,
    /// `Geometry::label` of the linked set.
    pub calibration_geometry: String,
    /// The flat set whose pre-calibration link this is; `None` for a light's.
    pub source_set_id: Option<i64>,
    /// `Geometry::label` of the lights (or of that flat set).
    pub source_geometry: String,
    /// In-scope lights carrying this link; `0` for a pre-calibration link.
    pub light_count: i64,
}

fn type_word(calibration_type: &str) -> &'static str {
    match calibration_type {
        "Dark" => "dark",
        "Flat" => "flat",
        "Bias" => "bias",
        "DarkFlat" => "dark flat",
        _ => "calibration",
    }
}

/// The parts both geometry sentences share: the first three entries, then
/// `+N more`.
fn mismatch_parts(mismatches: &[GeometryMismatch]) -> String {
    let parts: Vec<String> = mismatches
        .iter()
        .take(3)
        .map(|m| match m.source_set_id {
            None => format!(
                "{} set #{} is {}, {} light{} {} {}",
                type_word(&m.calibration_type),
                m.calibration_set_id,
                m.calibration_geometry,
                m.light_count,
                if m.light_count == 1 { "" } else { "s" },
                if m.light_count == 1 { "is" } else { "are" },
                m.source_geometry
            ),
            Some(source_set_id) => format!(
                "flat set #{source_set_id} is {}, its {} #{} is {}",
                m.source_geometry,
                type_word(&m.calibration_type),
                m.calibration_set_id,
                m.calibration_geometry
            ),
        })
        .collect();
    let more = if mismatches.len() > 3 {
        format!("; +{} more", mismatches.len() - 3)
    } else {
        String::new()
    };
    format!("{}{more}", parts.join("; "))
}

/// The blocker sentence for a non-empty size / mixed mismatch list. The
/// Export tab mirrors it verbatim
/// (`src/components/export/modeBlocker.ts::geometryMismatchSentence`); the
/// collab verdict classifies it by "does not fit the frame size".
pub fn mismatch_sentence(mismatches: &[GeometryMismatch]) -> String {
    format!(
        "Calibration does not fit the frame size — {}; re-link with Find Calibration",
        mismatch_parts(mismatches)
    )
}

/// The advisory sentence for links at another recorded sub-frame origin
/// (G-7: a manual choice the owner allows). Never a blocker; mirrored
/// verbatim by `src/components/export/modeBlocker.ts::geometryOriginSentence`.
pub fn origin_sentence(mismatches: &[GeometryMismatch]) -> String {
    format!("Calibration at another sub-frame origin — {}", mismatch_parts(mismatches))
}
```

`ts_export.rs`: add `crate::calibration::geometry::GeometryMismatch,` on the line before `crate::api::lights::ExportReadiness,`.

`api/lights.rs`:
1. Imports: `use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};`, `use crate::calibration::geometry::{self, Fit, Geometry, GeometryMismatch, LazyFrameGeometry, Refusal};` and `use crate::models::CalibrationLink;`.
2. `ExportReadiness`, after `calibration_warnings`:

```rust
    /// Calibration links whose set does not fit the frames it would calibrate
    /// in SIZE (owner ruling 2026-10-20; rulings R2, R8): a light's dark and
    /// flat (its bias only when no dark resolved), and every pre-calibration
    /// link of a flat set the run builds or rebuilds. Blocks the
    /// calibrated-lights mode and the stacking plan (`links`), never the raw
    /// modes (R6). Empty for an attested set.
    #[serde(default)]
    pub geometry_mismatches: Vec<GeometryMismatch>,
    /// The same links at the same size but another recorded sub-frame origin
    /// (G-7 — auto-link never makes one, so it is a manual choice). Advisory:
    /// the Export tab lists it, the stacking plan warns, nothing blocks.
    /// Empty for an attested set.
    #[serde(default)]
    pub geometry_warnings: Vec<GeometryMismatch>,
```
3. New types and fns (above `filter_export_data_to_frames`):

```rust
/// One linked light as the readiness pass's first loop saw it — the geometry
/// pass reuses these links instead of reading them again (review I-3).
struct LinkedLight {
    frame_id: i64,
    links: Vec<CalibrationLink>,
    /// A Dark link resolved to a master: the fact the missing-master-file
    /// pass keys the bias on (review fix #1). The geometry pass keys on the
    /// same one (review I-4) — a Dark link to a set with no master leaves the
    /// engine reading the bias, so the bias is judged.
    dark_resolved: bool,
}

/// What the geometry pass found.
#[derive(Default)]
struct GeometryTally {
    /// Size or mixed refusals — blockers.
    mismatches: Vec<GeometryMismatch>,
    /// Another recorded sub-frame origin at the same size — advisory (G-7).
    warnings: Vec<GeometryMismatch>,
}

fn cached_set_geometry(
    conn: &Connection,
    cache: &mut HashMap<i64, Geometry>,
    set_id: i64,
) -> Result<Geometry, ApiError> {
    if let Some(g) = cache.get(&set_id) {
        return Ok(*g);
    }
    let g = geometry::geometry_for_set(conn, set_id)?;
    cache.insert(set_id, g);
    Ok(g)
}

/// The geometry pass of the readiness gate (owner ruling 2026-10-20; rulings
/// R2, R8, G-7). Set geometry is read once per set; a light's stored header
/// is parsed only when a linked set of its size records an origin (review
/// I-3, ruling R10). A side that records no size blocks nothing (R1): counted
/// and logged once.
fn geometry_tally(
    conn: &Connection,
    linked: &[LinkedLight],
    missing_master_ids: &[i64],
) -> Result<GeometryTally, ApiError> {
    let mut sets: HashMap<i64, Geometry> = HashMap::new();
    let mut unknown = 0usize;

    // (origin only?, calibration type, set id, light label) -> (set label, light count)
    let mut light_links: BTreeMap<(bool, String, i64, String), (String, i64)> = BTreeMap::new();
    let mut flats_to_build: BTreeSet<i64> = BTreeSet::new();
    let mut raw_flat: HashMap<i64, bool> = HashMap::new();
    for light in linked {
        let mut frame = LazyFrameGeometry::load(conn, light.frame_id)?;
        // The links the engine resolves (`light_resolve::resolve_frame_inputs`):
        // dark and flat always, the bias only without a resolved dark (R8).
        for (calibration_type, applies) in [("Dark", true), ("Flat", true), ("Bias", !light.dark_resolved)] {
            if !applies {
                continue;
            }
            let Some(set_id) = link_set_id(&light.links, calibration_type) else {
                continue;
            };
            let set = cached_set_geometry(conn, &mut sets, set_id)?;
            match frame.fit(conn, &set)? {
                Fit::Refused(refusal) => {
                    let key = (
                        refusal == Refusal::Origin,
                        calibration_type.to_string(),
                        set_id,
                        frame.report_label(refusal),
                    );
                    light_links.entry(key).or_insert((set.label(), 0)).1 += 1;
                }
                Fit::Unknown => unknown += 1,
                Fit::Fits => {}
            }
            if calibration_type == "Flat" {
                let is_raw = match raw_flat.get(&set_id) {
                    Some(v) => *v,
                    None => {
                        let v: bool = conn.query_row(
                            "SELECT imagetyp = 'Flat' AND is_master_library = 0 FROM calibration_set WHERE id = ?1",
                            params![set_id],
                            |r| r.get(0),
                        )?;
                        raw_flat.insert(set_id, v);
                        v
                    }
                };
                if is_raw {
                    flats_to_build.insert(set_id);
                }
            }
        }
    }
    // Flats stage 0.5 rebuilds: the provenance source of every missing master flat.
    for &master_set_id in missing_master_ids {
        let imagetyp: String = conn.query_row(
            "SELECT imagetyp FROM calibration_set WHERE id = ?1",
            params![master_set_id],
            |r| r.get(0),
        )?;
        if imagetyp != "MasterFlat" {
            continue;
        }
        match crate::db::master_provenance::get(conn, master_set_id) {
            Ok(Some(p)) => {
                if let Some(source_set_id) = p.source_set_id {
                    flats_to_build.insert(source_set_id);
                }
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(
                master_set_id,
                %error,
                "master provenance unreadable; its flat's pre-calibration geometry is not checked"
            ),
        }
    }

    let mut tally = GeometryTally::default();
    for ((origin_only, calibration_type, calibration_set_id, source_geometry), (calibration_geometry, light_count)) in
        light_links
    {
        let m = GeometryMismatch {
            calibration_type,
            calibration_set_id,
            calibration_geometry,
            source_set_id: None,
            source_geometry,
            light_count,
        };
        if origin_only {
            tally.warnings.push(m);
        } else {
            tally.mismatches.push(m);
        }
    }
    for flat_set_id in flats_to_build {
        let flat = cached_set_geometry(conn, &mut sets, flat_set_id)?;
        for link in crate::db::calibration_links::get_links_for_calibration_set(conn, flat_set_id)? {
            if !matches!(link.calibration_type.as_str(), "DarkFlat" | "Dark" | "Bias") {
                continue;
            }
            let set = cached_set_geometry(conn, &mut sets, link.calibration_set_id)?;
            let refusal = match geometry::fit(&flat, &set) {
                Fit::Refused(refusal) => refusal,
                Fit::Unknown => {
                    unknown += 1;
                    continue;
                }
                Fit::Fits => continue,
            };
            let m = GeometryMismatch {
                calibration_type: link.calibration_type.clone(),
                calibration_set_id: link.calibration_set_id,
                calibration_geometry: set.label(),
                source_set_id: Some(flat_set_id),
                source_geometry: flat.label(),
                light_count: 0,
            };
            if refusal == Refusal::Origin {
                tally.warnings.push(m);
            } else {
                tally.mismatches.push(m);
            }
        }
    }
    if unknown > 0 {
        tracing::warn!(count = unknown, "calibration geometry not recorded on one side; size not compared");
    }
    Ok(tally)
}
```
   (`get_links_for_calibration_set` returns a set's links `ORDER BY calibration_type`, one per type, so the list is deterministic.)
4. `readiness_from_data`:
   - In the first loop, declare `let mut linked: Vec<LinkedLight> = Vec::new();` before it, and at the END of the loop body (after the `if dark.is_none() { … }` bias block) push `linked.push(LinkedLight { frame_id, dark_resolved: dark.is_some(), links });` — `links` is only borrowed above, so it moves here.
   - Move the existing `let attested = crate::db::collab::frames_set_attested(…).unwrap_or_else(…);` block, unchanged, up to just before the geometry pass (it reads one row and depends on nothing computed in between).
   - Right after `let missing_master_files = missing_masters.len() as i64;`:

```rust
    // Geometry (owner ruling 2026-10-20; R2, R8, G-7). An attested set's
    // lights are copied as they are (R3, wave 6 §13): no calibration applies,
    // so nothing about its links is judged.
    let geometry = if attested {
        GeometryTally::default()
    } else {
        let missing_master_ids: Vec<i64> = missing_masters.values().copied().collect();
        geometry_tally(conn, &linked, &missing_master_ids)?
    };
```
   - add `geometry_mismatches = geometry.mismatches.len(), geometry_warnings = geometry.warnings.len(),` to the `"export readiness computed"` debug, and `geometry_mismatches: geometry.mismatches, geometry_warnings: geometry.warnings,` to the returned struct.
5. `check_mode_ready`, between the `unlinked_lights` arm and the `missing_master_files` arm (only the SIZE list blocks; `geometry_warnings` never reaches this function's verdict):

```rust
        // Geometry (owner ruling 2026-10-20): a link whose set does not fit
        // the frames' size is a linking problem; after the unlinked-lights arm
        // and before the missing-file one, mirrored by the Export tab.
        ExportMode::CalibratedLights if !r.geometry_mismatches.is_empty() => Err(
            crate::calibration::geometry::mismatch_sentence(&r.geometry_mismatches),
        ),
```
6. `check_mode_ready_truth_table`'s `ready` literal: add `geometry_mismatches: Vec::new(), geometry_warnings: Vec::new(),`.

`api/masters.rs::select_flat_precal`, inside the loop after the `if cal_type == "Dark" { … }` exposure block and before the `let path: String = …` lookup:

```rust
        // Geometry (owner ruling 2026-10-20; plan ruling R7, G-7): a
        // pre-calibration master of another size can never calibrate these
        // flats — refused here, at selection, naming both (the engine's own
        // guard refuses the same build later, after reading every flat). The
        // same size at another recorded sub-frame origin is a manual choice
        // (auto-link never makes it): used as linked, with a warning.
        let flat_geometry = crate::calibration::geometry::geometry_for_set(conn, set_id)?;
        let precal_geometry = crate::calibration::geometry::geometry_for_set(conn, precal_set)?;
        match crate::calibration::geometry::fit(&flat_geometry, &precal_geometry) {
            crate::calibration::geometry::Fit::Refused(crate::calibration::geometry::Refusal::Origin) => {
                tracing::warn!(
                    source_set_id = set_id,
                    master_set_id = precal_set,
                    calibration_type = cal_type,
                    source_geometry = %flat_geometry.label(),
                    set_geometry = %precal_geometry.label(),
                    "flat pre-calibration at another sub-frame origin; used as linked"
                );
                warnings.push(format!(
                    "linked {} master #{precal_set} is {}, the flats are {} — another sub-frame origin, used as linked",
                    cal_type.to_lowercase(),
                    precal_geometry.label(),
                    flat_geometry.label()
                ));
            }
            crate::calibration::geometry::Fit::Refused(_) => {
                tracing::warn!(
                    source_set_id = set_id,
                    master_set_id = precal_set,
                    calibration_type = cal_type,
                    source_geometry = %flat_geometry.label(),
                    set_geometry = %precal_geometry.label(),
                    "flat pre-calibration refused: the master does not fit the frame size"
                );
                return Err(ApiError::Invalid(format!(
                    "linked {} master #{precal_set} is {}, the flats are {} — re-link the flats' pre-calibration",
                    cal_type.to_lowercase(),
                    precal_geometry.label(),
                    flat_geometry.label()
                )));
            }
            crate::calibration::geometry::Fit::Fits | crate::calibration::geometry::Fit::Unknown => {}
        }
```
(A `use crate::calibration::geometry::{Fit, Refusal};` at the top of the function body shortens the arms — either spelling.) Update the fn's doc comment: "A linked master of another size is refused (plan ruling R7), not skipped; the same size at another recorded sub-frame origin is used with a warning (G-7)."

`stacking/plan.rs::frame_set_inputs` (hand-formatted, no rustfmt):
- right after the `if readiness.unlinked_lights > 0 { … }` block, inside the not-attested branch:

```rust
        // Geometry (owner ruling 2026-10-20, R2): the readiness gate's own
        // tally, so the plan and the export can never disagree.
        if !readiness.geometry_mismatches.is_empty() {
            blockers.push(PlanBlocker {
                code: "links".to_string(),
                message: crate::calibration::geometry::mismatch_sentence(
                    &readiness.geometry_mismatches,
                ),
            });
        }
```
- the returned `SubjectInputs { … warnings: Vec::new(), … }` gets the origin warning instead (G-7: a warning, never a blocker; `build_plan` appends `arm_warnings` to the plan's own). Before the `Ok(SubjectInputs {`:

```rust
    // G-7: links at another recorded sub-frame origin (a manual choice) are
    // a WARNING on the plan, never a blocker. Empty for an attested set.
    let warnings = if readiness.geometry_warnings.is_empty() {
        Vec::new()
    } else {
        vec![crate::calibration::geometry::origin_sentence(&readiness.geometry_warnings)]
    };
```
   and `warnings,` in place of `warnings: Vec::new(),`.

`collab/gate.rs::is_calibration_reason`, before the `could not verify calibration` line:

```rust
        // CalibratedLights: "Calibration does not fit the frame size — …" (geometry, 2026-10-20).
        || f.contains("does not fit the frame size")
```

TypeScript:

```bash
TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract
cargo test -p athenaeum-core --test ts_contract
git diff --stat src/types   # models.ts only: GeometryMismatch (new), ExportReadiness.geometryMismatches + .geometryWarnings
```
In `src/components/export/modeBlocker.test.ts`'s `ready` fixture add `geometryMismatches: [], geometryWarnings: [],`.

- [ ] **Step 4: Run the tests to verify they pass, then the baseline comparison** (review I-5)

```bash
cargo test -p athenaeum-core --lib calibration::geometry api::lights api::masters collab::gate -- --test-threads=4
cargo test -p athenaeum-core --lib stacking:: -- --test-threads=4 2>&1 | tee "$CARGO_TARGET_DIR/geom-stacking-after.txt" | tail -5
cargo test -p athenaeum-core --lib api::stacking -- --test-threads=4 2>&1 | tee -a "$CARGO_TARGET_DIR/geom-stacking-after.txt" | tail -5
grep -E "^test .* FAILED$" "$CARGO_TARGET_DIR/geom-stacking-after.txt"
npx tsc --noEmit
```
Expected: the new tests PASS. Compare the stacking failures with Step 0's baseline. For EVERY test that fails now and did not fail then:
1. Read its `links` blocker message (the sentence names the set id and both sizes) and its fixture: the lights' size (`light_spec` / `LightSpec { width, height }` or the test's constant) beside the `add_master_dark_and_flat(&f, &ids, W, H)` arguments.
2. If the two sizes differ and the test is NOT about frame size, align the MASTER size to the lights' (`add_master_dark_and_flat(&f, &ids, <light width>, <light height>)`), re-run that test alone, and list it with both sizes for the commit message.
3. If the test's intent IS a size mismatch (it asserts an engine geometry error, a size-related blocker, or names "mismatch" / "geometry" in its doc), or the blocker is not a geometry sentence, or the sizes already agree: STOP and report — never weaken the gate, never edit the assertion (G-3). (`calibration_library::light_cal`'s `preloaded master geometry mismatch` test calls the engine directly, not through the gate; it is expected to stay green untouched.)
Then the task gate.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/calibration/geometry.rs crates/athenaeum-core/src/api/lights.rs crates/athenaeum-core/src/api/masters.rs crates/athenaeum-core/src/stacking/plan.rs crates/athenaeum-core/src/collab/gate.rs crates/athenaeum-core/src/ts_export.rs src/types/models.ts src/components/export/modeBlocker.test.ts
# plus every fixture file Step 4 aligned, by name
git commit -m "fix(stacking): the readiness gate names calibration links that do not fit the frame size - calibrated export, the stacking plan (links) and the collab verdict refuse up front; another sub-frame origin is a warning (G-7); a misfit flat pre-calibration is refused at selection (R2, R7, R8)

<one line per aligned fixture: test name - lights WxH, masters were WxH>

Co-Authored-By: Claude <your model name> <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01NDfRyApE6tYi9ZMD7b4g8m"
```

---

### Task 7: Frontend

**Files:**
- Modify: `src/components/calibration/pickerModel.ts`, `src/components/calibration/CandidateCard.tsx`, `src/components/calibration/CalibrationTableView.tsx`
- Modify: `src/components/export/modeBlocker.ts`, `src/components/export/ExportTab.tsx`, `src/components/stacking/panels/CalibratePanel.tsx`, `src/components/CalibrationHierarchyView.tsx`
- Test: `src/components/calibration/CandidateCard.test.tsx` (new), `src/components/calibration/pickerModel.test.ts` (new), `src/components/export/modeBlocker.test.ts`

**Interfaces:**
- Consumes: verdict name `"geometry"` — status `mismatch` for size / mixed, `warning` for another origin (Task 2); `CalibrationSetDetail.xorgsubf/yorgsubf`, `LightFrameWithCalibration.naxis1/naxis2`, a set's `warnings[].warning_type === 'geometry'` (Task 5); `ExportReadiness.geometryMismatches` / `.geometryWarnings`, `GeometryMismatch` and the two Rust sentences (Task 6).
- Produces: `geometryLabel(set)`, `geometryRefuses(parameters)`, `geometryWarns(parameters)`, `sizeCellState(row)` in `pickerModel.ts`; `geometryMismatchSentence(m)`, `geometryOriginSentence(m)` in `modeBlocker.ts`.
- G-7 in the UI: a size misfit card is locked (except the linked one); an origin misfit card is selectable and carries a warning badge — it is listed where every refused set is (Show all, or Linked now; the visibility rule is unchanged); the hierarchy's Size cell warns on it; the Export tab lists the origin sentence among its calibration warnings; the Calibrate panel counts both lists.

- [ ] **Step 1: Write the failing tests**

`src/components/calibration/pickerModel.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import type { ParameterVerdict } from '../../types/models';
import { describeDifference, geometryLabel, geometryRefuses, geometryWarns, sizeCellState } from './pickerModel';

const verdict = (status: ParameterVerdict['status']): ParameterVerdict => ({
  name: 'geometry', enforced: true, status,
  frameValue: '9576×6388', setValue: '6384×4258 @ 1584,1064',
  diff: null, warningThreshold: null, matchingThreshold: null,
});

describe('geometry in the picker', () => {
  it('spells the size like the core, origin when known', () => {
    expect(geometryLabel({ naxis1: 9576, naxis2: 6388, xorgsubf: null, yorgsubf: null })).toBe('9576×6388');
    expect(geometryLabel({ naxis1: 6384, naxis2: 4258, xorgsubf: 1584, yorgsubf: 1064 })).toBe('6384×4258 @ 1584,1064');
    expect(geometryLabel({ naxis1: null, naxis2: null, xorgsubf: null, yorgsubf: null })).toBeNull();
  });
  it('a geometry mismatch refuses; an unknown size does not', () => {
    expect(geometryRefuses([verdict('mismatch')])).toBe(true);
    expect(geometryRefuses([verdict('unknown')])).toBe(false);
    expect(geometryRefuses([{ ...verdict('mismatch'), name: 'gain' }])).toBe(false);
  });
  it('reads the difference as needed → offered', () => {
    expect(describeDifference(verdict('mismatch'))).toEqual({
      label: 'Frame size', change: '9576×6388 → 6384×4258 @ 1584,1064', limit: null,
    });
  });
  it('another sub-frame origin warns and never refuses (G-7)', () => {
    expect(geometryRefuses([verdict('warning')])).toBe(false);
    expect(geometryWarns([verdict('warning')])).toBe(true);
    expect(geometryWarns([verdict('mismatch')])).toBe(false);
    expect(describeDifference(verdict('warning')).label).toBe('Sub-frame origin');
  });
  it('the hierarchy size cell: match, size misfit, origin warning, unknown', () => {
    const row = { size: '9576×6388', sizeKey: '9576×6388', cameraSize: '9576×6388', originWarning: null };
    expect(sizeCellState(row)).toEqual({ matches: true, title: 'Frame size' });
    expect(sizeCellState({ ...row, cameraSize: '6384×4258' })).toEqual({ matches: false, title: 'Frame size' });
    expect(sizeCellState({ ...row, originWarning: 'Dark set #5 is … — another sub-frame origin' }))
      .toEqual({ matches: false, title: 'Dark set #5 is … — another sub-frame origin' });
    expect(sizeCellState({ ...row, cameraSize: null })).toEqual({ matches: null, title: 'Frame size' });
  });
});
```

`src/components/calibration/CandidateCard.test.tsx`:

```tsx
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { CandidateCard } from './CandidateCard';
import type { CalibrationSetWithScore } from '../../types/models';

afterEach(cleanup);

const candidate = (status: 'match' | 'mismatch' | 'warning'): CalibrationSetWithScore => ({
  set: {
    id: 1843, imagetyp: 'MasterDark', exptime: 120, ccd_temp: -10, temp_min: -10, temp_max: -10,
    gain: 100, offset: 50, binning: '1x1', instrume: 'ASI6200MM', filter: null,
    date_start: '2023-07-28T17:24:40Z', date_end: '2023-07-28T18:24:40Z', date_display: '2023-07-28',
    frame_count: 1, is_master: true, naxis1: 6384, naxis2: 4258, xorgsubf: 1584, yorgsubf: 1064,
    bayerpat: null, swcreate: null, xpixsz: null, format: 'FITS', focallen: null,
    uuid: null, updated_at: null, superseded_by_set_id: null,
  },
  match_score: 0.9,
  compatible: status === 'match',
  parameters: [{
    name: 'geometry', enforced: true, status,
    frameValue: '9576×6388', setValue: '6384×4258 @ 1584,1064',
    diff: null, warningThreshold: null, matchingThreshold: null,
  }],
  match_details: {
    instrume_match: true, binning_match: true, gain_match: true, offset_match: true,
    exptime_match: true, filter_match: true, temp_diff: 0, date_diff_days: 0,
  },
});

describe('CandidateCard geometry', () => {
  it('shows the size and origin among the set facts', () => {
    render(<CandidateCard candidate={candidate('match')} selected={false} isCurrent={false} onSelect={vi.fn()} />);
    expect(screen.getByRole('button').textContent).toContain('ASI6200MM · 120 s · -10.0 °C · 1x1 · 6384×4258 @ 1584,1064');
  });
  it('a refused set that is not selected cannot be chosen', () => {
    const onSelect = vi.fn();
    render(<CandidateCard candidate={candidate('mismatch')} selected={false} isCurrent={false} onSelect={onSelect} />);
    const button = screen.getByRole('button');
    expect(button).toBeDisabled();
    fireEvent.click(button);
    expect(onSelect).not.toHaveBeenCalled();
  });
  it('the linked-now refused set stays deselectable', () => {
    const onSelect = vi.fn();
    render(<CandidateCard candidate={candidate('mismatch')} selected={true} isCurrent={true} onSelect={onSelect} />);
    const button = screen.getByRole('button');
    expect(button).not.toBeDisabled();
    fireEvent.click(button);
    expect(onSelect).toHaveBeenCalledTimes(1);
  });
  it('another sub-frame origin is selectable, with a warning badge (G-7)', () => {
    const onSelect = vi.fn();
    render(<CandidateCard candidate={candidate('warning')} selected={false} isCurrent={false} onSelect={onSelect} />);
    const button = screen.getByRole('button');
    expect(button).not.toBeDisabled();
    expect(button.textContent).toContain('Another sub-frame origin');
    expect(button.textContent).toContain('⚠Sub-frame origin9576×6388 → 6384×4258 @ 1584,1064');
    fireEvent.click(button);
    expect(onSelect).toHaveBeenCalledTimes(1);
  });
});
```
(Adjust the facts string only if `identityFacts`' existing separators differ — read it; the size must come last.)

`modeBlocker.test.ts`, append (fold the two new imports into the file's existing import lines):

```ts
import { geometryMismatchSentence, geometryOriginSentence } from './modeBlocker';
import type { GeometryMismatch } from '../../types/models';

const mm = (calibrationType: string, calibrationSetId: number, calibrationGeometry: string,
  sourceSetId: number | null, sourceGeometry: string, lightCount: number): GeometryMismatch =>
  ({ calibrationType, calibrationSetId, calibrationGeometry, sourceSetId, sourceGeometry, lightCount });

describe('geometry blocker', () => {
  it('mirrors the core sentence verbatim', () => {
    // Same vector as core `calibration::geometry::tests::mismatch_sentence_vectors`.
    expect(geometryMismatchSentence([
      mm('Dark', 1843, '6384×4258', null, '9576×6388', 151),
      mm('Dark', 1267, '6384×4258 @ 1584,1064', 558, '9576×6388', 0),
    ])).toBe('Calibration does not fit the frame size — dark set #1843 is 6384×4258, 151 lights are 9576×6388; flat set #558 is 9576×6388, its dark #1267 is 6384×4258 @ 1584,1064; re-link with Find Calibration');
    expect(geometryMismatchSentence([
      mm('Flat', 1, 'mixed sizes', null, '9576×6388', 1),
      mm('Bias', 2, '6384×4258', null, '9576×6388', 2),
      mm('DarkFlat', 3, '6384×4258', 9, '9576×6388', 0),
      mm('Dark', 4, '6384×4258', null, '9576×6388', 3),
    ])).toBe('Calibration does not fit the frame size — flat set #1 is mixed sizes, 1 light is 9576×6388; bias set #2 is 6384×4258, 2 lights are 9576×6388; flat set #9 is 9576×6388, its dark flat #3 is 6384×4258; +1 more; re-link with Find Calibration');
  });
  it('blocks only the calibrated mode, after unlinked lights and before missing files', () => {
    const geo = [mm('Dark', 1843, '6384×4258', null, '9576×6388', 1)];
    expect(modeBlocker({ ...ready, geometryMismatches: geo }, 'calibratedLights')).toContain('does not fit the frame size');
    expect(modeBlocker({ ...ready, geometryMismatches: geo, unlinkedLights: 1 }, 'calibratedLights')).toBe('1 light has no calibration links');
    expect(modeBlocker({ ...ready, geometryMismatches: geo, missingMasterFiles: 1 }, 'calibratedLights')).toContain('does not fit the frame size');
    expect(modeBlocker({ ...ready, geometryMismatches: geo }, 'rawWithMasters')).toBeNull();
    expect(modeBlocker({ ...ready, geometryMismatches: geo, attested: true }, 'calibratedLights')).toBeNull();
  });
  it('another sub-frame origin is a warning sentence, never a blocker (G-7)', () => {
    // Same vector as core `calibration::geometry::tests::origin_sentence_vectors`.
    expect(geometryOriginSentence([
      mm('Dark', 1518, '6384×4258 @ 0,0', null, '6384×4258 @ 1584,1064', 151),
      mm('Bias', 7, '6384×4258 @ 0,0', 558, '6384×4258 @ 1584,1064', 0),
    ])).toBe('Calibration at another sub-frame origin — dark set #1518 is 6384×4258 @ 0,0, 151 lights are 6384×4258 @ 1584,1064; flat set #558 is 6384×4258 @ 1584,1064, its bias #7 is 6384×4258 @ 0,0');
    const warn = [mm('Dark', 1518, '6384×4258 @ 0,0', null, '6384×4258 @ 1584,1064', 1)];
    expect(modeBlocker({ ...ready, geometryWarnings: warn }, 'calibratedLights')).toBeNull();
  });
});
```

- [ ] **Step 2: Run them to verify they fail**

Run: `npx vitest run src/components/calibration src/components/export`
Expected: FAIL — the helpers do not exist; the card is not disabled; no origin badge.

- [ ] **Step 3: Implement**

`pickerModel.ts`: add `geometry: 'Frame size',` to `PARAM_LABEL`; in `describeDifference`, right after `const label = …`, a geometry verdict with status `'warning'` reads as another origin (G-7):

```ts
  if (p.name === 'geometry' && p.status === 'warning') {
    return { label: 'Sub-frame origin', change: `${p.frameValue ?? '—'} → ${p.setValue ?? '—'}`, limit: null };
  }
```
Import `CalibrationSetDetail` in the type import; add:

```ts
/** `9576×6388`, or `6384×4258 @ 1584,1064` when the set records its
 *  sub-frame origin — the core's `Geometry::label` spelling. Null when the
 *  set records no size. */
export function geometryLabel(
  set: Pick<CalibrationSetDetail, 'naxis1' | 'naxis2' | 'xorgsubf' | 'yorgsubf'>,
): string | null {
  if (set.naxis1 == null || set.naxis2 == null) return null;
  const size = `${set.naxis1}×${set.naxis2}`;
  return set.xorgsubf != null && set.yorgsubf != null
    ? `${size} @ ${set.xorgsubf},${set.yorgsubf}`
    : size;
}

/** The frame-size rule is not the user's to relax (owner ruling 2026-10-20):
 *  a set of another size (or of mixed sizes) can never be chosen. */
export function geometryRefuses(parameters: ParameterVerdict[]): boolean {
  return parameters.some(p => p.name === 'geometry' && p.status === 'mismatch');
}

/** The same size at another recorded sub-frame origin: auto-link never takes
 *  it, a person may — with a warning (owner, G-7: "auto no, manual yes"). */
export function geometryWarns(parameters: ParameterVerdict[]): boolean {
  return parameters.some(p => p.name === 'geometry' && p.status === 'warning');
}

/** The calibration hierarchy's Size cell: green when the set's size is the
 *  lights', warning on another size or (G-7) another recorded origin, muted
 *  when either size is unknown. `title` is what the cell says on hover. */
export function sizeCellState(row: {
  size: string | null;
  sizeKey: string | null;
  cameraSize: string | null;
  originWarning: string | null;
}): { matches: boolean | null; title: string } {
  if (row.originWarning != null) return { matches: false, title: row.originWarning };
  const matches = row.cameraSize != null && row.sizeKey != null ? row.sizeKey === row.cameraSize : null;
  return { matches, title: 'Frame size' };
}
```

`CandidateCard.tsx`: import `geometryLabel, geometryRefuses, geometryWarns`; in `identityFacts` push `geometryLabel(set)` last when non-null; in the component:

```tsx
  // A set of another frame size can never calibrate these frames (owner
  // ruling 2026-10-20) — but the one linked now stays clickable, so the user
  // can deselect it and clear the stale choice.
  const locked = geometryRefuses(parameters) && !selected;
  // The same size at another recorded sub-frame origin: never auto-linked,
  // allowed by hand with a warning (G-7) — selectable, badged.
  const otherOrigin = geometryWarns(parameters);
  const originVerdict = parameters.find(p => p.name === 'geometry' && p.status === 'warning');
```
and on the `<button>`: `onClick={locked ? undefined : onSelect}`, `disabled={locked}`, `title={locked ? 'Another frame size — this set cannot calibrate these frames' : undefined}`, and append `${locked ? ' opacity-60 cursor-not-allowed' : ''}` to its `className`. In the header row, before the `Linked now` badge, render the warning badge when `otherOrigin`:

```tsx
          {otherOrigin && (
            <span
              className="rounded bg-warning-muted px-1.5 py-0.5 text-xs font-medium text-warning"
              title="Same frame size, another sub-frame origin — never linked automatically; allowed by hand"
            >
              Another sub-frame origin
            </span>
          )}
```
and let the differences line show it: `const differences: ParameterVerdict[] = [...blockersOf(parameters), ...(originVerdict ? [originVerdict] : [])];`, and in the map render a warning-status entry with the glyph `⚠` (`p.status === 'warning' ? '⚠' : unknown ? '?' : '✕'`), `text-warning` like a mismatch. `describeDifference` labels it "Sub-frame origin".

`CalibrationTableView.tsx`:
1. Import `geometryLabel, sizeCellState` from `./pickerModel`.
2. Add to `FlatRow`, `DarkRow`, `BiasRow`: `size: string | null; sizeKey: string | null; cameraSize: string | null; originWarning: string | null;`.
3. `linkedCameraParams` also returns `cameraSize: f.naxis1 != null && f.naxis2 != null ? \`${f.naxis1}×${f.naxis2}\` : null` (and `cameraSize: null` in the fallback); widen its return type.
4. In each of the three row pushes add:
   ```ts
      size: geometryLabel(s),
      sizeKey: s.naxis1 != null && s.naxis2 != null ? `${s.naxis1}×${s.naxis2}` : null,
      // G-7: the core flags a link at another recorded sub-frame origin (Task 5).
      originWarning: set.warnings.find(w => w.warning_type === 'geometry')?.message ?? null,
   ```
   (`set` is the loop's `CalibrationSetWithFrameCount`, `s = set.set`.)
5. A shared cell next to `MatchCells`:
   ```tsx
   /** The set's frame size, badged against the lights that use it; a
    *  warning (with the core's sentence on hover) when it is linked at
    *  another recorded sub-frame origin (G-7). */
   function SizeCell({ row }: { row: { size: string | null; sizeKey: string | null; cameraSize: string | null; originWarning: string | null } }) {
     const { matches, title } = sizeCellState(row);
     return (
       <td className="px-1.5 py-1 text-sm whitespace-nowrap">
         <MatchBadge label={title} value={row.size} matches={matches} />
         {row.originWarning && <span className="ml-1 text-warning" title={row.originWarning}>⚠</span>}
       </td>
     );
   }
   ```
6. In the flat, dark and bias tables: a header cell `<th className="px-1.5 py-1.5 text-left text-xs font-semibold text-content-secondary whitespace-nowrap sticky top-0 z-10 bg-surface-elevated">Size</th>` right after the `<SortTh field="camera" label="Camera" … />`, and `<SizeCell row={row} />` right after each body's camera cell (`{row.camera ?? '—'}`). Not hidden in compact mode — the rule is not a detail.

`modeBlocker.ts`: import `GeometryMismatch`; add, above `modeBlocker`:

```ts
const TYPE_WORD: Record<string, string> = { Dark: 'dark', Flat: 'flat', Bias: 'bias', DarkFlat: 'dark flat' };
const typeWord = (t: string): string => TYPE_WORD[t] ?? 'calibration';

/** Mirrors core `calibration::geometry::mismatch_parts`. */
function geometryMismatchParts(m: GeometryMismatch[]): string {
  const parts = m.slice(0, 3).map(x =>
    x.sourceSetId == null
      ? `${typeWord(x.calibrationType)} set #${x.calibrationSetId} is ${x.calibrationGeometry}, ${x.lightCount} light${x.lightCount === 1 ? '' : 's'} ${x.lightCount === 1 ? 'is' : 'are'} ${x.sourceGeometry}`
      : `flat set #${x.sourceSetId} is ${x.sourceGeometry}, its ${typeWord(x.calibrationType)} #${x.calibrationSetId} is ${x.calibrationGeometry}`,
  );
  const more = m.length > 3 ? `; +${m.length - 3} more` : '';
  return `${parts.join('; ')}${more}`;
}

/** Mirrors core `calibration::geometry::mismatch_sentence` verbatim. */
export function geometryMismatchSentence(m: GeometryMismatch[]): string {
  return `Calibration does not fit the frame size — ${geometryMismatchParts(m)}; re-link with Find Calibration`;
}

/** Mirrors core `calibration::geometry::origin_sentence` verbatim (G-7: an
 *  advisory line, never a blocker). */
export function geometryOriginSentence(m: GeometryMismatch[]): string {
  return `Calibration at another sub-frame origin — ${geometryMismatchParts(m)}`;
}
```
and in the `calibratedLights` branch, after the `unlinkedLights` return: `if (r.geometryMismatches.length > 0) return geometryMismatchSentence(r.geometryMismatches);`. Update the doc comment's order sentence: "masters-missing → unlinked-lights → geometry → missing master FILES".

`ExportTab.tsx` (calibrated mode, line ~564): the origin sentence joins the calibration warnings — import `geometryOriginSentence` from `./modeBlocker` and pass

```tsx
                <CalibrationWarnings
                  warnings={[
                    ...(readiness?.calibrationWarnings ?? []),
                    ...(readiness && readiness.geometryWarnings.length > 0
                      ? [geometryOriginSentence(readiness.geometryWarnings)]
                      : []),
                  ]}
                />
```

`CalibratePanel.tsx`, after the missing-master-files `<li>`:

```tsx
            {readiness.geometryMismatches.length > 0 && (
              <li className="text-warning">
                <span className="tabular-nums">{readiness.geometryMismatches.length}</span>{' '}
                calibration link(s) do not fit the frame size
              </li>
            )}
            {readiness.geometryWarnings.length > 0 && (
              <li className="text-warning">
                <span className="tabular-nums">{readiness.geometryWarnings.length}</span>{' '}
                calibration link(s) at another sub-frame origin (chosen by hand)
              </li>
            )}
```
(The stacking plan's own warning list already shows the origin sentence — Task 6 puts it in `plan.warnings`, which `StackingTab` renders.)

`CalibrationHierarchyView.tsx` — the manual-apply `catch` only logs today, so a refused assignment would vanish (G-4); make it notify, with the `generic` kind the same file already uses for its other apply notice (review M-1; `notify` is already in the hook's dependency list):

```tsx
      } catch (error) {
        console.error('Failed to apply manual calibration:', error);
        notify({
          title: 'Could not apply the calibration',
          detail: String(error),
          kind: 'generic',
          tone: 'warning',
          hasErrors: true,
        });
      }
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
npx vitest run src/components/calibration src/components/export src/components/stacking
npx tsc --noEmit
git diff --exit-code src/types
```
Expected: PASS, no type drift.

- [ ] **Step 5: Commit**

```bash
git add src/components/calibration/ src/components/export/ src/components/stacking/panels/CalibratePanel.tsx src/components/CalibrationHierarchyView.tsx
git commit -m "feat(calibration): the picker and the calibration tables show each set's frame size; a set of another size cannot be chosen (the linked one can still be deselected), another sub-frame origin is selectable with a warning (G-7); the Export tab and the Calibrate panel read the geometry blocker and warnings

Co-Authored-By: Claude <your model name> <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01NDfRyApE6tYi9ZMD7b4g8m"
```

---

### Task 8: Docs + the full gate

**Env (every cargo call in this task):** `export CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/.superpowers/target-geom CARGO_BUILD_JOBS=4`, run from the worktree root, tests always `-- --test-threads=4`, one compile at a time (Global Constraints).

**Files:**
- Modify: `docs/catalog/README.md`, `docs/export/calibrated-lights.md`, `docs/stacking/README.md`, `docs/masters/README.md`, `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`, `CLAUDE.md`, `docs/superpowers/open-items.md`

- [ ] **Step 1: Write the docs**

`docs/catalog/README.md` — new section after "Settings, Coordinates, FITS":

```markdown
## Calibration geometry (2026-10-20)

**A calibration set of another frame size never calibrates a frame** (owner ruling 2026-10-20). Size is `frames.naxis1 × frames.naxis2` — the stored array, so after binning (binning stays its own matcher parameter); a set's size is its members' agreed size, and a set whose members disagree ("mixed sizes") fits nothing. It is not a `MatchMode`: no setting relaxes it, and no manual choice either. The app never crops: calibration happens only when sizes match. A side that records no size blocks nothing (one `warn!` per call). The rule lives in `calibration::geometry` (`fit`, `geometry_for_frame`, `geometry_for_set`, `geometry_for_frames`, `LazyFrameGeometry`).

**The sub-frame origin** `XORGSUBF`/`YORGSUBF` (where a same-size crop sits on the sensor) is read from the stored header blob — a set's: its lowest-frame-id member's — and compared only when both sides record it. Owner ruling G-7 ("auto no, manual yes"): auto-link never links a set at another recorded origin, and Find Calibration removes such an automatic link; a person may choose it by hand — the picker shows it selectable with a warning badge, the assignment is accepted with a `warn!`, and the hierarchy, the export readiness (`ExportReadiness.geometry_warnings`) and the stacking plan warn about it, never block. In-app masters stamp the origin their members agree on (G-5, `calibration_library::headers`); a master built before 2026-10-20, or from members that disagree or record none, records none and is not compared on origin until rebuilt.

Where the size rule holds: the candidate engine (`configurable_matcher::find_calibration_candidates` — auto-link and the manual list; verdict `geometry` on every candidate, `mismatch` for size, `warning` for origin); raw-frame grouping (`detect_flat_groups` / `detect_dark_groups` / `detect_bias_groups` take the source's size); the flat pattern path and the on-demand dark/bias path (each group is judged before any set row is written, so a refused group leaves nothing behind); manual assignment (`manual_assign_calibration` / `manual_assign_subcalibration` refuse before writing); the flat pre-calibration chain (`api::masters::select_flat_precal` refuses a linked master of another size); the readiness gate (`ExportReadiness.geometry_mismatches` — a calibrated-lights blocker, the stacking plan's `links` blocker, the collab verdict's `linkCalibration`). Find Calibration removes an AUTOMATIC link that no longer fits (`prune_unfit_auto_links`, warn-logged); a manual link is the user's and stays until the picker clears it — the gates name it. Set formation does not key on size: a mixed set is refused, never split. Plan: `docs/superpowers/plans/2026-10-20-calibration-geometry-plan.md` (rulings R1–R13, G-1–G-8).
```

`docs/export/calibrated-lights.md`, the **Gate** paragraph: "three ordered blockers" → "four ordered blockers", and insert after (2) unlinked lights: "(3) `geometry_mismatches` — a linked set whose frame size differs from the lights' (or whose members are of mixed sizes), or a flat set's pre-calibration link of another size: "Calibration does not fit the frame size — dark set #N is W×H, K lights are W×H; …; re-link with Find Calibration" (2026-10-20, `docs/catalog/README.md` → Calibration geometry)"; renumber missing files to (4). Add `geometryMismatches` and `geometryWarnings` to the listed `ExportReadiness` fields, with one sentence: "`geometryWarnings` — a link at the same size but another recorded sub-frame origin (a manual choice, G-7) — is advisory: the tab lists "Calibration at another sub-frame origin — …" among the calibration warnings, the gate never blocks on it."

`docs/stacking/README.md`, in the plan-gate paragraph after the blocker-code list: "`links` also carries the calibration-geometry blocker (a linked set of another frame size, `ExportReadiness.geometry_mismatches`, 2026-10-20) — so a run with a cropped dark is refused up front instead of dying in Masters or Calibrate. A link at the same size but another recorded sub-frame origin (a manual choice, G-7) is a plan WARNING (`geometry_warnings`), never a blocker."

`docs/masters/README.md`, one paragraph after **Relink/supersede**: "**Pre-calibration geometry** (2026-10-20): `select_flat_precal` refuses a linked DarkFlat/Dark/Bias master of another frame size than the flats (`ApiError::Invalid` naming both), instead of skipping it; the stacking plan names the same link up front. A master of the same size at another recorded sub-frame origin (a manual choice, G-7) is used as linked, with a warning in the build's warnings. **Sub-frame origin** (G-5): a built master stamps `XORGSUBF`/`YORGSUBF` when every member records the same origin (`headers::load_header_inputs` → `MasterHeaderInputs.origin` → `build_master_cards`); members that disagree, or some that record none, leave it unstamped with a `warn!`; none recording one is a `debug!`. Masters built before 2026-10-20 carry no origin until rebuilt. NAXIS comes from the written file, as before."

Logging spec, a dictionary note at the end of the "Dictionary" notes:

```markdown
**Dictionary extension (calibration geometry, `calibration::geometry` + `calibration::{configurable_matcher, hierarchy}` + `calibration_library::headers` + `db::calibration_links` + `api::{calibration, lights, masters}`, 2026-10-20):** `source_geometry` / `set_geometry` (string; `Geometry::label` — `9576×6388`, `6384×4258 @ 1584,1064`, `mixed sizes`, `size not recorded` — of the frame or flat set being calibrated, and of the calibration set or of the raw group a set would be made of) on `"automatic calibration link removed: the set does not fit the frame geometry"` (warn), `"flat group does not fit the light's frame geometry; trying the next"` / `"reused flat set does not fit the light's frame geometry; trying the next"` (warn), `"dark group does not fit the frame geometry; trying the next"` / `"bias group …"` (warn), `"manual calibration refused: the set does not fit the frame size"` / `"manual sub-calibration refused: …"` (warn), `"manual calibration accepted at another sub-frame origin"` / `"manual sub-calibration accepted at another sub-frame origin"` (warn, G-7), `"flat pre-calibration refused: the master does not fit the frame size"` (warn) and `"flat pre-calibration at another sub-frame origin; used as linked"` (warn, G-7); `geometry_mismatches` / `geometry_warnings` (usize; on the `"export readiness computed"` debug — size blockers and advisory origin entries). Registered here because existing events already use them (review M-4): `calibration_type` (string; the link type `Dark` / `Flat` / `Bias` / `DarkFlat` — the link writers'), `source_set_id` (i64; the calibration set being calibrated on a set → set link event, e.g. a flat's pre-calibration — `set_id` stays the calibration set it is linked to), `master_set_id` (i64; a master calibration set's id on an event that also names another set — the readiness rebuild pass, `select_flat_precal`), `age_days` (i64; a flat group's age on the `"pattern selected flat match"` debug). `"calibration geometry not recorded on one side; size not compared"` (warn) carries the canonical `count` (and `frame_id` from the candidate engine); `"no dark group fits the frame geometry; none created on demand"` (warn) carries `frame_id` and `count`; `"master origin not stamped: members disagree or some record none"` (warn, G-5) and its `debug!` twin carry `set_id` and `count`; `"sub-frame origin check failed; the hierarchy shows no origin warning for this set"` (warn) carries `set_id` and `error`. The grouping debugs (`"executing flat search query"`, `"finding flats for light frame"`) carry the source size as the registered `width` / `height`.
```

`CLAUDE.md`, section "Calibration Matching", after the **Per-pair parameters** paragraph:

```markdown
**Geometry is not a parameter** (2026-10-20): a set of another frame size (NAXIS1×NAXIS2, after binning) never calibrates — auto-link, the manual list, manual assignment, the readiness and stacking-plan gates all ask `calibration::geometry::fit`. Another sub-frame origin at the same size (`XORGSUBF`/`YORGSUBF` from the stored header, compared only when both record it): auto-link never, by hand with a warning (owner, G-7). In-app masters stamp their members' agreed origin (G-5). Reference: `docs/catalog/README.md` → Calibration geometry.
```

`docs/superpowers/open-items.md`, at the end of the geometry entry (keep the entry):

```markdown
**Status (2026-10-20).** Fixed on branch `calib-geometry` (plan `docs/superpowers/plans/2026-10-20-calibration-geometry-plan.md`). This entry stays until the owner-catalog acceptance (plan Task 9) passes on frame set 182.
```

- [ ] **Step 2: The full gate**

```bash
cargo test -p athenaeum-core --all-targets -- --test-threads=4
cargo check -p athenaeum-core --no-default-features
cargo check -p athenaeum-tauri -p athenaeum-web
npm run test
npx tsc --noEmit
git diff --exit-code src/types
grep -rn "println!\|eprintln!" crates/athenaeum-core/src/calibration/geometry.rs   # nothing
```
Expected: all green. A failure in the collab live-exchange wall-clock or landing free-bytes tests under parallel load is the known flake (memory note): re-run that test alone; anything else is stop-and-report.

- [ ] **Step 3: Commit**

```bash
git add docs/ CLAUDE.md
git commit -m "docs: calibration geometry - the rule, where it holds, the gates, the logging fields; the open-items entry stays until the owner-catalog acceptance

Co-Authored-By: Claude <your model name> <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01NDfRyApE6tYi9ZMD7b4g8m"
```

---

### Task 9: Real-data acceptance on a `.dev` copy of the owner's catalog (controller + owner; steps only)

The dev catalog of 2026-10-05 does NOT match the open-items description (there, set 182's lights are OFFSET 1 and every ASI6200MM dark OFFSET 50; the full-frame darks are gain 50, the ROI ones gain 100), so the acceptance needs a fresh copy of the owner's catalog. Ruling G-1: **the controller makes it, the allowed way, after the owner's word** — one `immutable=1` read of the production file into a `.backup`; the production file is never opened writable and nothing else ever reads it. The copy carries production folder roots, so every one is repointed to scratch BEFORE the app starts (a dev launch mounts the transfer and collaboration roots, and stage 0.5 writes into the calibration library).

- [ ] **Step 1: The copy (controller, after the owner's word).** The owner quits BOTH app builds first. Nothing but the one `.backup` below ever touches `$PROD` — no `prepare-catalog.sh`, no query, no checkpoint on the source itself; every later step works on the copy. Then:
```bash
PROD="$HOME/Library/Application Support/com.vsharifov.athenaeum/athenaeum.db"
DEV="$HOME/Library/Application Support/com.vsharifov.athenaeum.dev"
ps -axo pid=,comm= | grep -i athenaeum      # expect nothing: no app build running (anything listed → ask the owner)
ls -l "$PROD-wal" 2>/dev/null               # immutable=1 does not read the WAL: a non-empty -wal → STOP and ask the
                                            # owner to open and quit the production app once (it checkpoints) — never
                                            # checkpoint it ourselves
cp "$DEV/athenaeum.db" "$HOME/Desktop/athenaeum.dev.backup-2026-10-20.db"
rm -f "$DEV/athenaeum.db-wal" "$DEV/athenaeum.db-shm"
sqlite3 "file:$PROD?immutable=1" ".backup '$DEV/athenaeum.db'"
sqlite3 "file:$DEV/athenaeum.db?immutable=1" "PRAGMA integrity_check;"   # ok
```
- [ ] **Step 2: Repoint every folder root to scratch — on the COPY, before the app starts** (G-1, mandatory). Pick a scratch folder with ≥ 120 GB free (164 frames × 9576×6388 float32 ≈ 40 GB per per-frame stage) and name it in the acceptance note. First record what the copy carries:
```bash
DB="$DEV/athenaeum.db"
SCRATCH="<scratch folder>"
mkdir -p "$SCRATCH"/{stacking-work,stacking-out,library,transfers-packages,transfers-working,incoming,collaboration}
sqlite3 "file:$DB?immutable=1" "SELECT key, value FROM settings WHERE key IN ('stacking.working_dir','stacking.output_dir','sync.outgoing_staging_dir','sync.incoming_working_dir');"
sqlite3 "file:$DB?immutable=1" "SELECT id, kind, path, enabled FROM scan_roots WHERE kind <> 'normal';"
sqlite3 "file:$DB?immutable=1" "SELECT frames_set_id, json_extract(config_json, '\$.paths') FROM stacking_set_config WHERE frames_set_id = 182;"
```
   Folder settings are stored as the raw path text (`db::set_setting`; read back trimmed by `api::sync::configured_dir` and `stacking::paths::resolve_dirs`) — if a value printed above starts with a double quote, write the new one the same way. Then write the COPY (never the production file):
```bash
sqlite3 "$DB" <<SQL
INSERT INTO settings (key, value, updated_at) VALUES
  ('stacking.working_dir', '$SCRATCH/stacking-work', datetime('now')),
  ('stacking.output_dir', '$SCRATCH/stacking-out', datetime('now')),
  ('sync.outgoing_staging_dir', '$SCRATCH/transfers-packages', datetime('now')),
  ('sync.incoming_working_dir', '$SCRATCH/transfers-working', datetime('now'))
ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at;
UPDATE scan_roots SET path = '$SCRATCH/library' WHERE kind = 'calibration_library';
UPDATE scan_roots SET path = '$SCRATCH/incoming' WHERE kind = 'sync_incoming';
UPDATE scan_roots SET path = '$SCRATCH/collaboration' WHERE kind = 'collaboration';
-- a per-set stacking paths override wins over the globals (spec §9.6): repoint it too
UPDATE stacking_set_config
   SET config_json = json_set(config_json, '\$.paths.workingDir', '$SCRATCH/stacking-work',
                                           '\$.paths.outputDir', '$SCRATCH/stacking-out')
 WHERE frames_set_id = 182 AND json_extract(config_json, '\$.paths') IS NOT NULL;
SQL
```
   Existing masters keep their production paths in `files.path` and are only READ by the run; the masters stage 0.5 builds land in `$SCRATCH/library`.
- [ ] **Step 3: Preflight, read-only, still before the app starts** (`sqlite3 "file:$DB?immutable=1"` — safe only while no app has the copy open). Record the output in the acceptance note:
```sql
-- set 182's lights: size and count
SELECT f.naxis1||'x'||f.naxis2, COUNT(*) FROM frames f
JOIN session_members sm ON sm.frame_id = f.id JOIN sessions s ON s.id = sm.session_id
JOIN imaging_nights n ON n.id = s.imaging_night_id
WHERE n.frames_set_id = 182 AND f.imagetyp = 'Light' GROUP BY 1;
-- every link of those lights and of their flat sets, with both sizes and the manual flag
WITH l182 AS (SELECT f.id, f.naxis1 w, f.naxis2 h FROM frames f
  JOIN session_members sm ON sm.frame_id = f.id JOIN sessions s ON s.id = sm.session_id
  JOIN imaging_nights n ON n.id = s.imaging_night_id
  WHERE n.frames_set_id = 182 AND f.imagetyp = 'Light'),
sg AS (SELECT csf.set_id, MIN(f.naxis1) w, MIN(f.naxis2) h, MAX(f.naxis1) w2, MAX(f.naxis2) h2
  FROM calibration_set_frames csf JOIN frames f ON f.id = csf.frame_id GROUP BY csf.set_id)
SELECT 'light', l.calibration_type, l.calibration_set_id, l.is_manual_override, sg.w||'x'||sg.h, l182.w||'x'||l182.h, COUNT(*)
FROM calibration_set_to_frames l JOIN l182 ON l182.id = l.source_id AND l.source_type = 'frame'
JOIN sg ON sg.set_id = l.calibration_set_id GROUP BY 2, 3, 4
UNION ALL
SELECT 'flat '||l.source_id, l.calibration_type, l.calibration_set_id, l.is_manual_override, sg.w||'x'||sg.h, src.w||'x'||src.h, 1
FROM calibration_set_to_frames l JOIN sg ON sg.set_id = l.calibration_set_id JOIN sg src ON src.set_id = l.source_id
WHERE l.source_type = 'calibration_set' AND l.source_id IN (
  SELECT DISTINCT x.calibration_set_id FROM calibration_set_to_frames x
  JOIN l182 ON l182.id = x.source_id AND x.source_type = 'frame' WHERE x.calibration_type = 'Flat');
-- the matching config the copy carries (absent = defaults)
SELECT value FROM settings WHERE key = 'calibration.matching_config';
```
   Expected per the open-items entry: 164 lights at 9576×6388; manual Dark links onto 1843/1844 (6384×4258); flat sets with Dark sub-links onto ROI darks. **STOP rule (G-1):** if the copy does not show that state (ROI masters linked for set 182 — or lights and darks do not tie on gain/offset under the stored config), stop and report; do not re-create the state by hand — the acceptance cannot show what it is meant to.
- [ ] **Step 4: Launch, check the folders, and the gate refuses up front (R2).** Launch the dev build from the worktree (`npm run tauri dev` — a debug build uses the `.dev` directory). Before ANY other action: Settings → Stacking, Settings → Transfers and Folders (calibration library, collaboration) must each show the `$SCRATCH` path written in Step 2 — any production path → quit, fix the copy, relaunch. Keep sync and collaboration idle throughout (no sign-in, no publish, no transfer). From here on, read the copy with `sqlite3 "file:$DB?mode=ro"` (it reads the WAL; `immutable=1` would miss the running app's newest writes). Open frame set 182 → Stacking. Expected: a `links` blocker "Calibration does not fit the frame size — dark set #1843 is 6384×4258, N lights are 9576×6388; …" (and the flat sub-links if they are still stale); Start is disabled. The Export tab's calibrated mode shows the same sentence.
- [ ] **Step 5: The picker (R3, R11, G-7).** Open the calibration picker on one light row of set 182 → Darks. Expected: the ROI master shows "Linked now" with "✕ Frame size 9576×6388 → 6384×4258" and stays clickable; with **Show all** on (refused sets are hidden without it — `candidate_is_visible`, review M-2), every other set of another size is greyed out and cannot be selected, and a set of the same size at another recorded sub-frame origin, if the catalog has one, is selectable with the "Another sub-frame origin" badge; the identity line of every card ends with its size. Click the linked ROI master to deselect it and apply (clears that row's manual dark). For every remaining row of set 182: "Reset to automatic".
- [ ] **Step 6: Fresh auto-match.** Run Find Calibration on set 182. Then, with `athenaeum-logs` `query_logs`, list the `"automatic calibration link removed: the set does not fit the frame geometry"` events (R9) and record them.
- [ ] **Step 7: Nothing of another size is linked.** Re-run Step 3's second query (now `mode=ro`). Expected: no row whose two sizes differ — lights' darks on the full-frame sets (1246 for 120 s, 1285 for 180 s, or their masters), the flats' pre-calibration on 1269 (1.0 s) / 1297 (0.5 s) / bias 1328 (or their masters), as the open-items workaround lists.
- [ ] **Step 8: The plan is clear and the run passes Masters and Calibrate.** Stacking tab: no `masters` / `links` / `masterFiles` blocker. Start a run with defaults. Expected: stage 0.5 (Masters) builds what it lists without `bad input: pre-calibration master is …`, into `$SCRATCH/library`; Calibrate completes every included light without `preloaded master geometry mismatch` (check `stacking-progress` in the UI and `query_logs` for errors in the run's operation). Then G-5 on real data — every master this run built carries the origin its members agree on:
```sql
SELECT mp.master_set_id, cs.imagetyp, MAX(fh.header LIKE '%XORGSUBF%') AS stamped
FROM master_provenance mp
JOIN calibration_set cs ON cs.id = mp.master_set_id
JOIN calibration_set_frames csf ON csf.set_id = mp.master_set_id
JOIN frames f ON f.id = csf.frame_id
JOIN fits_header fh ON fh.file_id = f.file_id
WHERE mp.created_at >= '2026-10-20'
GROUP BY 1, 2;
```
   For every `stamped = 0` row the run's log holds `"master origin not stamped: members disagree or some record none"` (warn) or its "no member records a sub-frame origin" debug; record which. Letting the run finish is optional; cancel after Calibrate if disk is short.
- [ ] **Step 9: Report, then close.** Record steps 3–8 (counts, ids, sizes, the run id and its stage times) in the acceptance note. Only on the owner's word that it passed: delete the open-items entry (a passing smoke is deleted there) in a docs commit. Restore the dev catalog afterwards if the owner wants it back: quit the app, `cp "$HOME/Desktop/athenaeum.dev.backup-2026-10-20.db" "$DEV/athenaeum.db"` (after removing `$DEV/athenaeum.db-wal` / `-shm`). Delete `$SCRATCH` once the note is written, unless the owner wants the built masters kept.
