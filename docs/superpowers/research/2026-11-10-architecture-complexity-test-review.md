# Architecture, complexity and test review (2026-11-10)

Read-only review at `a440b267`. Nothing was changed. Numbers marked *sampled* come from reading a sample of test
bodies, not from a full count. Timings are from one run of the core lib test binary built 2026-11-07 on the dev Mac.

## Measurements

| What | Value |
| ---- | ---- |
| Rust lines in `crates/` | 525k, of which 275k (52 %) are tests |
| `athenaeum-core` | 467k lines, 4 725 tests |
| Frontend | 114k lines, 144 test files (25k lines) |
| `docs/superpowers` | 235k lines (plans 188k in 132 files) |
| Commits in 60 days | 1 329; lines changed: 453k source, 203k docs |
| Core lib tests, wall time | 195 s (4 558 passed) |
| Cold build of the core test binary | 8 min 47 s |
| Rebuild after touching `stacking/run.rs` | 24 s (lower bound: no real edit) |
| `cargo check` after a touch | 10 s |

Where the test time goes (sum of per-test times, 1 919 s under parallel load):

| Cluster | Tests | Time |
| ---- | ---- | ---- |
| `stacking::run` | 228 | 383 s |
| `api::collab_live` | 247 | 289 s |
| `sharing::iroh` | 174 | 277 s |
| `api::collab` | 177 | 160 s |
| `api::collab_v3_live_e2e_tests` | 10 | 92 s |
| everything else | 3 700 | 718 s |

61 tests of 5 s or more take 34 % of the time; 371 tests of 1 s or more take 62 %.

## Verdict

The layering is sound and should stay: wrappers → `core::api` → domain modules, one generic frontend `invoke`,
generated TS types, the stacking cache design, two separate transfer pipelines, Perseus as its own crate. The drag
comes from five other things, in this order.

### 1. Dead transfer code, and the slowest tests in the suite run it

- The package-era swarm fetch has no production caller: `fetch_collection_multi` (`sharing/iroh/blobs.rs:1414`), the
  batch driver in `assign.rs` (`fetch_children_assigned` :974, `fetch_items_assigned` :1021), `SwarmFetchMode`,
  `role_fetch_multi*` in `node.rs`. Only tests and `examples/swarm_bench.rs` call it. About 850 production lines.
- 19 tests in `sharing/iroh/tests.rs` enter through that path. They take 221 s of the file's 243 s, 11 % of all test
  time, including the three slowest tests of the suite (64 s, 42 s, 32 s). They also cover hedging and `run_child`,
  which the live collab path shares, so the hedging cases must move to the `run_live` entry before the rest is deleted.
- The old "project announce" is wired through personal sync (`announce_project` / `request_project` on the transport
  trait, an engine branch at `sync/engine.rs:1521`) and never fires: nothing in production builds a `ProjectStamp`.
  About 300–400 lines. The frozen wire variants stay.
- `IrohTransport` (about 800 lines) and `LoopbackTransport` (901 lines) have no production constructor but compile
  into production. Put them behind a test-support feature.
- `docs/transfers/README.md:21` and `:30` describe the dead paths as live.

### 2. Logic duplicated by hand in the two hosts

- About 60 % of the wrappers are thin (8–14 lines). The rest is not: 24 Tauri functions over 40 lines hold 2.3k lines,
  39 web functions hold 3.5k. The same logic sits in both hosts: `plate_solve_batch` (355 / 353 lines),
  `split_frame_set` (239 / 206, raw SQL), `export_to_wbpp` (234 / 243), `load_wbpp_config`, `create_frame_set_inner`,
  `find_new_frames_for_set`, `auto_merge_new_frames_for_set`, `autofind_objects_from_coordinates`,
  `recheck_missing_files`, archive start / restore / delete, `set_setting` side effects. Roughly 1.7k lines per host.
- The copies have already diverged:
  - web `check_missing_files_in_scan_root` (`routes/missing_files.rs:86`) documents SSE progress and emits none; the
    Tauri side delegates to core and does;
  - `plate_solve_batch` catches a panic per frame on Tauri and per batch on web;
  - `split_frame_set` logs on one side only.
- Nothing checks that the two command lists match (334 Tauri names, 338 routes, 4 web-only on purpose). A 50-line
  test that parses `generate_handler!` and `.route("/api/…")` would.
- Up to 16 registered commands have no caller in `src/` by name (for example `get_files_by_directory`,
  `get_export_preview`, `set_collab_policy`, `get_calibration_route`). Confirm each before deleting.
- Not worth doing: a generic `/invoke/:command` route, a proc-macro for both hosts, typed TS command signatures.

### 3. Giant files with inline tests

