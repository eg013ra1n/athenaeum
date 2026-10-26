# HiPS Offline Pack (phase 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this repo the `lean-execution` skill overrides subagent-driven-development where they conflict.

**Goal:** Let a user download the Sky Chart's survey tiles ahead of time: the whole sky at a chosen order plus full detail around their imaging locations and selected catalog objects, so the chart works in the field without internet.

**Architecture:** Core gets a pack module (`hips/pack.rs`) with two parts. The first turns a selection into a deduplicated tile list: whole-sky orders 0..=k, plus HEALPix cone coverage at orders k+1..=6 around each region. The second runs that list on a background thread with 4 workers into the existing `HipsStore`, skipping what is already stored, honouring cancel and `429`/`503` + `Retry-After`. `api/hips.rs` owns one process-wide pack slot (start / cancel / status / estimate) and emits `hips-download-progress`. Both hosts get four thin commands. The Settings → General → Sky background section gets the selection, estimate, Download/Resume/Cancel and progress. The chart gets only a link to that section.

**Tech Stack:** Rust (`cdshealpix` 0.9.1 `nested::cone_coverage_approx`, `reqwest::blocking`, `std::thread`), Tauri 2 + Axum, React/TS + vitest.

**Spec:** GitHub eg013ra1n/athenaeum#18, the issue body (phase 2 mechanics) and the comment "Phase 2: offline pack, revised design (owner ruling 2026-10-26)", which overrides the body on what a pack covers. Phase 1 reference: `docs/skychart/README.md` (HiPS section).

## Global Constraints

- Whole-sky order selectable **0–5, default 4**. Whole-sky order 6 is never a pack.
- Detail order is **6** (`hips::DISPLAY_MAX_ORDER`). Detail regions cover orders **k+1..=6**, with k = the selected whole-sky order. The renderer falls back to *ancestors only* (`hipsLayer.ts:246`), so a missing order between k and 6 would show the coarse sky.
- Detail groups (checkboxes): **imaging locations** (default on), **Messier** (default on, 108 objects), **NGC/IC brighter than 10ᵐ** (default off, 491 objects in `resources/dsos.json`).
- Radii: an imaging location uses half the diagonal of its `fov_width × fov_height` **+ 1°**, or **2°** when it has no FOV. A catalog object uses **3°**.
- **4** parallel requests. User-Agent is the existing `Athenaeum/<version> (+https://artfrom.space)`.
- `429` / `503`: wait `Retry-After` seconds (capped at **300 s**), else exponential backoff **5 s → 300 s**. A tile that is still busy after **6** waits counts as failed. **20 consecutive** network errors (not 404, not busy) stop the run as `failed`. **4 consecutive** busy-failed tiles (a fetched tile or a 404 resets the count) also stop it as `failed` ("survey busy"); a `Retry-After` shorter than the 5 s backoff start waits the backoff start.
- One pack at a time per process. A second start → `ApiError::Conflict`.
- An explicit pack works with `skychart.hips_online` off. The toggle stays the on-demand switch only.
- Progress event name **`hips-download-progress`**, at most **4 per second**, plus a final one on every terminal state.
- Size per tile = `hips_estsize` (KB) × 1024 / `hips_nb_tiles` from the stored `properties`. Without stored properties, use the preset's `approx_tile_bytes` = **67 000** for DSS colored. A start whose missing-tile estimate exceeds free disk space (`disk::free_bytes`) − **512 MiB** is refused with `ApiError::Conflict`.
- Never redistribute tiles; fetch from the survey master only (`HipsSurvey::base_url`). The pack never uses `OperationQueue`.
- Logging: `tracing` only, message = short stable phrase, snake_case fields. New fields go into the logging field dictionary.
- Settings page contract: no Save button, no banner, `notify()` on failure only, StrictMode-safe listener pattern (CLAUDE.md).
- Design tokens only, no raw colours. No `@tauri-apps/*` outside `src/api/`.
- No AI attribution in any GitHub text; commits carry the harness trailers.

## Review Focus

1. **App quit or crash mid-pack, then restart.** The user expects Resume to continue the same selection. `pack.json` keeps the last selection, and status after restart reports it as `interrupted`. The test lives in Task 3.
2. **Laptop goes offline mid-pack** (Wi-Fi drops in the field). The user expects a clear stop, not 16 000 failed tiles: 20 consecutive network errors stop the run as `failed`, with the finished tiles kept. Test in Task 3.
3. **CDS throttles (`429` with `Retry-After`).** The user expects the pack to slow down and finish, not to fail. Test in Task 1 (parsing) and Task 3 (waits, then succeeds).
4. **Clear pressed while a pack runs.** The user expects no half-written survey: `clear_hips_cache` returns `Conflict` while the slot is running. Test in Task 4.
5. **No imaging locations and both catalog boxes off.** The pack is whole sky only and the estimate says so; it does not error. Test in Task 2.

