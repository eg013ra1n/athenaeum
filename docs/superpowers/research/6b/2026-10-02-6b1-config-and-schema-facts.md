# 6b steps 0 and 2: config pin and table rebuild (research facts)

Repo state: `collab-wave6b` HEAD == local `main` == `17e2e96e`. So "recorded on main
before the change" means recorded on the current HEAD. All line numbers are from that commit.

---------------------------------------------------------------------------------------------

## STEP 0: the config pin

### 0.1 `GroupingConfig`: definition, fields, serde

`crates/athenaeum-core/src/stacking/config.rs:100-114`

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct GroupingConfig {
    /// Clamped to a floor of [`MIN_EXPOSURE_TOLERANCE_SEC`] by
    /// [`resolve_config`] — never trusted raw from a stored document.
    pub exposure_tolerance_sec: f64,
}
impl Default for GroupingConfig { fn default() -> Self { GroupingConfig { exposure_tolerance_sec: 2.0 } } }
```

- It has one field. The struct-level `default` attribute means `{}` decodes. No
  `deny_unknown_fields`, so a legacy `"splitByExposure"` is silently dropped
  (`legacy_split_by_exposure_field_is_ignored`, config.rs:1096).
- `resolve_config` (config.rs:408-471) clamps the tolerance to `MIN_EXPOSURE_TOLERANCE_SEC = 0.01`
  (config.rs:72, clamp at 431-438).
- It is TS-exported: `ts_export.rs:400` → `src/types/stacking.ts:195`
  (`export type GroupingConfig = { exposureToleranceSec: number, }`). New fields regenerate this.
- `StackingConfig` (config.rs:31-46) has `version, grouping, calibration, measurement, selection,
  reference, registration, normalization, integration, drizzle, output, paths`, and
  `STACKING_CONFIG_VERSION = 1` (config.rs:27).
- `OutputConfig` (config.rs:310-315) has `{ format: OutputFormat, cleanup: CleanupPolicy }`. It is
  `Copy + Eq + Default` with `#[serde(rename_all="camelCase", default)]`. If `coverageMap` and
  `cropToCoverage` are bools, `Copy`/`Eq`/`Default` still derive.
- The consumer of grouping is `stacking::groups::group_frames(conn, set_id, &cfg.grouping)`
  (groups.rs:400 and 481).

### 0.2 The hash machinery: verbatim

**Run fingerprint**, config.rs:589-597. It is not a stage hash.
```rust
pub fn config_hash(cfg: &StackingConfig) -> String {
    let value = serde_json::to_value(cfg).expect("StackingConfig always serializes");
    let payload = serde_json::json!({ "config": value, "drizzleKernelVersion": DRIZZLE_KERNEL_VERSION });
    let json = serde_json::to_string(&payload).expect(...);
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(json.as_bytes()))
}
```
`DRIZZLE_KERNEL_VERSION = 2` (config.rs:578).

**Stage hash**, config.rs:619-649.
```rust
#[derive(Serialize)]
pub struct SourceIdentity { pub file_id: i64, pub size: i64, pub modified_at: String }

pub fn stage_hash(config_subtree: &serde_json::Value, upstream: &[&str], sources: &[SourceIdentity]) -> String {
    let mut sorted_upstream: Vec<&str> = upstream.to_vec();
    sorted_upstream.sort_unstable();
    let mut sorted_sources: Vec<&SourceIdentity> = sources.iter().collect();
    sorted_sources.sort_by(|a, b| a.file_id.cmp(&b.file_id).then(a.size.cmp(&b.size))
        .then_with(|| a.modified_at.cmp(&b.modified_at)));
    let payload = serde_json::json!({ "config": config_subtree, "upstream": sorted_upstream, "sources": sorted_sources });
    let json = serde_json::to_string(&payload).expect("stage hash payload always serializes");
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(json.as_bytes()))
}
```
- The algorithm is XXH3-64 over the compact JSON of
  `{"config":<subtree>,"sources":[{file_id,size,modified_at}…],"upstream":[…]}`, printed as 16 hex
  digits.
- Keys come out sorted because the workspace `serde_json` (1.0.151) has no `preserve_order`. I
  checked `Cargo.lock`: serde_json has no `indexmap` dependency. A dependency that later turns
  `preserve_order` on would move every hash. The golden pin would catch that.
- The `SourceIdentity` field names are snake_case on purpose (no `rename_all`).

**The four subtrees.** These are the only ones. Their docs say "the four are
calibration/measurement/registration/normalization".

| fn | config.rs | serializes |
| ---- | ---- | ---- |
| `calibration_subtree` | 654-670 | `{"calibration": cfg.calibration minus "format", "grouping": cfg.grouping}` |
| `measurement_subtree` → `measurement_subtree_with_fit_version` | 678-694 | `{"measurement": cfg.measurement, "normalization": {"scaleEstimator": …}, "psfFitVersion": PSF_FIT_VERSION}` |
| `registration_subtree` → `_with_fit_version` | 708-722 | `{"registration": cfg.registration, "psfFitVersion": …}` |
| `normalization_subtree` → `_with_fit_version` | 741-759 | `{"normalization": cfg.normalization, "measurement": {"psfModel", "maxStars"}, "psfFitVersion": …}` |

`calibration_subtree` verbatim (config.rs:654-670):
```rust
pub fn calibration_subtree(cfg: &StackingConfig) -> serde_json::Value {
    // `calibration.format` ... must not move this hash ... Pinned by `format_moves_the_run_fingerprint_but_no_stage_hash`.
    let mut calibration = serde_json::to_value(&cfg.calibration)
        .expect("CalibratedLightOptions serializes");
    if let Some(obj) = calibration.as_object_mut() {
        obj.remove("format");
    }
    serde_json::json!({
        "calibration": calibration,
        "grouping": cfg.grouping,
    })
}
```

- `cfg.calibration` is `export::CalibratedLightOptions` (export/models.rs:318-390). Its keys are
  `flatNorm, flatNormMode, params, hotPixelCorrection, debayerOsc, format`. `keep_mosaic` and
  `skip_fsync` are `#[serde(skip)]` and never enter the hash.
- `selection`, `reference`, `integration`, `drizzle`, `output` and `paths` are in NO subtree.
  `output.*` only reaches `config_hash`.
- **The step-0 change is a one-liner.** Replace `"grouping": cfg.grouping` with
  `"grouping": { "exposureToleranceSec": cfg.grouping.exposure_tolerance_sec }`.
  - `json!` on an f64 and `to_value` of the derived struct both go through `Number::from_f64`, so
    `2.0` serializes as `2.0` (and NaN would be `null`) either way. The JSON is byte-identical.
  - Add a direct pin: `serde_json::to_string(&calibration_subtree(&cfg)["grouping"]) ==
    r#"{"exposureToleranceSec":2.0}"#`.

