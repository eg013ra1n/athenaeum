# Perseus Send-Workflow (Phase 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace Perseus's per-file immediate send with a batcher (pending accumulator + manual/auto flush) building one multi-frame package per batch, plus a web "To sync" tree and batched history.

**Architecture:** A new `perseus::batcher` sits between the watcher's stable-file stream and the existing multi-target `enqueue_package_to_all`, replacing `spawn_enqueue_consumer`. The pending set is the batcher's in-memory accumulator (watcher-fed, reconstructed on restart via the durable `perseus_seen` filter — no persisted queue). A flush (auto quiet-timer / manual button) drains the accumulator into ONE package via a generalized `build_batch_package`, fans it to all targets, records each file in `perseus_seen`, and writes a lightweight `perseus_batch` row. The web page gains a To-sync tree + batched history; `athenaeum-core` is untouched.

**Tech Stack:** Rust (`perseus` crate only), tokio (mpsc + watch + time), rusqlite (`perseus.db` WAL), `toml_edit` (config write-back), Axum (the embedded web page), hand-written HTML/JS.

## Global Constraints

- **Spec:** `docs/superpowers/specs/2026-07-12-perseus-send-workflow-design.md`.
- **`perseus` crate + its web page only.** No `athenaeum-core` change. Reuse the existing `enqueue_package_to_all`, `SharedPackageCleanup`, Plan-3 dedup, `SeenStore`, `write_package`, `compute_rel_path`.
- **Pending is DERIVED, never a persisted queue table** — the accumulator is rebuilt from the watcher's startup scan filtered by `SeenStore::should_enqueue`. Never silently drop an unsent file (the `perseus_seen` invariant).
- **A batch is ONE multi-frame package** (many `ManifestRecord`s in one `write_package`), fanned to all targets by `enqueue_package_to_all`, with `coord.register(&pkg_dir, delivered)` exactly as the current per-file path does.
- **Manual = flush the WHOLE pending set** (no subset selection). **Auto = quiet-period debounce only** (`auto_quiet_secs`, no periodic cap, never per-file).
- **`record_seen` only after a successful enqueue** (`first_id.is_some()`), per file, keyed to the batch's `pkg_dir` — identical to the current path, so a fully-failed enqueue leaves files pending.
- **Empty pending → no-op**, never build/enqueue an empty package.
- **Web endpoints behind the existing bearer auth**; config edits via `toml_edit` + validate + atomic rename (mirror `apply_retention_edit`/`apply_capture_dirs_edit`). Config changes live-apply via a `watch` channel (like retention), no engine restart.
- **Author every commit as `eg013ra1n <vilen.sharifov@gmail.com>`; no Claude footer.** GitLab only. Branch `0.4.0`.
- **Gates per task:** `cargo test -p perseus` green + warning-free; `cargo build --workspace` clean. Frontend (index.html) task: manual-render note (no JS test runner).

---

### Task 1: Config — `Mode::Manual` + `auto_quiet_secs` + live send-config

Add the manual send mode and the quiet-period to the config, plus a `toml_edit` editor and a `watch` channel so the web page can change them live.

**Files:**
- Modify: `crates/perseus/src/config.rs` (`enum Mode` gains `Manual`; add `auto_quiet_secs`; a `SendCfg` snapshot)
- Modify: `crates/perseus/src/config_template.toml` (document `mode`/`auto_quiet_secs`)
- Modify: `crates/perseus/src/config_edit.rs` (`apply_send_mode_edit`)
- Test: `crates/perseus/src/config.rs` + `crates/perseus/src/config_edit.rs` (inline)