---

### Task 1: Busy-aware tile fetch

**Files:**
- Modify: `crates/athenaeum-core/src/hips/mod.rs` (`get_bounded` :327, `fetch_tile_from` :361, tests module)

**Interfaces:**
- Produces: `pub struct ServerBusy { pub retry_after: Option<std::time::Duration> }`, which implements `std::error::Error + Display`. `get_bounded` returns `Err(anyhow::Error::new(ServerBusy{..}))` for HTTP 429 and 503. Callers detect it with `e.downcast_ref::<ServerBusy>()`.
- Produces: `pub fn fetch_tile_from(base_url: &str, format: TileFormat, order: u8, npix: u64) -> Result<FetchedTile>`, made `pub(crate)` so the pack runner can point it at a test server.
- Produces: `pub approx_tile_bytes: u64` on `HipsSurvey`, `67_000` for `DSS_COLOR`.

- [ ] **Step 1: Write failing tests** in the existing `tests` module. Use its local `TcpListener` fixture style.
  - `busy_429_carries_retry_after`: the server answers `HTTP/1.1 429 Too Many Requests` with `Retry-After: 7`. Then `fetch_tile_from(..)` is `Err` and `downcast_ref::<ServerBusy>().unwrap().retry_after == Some(Duration::from_secs(7))`.
  - `busy_503_without_header`: a `503` without a header gives `retry_after == None`.
  - `retry_after_http_date_is_ignored`: `Retry-After: Wed, 21 Oct 2026 07:28:00 GMT` gives `None` (only delta-seconds are honoured).
  - `other_5xx_is_not_busy`: `500` is `Err` with `downcast_ref::<ServerBusy>().is_none()`.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib hips::tests`. Expected: the 4 new tests FAIL.
- [ ] **Step 3: Implement.** In `get_bounded`, before the `!status.is_success()` bail, map 429/503 to `ServerBusy`, parsing `Retry-After` as `u64` seconds. Add `approx_tile_bytes` to the preset and fill it for `DSS_COLOR`.
- [ ] **Step 4: Run** the same command. Expected: all `hips::tests` PASS.
- [ ] **Step 5: Commit** with `feat(hips): a busy survey (429/503) is a typed error with its Retry-After`.

### Task 2: Pack selection → tile list

**Files:**
- Create: `crates/athenaeum-core/src/hips/pack.rs` (declare `pub mod pack;` in `hips/mod.rs`)
- Modify: `crates/athenaeum-core/src/plate_solve/dso_lookup.rs` (add `pub fn entries(&self) -> &[Dso]`)

**Interfaces:**
- Produces (all `#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]`, `#[serde(rename_all = "camelCase")]`):
  - `pub struct PackSelection { pub survey_id: String, pub sky_order: u8, pub around_locations: bool, pub around_messier: bool, pub around_ngc_ic: bool }`. `impl Default`: `"dss-color"`, 4, true, true, false.
  - `pub struct PackRegion { pub ra_deg: f64, pub dec_deg: f64, pub radius_deg: f64 }`
- Produces: `pub fn location_regions(locs: &[ImagingLocation]) -> Vec<PackRegion>`, using the radius rule from Global Constraints. Every location (frame set or cluster) the chart draws is included.
- Produces: `pub fn catalog_regions(cat: &DsoCatalog, messier: bool, ngc_ic: bool) -> Vec<PackRegion>`. Messier means the designation starts with `"M "`. NGC/IC means the first word is `NGC` or `IC` and `magnitude <= Some(10.0)`. Radius 3°.
- Produces: `pub fn pack_tiles(sky_order: u8, regions: &[PackRegion]) -> Vec<(u8, u64)>`. It returns, sorted ascending by (order, npix) and deduplicated: every tile of orders `0..=sky_order`, then for each region `cdshealpix::nested::cone_coverage_approx(o, ra.to_radians(), dec.to_radians(), radius.to_radians()).to_flat_array()` for `o in sky_order+1..=6`.
- Errors: `sky_order > 5` → `bail!("whole-sky pack order must be 0–5")`. This becomes `ApiError::Invalid` in Task 4.