**The per-stage hash helpers**, `crates/athenaeum-core/src/stacking/plan.rs`. All are `pub(crate)`.

- `calibration_hash_for(conn, cfg, frame: &GroupFrame, divisors) -> Result<String, ApiError>`
  (314-347) computes `stage_hash(&calibration_subtree(cfg), &[master_identity(p) for each resolved
  master], &[SourceIdentity{frame.file_id, frame.size, frame.modified_at}])`.
  - `master_identity` (274-292) is `"<path>|<len>|<mtime rfc3339>"`, or `"<path>|missing"`. It reads
    the filesystem.
  - It resolves masters via `export::resolve_generation_cached(conn, frame.frame_id, …)`. This
    needs a DB plus real master files, so it is not golden-stable in a test: tempdir paths and
    mtimes vary.
- `HashMemo::calibration_hash` / `calibration_hash_checked` (360-437) memoize the call above.
- `measurement_hash_for(cfg, calibrated_hash) = stage_hash(&measurement_subtree(cfg),
  &[calibrated_hash], &[])` (442-444).
- `registration_hash_for(cfg, reference_frame_id, reference_calibrated_hash,
  frame_calibrated_hash, frame_measurement_hash)` (474-492). Its upstream list is
  `["ref:<id>", ref_calib, frame_calib, "measure:<m>" if non-empty]`.
- `normalization_hash_for(cfg, registration_hash, measurement_hash, reference_member_ids: &[i64],
  reference_hash, calibration: Option<&SeedsCalibration>, beta: Option<f64>)` (581-633). Its
  upstream list is:
  - `["register", reg, "measure:<m>"?]`;
  - `ln_ref:<id>…`;
  - `"ln_reference_hash:<h>"` when non-empty;
  - `ln_calib_frame:<id>…` and `ln_calib_k:<ch>:{:.6}`;
  - `"ln_beta:{:.6}"`.
- `seeds_calibration_hash_for(cfg, reference_hash, calibration_frames: &[(i64,&str,&str)],
  beta)` (658-676) is used for the `ln_calibration` artifact.
- `SeedsCalibration` is `stacking::ln::SeedsCalibration { frame_ids: Vec<i64>, k: Vec<f64> }`. It
  is constructed in a test at plan.rs:4497.
- The run composes the LN-reference token at run.rs:8520-8580: `combined_registration_hash` and
  `combined_measurement_hash` are the sorted member hashes joined with `","`.
- Integrate, Drizzle and Output have **no** stage hash and no cache. `DRIZZLE_KERNEL_VERSION`
  rides `config_hash` only.

**Where the stage hashes are consumed.**
- `plan.rs`: 1286-1287 (register stale), 2099-2102 (measure), 2178 (LN), and `is_fresh` (695).
- `run.rs`: 3035, 3174, 4075, 4301-4302, 8545-8571, 8858-8881.

**Every artifact lookup is also keyed by `group_key`.** The keyed functions are
`find_artifact(conn, set, group_key, kind, frame_id)` and the unique index
`(frames_set_id, group_key, kind, COALESCE(frame_id,0))`. Any regrouping, for example a new
`exposureMode`, therefore misses every per-frame artifact (calibrated, metrics, fits, registered,
ln), not only the group-level ones. That is the "existing behaviour for any regrouping" in §8. It
is broader than the phrase "group-keyed artifacts" suggests.

### 0.3 Existing tests that pin hashes or subtrees (config.rs `mod tests`, 761-1615)

| test | line | asserts |
| ---- | ---- | ---- |
| `config_hash_is_canonical_and_pinned` | 1472-1575 | **The only golden literal in stacking:** `config_hash(&StackingConfig::default()) == "b3240460a4cec0de"`. It carries a long "Changed here (…)" history comment, one paragraph per re-pin. |
| `two_pass_moves_the_config_hash_but_no_stage_hash` | 807-826 | `config_hash` moves; reg/meas/calib stage hashes are equal. |
| `local_scale_moves_the_normalization_stage_hash` | 837-860 | LN moves; reg/meas/calib do not. |
| `the_tps_fields_move_the_registration_stage_hash` | 870-906 | reg moves; meas/calib do not. |
| `geometry_moves_the_registration_stage_hash` | 914-935 | |
| `bayer_moves_the_run_fingerprint_but_no_stage_hash` | 944-967 | All four stage hashes are equal. |
| `format_moves_the_run_fingerprint_but_no_stage_hash` | 969-989 | Also checks that `calibration_subtree(&cfg)["calibration"].get("format").is_none()`. |
| `stage_hash_is_stable_and_sensitive` | 1154-1192 | Also checks that serde_json orders keys. |
| `config_hash_changes_when_the_seed_population_rules_change` | 1198-1235 | |
| `all_three_fit_derived_subtree_hashes_follow_the_psf_fit_version` | 1245-1277 | |
| `stage_hash_is_order_insensitive` | 1416-1469 | |
| `the_drizzle_kernel_version_rides_the_fingerprint_and_no_stage_hash` | 1583-1614 | |
| `serde_names_follow_the_spec` | 991-1029 | Substring needles, e.g. `"exposureToleranceSec":2.0`. Adding grouping/output fields keeps these passing; only the `drizzle` and `paths` needles are whole-object. |

plan.rs DB-backed hash tests use `test_fixtures::frame_set` / `add_light` /
`add_master_dark_and_flat`. Examples: `artifact_without_path_or_size_is_stale` (4048) and
`fresh_artifacts_and_registration_leave_nothing_stale` (4298-4477). They compare
helper-vs-helper and pin no literals. Pure-input hash tests:
`the_ln_hashes_follow_the_measure_seed_config` (4488), `the_ln_hashes_follow_the_group_beta`
(4578), `the_registration_hash_follows_the_measure_seed_config` (4654). Other golden-pin precedent
in the repo: `sharing/wire_golden_tests.rs` (wire bytes).

**Consequences for 6b to write into the plan:**
- **`config_hash_is_canonical_and_pinned` WILL go red.** It fails when `grouping.exposureMode` /
  `stopsWidth` (step 7) and `output.coverageMap` / `cropToCoverage` (step 10) land, because
  `config_hash` serializes the whole config.
  - Re-pin it with a new "Changed here" paragraph, as the history comment requires.
  - The CHANGELOG comment at 1559-1562 says this pin was once missed locally. Run
    `stacking::config` whenever a config struct gains a field.
  - Side effect of a `config_hash` move, documented on `DRIZZLE_KERNEL_VERSION` (config.rs:568-577)
    and plan.rs:1936-1947: the plan gate's LN-exclusion memory (`last_run.config_hash == hash`)
    empties once. An LN-excluded frame is then re-owed one Normalize pass on the first plan. No
    `stacking_artifacts` row goes stale.