**Interfaces:**
- Consumes: existing `Config`, `Mode` (currently `enum Mode { Auto }`, deserialized from `mode = "auto"`; it IS the send-behaviour mode — verify by grepping `Mode::` usage before extending).
- Produces:
  ```rust
  // config.rs
  pub enum Mode { Auto, Manual }                     // Manual added; serde rename_all = "lowercase"
  // in Config:
  #[serde(default = "default_auto_quiet_secs")] pub auto_quiet_secs: u64  // default 60
  fn default_auto_quiet_secs() -> u64 { 60 }
  #[derive(Clone, Copy, PartialEq, Eq)]
  pub struct SendCfg { pub mode: Mode, pub auto_quiet_secs: u64 }
  impl Config { pub fn send_cfg(&self) -> SendCfg { SendCfg { mode: self.mode, auto_quiet_secs: self.auto_quiet_secs } } }
  // config_edit.rs
  pub fn apply_send_mode_edit(config_path: &Path, mode: Mode, auto_quiet_secs: u64) -> Result<Config>;
  ```

- [ ] **Step 1: Write the failing test**

```rust
// config_edit.rs tests
#[test]
fn apply_send_mode_edit_roundtrips() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_min_config(dir.path()); // reuse the existing min-config test helper
    let cfg = apply_send_mode_edit(&path, Mode::Manual, 30).unwrap();
    assert_eq!(cfg.mode, Mode::Manual);
    assert_eq!(cfg.auto_quiet_secs, 30);
    let reloaded = Config::load_lenient(&path).unwrap();
    assert_eq!(reloaded.mode, Mode::Manual);
    assert_eq!(reloaded.auto_quiet_secs, 30);
}
```

- [ ] **Step 2: Run, confirm fail** — `cargo test -p perseus --lib apply_send_mode_edit_roundtrips` → FAIL (`Mode::Manual`/`auto_quiet_secs`/`apply_send_mode_edit` absent).

- [ ] **Step 3: Implement** — add `Manual` to `Mode` (keep its `serde(rename_all = "lowercase")` / `FromStr`); add `auto_quiet_secs` with the serde default; `SendCfg` + `Config::send_cfg`; `apply_send_mode_edit` (mirror `apply_retention_edit`: load, `toml_edit` set `mode` + `auto_quiet_secs`, re-parse + `validate`, atomic `tmp`+rename). Document both keys in `config_template.toml`.

- [ ] **Step 4: Run** — the test + `cargo test -p perseus` green, warning-free.

- [ ] **Step 5: Commit**

```bash
git add crates/perseus/src/config.rs crates/perseus/src/config_template.toml crates/perseus/src/config_edit.rs
git commit -m "feat(perseus): send Mode::Manual + auto_quiet_secs config + live-edit"
```

---

### Task 2: `perseus_batch` store

The lightweight batch record that groups history and distinguishes auto vs manual.

**Files:**
- Create: `crates/perseus/src/batch_store.rs`
- Modify: `crates/perseus/src/lib.rs` (`mod batch_store;`)
- Test: `crates/perseus/src/batch_store.rs` (inline)