- `stacking/run.rs`: 38.7k lines (16.4k production, 22k tests), touched by 181 commits in 60 days. Split it into
  `run/` per stage, moves only, tests moving with their stage. This helps navigation and merge collisions, not
  compile time.
- `api/collab.rs`: 16.6k lines; `run_publish` is one async function of 2 196 lines (:5167). `api/sync.rs`: 12.5k.
  Split by section when next touched.
- Artifact kinds are bare strings: 57 database call sites and 39 hand-written `artifact_group_key` wrappers, about 93
  kind literals, two lists kept by hand. An `ArtifactKind` enum whose database layer applies the frame / group key
  rule removes the class of wrong-key mistakes without a migration.
- `RunContext` has 43 fields, eight of them parallel maps keyed by group. Fold into one per-group struct when the
  file is split.
- 27 % of stacking production lines are comments; 1 283 of them carry plan / task / fix-round tags that the README
  already records.

### 4. Tests: few are useless, many are expensive

- *Sampled*, stacking / api / db / integration / scanner: 344 tests read, 79 % useful, 7 % delete-grade (assert
  nothing, pin a constant to itself, grep logs or source), 11 % merge-grade (families and giant fixtures).
- 66 % of the test region of `run.rs` is setup; assertions are 18 %. `seed_star_group` is copied with one varied line
  at least five times; `test_context(` is hand-built 77 times; 28 tests exceed 150 lines.
- About 256 transfer tests start a real iroh endpoint, about 142 a local HTTP server, about 290 wait on the wall
  clock. The polling helper is written seven times. The thread ceiling (4 096 threads in one test binary) comes from
  the same fact: everything runs in one process.
- `collab/fake_hub.rs` is a second hub implementation (about 3 650 lines plus 26 self-tests), kept in step with the
  real hub by commit-hash comments only. No test runs it against the real hub. Two more hub doubles exist (wiremock
  in `api/collab.rs`, a third style in `hub_client.rs`).
- Frontend: 53 test files carry the identical `api` mock, 22 write their own command router, 155 asserts are on class
  names. `ProjectDetail.test.tsx` (2 166 lines, 102 cases) re-tests tabs that have their own test files.
- A stage default change forces about 48 golden hash literals to be re-recorded (13 in `plan.rs`, 35 in
  `config.rs`). The pins are deliberate; pins of burnt versions are not needed.

### 5. Process weight

- `docs/superpowers` is 235k lines and took 203k changed lines in 60 days, against 453k for all source.
- Compile time is not the main cost: a rebuild after an edit is tens of seconds. The full local run is 195 s, of
  which the dead-path tests are the largest single removable part.

## What was done after the review (2026-11-10)

Finding 1 is closed (merges `c6e7f301` and `1244f575`):

- The batch swarm fetch, the project announce sender, the sender's project id and the engine's batch-only
  optionality are removed. The loopback and bare iroh transports sit behind the `test-support` feature.
- The engine's hedging, stall and failover tests moved to the live entry (`run_live`) before the batch driver went.
- The forecast above was wrong about time. The moved tests take as long as the old ones, because their time is the
  production back-off ladder and the slow provider's rate, not the entry point: the core lib run went from 195 s to
  about 180 s, within noise. The gain is in code (about 3.5k lines), not in test time.
- Moving the tests exposed a product defect: a live run's hedge budget could not pay for a hedge with few items in
  flight. It is fixed (a floor on the cap; outstanding charges and a debt of duplicated bytes instead of tokens) and
  described in `docs/transfers/README.md`, "Live hedge budget". A real slow-provider run on three devices is owed.
- Still open in the fetch engine: a new item that lands on an already slow peer cannot be hedged until its size is
  known (first 16 KiB), and the provider pick prefers the least-loaded peer, which a slow peer is.

The thread ceiling, measured on the same day:

- The core lib test process reaches about 3 900 threads 73 s into the run, during `api::collab*`, and stays between
  3 100 and 4 090 until the end. The limit is 4 096 (`kern.num_taskthreads`).
- `zip_store` runs last and fails with `EAGAIN` on thread spawn: 5 of 5 full runs on the cleanup branches, 6 to 8
  tests each; the same tests pass alone. Without the 27 `sharing::iroh::tests::live_*` tests the peak is 4 087 and
  the run passes.
- So the threads are accumulated by earlier tests and not released. Finding what holds them is the next piece of
  work in this review's test section; it decides whether the local gate can be trusted at all.

## Candidate list: delete