- **A stale comment for step 0 to fix**, config.rs:1488-1490: "the calibrate/measure/register stage
  hashes never fold in `grouping`". This is false; `calibration_subtree` folds it. It is the fable
  C1 finding.

### 0.4 How a test can compute "every stage hash of a fixed frame-set config"

- The DB-backed path (`calibration_hash_for`) cannot produce golden literals: master identities
  carry tempdir paths and mtimes.
- The pin should feed fixed literal inputs to the pure functions. `stage_hash` is the exact formula
  `calibration_hash_for` uses, with the master identities and `SourceIdentity` supplied as
  literals.
- Everything is reachable from `crate::stacking::plan` / `crate::stacking::config`. The module is
  gated `#[cfg(all(feature="render", feature="solver"))]` (lib.rs:55), and both are default
  features.
- The natural home is plan.rs `mod tests` (2361+; `use super::*` already sees every helper), or
  config.rs `mod tests` via `crate::stacking::plan::…`.

Sketch:
```rust
#[test]
fn stage_hashes_are_pinned_to_golden_literals() {
    use crate::stacking::config::{preset, StackingPreset, calibration_subtree, stage_hash, SourceIdentity};
    // A non-default config too, so the grouping half of the calibration subtree is exercised
    // with a non-default number (and LN/drizzle/TPS on, so every subtree is non-trivial).
    let mut custom = StackingConfig::default();
    custom.grouping.exposure_tolerance_sec = 5.0;
    custom.normalization.local.enabled = true;
    custom.registration.distortion = crate::stacking::register::DistortionChoice::Tps;
    let configs = [
        ("default", StackingConfig::default()),
        ("fastPreview", preset(StackingPreset::FastPreview)),
        ("maximumQuality", preset(StackingPreset::MaximumQuality)),
        ("custom", custom),
    ];
    let masters = ["/m/dark.fits|100|2026-01-01T00:00:00+00:00", "/m/flat.fits|200|2026-01-01T00:00:00+00:00"];
    let src = || [SourceIdentity { file_id: 42, size: 123_456, modified_at: "2026-01-01T00:00:00Z".into() }];
    let calib_k = SeedsCalibration { frame_ids: vec![11, 22], k: vec![0.994] };
    let mut got = Vec::new();
    for (name, cfg) in &configs {
        let calib = stage_hash(&calibration_subtree(cfg), &masters, &src());
        let meas  = measurement_hash_for(cfg, &calib);
        let reg   = registration_hash_for(cfg, 7, "ref-calib", &calib, &meas);
        let ln_ref = normalization_hash_for(cfg, "r1,r2", "m1,m2", &[11, 22], "", None, Some(1.5));
        let ln    = normalization_hash_for(cfg, &reg, &meas, &[11, 22], &ln_ref, Some(&calib_k), Some(1.5));
        let ln_cal = seeds_calibration_hash_for(cfg, &ln_ref, &[(11, "r1", "m1"), (22, "r2", "m2")], Some(1.5));
        got.push(format!("{name}: calib={calib} meas={meas} reg={reg} ln_ref={ln_ref} ln={ln} ln_cal={ln_cal}"));
    }
    // GOLDEN recorded on main 17e2e96e BEFORE the step-0 edit; first run with placeholders,
    // copy the `left` side from the assert_eq! failure (or `-- --nocapture` + println!).
    assert_eq!(got, vec![/* 4 literal lines */]);
    // and the subtree shape itself:
    assert_eq!(serde_json::to_string(&calibration_subtree(&StackingConfig::default())["grouping"]).unwrap(),
               r#"{"exposureToleranceSec":2.0}"#);
}
```

Recording procedure:
1. Commit the test with placeholder literals.
2. On main, run `cargo test -p athenaeum-core --lib stacking::plan::tests::stage_hashes_are_pinned`.
   The `assert_eq!` failure prints the real values. Paste them in.
3. Make the `calibration_subtree` edit.
4. Re-run; the test must stay green.

Then, in step 7/10, add the new fields and assert the SAME literals still hold:
- with `exposureMode` and `stopsWidth` set to non-default values on each config;
- with `output.coverageMap` / `cropToCoverage` flipped.

Only `config_hash_is_canonical_and_pinned` moves.

Optional extra: a DB-backed equality check in plan.rs. It would assert that
`calibration_hash_for(conn, cfg, frame) == stage_hash(&calibration_subtree(cfg), &[identities of
the fixture masters], &[the frame's SourceIdentity])`. That proves the golden formula is the one
the plan really uses.

---------------------------------------------------------------------------------------------

## STEP 2: the table rebuild and handles

### 2.1 Current DDL: `crates/athenaeum-core/src/db/schema.rs`

**`stacking_runs`** (1167-1184). It has no AUTOINCREMENT.
```sql
CREATE TABLE IF NOT EXISTS stacking_runs (
    id INTEGER PRIMARY KEY,
    frames_set_id INTEGER NOT NULL REFERENCES frames_set(id) ON DELETE CASCADE,
    status TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    config_json TEXT NOT NULL,
    config_hash TEXT NOT NULL,
    reference_frame_id INTEGER,          -- no FK (good: can hold a handle)
    reference_mode TEXT NOT NULL,
    working_dir TEXT NOT NULL,
    output_dir TEXT NOT NULL,
    summary_json TEXT,
    error TEXT
)
```

**`stacking_run_groups`** (1185-1209), plus `ALTER … ADD COLUMN beta REAL` behind a
`column_exists` guard (2838-2840). That guard is the template for `exposure_class`.
```sql
CREATE TABLE IF NOT EXISTS stacking_run_groups (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES stacking_runs(id) ON DELETE CASCADE,
    group_key TEXT NOT NULL, instrume TEXT, color_mode TEXT NOT NULL, filter TEXT, binning INTEGER,
    width INTEGER, height INTEGER, exposure REAL, frame_count INTEGER NOT NULL,
    included_count INTEGER NOT NULL, master_path TEXT, drizzle_path TEXT, rejection_low_path TEXT,
    rejection_high_path TEXT, stats_json TEXT, status TEXT NOT NULL, error TEXT,
    UNIQUE(run_id, group_key)
)
```