**Interfaces:**
- Produces:
  ```rust
  pub struct BatchStore { conn: std::sync::Mutex<rusqlite::Connection> }
  #[derive(Debug, Clone, PartialEq)]
  pub struct BatchRow { pub package_ref: String, pub mode: String, pub created_at: String, pub file_count: i64 }
  impl BatchStore {
      pub fn open(path: impl AsRef<Path>) -> Result<Self>;           // CREATE TABLE IF NOT EXISTS, WAL
      pub fn record(&self, package_ref: &str, mode: &str, created_at: &str, file_count: usize) -> Result<()>;
      pub fn list(&self) -> Result<Vec<BatchRow>>;                    // newest-first
  }
  ```
  DDL:
  ```sql
  CREATE TABLE IF NOT EXISTS perseus_batch (
      package_ref TEXT PRIMARY KEY,
      mode        TEXT NOT NULL,
      created_at  TEXT NOT NULL,
      file_count  INTEGER NOT NULL
  )
  ```

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn record_and_list_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let s = BatchStore::open(dir.path().join("perseus.db")).unwrap();
        s.record("pkg-a", "auto", "2026-07-12T01:00:00Z", 3).unwrap();
        s.record("pkg-b", "manual", "2026-07-12T02:00:00Z", 5).unwrap();
        let rows = s.list().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].package_ref, "pkg-b"); // newest first
        assert_eq!(rows[0].mode, "manual");
        assert_eq!(rows[1].file_count, 3);
        // idempotent upsert on the same package_ref
        s.record("pkg-b", "manual", "2026-07-12T02:00:00Z", 5).unwrap();
        assert_eq!(s.list().unwrap().len(), 2);
    }
}
```

- [ ] **Step 2: Run, confirm fail** — `cargo test -p perseus --lib batch_store::` → FAIL.

- [ ] **Step 3: Implement** — `open` (WAL pragma like `SeenStore::open`, run DDL), `record` (`INSERT ... ON CONFLICT(package_ref) DO UPDATE`), `list` (`ORDER BY created_at DESC, package_ref DESC`). `mod batch_store;` in `lib.rs`.

- [ ] **Step 4: Run** — the test + `cargo test -p perseus` green, warning-free.

- [ ] **Step 5: Commit**

```bash
git add crates/perseus/src/batch_store.rs crates/perseus/src/lib.rs
git commit -m "feat(perseus): perseus_batch store — send-batch records for grouped history"
```

---

### Task 3: `build_batch_package` — one package from N files

Generalize the single-file `build_package_for_file` into a multi-file builder.

**Files:**
- Modify: `crates/perseus/src/run.rs` (add `build_batch_package`; `build_package_for_file` may delegate to it or stay for `enqueue_file`)
- Test: `crates/perseus/src/run.rs` (inline)

**Interfaces:**
- Consumes: `parse_frame`, `compute_rel_path`, `package::{xxh3_full_file, write_package, ManifestRecord, MANIFEST_VERSION, PayloadKind}`, `config.packages_dir()`.
- Produces:
  ```rust
  /// Build ONE package containing a record per (capture_dir, file). A file that
  /// vanished or fails to parse/hash is dropped with a `warn!` (never fatal).
  /// Returns (pkg_dir, included_count). Empty input OR all-dropped → Err.
  pub fn build_batch_package(
      config: &Config,
      files: &[(std::path::PathBuf /*capture_dir*/, std::path::PathBuf /*file*/)],
      origin_device: &str,
  ) -> Result<(std::path::PathBuf, usize)>;
  ```

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn build_batch_package_bundles_all_present_files() {
    // helper: write 3 real FITS-ish capture files under a temp capture_dir
    let (config, cap, files) = three_capture_files(); // (Config, capture_dir, Vec<PathBuf>)
    let input: Vec<_> = files.iter().map(|f| (cap.clone(), f.clone())).collect();
    let (pkg_dir, n) = build_batch_package(&config, &input, &"aa".repeat(32)).unwrap();
    assert_eq!(n, 3);
    let recs = athenaeum_core::package::read_manifest(&pkg_dir).unwrap();
    assert_eq!(recs.len(), 3); // one manifest, 3 records
}
#[test]
fn build_batch_package_drops_vanished_file() {
    let (config, cap, files) = three_capture_files();
    std::fs::remove_file(&files[1]).unwrap(); // one gone before build
    let input: Vec<_> = files.iter().map(|f| (cap.clone(), f.clone())).collect();
    let (pkg_dir, n) = build_batch_package(&config, &input, &"aa".repeat(32)).unwrap();
    assert_eq!(n, 2); // the survivor count
    assert_eq!(athenaeum_core::package::read_manifest(&pkg_dir).unwrap().len(), 2);
}
```
(Reuse whatever `run.rs`/`watcher.rs` tests already use to fabricate a parseable capture file; if none, mirror `build_package_for_file`'s existing test fixture.)

- [ ] **Step 2: Run, confirm fail** — `cargo test -p perseus --lib build_batch_package_bundles_all_present_files build_batch_package_drops_vanished_file` → FAIL.

- [ ] **Step 3: Implement** — for each `(capture_dir, file)`: build a `ManifestRecord` exactly as `build_package_for_file` does (`parse_frame`, minted `frame_uuid`, `compute_rel_path(config, capture_dir, file)`, `byte_size`, `xxh3_full_file`), collecting `(file.to_path_buf(), record)`; a per-file error → `warn!` + skip. If the collected vec is empty → `Err`. `let pkg_dir = config.packages_dir().join(Uuid::new_v4().to_string());` then `write_package(&pkg_dir, records)`; return `(pkg_dir, records.len())`. Keep `build_package_for_file` working for `Agent::enqueue_file` (it can call `build_batch_package(config, &[(capture_dir, file)], origin_device)` and unwrap the single count, or stay as-is).

- [ ] **Step 4: Run** — both tests + `cargo test -p perseus` green, warning-free.

- [ ] **Step 5: Commit**

```bash
git add crates/perseus/src/run.rs
git commit -m "feat(perseus): build_batch_package — one multi-frame package from N capture files"
```

---

### Task 4: The batcher (accumulator + auto/manual flush) — replaces `spawn_enqueue_consumer`

The core: accumulate the watcher's stable files, flush on the quiet-timer (auto) or a manual signal, build one batch, fan out, record.

**Files:**
- Create: `crates/perseus/src/batcher.rs`
- Modify: `crates/perseus/src/lib.rs` (`mod batcher;`)
- Modify: `crates/perseus/src/run.rs` (`Agent::start_with_transport` spawns the batcher instead of `spawn_enqueue_consumer`; `Agent` holds a `BatcherHandle`; remove `spawn_enqueue_consumer`)
- Test: `crates/perseus/src/batcher.rs` (inline, tokio time + a fake enqueue sink)

**Interfaces:**
- Consumes: `build_batch_package` (Task 3), `enqueue_package_to_all` (`run.rs`, `(Vec<Arc<SyncEngineHandle>>, &Path) -> (Option<i64>, usize)`), `record_seen` (`run.rs`), `BatchStore` (Task 2), `SendCfg`/`Mode` (Task 1), `SharedPackageCleanup`.
- Produces:
  ```rust
  #[derive(Clone)]
  pub struct BatcherHandle {
      pending: std::sync::Arc<std::sync::Mutex<std::collections::BTreeSet<(std::path::PathBuf, std::path::PathBuf)>>>,
      flush_tx: tokio::sync::mpsc::Sender<()>,
  }
  impl BatcherHandle {
      pub fn pending_snapshot(&self) -> Vec<(std::path::PathBuf, std::path::PathBuf)>;
      pub async fn flush_now(&self);   // manual "Send N pending"; no-op if empty (batcher guards)
  }
  /// Drives accumulate + auto-quiet-flush + manual-flush. Returns the handle + the loop task.
  pub fn spawn_batcher(
      stable_rx: tokio::sync::mpsc::Receiver<(std::path::PathBuf, std::path::PathBuf)>,
      engines: Vec<std::sync::Arc<athenaeum_core::sync::SyncEngineHandle>>,
      seen: std::sync::Arc<crate::seen::SeenStore>,
      batches: std::sync::Arc<crate::batch_store::BatchStore>,
      config: crate::config::Config,
      origin_device: String,
      cleanup: Option<std::sync::Arc<athenaeum_core::sync::SharedPackageCleanup>>,
      send_cfg_rx: tokio::sync::watch::Receiver<crate::config::SendCfg>,
  ) -> (BatcherHandle, tokio::task::JoinHandle<()>);
  ```

- [ ] **Step 1: Write the failing tests** (use `#[tokio::test(start_paused = true)]` so the quiet-timer is deterministic; inject engines via a loopback/test `SyncEngineHandle` or refactor the flush to call a small `flush_once(&pending, &deps, mode)` unit you can test without a real engine — prefer the latter: make `flush_once` build+enqueue+record and return a `FlushOutcome { package_ref, file_count }`, and test it against a temp `SeenStore`+`BatchStore` with a loopback engine).

```rust
#[tokio::test(start_paused = true)]
async fn auto_flushes_after_quiet_period_not_before() {
    let h = /* spawn_batcher with SendCfg{ Auto, auto_quiet_secs: 60 }, loopback engine */;
    feed_stable(&h, &[f1, f2]).await;                 // two files arrive
    tokio::time::advance(Duration::from_secs(59)).await;
    assert_eq!(batches.list().unwrap().len(), 0, "no flush before quiet elapses");
    tokio::time::advance(Duration::from_secs(2)).await; // cross 60s of quiet
    wait_until(|| batches.list().unwrap().len() == 1).await;
    let b = &batches.list().unwrap()[0];
    assert_eq!(b.file_count, 2);
    assert_eq!(b.mode, "auto");
    assert!(h.pending_snapshot().is_empty(), "pending drained");
}
#[tokio::test(start_paused = true)]
async fn a_new_file_resets_the_quiet_timer() { /* file at t0, another at t50 → flush at ~t110 not t60 */ }
#[tokio::test]
async fn manual_flush_sends_whole_pending_and_empty_is_noop() {
    // SendCfg{ Manual, .. }: no auto flush; flush_now() sends all pending as one "manual" batch;
    // a second flush_now() with empty pending records no new batch.
}
```

- [ ] **Step 2: Run, confirm fail** — the three tests → FAIL (batcher absent).

- [ ] **Step 3: Implement** — `spawn_batcher` runs a `tokio::select!` loop:
  - `Some((cap, path)) = stable_rx.recv()` → `pending.lock().insert((cap, path))`; if `mode == Auto`, arm/reset a quiet deadline `now + auto_quiet_secs`.
  - quiet timer elapses (auto, pending non-empty) → `flush(Mode::Auto)`.
  - `Some(()) = flush_rx.recv()` (manual signal) → `flush(Mode::Manual)`.
  - `send_cfg_rx.changed()` → update local `mode`/`auto_quiet_secs` (live-apply); switching to Manual disarms the timer, to Auto re-arms if pending.
  - `flush(mode)`: drain `pending` into a `Vec`; if empty → return; `build_batch_package(&config, &files, &origin_device)` → `(pkg_dir, n)`; `enqueue_package_to_all(&engines, &pkg_dir)` → `(first_id, delivered)`; `if let Some(c) = &cleanup { c.register(&pkg_dir, delivered); }`; on `Some(_)` → `for (_, f) in &files { record_seen(&seen, f, &pkg_dir.to_string_lossy()); }` + `batches.record(&pkg_dir.to_string_lossy(), mode.as_str(), &now_rfc3339(), n)`; on `None` (zero delivered) → `error!` and **re-insert the files into `pending`** so they retry next flush (they were never `record_seen`'d). A `build_batch_package` `Err` (all vanished) → drop, `warn!`.
  - Wire it: `Agent::start_with_transport` builds the `BatchStore` + the `watch::channel(config.send_cfg())`, calls `spawn_batcher`, stores the `BatcherHandle` on `Agent`; delete `spawn_enqueue_consumer`. The `send_cfg` watch sender is held where the web layer can update it (Task 6) — thread it out like the retention `watch` sender.

- [ ] **Step 4: Run** — the three tests + `cargo test -p perseus` + `cargo build --workspace` green, warning-free.

- [ ] **Step 5: Commit**

```bash
git add crates/perseus/src/batcher.rs crates/perseus/src/lib.rs crates/perseus/src/run.rs
git commit -m "feat(perseus): batcher — accumulate + auto-quiet/manual flush, one package per batch"
```

---

### Task 5: Pending tree derivation

Group the accumulator snapshot into a tree by `rel_path` for the web view.

**Files:**
- Create: `crates/perseus/src/pending.rs`
- Modify: `crates/perseus/src/lib.rs` (`mod pending;`)
- Test: `crates/perseus/src/pending.rs` (inline)

**Interfaces:**
- Consumes: the accumulator snapshot `Vec<(PathBuf /*capture_dir*/, PathBuf /*file*/)>`, `compute_rel_path` (`run.rs`), `Config`.
- Produces:
  ```rust
  #[derive(Debug, Clone, serde::Serialize, PartialEq)]
  #[serde(rename_all = "camelCase")]
  pub struct PendingNode { pub name: String, pub count: usize,
      pub children: Vec<PendingNode>, pub files: Vec<String> } // files = leaf rel_paths
  /// Group each pending file by its compute_rel_path segments (object / date / type / file).
  pub fn pending_tree(snapshot: &[(std::path::PathBuf, std::path::PathBuf)], config: &crate::config::Config) -> PendingNode;
  ```

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn groups_files_by_rel_path_segments() {
    let cap = PathBuf::from("/data/astro");
    let snap = vec![
        (cap.clone(), cap.join("M31/2026-07-12/lights/L_0001.fits")),
        (cap.clone(), cap.join("M31/2026-07-12/lights/L_0002.fits")),
        (cap.clone(), cap.join("M31/2026-07-12/flats/F_0001.fits")),
    ];
    let cfg = single_root_config("/data/astro");
    let root = pending_tree(&snap, &cfg);
    assert_eq!(root.count, 3);
    let m31 = root.children.iter().find(|n| n.name == "M31").unwrap();
    assert_eq!(m31.count, 3);
    let date = &m31.children[0]; // 2026-07-12
    let lights = date.children.iter().find(|n| n.name == "lights").unwrap();
    assert_eq!(lights.count, 2);
    assert_eq!(lights.files.len(), 2);
}
```

- [ ] **Step 2: Run, confirm fail** — `cargo test -p perseus --lib groups_files_by_rel_path_segments` → FAIL.

- [ ] **Step 3: Implement** — for each snapshot entry, `rel = compute_rel_path(config, capture_dir, file)`; split `rel` on `/`; insert into a trie accumulating `count`; leaf segment goes into the parent node's `files`. Sort children by `name`. Root `name = ""`, `count = total`.

- [ ] **Step 4: Run** — the test + `cargo test -p perseus` green, warning-free.

- [ ] **Step 5: Commit**

```bash
git add crates/perseus/src/pending.rs crates/perseus/src/lib.rs
git commit -m "feat(perseus): pending_tree — group the accumulator by rel_path for the web view"
```

---

### Task 6: Web endpoints — pending / send-mode / send-now / batches

Wire the batcher + batch store into `WebState` and expose the four endpoints.

**Files:**
- Modify: `crates/perseus/src/web.rs` (`WebState` gains `batcher: RwLock<Option<BatcherHandle>>`, `batches: Arc<BatchStore>`, `send_cfg_tx`; four handlers; `build_router` routes)
- Modify: `crates/perseus/src/supervisor.rs` (the `attach`/`on_agent` seam threads the `BatcherHandle` + `send_cfg` sender into `WebState`, like the cleanup/engine wiring)
- Test: `crates/perseus/src/web.rs` (inline, mirroring the existing capture-dirs/targets handler tests)

**Interfaces:**
- Consumes: `BatcherHandle` (Task 4: `pending_snapshot`, `flush_now`), `pending_tree` (Task 5), `BatchStore::list` (Task 2), `apply_send_mode_edit` + `SendCfg` (Task 1), engines' `status_snapshot()` for per-target/batch status.
- Produces (all behind the existing `api` bearer layer):
  ```
  GET  /api/pending      -> { tree: PendingNode, mode: "auto"|"manual", autoQuietSecs, count }
  GET  /api/send-mode    -> { mode, autoQuietSecs }
  PUT  /api/send-mode    -> body { mode, autoQuietSecs } ; apply_send_mode_edit + send_cfg_tx.send(...) ; returns the applied config
  POST /api/send-now     -> flush_now() ; returns { flushed: count } (0 = nothing pending)
  GET  /api/batches      -> [ { packageRef, mode, createdAt, fileCount, new, duplicate, targets:[{name, state}], outcome } ]  (BatchStore::list joined with engine history/{new,duplicate})
  ```

- [ ] **Step 1: Write the failing test**

```rust
// web.rs tests — mirror the existing capture-dirs handler test harness (a WebState with a temp config + stores).
#[tokio::test]
async fn put_send_mode_applies_and_get_reflects() {
    let state = test_state_with_config(/* mode=auto */);
    let applied = api_put_send_mode(State(state.clone()), Json(SendModeEdit{ mode: "manual".into(), auto_quiet_secs: 45 })).await;
    // assert 200 + the on-disk config now parses mode=manual, auto_quiet_secs=45
    let got = api_get_send_mode(State(state.clone())).await; // reflects manual/45
}
#[tokio::test]
async fn send_now_with_empty_pending_is_noop() {
    let state = test_state_with_batcher(/* empty accumulator */);
    let r = api_send_now(State(state)).await; // { flushed: 0 }, no batch row
}
```

- [ ] **Step 2: Run, confirm fail** — the two tests → FAIL (handlers/routes absent).

- [ ] **Step 3: Implement** —
  - `WebState`: add `batcher: RwLock<Option<BatcherHandle>>` (set on attach, cleared on detach — mirror `engine`/`cleanup`), `batches: Arc<BatchStore>`, `send_cfg_tx: watch::Sender<SendCfg>` (shared with the batcher).
  - `api_get_pending`: `batcher.read()` snapshot → `pending_tree` → JSON with the current `mode`/`auto_quiet_secs` (from `state.config`).
  - `api_get_send_mode`/`api_put_send_mode`: read/write via `apply_send_mode_edit`, then `send_cfg_tx.send(cfg.send_cfg())` (live-apply) + swap `state.config`; return the applied values; a 409/400 on an invalid mode string.
  - `api_send_now`: `batcher.read().flush_now().await`; return `{ flushed: <pending count at flush> }` (capture the count before/at flush — have `flush_now` return the flushed count, or read the snapshot len just before).
  - `api_batches`: `batches.list()` joined with the engine's per-package history/status + `{new,duplicate}` (from the sync-finished data the page already tracks for `/api/history`) + per-target states from `status_snapshot()`.
  - Register all four in `build_router` inside the auth group; thread `BatcherHandle` + `send_cfg` sender through the supervisor `attach` seam.

- [ ] **Step 4: Run** — the two tests + `cargo test -p perseus` + `cargo build --workspace` green, warning-free.

- [ ] **Step 5: Commit**

```bash
git add crates/perseus/src/web.rs crates/perseus/src/supervisor.rs
git commit -m "feat(perseus): web endpoints — pending tree, send-mode, send-now, batched history"
```

---

### Task 7: Web UI — "To sync" tree + mode toggle + batched history

The `index.html` section that renders the pending tree, the manual/auto toggle + send button, and the grouped history.

**Files:**
- Modify: `crates/perseus/src/web/index.html` (new "To sync" `<section>` + JS; batched-history section)
- Test: none (no JS runner) — gate is a manual-render checklist in the report.

**Interfaces:**
- Consumes: `GET /api/pending`, `GET/PUT /api/send-mode`, `POST /api/send-now`, `GET /api/batches` (Task 6). Reuse the existing `api()`/`getJson()`/`esc`/`tick()` helpers and the capture-dirs section as the styling template.

- [ ] **Step 1: Confirm the current page renders** — load the page (owner smoke), note the existing sections as the pattern.

- [ ] **Step 2: Implement** —
  - **"To sync"** `<section>`: a mode toggle (Auto / Manual radio → `PUT /api/send-mode`), an `auto_quiet_secs` input (shown for Auto), a **"Send N pending"** button (enabled in Manual when count>0 → `POST /api/send-now`), and a **tree** render of `GET /api/pending`'s `PendingNode` (collapsible nodes with counts; leaf files). Poll on the existing `tick()` (2s).
  - **Batched history**: render `GET /api/batches`; **auto** batches collapsed under a **calendar-day header** (`createdAt` date), **manual** batches listed individually; each row shows `mode`, `fileCount`, `new N / duplicate M`, outcome; expand → per-target `{name, state}`.
  - Design: reuse the page's existing CSS/markup idioms (the capture-dirs + history sections); `esc()` all interpolated strings.

- [ ] **Step 3: Manual render check** (report checklist): Auto/Manual toggle persists (reload), the tree matches the pending files, "Send N pending" flushes and the batch appears in history, auto batches group by day, per-target expand works, an empty pending disables the send button.

- [ ] **Step 4: Gate** — `cargo build --workspace` clean (the HTML is embedded; a build proves it's wired). `cargo test -p perseus` unaffected.

- [ ] **Step 5: Commit**

```bash
git add crates/perseus/src/web/index.html
git commit -m "feat(perseus): web UI — To-sync tree, manual/auto toggle, batched history"
```

---

## Self-Review

**Spec coverage:** §2.1 derived pending → Tasks 4 (accumulator) + 5 (tree); §2.2 aggregate + per-target → Task 6 (`/api/batches` per-target) + 7 (drill-down); §2.3 manual whole-set → Task 4 (`flush(Manual)`) + 6/7 (send-now); §2.4 auto quiet-period → Task 4 (timer); §2.5 batch=one package → Task 3 + 4; §2.6 live mode → Task 1 (`SendCfg` watch) + 6 (`send_cfg_tx`). §6 `perseus_batch` → Task 2. §7 UI → Tasks 6+7. §8 edge cases: restart→pending reconstruct (Task 4 relies on the watcher startup scan + `should_enqueue`), target-offline (`enqueue_package_to_all` best-effort, unchanged), deleted-before-flush (Task 3 drop + Task 4 re-insert-on-zero-delivered), empty no-op (Task 4). §9 tests → each task.

**Placeholder scan:** test helpers point at existing harnesses (`config_edit` min-config, `run.rs` capture-file fixture, `web.rs` handler-test state, `SeenStore`/retention WAL open); the batcher test explicitly uses `start_paused` + a `flush_once` seam rather than a vague "test the timer". No TODOs.

**Type consistency:** `SendCfg { mode: Mode, auto_quiet_secs: u64 }` (Task 1) flows into `spawn_batcher`'s `watch::Receiver<SendCfg>` (Task 4) and `send_cfg_tx` (Task 6). `BatcherHandle { pending_snapshot, flush_now }` (Task 4) is consumed by `pending_tree` (Task 5) + the web handlers (Task 6). `BatchStore::{record,list}` + `BatchRow` (Task 2) used by Task 4 (record) and Task 6 (list). `build_batch_package -> (PathBuf, usize)` (Task 3) called only by Task 4. `enqueue_package_to_all`/`record_seen`/`SharedPackageCleanup` are the unchanged core seam.

**Ordering:** 1 (config) → 2 (batch store) → 3 (build_batch_package) → 4 (batcher, uses 1+2+3) → 5 (pending tree) → 6 (web, uses 2+4+5) → 7 (UI, uses 6). Each ends green on `perseus`; `cargo build --workspace` stays green (perseus-only, core untouched).

---

## Execution Handoff

Execute with **superpowers:subagent-driven-development** (owner's standing choice): fresh rust-engineer + opus per Rust task, frontend-dev + opus for Task 7, opus reviewer after each, broad review at the end. Ledger: `.superpowers/sdd/progress-p2.md`. Perseus-only; no deploy (rides the same held joint-hub ship as Phase 1, but adds no hub/core dependency).