| Where | What | Why |
| ---- | ---- | ---- |
| `stacking/config.rs` :2035 :2071 :2120 :2169 :2284 | hash pins of measure versions four to seven; `measure_version_is_eight` | burnt versions; constant equals literal |
| `stacking/config.rs` :1721 :1305 :1673 | `presets`, enum names, the 48-line serde needle list | second copy of the code |
| `stacking/run.rs` :24717 :24728 :16836 :25606 | two-line mapping tests, serde round trip, log-field grep | copies; log text |
| `stacking/telemetry.rs` :561 :585 :602 :644; `measure.rs` :4132 :4021 | log field names and order (one sleeps 1.1 s) | change detectors |
| `stacking/test_fixtures.rs` :919 :991 | fixture self-tests | tests of helpers |
| `db/schema.rs` :5985 :6006 :6714 :6733 :6671; `db/stacking.rs` :1363 | "table exists" after `init_db` | re-assert the DDL |
| `db/stacking.rs` :2068 :2097 :2109 | `EXPLAIN QUERY PLAN` text | depends on the SQLite version; owner's call |
| `api/compute.rs` :151 :192; `api/mod.rs` :248 | defaults and `Display` | constants |
| `sync/engine_tests.rs` :303 :5133; `sync/store.rs` :3419 :4179 | one column through a full engine; no-op; empty store | duplicates, no assertion |
| `sync/receiver.rs` :5927 | `include_str!` source grep with a magic count | change detector |
| `sync/receiver.rs` :7912 :7935 :8298; `sync/status.rs` :720 | constant and enum-to-label tables | copies |
| `collab/fake_hub.rs` :3834–4829 | 26 self-tests of the double | tests of a test double |
| `collab/gate.rs` :1161; `api/sync.rs` :9962 | duplicates of :1082 and :9947 | duplicates |
| `src/test/smoke.test.ts` | `1 + 1` | asserts nothing |
| `src/indexCss.test.ts`, `utils/filterColors.test.ts` | CSS source grep; 12 copied hex values | copies |
| `GridCard`, `Checkbox`, `ConfirmDialog`, `LabeledToolbar` tests | class-name and pixel asserts | change detectors |
| `ProjectDetail.test.tsx` :1403 :506 :1299 | asserts a removed call is absent; duplicates | removed behaviour; duplicates |

## Candidate list: merge into table tests or shared runs

| Where | Family | Size |
| ---- | ---- | ---- |
| `stacking/run.rs` 26580–29256 | `local_normalization_*` (10+ full runs); fallback-wording family | 13 + 6 tests, about 1 470 lines |
| `stacking/run.rs` :18073 :18963 :20092 vs :27069 | regroup reuse at stage and umbrella level | keep one layer |
| `stacking/run.rs` :32861 :33033 :33113 | three full XISF runs | one run |
| `stacking/plan.rs` | `scale_warning_*` ×7, `register_stale_*` ×7 | about 1 570 lines |
| `api/stacking_aligned.rs` 469–768 | 12 tests, 16 real runs | 21 s |
| `db/operations.rs` | `*_carries_cfa` ×6, `*_nests_inside_outer_transaction` ×8, `upper_bound_*` ×7 | one helper each |
| `scanner/mod.rs` 2926–3395 | six serial / parallel twins, each with a 1.1 s sleep | use `File::set_modified` (already at :4572) |
| serde-name tests in seven files | `serde_names_*` | one table |
| `api/scan_roots.rs`, `api/masters.rs` | `library_dir_*` ×8, positive-path `is_ok` controls ×7 | tables |
| `sharing/iroh/proto.rs` 384–476 | seven postcard round trips with one body | one macro table |
| resend / cancel / revoke cycle | tested at four layers (e2e, receiver, api, engine) | keep e2e plus one unit layer |
| `tests/sync_e2e.rs` | 22 tests repeat a 100-line rig | one `Rig` struct, about 2k lines |
| Frontend | 53 identical `api` mocks, 22 command routers | one shared helper |

## Leave alone

- Golden, wire and numeric pins: `config_hash_is_canonical_and_pinned`, `stage_hashes_are_pinned_to_golden_literals`,
  the measure "exactly these numbers" tests, bit-for-bit oracles, `ts_contract`, wire golden tests.
- `sync_e2e` crash and restart scenarios; `collab/scheduler/core.rs` state-machine tests; hub request-shape pins.
- Security negatives (`mkdir_rejects_*`, `path_policy_*`); `#[ignore]` real-data tests.
- The stacking stage pipeline, the cache key design, admission sizing, the shared integration engine, the collab live
  runtime structure, `SyncStore` with two implementations.

## Not verified

- Timings come from one run on a loaded machine; per-test times include contention.
- Sample rates are not population rates; the transfer and frontend samples were aimed at suspicious families.
- "No caller" was checked by name, not through macros or callers outside this repo.
- Only `plate_solve_batch` and `split_frame_set` were diffed between hosts; the other duplicated pairs were measured.
- Line counts for dead code are from item boundaries, ±10 %.