**`stacking_run_frames`** (1210-1231)
```sql
CREATE TABLE IF NOT EXISTS stacking_run_frames (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES stacking_runs(id) ON DELETE CASCADE,
    group_id INTEGER NOT NULL REFERENCES stacking_run_groups(id) ON DELETE CASCADE,
    frame_id INTEGER NOT NULL REFERENCES frames(id) ON DELETE CASCADE,
    included INTEGER NOT NULL, exclusion_reason TEXT, weight REAL, weight_channels_json TEXT,
    metrics_json TEXT, reg_status TEXT, reg_model TEXT, reg_rms_px REAL, reg_inliers INTEGER,
    reg_inlier_ratio REAL, reg_flipped INTEGER, rejected_fraction REAL,
    UNIQUE(run_id, frame_id)
)
```

**`stacking_artifacts`** (1232-1247)
```sql
CREATE TABLE IF NOT EXISTS stacking_artifacts (
    id INTEGER PRIMARY KEY,
    frames_set_id INTEGER NOT NULL REFERENCES frames_set(id) ON DELETE CASCADE,
    frame_id INTEGER REFERENCES frames(id) ON DELETE CASCADE,
    group_key TEXT NOT NULL, kind TEXT NOT NULL, path TEXT, config_hash TEXT NOT NULL,
    size INTEGER, modified_at TEXT, payload_json TEXT, created_at TEXT NOT NULL
)
```

**`stacking_set_config`** (1248-1256)
```sql
CREATE TABLE IF NOT EXISTS stacking_set_config (
    frames_set_id INTEGER PRIMARY KEY REFERENCES frames_set(id) ON DELETE CASCADE,
    config_json TEXT NOT NULL,
    excluded_frame_ids_json TEXT NOT NULL DEFAULT '[]',
    updated_at TEXT NOT NULL
)
```

**`master_lights`** (1265-1283)
```sql
CREATE TABLE IF NOT EXISTS master_lights (
    id INTEGER PRIMARY KEY,
    frames_set_id INTEGER NOT NULL REFERENCES frames_set(id) ON DELETE CASCADE,
    run_id INTEGER NOT NULL REFERENCES stacking_runs(id) ON DELETE CASCADE,
    group_key TEXT NOT NULL, kind TEXT NOT NULL, path TEXT NOT NULL, format TEXT NOT NULL,
    width INTEGER NOT NULL, height INTEGER NOT NULL, channels INTEGER NOT NULL, frames INTEGER NOT NULL,
    total_exposure_s REAL, created_at TEXT NOT NULL,
    UNIQUE(run_id, group_key, kind)
)
```

**Indexes: one unconditional loop on every `init_db`** (1284-1310)
```sql
CREATE INDEX IF NOT EXISTS idx_stacking_runs_set ON stacking_runs(frames_set_id)
CREATE INDEX IF NOT EXISTS idx_stacking_run_groups_run ON stacking_run_groups(run_id)
CREATE INDEX IF NOT EXISTS idx_stacking_run_frames_run ON stacking_run_frames(run_id)
CREATE INDEX IF NOT EXISTS idx_stacking_run_frames_frame ON stacking_run_frames(frame_id)
CREATE INDEX IF NOT EXISTS idx_stacking_artifacts_set ON stacking_artifacts(frames_set_id)
CREATE UNIQUE INDEX IF NOT EXISTS stacking_artifacts_key
   ON stacking_artifacts(frames_set_id, group_key, kind, COALESCE(frame_id, 0))     -- 1290-1291
CREATE INDEX IF NOT EXISTS idx_stacking_run_frames_group ON stacking_run_frames(group_id)
CREATE INDEX IF NOT EXISTS idx_stacking_artifacts_frame ON stacking_artifacts(frame_id)
CREATE INDEX IF NOT EXISTS idx_stacking_set_config_set ON stacking_set_config(frames_set_id)
CREATE INDEX IF NOT EXISTS idx_master_lights_set ON master_lights(frames_set_id)
CREATE INDEX IF NOT EXISTS idx_master_lights_run ON master_lights(run_id)
```
The live real-DB schema (dev copy, see 2.7) matches this exactly. No triggers or views reference
these tables.

**`collab_projects`** (2263-2286), plus guarded ALTERs at 2294-2404, 2508-2568 and 2534-2551.
```sql
CREATE TABLE IF NOT EXISTS collab_projects (
    project_id TEXT PRIMARY KEY, slug TEXT NOT NULL, title TEXT NOT NULL, data_role TEXT NOT NULL,
    is_coordinator INTEGER NOT NULL DEFAULT 0, require_approval INTEGER NOT NULL DEFAULT 0,
    pending_announcements INTEGER NOT NULL DEFAULT 0,  -- renamed to pending_frames at 2312-2319
    project_status TEXT NOT NULL DEFAULT 'active', target_name TEXT NOT NULL,
    target_ra_deg REAL NOT NULL, target_dec_deg REAL NOT NULL, target_radius_deg REAL NOT NULL,
    membership_version INTEGER NOT NULL, snapshot_payload_b64 TEXT NOT NULL,
    snapshot_signature_b64 TEXT NOT NULL, members_json TEXT NOT NULL, thresholds_version INTEGER,
    thresholds_rules_json TEXT, fetched_at TEXT NOT NULL DEFAULT (datetime('now'))
)
-- + auto_replicate, gov_caps_json, synced_caps_json, hub_version, manifest_cursor, dictionary_version,
--   dictionary_json, policy_json, replication_paused, auto_publish, publish_mode, last_publish_run,
--   lost_at, feed_epoch, holder_seq, publishing_device_id, publishing_device_name, goals_json,
--   members_seen_json   (each `if !column_exists(...) { ALTER TABLE ... ADD COLUMN ... }`)
```
`canonical_grid_json TEXT` uses the same guard pattern; place it after 2568. `project_id` is a TEXT
PK, so it is unique and a valid FK parent.

### 2.2 Foreign keys

**FKs INTO the tables being changed.** I grepped all crates for `REFERENCES …`.
- **INTO `stacking_runs`**:
  - `stacking_run_groups.run_id` (1188);
  - `stacking_run_frames.run_id` (1213);
  - `master_lights.run_id` (1269).
  - **This is why FK enforcement must be OFF.** Otherwise `DROP TABLE stacking_runs`
    cascade-deletes every group, run-frame and master-light row.
- **INTO `stacking_run_groups`**: `stacking_run_frames.group_id` (1214). Groups are not rebuilt
  (ALTER only).
- **INTO `stacking_run_frames`, `stacking_artifacts`, `master_lights` and `stacking_set_config`**:
  none.