- [ ] **Step 1: Write failing tests** in `pack.rs`:
  - `whole_sky_counts`: `pack_tiles(3, &[]).len() == 1020` and `pack_tiles(4, &[]).len() == 4092`.
  - `whole_sky_order_6_refused`: a separate `fn checked_sky_order(o: u8) -> Result<u8>` (used by Task 4) returns `Err` for 6.
  - `region_adds_orders_above_sky_only`: for M 31 (10.6847°, 41.2690°, r = 3°) at `sky_order = 4`, every added tile has order 5 or 6. The count of order-6 tiles lies in 30..=90 (a 3° cap ≈ 28 deg², order-6 tile ≈ 0.84 deg², plus the approx coverage's edge).
  - `overlapping_regions_dedup`: two identical regions give the same list as one.
  - `no_regions_is_whole_sky_only` (Review Focus 5): `pack_tiles(4, &location_regions(&[])) == pack_tiles(4, &[])`.
  - `location_radius_from_fov`: a location with fov 2.0 × 1.5 gives radius `1.25 + 1.0`; without fov gives `2.0`.
  - `catalog_groups_counts`: on `DsoCatalog::load()`, `catalog_regions(&cat, true, false).len() == 108` and `catalog_regions(&cat, false, true).len() == 491`.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib hips::pack`. Expected: compile FAIL (module missing).
- [ ] **Step 3: Implement** `pack.rs` (selection types, region builders, `pack_tiles`, `checked_sky_order`) and `DsoCatalog::entries`.
- [ ] **Step 4: Run** the same command. Expected: PASS.
- [ ] **Step 5: Commit** with `feat(hips): a pack is the whole sky to order k plus order k+1..6 around chosen regions`.

### Task 3: Pack runner

**Files:**
- Modify: `crates/athenaeum-core/src/hips/pack.rs`
- Modify: `crates/athenaeum-core/src/hips/mod.rs` (`HipsStore`: add `pub fn has(&self, order, npix) -> bool`, which is true for a stored image or an `.absent` marker without reading bytes; add `pub fn read_pack_state / write_pack_state` for `<store>/pack.json` via `write_atomic`)

**Interfaces:**
- Consumes: Task 1 `fetch_tile_from`, `ServerBusy`. Task 2 `PackSelection`.
- Produces:
  - `pub enum PackState { Idle, Running, Cancelling, Done, Cancelled, Failed, Interrupted }`, serialised lowercase.
  - `pub struct PackProgress { pub state: PackState, pub selection: Option<PackSelection>, pub total: u64, pub present: u64, pub fetched: u64, pub absent: u64, pub failed: u64, pub bytes_fetched: u64, pub waiting_s: Option<u32>, pub error: Option<String> }`. `present` counts tiles already on disk at the start. Done when `present + fetched + absent + failed == total`.
  - `pub struct PackRunConfig { pub workers: usize /*4*/, pub busy_cap: Duration /*300 s*/, pub backoff_start: Duration /*5 s*/, pub max_busy_waits: u32 /*6*/, pub max_consecutive_errors: u32 /*20*/ }`, with `Default` set to those values. Tests pass millisecond durations.
  - `pub fn run_pack(store: &HipsStore, base_url: &str, format: TileFormat, tiles: &[(u8, u64)], initial: PackProgress, cfg: &PackRunConfig, cancel: &AtomicBool, on_progress: &(dyn Fn(&PackProgress) + Sync)) -> PackProgress`. It is synchronous; the caller owns the thread. `initial` carries the selection and `total` (state `Running`). It writes `pack.json` = `{ selection, state }` at start (`running`) and at the end (terminal state).
  - `pub fn interrupted_status(store: &HipsStore) -> Option<PackProgress>`. If `pack.json` says `running` and no run is live, this returns `state = Interrupted` with the stored selection (Review Focus 1).
- Behaviour: workers take indices from a shared `AtomicUsize`. Tiles with `store.has` true are counted `present` up front, without network. A busy tile sleeps (in steps of ≤ 200 ms, checking `cancel`) and retries the same tile. `on_progress` is throttled to ≤ 4/s plus one final call. Cancel finishes in-flight requests, writes nothing partial (atomic writes already), and ends `Cancelled`.

- [ ] **Step 1: Write failing tests** against a local HTTP fixture that serves a scripted response per path (extend the existing `TcpListener` helper into a small multi-request server in the test module):
  - `fetches_missing_and_skips_present`: 10 tiles, 3 pre-stored → `present == 3`, `fetched == 7`, every file exists.
  - `records_404_as_absent`.
  - `cancel_keeps_finished_tiles`: cancel after the 3rd response (the server's handler sets the flag) → state `Cancelled`, `fetched >= 3`, a rerun gives `present >= 3`.
  - `busy_then_ok` (Review Focus 3): the first response is `429` + `Retry-After: 0`, then 200 → the tile is fetched, `failed == 0`.
  - `busy_forever_fails_tile_after_max_waits`: always `503` → that tile is `failed == 1`, the run is `Done`.
  - `offline_stops_after_consecutive_errors` (Review Focus 2): the server is closed (connection refused) → state `Failed` after `max_consecutive_errors` errors, `error` names the last error.
  - `interrupted_after_restart` (Review Focus 1): write `pack.json` with state `running` → `interrupted_status` gives `Interrupted` and the same selection.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib hips::pack`. Expected: the new tests FAIL.
- [ ] **Step 3: Implement** `run_pack`, `interrupted_status`, and the store helpers. Log once per run: `info!("hips pack started", survey_id, tiles_total)`, and on the end `info!("hips pack finished", survey_id, pack_state, tiles_fetched, tiles_absent, tiles_failed, bytes)`. A busy wait is `debug!("hips survey busy", survey_id, retry_after_s)`. A failed tile is `warn!` once per tile.
- [ ] **Step 4: Run** the same command. Expected: PASS. Run it 5× (`for i in 1 2 3 4 5; do …; done`) to catch timing flakes.
- [ ] **Step 5: Commit** with `feat(hips): pack runner — 4 workers, resume, cancel, polite to a busy survey`.

### Task 4: Pack API (estimate / start / cancel / status)

**Files:**
- Modify: `crates/athenaeum-core/src/api/hips.rs`
- Modify: `crates/athenaeum-core/src/ts_export.rs` (register `PackSelection`, `PackProgress`, `PackState`, `HipsPackEstimate`)
- Regenerate: `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`, then commit the generated TS

**Interfaces:**
- Consumes: Tasks 2–3; `crate::api::spatial::get_imaging_locations(&conn)`; `api::objects::load_catalog` (make it `pub(crate)`); `disk::free_bytes`.
- Produces:
  - `pub struct HipsPackEstimate { pub total: u64, pub present: u64, pub missing: u64, pub bytes_per_tile: u64, pub estimated_bytes: u64, pub free_bytes: Option<u64>, pub fits: bool }`, where `estimated_bytes = missing × bytes_per_tile`. `fits = free_bytes.map_or(true, |f| estimated_bytes + 512 MiB <= f)`.
  - `pub fn estimate_hips_pack(ctx: &ServiceContext, selection: &PackSelection) -> Result<HipsPackEstimate, ApiError>`. It makes no network access, ever.
  - `pub fn start_hips_pack(ctx: Arc<ServiceContext>, emitter: Arc<dyn ProgressEmitter>, selection: PackSelection) -> Result<PackProgress, ApiError>`. It validates the survey and `checked_sky_order`, refuses when a run is live (`Conflict`) or `!fits` (`Conflict`, message names both sizes), and fetches and validates `properties` when they are missing (ignoring `skychart.hips_online`, since an explicit pack is allowed). It then spawns a named `std::thread` (`hips-pack`) that runs `run_pack` and emits `hips-download-progress` on every progress, and returns the initial `Running` progress at once.
  - `pub fn cancel_hips_pack() -> Result<(), ApiError>`. With no live run it returns `NotFound`.
  - `pub fn get_hips_pack_status(ctx: &ServiceContext, survey_id: &str) -> Result<PackProgress, ApiError>`. It returns the live run's latest progress, else `interrupted_status`, else the last terminal state from `pack.json`, else `Idle`.
  - The pack slot is a process-wide `static PACK: OnceLock<Mutex<Option<LivePack>>>` in `api/hips.rs`, next to the existing `validated()`, with `LivePack { survey_id, cancel: Arc<AtomicBool>, latest: Arc<Mutex<PackProgress>> }`. **Deviation from #18 ("handle in `ServiceContext`")**: `ServiceContext` has 26 struct-literal construction sites (mostly tests). One pack per process is the rule anyway, and the module already keeps per-process state the same way.
  - `clear_hips_cache` returns `ApiError::Conflict("a sky pack download is running")` while the slot holds a live run for that survey (Review Focus 4).
- Ordering inside `start`: the slot is claimed under the mutex **before** spawning, and the thread clears it on exit, so two racing starts cannot both run.

- [ ] **Step 1: Write failing tests** in `api/hips.rs` `#[cfg(test)]`, on `ServiceContext::new_for_tests` with a temp DB. They share the global slot and core has no `serial_test` dev-dep (do not add one), so the slot-touching cases (`second_start_conflicts`, `clear_conflicts_while_running`) run in sequence inside one test function, `pack_slot_rules`, which empties the slot at the end.
  - `estimate_is_offline_and_counts_present`: pre-store 5 order-0 tiles, `sky_order = 0`, no regions → `total 12, present 5, missing 7, bytes_per_tile 67_000`.
  - `estimate_uses_properties_size`: stored `properties` with `hips_estsize = 100` and `hips_nb_tiles = 10` → `bytes_per_tile == 10_240`.
  - `start_refuses_order_6` → `Invalid`.
  - `second_start_conflicts` and `clear_conflicts_while_running`: put a fake `LivePack` into the slot directly through a `#[cfg(test)]` helper, without a network run.
  - `status_idle_without_pack_json`.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib api::hips`. Expected: FAIL.
- [ ] **Step 3: Implement**, then regenerate the TS contract.
- [ ] **Step 4: Run** `cargo test -p athenaeum-core --lib api::hips && cargo test -p athenaeum-core --test ts_contract`. Expected: PASS.
- [ ] **Step 5: Commit** with `feat(hips): pack estimate, start, cancel and status in the command layer`.

### Task 5: Commands on both hosts

**Files:**
- Modify: `crates/athenaeum-tauri/src/commands/hips.rs`, `crates/athenaeum-tauri/src/lib.rs` (`invoke_handler!`, after `clear_hips_cache` at :346)
- Modify: `crates/athenaeum-web/src/routes/hips.rs`, `crates/athenaeum-web/src/routes/mod.rs` (after :200)
- Modify: `docs/architecture/commands.md` (313 → **317**, list the four names), `CLAUDE.md:81` (313 → 317)

**Interfaces:**
- Consumes: Task 4.
- Produces four commands, all `#[tracing::instrument(skip_all, err)]` (Axum `err(Debug)`):
  - `estimate_hips_pack { selection: PackSelection } -> HipsPackEstimate`, under `spawn_blocking`.
  - `start_hips_pack { selection: PackSelection } -> PackProgress`. Tauri: `Arc::new(TauriProgressEmitter(app_handle))` (as `start_stacking`). Axum: `Arc::new(SseProgressEmitter::new(state.event_tx.clone()))`. Under `spawn_blocking`, because `start` may fetch `properties`.
  - `cancel_hips_pack {} -> ()`.
  - `get_hips_pack_status { surveyId: String } -> PackProgress`.
- [ ] **Step 1: Implement** the wrappers, mirroring `get_hips_surveys` / `start_stacking` exactly.
- [ ] **Step 2: Run** `cargo check --workspace && cargo test -p athenaeum-web`. Expected: both pass. Also run the strict recount `grep -rE '^\s*#\[tauri::command(\]|\()' crates/athenaeum-tauri/src/commands/*.rs | wc -l`. Expected: `317`.
- [ ] **Step 3: Commit** with `feat(hips): pack commands on the desktop and web hosts`.

### Task 6: Settings section — pack controls

**Files:**
- Create: `src/hooks/useHipsPack.ts`, `src/hooks/useHipsPack.test.ts`
- Modify: `src/components/settings/sections/SkyBackgroundSection.tsx`, `src/settings/registry.ts` (`general.skyBackground` gets field `pack`)

**Interfaces:**
- Consumes: Task 5 commands, the generated `PackSelection` / `PackProgress` / `HipsPackEstimate` types.
- Produces: `useHipsPack(surveyId: string): { selection, setSelection, estimate: HipsPackEstimate | null, progress: PackProgress | null, start(): Promise<void>, cancel(): Promise<void> }`.
  - On mount it loads `get_hips_pack_status`. If the status carries a selection (interrupted, cancelled or failed), that becomes the initial selection; otherwise `PackSelection` defaults.
  - On each selection change it calls `estimate_hips_pack`, debounced 250 ms.
  - It listens to `hips-download-progress` with the CLAUDE.md StrictMode-safe pattern, filtered to `surveyId`.
  - On a terminal state it reloads the estimate, and calls the section's `load()` (cache size) through an `onFinished` callback argument.
- UI, per survey row, under the cache line:
  - an order select 0–5 labelled with pixel size, e.g. "4 — 26″/px";
  - three checkboxes: "Detail around my imaging locations", "Detail around Messier objects", "Detail around NGC/IC brighter than 10ᵐ";
  - one line: "{missing} tiles to download, about {size}; {free} free" (uses `formatBytes`), with `text-error` and Download disabled when `!fits`;
  - buttons: **Download** (idle/done), **Resume** (interrupted/cancelled/failed), **Cancel** (running);
  - a progress bar `(present+fetched+absent+failed)/total` with "{fetched} downloaded · {failed} failed", plus "Survey busy, waiting {waiting_s} s" when set.
- Notifications: `notify()` on start failure, and on a `failed` terminal state (title "Sky pack stopped", detail = `error`). None on success or progress.
- Registry field `pack`: label "Offline pack", help "Download the sky ahead of time for use without internet. Tiles stay on this computer.", keywords `['offline', 'download', 'pack', 'hips', 'messier', 'ngc', 'field']`.
- [ ] **Step 1: Write failing vitest** `useHipsPack.test.ts`, mocking `api`:
  - `restores selection from an interrupted status`;
  - `re-estimates once per debounced change`;
  - `ignores progress for another survey`;
  - `listener is registered once under StrictMode double mount` (render in `<StrictMode>`, assert one active listener after unmount/remount).
- [ ] **Step 2: Run** `npx vitest run src/hooks/useHipsPack.test.ts`. Expected: FAIL.
- [ ] **Step 3: Implement** the hook and the section UI.
- [ ] **Step 4: Run** `npx vitest run && npx tsc --noEmit`. Expected: all pass.
- [ ] **Step 5: Commit** with `feat(skychart): offline pack controls in Settings → Sky background`.

### Task 7: Chart link to the pack

**Files:**
- Modify: `src/pages/SkyChart.tsx` (credit/status strip, ≈ :472–490)

- [ ] **Step 1: Implement.** In the status `<span>`, when `sky.survey` is set, add a `pointer-events-auto` text button "Download for offline…" that runs `navigate('/settings?tab=general&section=general.skyBackground')`. It sits after the status text, separated by ` · ` when a status is shown. Styling: `text-accent hover:underline`.
- [ ] **Step 2: Verify** with `npx tsc --noEmit`. Then, in `npm run tauri dev` on the dev DB, click the link: Settings opens on General, scrolls to Sky background and flashes it (`useSectionDeepLink`).
- [ ] **Step 3: Commit** with `feat(skychart): a "Download for offline" link from the chart to the pack settings`.

### Task 8: Docs and acceptance

**Files:**
- Modify: `docs/skychart/README.md` (HiPS section: phase 2, the selection rules and numbers from Global Constraints, the process-wide slot and why, `pack.json`)
- Modify: `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` field dictionary: `tiles_total`, `tiles_fetched`, `tiles_absent`, `tiles_failed`, `pack_state`, `retry_after_s`
- Modify: `docs/superpowers/open-items.md` (owed smokes below, if any are not run)

- [ ] **Step 1: Gates.** Run `cargo test -p athenaeum-core` (all targets), `cargo test -p athenaeum-web`, `cargo check --workspace`, `cargo check -p athenaeum-core --no-default-features`, `npx tsc --noEmit`, `npx vitest run`. Expected: all green.
- [ ] **Step 2: Real-data acceptance (desktop, dev DB, live CDS).** This is one real pack at **order 3, Messier on, locations on, NGC/IC off**. It is the only bulk fetch allowed, and it is done by hand, never in tests.
  - The estimate before start shows a tile count and a size, and the size matches the result within 30 %.
  - Cancel at about 30 %, quit the app, relaunch. Settings shows **Resume** with the same selection, and Resume finishes with `failed == 0`.
  - Turn online off and disconnect the network. The chart shows the whole sky. Zoomed to M 31 and to one of your frame sets, the detail is sharp (order 6) and no request leaves the machine (no `hips tile fetched` debug line in the log).
  - Web build (`athenaeum-web` with `ATHENAEUM_API_KEY`): the pack starts and its progress streams over SSE.
- [ ] **Step 3: Docs.** Write the README and log-dictionary entries. Delete the memory `project_hips_phase2_pack_design.md` and its `MEMORY.md` line, since the ruling now lives in the README.
- [ ] **Step 4: Commit** with `docs(skychart): offline pack — selection rules, runner, acceptance`.