- **INTO `collab_projects`** (all `ON DELETE CASCADE`):
  - `project_frames_local` (2441), `collab_prepared_frames` (2473), `collab_withheld_frames` (2487);
  - `collab_my_claims` (2699), `collab_outbox` (2707), `collab_holder_devices` (2716),
    `collab_holder_claims` (2724), `collab_deletions` (2731), `collab_receive_sessions` (2759).

**FKs OUT of the stacking tables**:
- `stacking_runs.frames_set_id` → `frames_set`;
- `stacking_run_frames.frame_id` → `frames`;
- `stacking_artifacts.frames_set_id` → `frames_set`, and `.frame_id` → `frames`;
- `stacking_set_config.frames_set_id` → `frames_set`;
- `master_lights.frames_set_id` → `frames_set`.

**`collab_projects` rows are never deleted in production code.**
- The only deletes are in tests: `db/collab_frames.rs:2513` and `db/collab.rs:1077`.
- `mark_lost` (db/collab.rs:346) only stamps `lost_at`.
- `upsert_project` (db/collab.rs:187-239) is `INSERT … ON CONFLICT(project_id) DO UPDATE`, not
  `INSERT OR REPLACE`. A poll refresh therefore never cascades into `stack_project_frames` /
  `stacking_*`. This confirms the §4.1 assumption.

### 2.3 The existing `archive_operations` 12-step rebuild (schema.rs:2010-2128, verbatim)

```rust
    // archive_operations: frames_set_id nullable + calibration_set_id.
    // SQLite can't drop NOT NULL via ALTER — rebuild once, detected by the
    // absence of the calibration_set_id column.
    if !column_exists(conn, "archive_operations", "calibration_set_id")? {
        // FK enforcement MUST be disabled around the rebuild. ... `DROP TABLE archive_operations`
        // performs an implicit DELETE of every row before dropping, which fires the ON DELETE CASCADE
        // ... The pragma is a no-op inside an open transaction, so it must be issued here, OUTSIDE
        // the BEGIN/COMMIT batch below.
        conn.pragma_update(None, "foreign_keys", false)?;
        let rebuild = conn.execute_batch(
            "BEGIN;
             CREATE TABLE archive_operations_new ( ... );
             INSERT INTO archive_operations_new (cols…) SELECT cols… FROM archive_operations;
             UPDATE sqlite_sequence SET seq = COALESCE((SELECT seq FROM sqlite_sequence WHERE name='archive_operations'), 0)
              WHERE name='archive_operations_new' AND seq < COALESCE((…), 0);
             INSERT INTO sqlite_sequence (name, seq) SELECT 'archive_operations_new', seq FROM sqlite_sequence
               WHERE name='archive_operations' AND NOT EXISTS (SELECT 1 FROM sqlite_sequence WHERE name='archive_operations_new');
             DROP TABLE archive_operations;
             ALTER TABLE archive_operations_new RENAME TO archive_operations;
             COMMIT;",
        );
        // A failed statement inside the batch leaves its explicit BEGIN open; `PRAGMA foreign_keys`
        // is a silent no-op while a transaction is pending, so roll back FIRST (audit M7).
        if rebuild.is_err() && !conn.is_autocommit() {
            if let Err(e) = conn.execute("ROLLBACK", []) {
                tracing::error!(error = %e, "migration batch rollback failed");
            }
        }
        // Re-enable FK enforcement whether or not the rebuild succeeded, then surface the error.
        let re_enable = conn.pragma_update(None, "foreign_keys", true);
        rebuild?;
        re_enable?;
        // Step 10 ... Scoped to the three tables the rebuild touches ... rather than a whole-DB check,
        // so an unrelated pre-existing violation elsewhere can't brick catalog startup.
        for table in ["archive_operations", "archive_operation_files", "archive_operation_steps"] {
            let violations: i64 = conn.query_row(
                &format!("SELECT COUNT(*) FROM pragma_foreign_key_check('{table}')"), [], |r| r.get(0))?;
            if violations > 0 {
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                    Some(format!("archive_operations rebuild broke foreign-key integrity: \
                         {violations} violation(s) reported by foreign_key_check on {table}"))));
            }
        }
        // Recreate the two indexes the rebuild dropped
        conn.execute("CREATE INDEX IF NOT EXISTS idx_archive_ops_status ON archive_operations(status)", [])?;
        conn.execute("CREATE INDEX IF NOT EXISTS idx_archive_ops_frames_set ON archive_operations(frames_set_id)", [])?;
    }
    conn.execute("CREATE INDEX IF NOT EXISTS idx_archive_ops_calibration_set ON archive_operations(calibration_set_id)", [])?;
```

Facts about this code:
- The probe is `column_exists(conn, table, col)` (schema.rs:309, `pragma_table_info`). The fresh
  CREATE TABLE (schema.rs:801-824) already has the new shape, so a fresh DB skips the rebuild.
- `INIT_DB_LOCK` (schema.rs:440) is a process-wide `std::sync::Mutex<()>` held for the whole
  `init_db` body (466-468; recovers from poison). The rebuild needs nothing extra beyond being
  inside `init_db`.
- Production connections run `PRAGMA foreign_keys = ON` (`db/mod.rs:51-63` `setup_connection`;
  also the bundled compile-time default).
- **The spec's ordering differs from the archive code.** Spec §4.2 says "runs
  `PRAGMA foreign_key_check` before commit … exactly as the archive rebuild does". The archive
  rebuild checks AFTER COMMIT and after FKs are back on, so a violation returns `Err` with the
  rebuild already committed.
  - Recommended: run the check INSIDE the transaction before COMMIT (the SQLite recipe's own
    order). That needs a `rusqlite::Transaction::new_unchecked` or a manual BEGIN plus `query_row`;
    `execute_batch` alone cannot return the count. On violation, roll back.
  - `foreign_key_check` works with `foreign_keys=OFF`.
  - Scope the check to the rebuilt tables plus `stacking_run_groups`, as the archive code does. A
    pre-existing violation in a stacking table would otherwise block startup every launch. The
    real copy has 0 (see 2.7).
- `stacking_runs` has no AUTOINCREMENT, so no `sqlite_sequence` carry-forward is needed. Copying
  `id` verbatim keeps every `run_id` / `group_id` child valid.

### 2.4 Schema tests that constrain the rebuild

- **`every_foreign_key_child_column_is_indexed`** (schema.rs:3399-3474, `mod identity_schema_tests`).
  - It runs on a fresh `init_db`.
  - For every table it reads `PRAGMA index_list`, and for each index takes the first NON-NULL name
    from `PRAGMA index_info`. Expression columns have NULL names and are filtered out.
  - It requires every `PRAGMA foreign_key_list` child column to be one of those leftmost names.
  - Consequences:
    - Every new FK child column needs its own index whose leftmost column it is:
      `stacking_runs.project_id`, `stacking_run_frames.project_frame`,
      `stacking_artifacts.project_id`, `stacking_artifacts.project_frame`, `master_lights.project_id`.
    - `stack_project_frames.project_id` is covered by the autoindex of
      `UNIQUE(project_id, frame_uuid)`. `stacking_project_config.project_id TEXT PRIMARY KEY` is
      covered by its `sqlite_autoindex`. Contrast `stacking_set_config`, whose INTEGER PRIMARY KEY
      is a rowid alias and needed the explicit `idx_stacking_set_config_set`.
    - An expression-led index (the COALESCE key) never counts. The test sees `group_key` as its
      leftmost column.
    - The test's doc warns that `CREATE INDEX IF NOT EXISTS` on a name already taken by a different
      column list is a silent no-op.
- `init_db_twice_keeps_the_stacking_tables` (db/stacking.rs:940-960) checks that the five tables
  exist after two `init_db` calls.
- `artifact_key_treats_null_frame_as_one_row` (db/stacking.rs:1113-1168) checks that a NULL
  `frame_id` upserts onto ONE row, and checks the delete helpers. It exercises the artifacts
  `ON CONFLICT`.
- `master_light_round_trip_and_cascade` (db/stacking.rs:1185-1306) checks the
  `UNIQUE(run_id, group_key, kind)` upsert and the `DELETE FROM stacking_runs` cascade.
- `list_unfinished_runs_reports_only_planning_and_running` (db/stacking.rs:1070-1111) asserts the
  `(id, frames_set_id)` tuple.
- Archive-rebuild tests to imitate for a stacking-rebuild test:
  - `archive_operations_rebuild_preserves_existing_rows` (schema.rs:4898);
  - `…_preserves_autoincrement_high_water_mark` (4931; builds the legacy shape by hand);
  - `…_preserves_child_rows` (5018-5150). This one hand-builds the parent and children, runs
    `init_db`, and asserts the children were not cascade-wiped and still join.
- `fresh_init_never_creates_light_calibrations_and_reinit_is_a_no_op` (3022) is the idempotence
  pattern.
- `concurrent_init_db_on_shared_file_is_race_free` (5197; 8 threads × 50 `init_db`s on one file)
  must stay green. That holds by `INIT_DB_LOCK` plus the probe.
- No existing test compares the fresh shape with the migrated shape for the stacking tables.
  `legacy_sync_tables_converge_to_fresh` (3577) does it for the sync tables with a
  `table_columns` helper (3553) and is the pattern to copy.
- A test-only `DROP TABLE stacking_run_groups` (run.rs:10115, fault injection) is unaffected.

### 2.5 Every SQL statement on these tables

All are in `crates/athenaeum-core/src/db/stacking.rs` unless noted. A workspace grep finds raw SQL
on these tables elsewhere only in schema.rs and one web test (below).

| fn | line | statement / what changes |
| ---- | ---- | ---- |
| `insert_run` | 373-391 | `INSERT INTO stacking_runs (frames_set_id, status, started_at, config_json, config_hash, reference_frame_id, reference_mode, working_dir, output_dir) VALUES (?1,'planning',…)`. **Needs a `project_id` column.** |
| `set_run_status` | 393-399 | `UPDATE stacking_runs SET status=?1 WHERE id=?2`. Unchanged. |
| `set_run_reference` | 401-412 | `UPDATE stacking_runs SET reference_frame_id=?1, reference_mode=?2 WHERE id=?3`. Unchanged. |
| `finish_run` | 416-429 | Unchanged. |
| `get_run` | 431-436 | `SELECT {RUN_COLUMNS} … WHERE id=?1`. **`row_to_run` (40-56) reads `frames_set_id` as `i64`, which fails with `InvalidColumnType` on a NULL.** `RUN_COLUMNS` is at 58-59. |
| `list_runs` | 439-453 | `… WHERE frames_set_id = ?1 ORDER BY started_at DESC, id DESC LIMIT ?2`. Needs a per-subject variant. |
| `active_run_for_set` | 458-468 | `SELECT id … WHERE frames_set_id=?1 AND status IN ('planning','running') ORDER BY id DESC LIMIT 1`. Per subject. |
| `list_unfinished_runs` | 476-486 | `SELECT id, frames_set_id … WHERE status IN ('planning','running') ORDER BY id`, returning `Vec<(i64,i64)>`. **Errors on the first NULL `frames_set_id`.** That would make `heal_interrupted_runs` (run.rs:545-593), which runs from every API entry point, fail once a project run exists. |
| `insert_group` / `update_group` / `list_groups` | 493-562 | Unchanged, except `exposure_class` in `NewGroup`, `GROUP_COLUMNS` (134-136) and `row_to_group`. |
| `upsert_frame_row` | 571-613 | `INSERT INTO stacking_run_frames (run_id, group_id, frame_id, …) VALUES (…) ON CONFLICT(run_id, frame_id) DO UPDATE SET group_id=…, included=…, … RETURNING id`. **This is the conflict-target hazard; see 2.6.** |
| `set_frame_rejected_fraction` | 623-634 | `UPDATE stacking_run_frames SET rejected_fraction=?1 WHERE run_id=?2 AND frame_id=?3`. Needs a `project_frame` twin. |
| `list_frame_rows` | 636-644 | `… WHERE run_id=?1`. **`row_to_frame` (159-178) reads `frame_id` as `i64`, which fails on NULL.** `FRAME_COLUMNS` is at 180-182. |
| `upsert_artifact` | 654-684 | `INSERT INTO stacking_artifacts (frames_set_id, frame_id, group_key, kind, path, config_hash, size, modified_at, payload_json, created_at) VALUES (…) ON CONFLICT(frames_set_id, group_key, kind, COALESCE(frame_id, 0)) DO UPDATE SET path, config_hash, size, modified_at, payload_json, created_at RETURNING id`. **Its target must be rewritten; see 2.6.** |
| `find_artifact` | 686-704 | `… WHERE frames_set_id=?1 AND group_key=?2 AND kind=?3 AND frame_id IS ?4`. Per subject: `project_id = ? … project_frame IS ?`. |
| `list_artifacts` | 706-735 | `… WHERE frames_set_id=?1 [AND kind=?2] ORDER BY id`. Test-only callers today. **`row_to_artifact` (200-214) reads `frames_set_id` as `i64`, which fails on NULL.** |
| `delete_artifacts` | 740-756 | `DELETE … WHERE frames_set_id=?1 AND kind IN (…)`. Per subject. |
| `delete_all_artifacts` | 765-771 | `DELETE … WHERE frames_set_id=?1`. Per subject. |
| `insert_master_light` | 782-817 | `INSERT INTO master_lights (frames_set_id, run_id, …) … ON CONFLICT(run_id, group_key, kind) DO UPDATE SET frames_set_id = excluded.frames_set_id, …`. **Its target is fine** because `UNIQUE(run_id, group_key, kind)` is kept. It must also SET `project_id = excluded.project_id`. **`row_to_master_light` (249-265) reads `frames_set_id` as `i64`.** |
| `list_master_lights` / `find_master_light` | 820-844 | Keyed by `run_id`. Unchanged. |
| `get_set_config` / `set_set_config` | 850-895 | `ON CONFLICT(frames_set_id)` on the PK. Unchanged (the table is unchanged). |
| web test `concurrent_previews_for_different_kinds_both_succeed_on_one_permit` | `athenaeum-web/src/routes/stacking.rs:854` | `SELECT frames_set_id FROM stacking_runs WHERE id = ?1` into an `i64`, then `NewMasterLight { frames_set_id, … }`. This is test code and moves with the `NewMasterLight` shape. |

Struct and TS knock-ons, all `i64` today:
- `StackingRunRow.frames_set_id` (24-38);
- `StackingRunFrameRow.frame_id` (138-157);
- `StackingArtifactRow.frames_set_id` (184-198);
- `MasterLight.frames_set_id` (224-247);
- `NewRun` / `NewArtifact` / `NewMasterLight` / `NewFrameRow` (270-359).

`StackingRunRow`, `StackingRunGroupRow` and `StackingRunFrameRow` are ts-exported
(ts_export.rs:423-425 → `src/types/stacking.ts:384` `framesSetId: number`, `:417` `frameId: number`).

Production callers of these functions (test modules start at plan.rs:2361, run.rs:9794,
api/stacking.rs:1149, paths.rs:876, web routes/stacking.rs:533):
- `insert_run`: run.rs:805.
- `get_run`: run.rs:612 and 831; api/stacking.rs:267 and 959. The preview at
  api/stacking.rs:969 does `frame_set_name(&conn, run_row.frames_set_id)`.
- `list_runs`: plan.rs:971, 981, 1119, 1200, 1946, 1979; api/stacking.rs:242.
- `active_run_for_set`: plan.rs:2326; api/stacking.rs:844.
- `list_unfinished_runs`: run.rs:549.
- `upsert_frame_row`: run.rs:5931.
- `set_frame_rejected_fraction`: run.rs:7778.
- `list_frame_rows`: plan.rs:1265, 1947; api/stacking.rs:270.
- `upsert_artifact`: run.rs:1876, 1891, 3180, 3250, 5795, 8926, 9233, 9572.
- `find_artifact`: run.rs:1598, 1606, 2909, 3041, 4124, 5825, 6801, 7897, 8592, 8782, 8820, 8869;
  plan.rs:759, 1312, 2002, 2022, 2052, 2054, 2077, 2153.
- `delete_artifacts` / `delete_all_artifacts`: paths.rs:852, 855, 862 (in `cleanup_work`, paths.rs:808).
- `insert_master_light`: run.rs:7758, 8186, 8203.
- `find_master_light`: api/stacking.rs:961.
- `get_set_config`: plan.rs:862; api/stacking.rs:341, 798, 850.
- `set_set_config`: api/stacking.rs:366.

### 2.6 ON CONFLICT vs the spec's new indexes (verified in an in-memory sqlite3 3.54 session)

1. **Spec vs `upsert_frame_row` (db/stacking.rs:578): it does not prepare.** The spec replaces
   `UNIQUE(run_id, frame_id)` with two partial unique indexes. Against a partial index,
   `ON CONFLICT(run_id, frame_id)` fails to prepare: *"ON CONFLICT clause does not match any
   PRIMARY KEY or UNIQUE constraint"* (verified). There are two working forms:
   - (a) Keep the partial indexes and add the predicate to the target:
     `ON CONFLICT(run_id, frame_id) WHERE frame_id IS NOT NULL DO UPDATE …`. The predicate must be
     expression-equal to the index's own WHERE (SQLite `sqlite3UpsertAnalyzeTarget` uses
     `ExprCompare`). Verified working. The project twin is
     `ON CONFLICT(run_id, project_frame) WHERE project_frame IS NOT NULL`.
   - (b) **Simpler, recommended.** Keep the table-level `UNIQUE(run_id, frame_id)` and add
     `UNIQUE(run_id, project_frame)` with no partial indexes. SQLite treats NULLs as distinct in a
     UNIQUE constraint, so a project row (frame_id NULL) never collides on the first, and vice
     versa. The existing `ON CONFLICT(run_id, frame_id)` stays valid byte-for-byte. Verified:
     repeated upserts on each key update in place, and distinct handles insert.
2. **`upsert_artifact` (db/stacking.rs:661) under the spec's COALESCE index.** The spec's unique
   index works, but the old 4-term target fails against it (verified, same error). The target must
   list the same 6 expressions. Order does not matter (verified with a shuffled target). Each
   expression must match token-for-token, e.g. `COALESCE(project_id,'')` with `''` and
   `COALESCE(frame_id,0)` with `0`:
   ```sql
   ON CONFLICT(COALESCE(frames_set_id,0), COALESCE(project_id,''), group_key, kind,
               COALESCE(frame_id,0), COALESCE(project_frame,0)) DO UPDATE SET …
   ```
3. **Name and placement trap in the unconditional index loop (schema.rs:1284-1310).**
   - The loop still contains the old `CREATE UNIQUE INDEX IF NOT EXISTS stacking_artifacts_key
     ON stacking_artifacts(frames_set_id, group_key, kind, COALESCE(frame_id, 0))`. It runs on every
     `init_db`.
   - The rebuild drops it (DROP TABLE drops its indexes). If the loop keeps that statement, the
     SECOND `init_db` re-creates the OLD key on the NEW table. That yields two unique indexes, with
     upserts targeting only one.
   - If the new index reuses the name `stacking_artifacts_key`, `IF NOT EXISTS` makes the loop and
     the rebuild disagree silently (the trap the FK test's doc describes).
   - So the loop and the rebuild must change in lockstep. Recommended:
     - move every index of the four rebuilt tables out of that loop into one helper that runs after
       the rebuild point (fresh and migrated paths alike);
     - give the unique key a new name (e.g. `stacking_artifacts_subject_key`).
4. **`insert_master_light` is fine.** `UNIQUE(run_id, group_key, kind)` is kept, and its
   `ON CONFLICT(run_id, group_key, kind)` matches.

### 2.7 Rehearsal on a copy of a real database (done, read-only on the copy)

- **The copy.** I copied `~/Library/Application Support/com.vsharifov.athenaeum.dev/athenaeum.db`
  (571 MB) with `sqlite3 -readonly <src> ".backup <scratch>/dev-copy.db"`. It is a prod snapshot
  (same size as prod).
- **Contents:**
  - `stacking_runs` 4 (max id 4); `stacking_run_groups` 8; `stacking_run_frames` 1104;
  - `stacking_artifacts` 368 (max id 736; 0 rows with NULL `frame_id`); `master_lights` 6;
  - `stacking_set_config` 1; `collab_projects` 4; `project_frames_local` 188.
- **Integrity:** `pragma_foreign_key_check` is 0 on every stacking table and 0 DB-wide. There are
  no triggers or views on these tables.
- **The rehearsal.** Script: `scratchpad/rehearsal.sql`, with `rehearsal2.sql` adding the coverage
  check. It ran in `sqlite3 :memory:` after `.restore` from the copy with `immutable=1`; neither
  the real DB nor the copy was written.
  - Steps: FK OFF → BEGIN → create `stack_project_frames` → rebuild `stacking_runs`,
    `stacking_run_frames` (option (b) uniques), `stacking_artifacts` (COALESCE key) and
    `master_lights` with the subject/frame pairs and CHECKs → recreate all indexes plus new FK-child
    indexes → `foreign_key_check` inside the transaction = 0 → COMMIT → FK ON.
  - Results:
    - row counts are identical before and after;
    - `stacking_run_groups` / `stacking_run_frames` FKs still resolve to `stacking_runs`;
    - FK check is 0;
    - the FK-child-index test logic reports no unindexed column;
    - the existing `upsert_frame_row` target still prepares and upserts.
  - The 570 MB copy was deleted afterwards. Re-make it with the `.backup` line above.
- **The CHECKs used in the rehearsal:**
  - runs, artifacts, master_lights: `CHECK ((frames_set_id IS NULL) <> (project_id IS NULL))`;
  - run_frames: `CHECK ((frame_id IS NULL) <> (project_frame IS NULL))`;
  - artifacts' frame pair: `CHECK (frame_id IS NULL OR project_frame IS NULL)` (both may be NULL).

### 2.8 Placement in `init_db`

- **The parent tables are created late.** `collab_projects` is created at schema.rs:2264, long
  after the stacking tables (1168), and `stack_project_frames` will be new.
- **SQLite allows an FK to a not-yet-existing table at CREATE time.** DML on a child whose parent
  table is missing errors with "no such table" (`sqlite3LocateTable` in `sqlite3FkCheck`).
- **Nothing in `init_db` between 1310 and 2264 touches the stacking tables.** It deletes from
  `black_hole` and `calibration_set_to_frames`, prunes calibration sets, and drops
  `light_calibrations` / package tables; none of that cascades into stacking.
- **Recommendation:**
  - Put the probe-guarded rebuild, plus the `stack_project_frames` / `stacking_project_config`
    creation, after the collab section, e.g. next to the `stacking_run_groups.beta` ALTER at 2838.
    `exposure_class` goes there too.
  - Then either:
    - (i) keep the 1168 CREATEs in the old shape, so a fresh DB rebuilds its empty tables once
      (cheap, one source for the new shape); or
    - (ii) update the 1168 CREATEs to the new shape, as the archive precedent did at 801-824, so the
      probe skips on fresh DBs.
  - Either way, the index creation for the four tables must run after the rebuild point (2.6.3).
- **The probe.** Spec: `!column_exists(conn, "stacking_runs", "project_id")`.

### 2.9 `collab_projects.canonical_grid_json`

- **Reads.**
  - Every read goes through `SELECT_COLS` (db/collab.rs:20-25) and the index-based `row_from_sql`
    (134-168).
  - `upsert_project`'s explicit column list (187-239) leaves out every column it does not name.
  - A new nullable column therefore breaks no reader and survives poll refreshes. `mark_lost`
    (346-355) does not touch it.
- **Precedent for a column refreshed with the project meta but kept off `CollabProjectRow`.** The
  `goals_json` / `members_seen_json` pair is written by `db::collab::set_page_extras`
  (db/collab.rs:388-399). It is called right after `upsert_project` in the refresh path
  (api/collab.rs:2683-2690); the getter is at db/collab.rs:~404.
- **Recommendation:** the same dedicated setter/getter pair (`set_canonical_grid` /
  `canonical_grid`).
  - This avoids touching `CollabProjectRow`, which is constructed in about 20 places: authz.rs:124,
    holdings.rs:2019/2035, collab_exchange.rs:2271, api/collab.rs:2401/7587/7872/9762/11637/14447,
    collab_autopublish.rs:473, export/project_collector.rs:180, db/collab.rs:806,
    test_support.rs:105, and more.
  - The feed consumer to replace is `api/collab_live/feed.rs:754-756`, which is
    `debug!("grid change seen; no consumer in this wave")`.

### 2.10 Real-database copy procedure: documented

- **Paths.**
  - Prod DB: `~/Library/Application Support/com.vsharifov.athenaeum/athenaeum.db` (MEMORY.md
    "Database issues").
  - Debug builds use the `.dev` sibling dir.
  - `ATHENAEUM_APP_DATA_DIR` overrides the desktop dir. `ATHENAEUM_DB_PATH` overrides the
    web/server DB (`athenaeum-web/src/main.rs:30`; docs/logging/README.md:109).
- **The documented copy tool.** `npm run dev:db-refresh` (`scripts/dev-db-refresh.mjs`; spec
  `docs/superpowers/specs/2026-08-09-dev-prod-data-separation-design.md`). It does a WAL-safe
  `sqlite3 .backup` of prod into the `.dev` dir, wipes the 8 transfer tables and 8 identity-bound
  settings keys, and symlinks `catalogs/`.
- **Precedent wording.** docs/backlog-v0.5.5.md:197: "accepted on a copy of the real catalog".
- **A concrete procedure for "tested on a copy of a real database before merge":**
  1. `sqlite3 -readonly "<.dev or prod>/athenaeum.db" ".backup /tmp/x/athenaeum.db"`.
  2. Run `init_db` on it: `ATHENAEUM_DB_PATH=/tmp/x/athenaeum.db cargo run -p athenaeum-web`
     (startup runs `init_db` via `Database::new`, db/mod.rs:162), or point a dev build at it with
     `ATHENAEUM_APP_DATA_DIR`.
  3. Diff the row counts against 2.7 (4/8/1104/368/6), check `pragma_foreign_key_check` = 0,
     `.schema` shows the new shape, and a second start is a no-op.
