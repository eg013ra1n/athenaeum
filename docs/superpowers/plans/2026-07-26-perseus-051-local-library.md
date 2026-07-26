# Perseus 0.5.1 — Local Library Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Perseus becomes local-library-oriented: a one-panel browser over the capture roots with send/delete/preview, a fire-at-time send scheduler, free-space display, send-batch-to-another-node, and retention transparency.

**Architecture:** Everything builds on existing Perseus stores (`perseus_seen`, `perseus_batch(+_files)`, batcher pending set, outbound rows) — the Library's file status is a derived join, never a second catalog. One new KV table (`perseus_meta`), one new index. All new APIs address files as `(root_index, rel_path-with-forward-slashes)`; one containment-guard implementation serves listing, send, delete, and preview. Spec: `docs/superpowers/specs/2026-07-26-perseus-051-local-library-design.md`.

**Tech Stack:** Rust (axum 0.8, tokio, rusqlite, notify), rustafits (direct dep, JPEG via turbojpeg), vanilla JS web page (`include_str!`, no npm), `windows-sys` (free space), `libc` statvfs.

## Global Constraints

- Branch: `0.5.1`. Subagents: rust-engineer for Rust tasks, frontend-dev for UI tasks, opus models (owner rule).
- Wire rel-paths are ALWAYS forward-slash; absolute paths never travel in library APIs (spec §7).
- Exactly ONE containment-guard implementation (`library.rs::resolve_in_root`) — every library route uses it.
- No new DB tables except `perseus_meta` (KV); one new index `idx_perseus_batch_files_source` (spec §9).
- Cargo feature `preview` is in default features AND stays enabled in the headless deb variant (spec §4).
- Zero `println!`/`eprintln!` in production code — `tracing` only (repo rule; CLI `main.rs` exempt).
- Web page stays vanilla JS / `include_str!` — no npm, Nord tokens via existing `style.css` variables.
- Empty batches never exist: any flush path with zero files is a no-op (existing batcher invariant, preserved by §3/§1a).
- Messages/log style: short stable phrase + snake_case fields (`info!(root = %dir.display(), "…")`).
- Gates per task: `cargo build -p perseus` (feature-relevant: `--no-default-features` must also build), `cargo test -p perseus`; final task runs workspace build + full test suite.

---

### Task 1: `library.rs` — path contract + containment guard

**Files:**
- Create: `crates/perseus/src/library.rs` (module `library` registered in `crates/perseus/src/lib.rs`)
- Modify: `crates/perseus/src/lib.rs` (add `pub mod library;`)
- Test: inline `#[cfg(test)]` in `library.rs`

**Interfaces:**
- Produces: `pub fn split_rel(rel: &str) -> anyhow::Result<Vec<String>>` — validates + splits a wire rel-path.
- Produces: `pub fn resolve_in_root(root: &Path, rel: &str) -> anyhow::Result<PathBuf>` — the ONE guard: split, join with native separators, canonicalize-and-prefix-check. Errors are `anyhow` with stable message prefixes (`"invalid path segment"`, `"path escapes root"`, `"not found"`).
- Produces: `pub fn to_wire_rel(root: &Path, abs: &Path) -> Option<String>` — inverse for building listing/status payloads (forward-slash join of components after the root prefix).

- [ ] **Step 1: Write the failing tests**

```rust
// crates/perseus/src/library.rs  (bottom, #[cfg(test)] mod tests)
#[test]
fn split_rel_accepts_plain_segments() {
    assert_eq!(split_rel("M31/2026-07-01/light_0001.fits").unwrap(),
               vec!["M31", "2026-07-01", "light_0001.fits"]);
    assert_eq!(split_rel("").unwrap(), Vec::<String>::new()); // root itself
}

#[test]
fn split_rel_rejects_hostile_segments() {
    for bad in ["..", "a/../b", ".", "a/./b", "a//b", "a\\b", "C:/x", "a\0b", "/abs"] {
        assert!(split_rel(bad).is_err(), "must reject {bad:?}");
    }
}

#[test]
fn resolve_in_root_stays_inside() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("cap"); std::fs::create_dir_all(root.join("M31")).unwrap();
    std::fs::write(root.join("M31/a.fits"), b"x").unwrap();
    let p = resolve_in_root(&root, "M31/a.fits").unwrap();
    assert!(p.ends_with("a.fits"));
    assert!(resolve_in_root(&root, "../outside").is_err());
}

#[cfg(unix)]
#[test]
fn resolve_in_root_rejects_symlink_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("cap"); std::fs::create_dir_all(&root).unwrap();
    let outside = tmp.path().join("secret"); std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("f.fits"), b"x").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
    assert!(resolve_in_root(&root, "link/f.fits").is_err(), "canonical prefix check must catch the escape");
}

#[test]
fn to_wire_rel_roundtrips_with_forward_slashes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("cap"); std::fs::create_dir_all(root.join("M31/sub")).unwrap();
    std::fs::write(root.join("M31/sub/a.fits"), b"x").unwrap();
    let abs = resolve_in_root(&root, "M31/sub/a.fits").unwrap();
    assert_eq!(to_wire_rel(&root, &abs).unwrap(), "M31/sub/a.fits");
}
```

- [ ] **Step 2: Run tests, verify they fail** — `cargo test -p perseus library::` → FAIL (module missing).

- [ ] **Step 3: Implement**

```rust
//! Library path contract (spec §7): wire rel-paths are forward-slash; the
//! containment guard canonicalizes BOTH sides and prefix-compares canonical
//! vs canonical (Windows: `\\?\C:\…` / `\\?\UNC\…` on both sides — consistent).
use std::path::{Path, PathBuf};
use anyhow::{bail, Context, Result};

pub fn split_rel(rel: &str) -> Result<Vec<String>> {
    if rel.is_empty() { return Ok(Vec::new()); }
    if rel.starts_with('/') { bail!("invalid path segment: absolute"); }
    let mut out = Vec::new();
    for seg in rel.split('/') {
        if seg.is_empty() || seg == "." || seg == ".."
            || seg.contains('\\') || seg.contains(':') || seg.contains('\0') {
            bail!("invalid path segment: {seg:?}");
        }
        out.push(seg.to_string());
    }
    Ok(out)
}

pub fn resolve_in_root(root: &Path, rel: &str) -> Result<PathBuf> {
    let segs = split_rel(rel)?;
    let mut joined = root.to_path_buf();
    for s in &segs { joined.push(s); }
    let canon_root = std::fs::canonicalize(root)
        .with_context(|| format!("canonicalize root {}", root.display()))?;
    let canon = std::fs::canonicalize(&joined)
        .with_context(|| format!("not found: {}", joined.display()))?;
    if !canon.starts_with(&canon_root) { bail!("path escapes root: {rel:?}"); }
    Ok(canon)
}

pub fn to_wire_rel(root: &Path, abs: &Path) -> Option<String> {
    let root = std::fs::canonicalize(root).ok()?;
    let abs = std::fs::canonicalize(abs).ok()?;
    let rel = abs.strip_prefix(&root).ok()?;
    let parts: Vec<_> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    Some(parts.join("/"))
}
```

- [ ] **Step 4: Run tests** → PASS. Also `cargo build -p perseus --no-default-features` → OK.
- [ ] **Step 5: Commit** — `feat(perseus): library path contract + containment guard (0.5.1 T1)`

---

### Task 2: `diskspace.rs` + `volumes` on status + UI chips

**Files:**
- Create: `crates/perseus/src/diskspace.rs`
- Modify: `crates/perseus/src/lib.rs` (register), `crates/perseus/Cargo.toml` (windows-sys), `crates/perseus/src/web.rs` (`StatusDto` + `api_status`), `crates/perseus/src/web/app.js` + `index.html` + `style.css` (chips)
- Test: inline in `diskspace.rs` + existing status route test extended

**Interfaces:**
- Produces: `pub struct VolumeInfo { pub root: PathBuf, pub free_bytes: u64, pub total_bytes: u64 }` (serde camelCase for the DTO copy in web.rs).
- Produces: `pub fn probe_volumes(paths: &[PathBuf]) -> Vec<VolumeInfo>` — one entry per unique volume; probe failures skip the entry with `warn!` (offline SMB root must not error the status page).

- [ ] **Step 1: Failing test**

```rust
#[test]
fn probe_dedupes_same_volume_and_survives_missing_path() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a"); let b = tmp.path().join("b");
    std::fs::create_dir_all(&a).unwrap(); std::fs::create_dir_all(&b).unwrap();
    let vols = probe_volumes(&[a.clone(), b, PathBuf::from("/definitely/not/mounted/xyz")]);
    assert_eq!(vols.len(), 1, "same filesystem → one volume; missing path skipped");
    assert!(vols[0].total_bytes > 0 && vols[0].free_bytes <= vols[0].total_bytes);
}
```

- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement.** Unix: `libc::statvfs` (`f_frsize * f_bavail` free, `* f_blocks` total), volume identity = `MetadataExt::dev()` (same pattern as file_op's move planner). Windows: `GetDiskFreeSpaceExW` via `windows-sys = { version = "0.59", features = ["Win32_Storage_FileSystem", "Win32_Foundation"] }` under `[target.'cfg(windows)'.dependencies]`; volume identity = the canonicalized volume prefix (`\\?\C:` / `\\?\UNC\server\share`). Accepts UNC roots. Probe the deepest existing ancestor when the exact path is momentarily gone (mounted-parent case); if none exists, skip with `warn!(root = %p.display(), "free-space probe skipped")`.
- [ ] **Step 4: Wire into `api_status`:** probe `config.capture_dirs_resolved()` + `config.data_dir`, add `volumes: Vec<VolumeDto>` (`{root, freeBytes, totalBytes}`) to `StatusDto`. Extend the existing status route test to assert `volumes` is non-empty and camelCase.
- [ ] **Step 5: UI.** Status header + (later) Library root rows: chip `Free: 412.3 GB` per volume, `class="chip chip-danger"` when `freeBytes < 10 * 1024**3` (fixed threshold, spec §5). Format with the existing byte formatter in `app.js`.
- [ ] **Step 6: Run `cargo test -p perseus` → PASS. Commit** — `feat(perseus): free-space probe per unique volume + status chips (0.5.1 T2)`

---

### Task 3: watcher survives a failed `watch()` — poll-only mode

**Files:**
- Modify: `crates/perseus/src/watcher.rs` (the `.watch(&capture_dir, RecursiveMode::Recursive)?` site, ~line 256)
- Test: inline in `watcher.rs`

**Interfaces:** none new — behavioral: a root whose `notify` watch cannot be established still discovers files via the existing per-tick `scan_eligible` sweep.

- [ ] **Step 1: Failing test.** The watch target is a path that exists at spawn but whose notify backend errors: simulate by passing a directory that is deleted between canonicalize and watch — instead, refactor for testability: extract the establish step into `fn try_establish_watch(watcher: &mut …, dir: &Path) -> bool` and test the spawned loop with a stub. Concretely: add a `#[cfg(test)]`-visible constructor knob `force_poll_only: bool` on the spawn fn (default false), and the test asserts a file created AFTER spawn in poll-only mode is still emitted stable within the poll cadence:

```rust
#[tokio::test]
async fn poll_only_mode_discovers_new_files() {
    let tmp = tempfile::tempdir().unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let seen = test_seen_store(&tmp);
    spawn_watcher_inner(tmp.path().to_path_buf(), tx, seen,
        Duration::from_millis(50), Duration::from_millis(50), /*force_poll_only=*/ true);
    tokio::time::sleep(Duration::from_millis(20)).await;
    std::fs::write(tmp.path().join("l1.fits"), b"data").unwrap();
    let (dir, file) = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await.expect("poll fallback must find the file").unwrap();
    assert!(file.ends_with("l1.fits")); let _ = dir;
}
```

(Adapt helper names to the file's existing test utilities — it already has stability-tracker tests with an injected clock; reuse their setup helpers.)

- [ ] **Step 2: Run → FAIL** (no `force_poll_only` knob / hard `?`).
- [ ] **Step 3: Implement.** Replace the `?` on `.watch(...)` with: on `Err(error)` → `tracing::warn!(path = %capture_dir.display(), %error, "filesystem watch unavailable — running poll-only (normal for SMB/NFS mounts)");` and skip event-channel arming; the loop's tick arm (`scan_eligible`) is unchanged and carries discovery alone. Keep the notify watcher alive only when established. Add rate-limited (once per ~60 ticks) `warn!` when `scan_eligible` itself errors (offline share), recovering silently when the mount returns.
- [ ] **Step 4: Run full watcher tests → PASS.**
- [ ] **Step 5: Commit** — `fix(perseus): a failed filesystem watch degrades to poll-only instead of killing the root (0.5.1 T3, spec §7)`

---

### Task 4: library listing API — status join

**Files:**
- Modify: `crates/perseus/src/batch_store.rs` (index + query), `crates/perseus/src/library.rs` (listing + status), `crates/perseus/src/web.rs` (route `GET /api/library`)
- Test: inline in both + route test in `web.rs`

**Interfaces:**
- Consumes: T1 `resolve_in_root`/`to_wire_rel`; `BatcherHandle::pending_snapshot()`; `SeenStore` (new query below); `BatchStore::packages_for_sources`; `StandaloneSyncStore::all_outbound`.
- Produces: `BatchStore::add_source_index()` — `CREATE INDEX IF NOT EXISTS idx_perseus_batch_files_source ON perseus_batch_files(source_path)` run in `open()`.
- Produces: `BatchStore::batches_for_source(&self, source: &str) -> Result<Vec<String>>` (package_refs containing this source).
- Produces: `SeenStore::is_recorded(&self, path: &Path) -> Result<bool>` (a live, non-deleted seen row exists).
- Produces (library.rs):

```rust
#[derive(serde::Serialize, PartialEq, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub enum FileStatus { Unsent, Queued, Sending, Delivered, Declined, Sent }

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryEntry { pub name: String, pub size: u64, pub mtime_ms: i64,
    pub status: FileStatus, pub batches: usize, pub retention: Option<String> }

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryListing { pub root: usize, pub path: String,
    pub dirs: Vec<String>, pub files: Vec<LibraryEntry> }

pub struct StatusSources<'a> { pub pending: &'a [(PathBuf, PathBuf)],
    pub batches: &'a BatchStore, pub seen: &'a SeenStore,
    pub outbound: &'a [OutboundRow] }

pub fn list_directory(root_idx: usize, root: &Path, rel: &str,
    src: &StatusSources<'_>) -> anyhow::Result<LibraryListing>;
```

Status precedence (test each): in pending set → `Queued`; else in ≥1 batch whose newest outbound row is live (`Announced|Sending|…` non-terminal) → `Sending`; newest terminal `Confirmed` → `Delivered`; newest terminal receiver-declined (reuse `resend::is_declined`) → `Declined`; else seen-recorded → `Sent`; else `Unsent`. `retention` stays `None` in this task (filled by T15).

- [ ] **Step 1: Failing tests** — one per status arm, driven through real stores in a tempdir (pattern: T4-style store fixtures already exist in `batch_store.rs`/`resend.rs` tests). Plus: listing is single-directory (a nested file does NOT appear at root level), dirs sorted, files sorted, offline root (`resolve_in_root` error) → `Err` (route maps to 502 with body `"root unavailable"`), non-FITS files listed too (browse-everything; only send/preview filter by type).
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement** `batches_for_source` (join through `perseus_batch_files.source_path = ?`), `is_recorded` (`SELECT deleted_at IS NULL …` per the store's actual column — read `seen.rs` DDL first and match), `list_directory` (one `read_dir`, per-file join).
- [ ] **Step 4: Route.** `GET /api/library?root=<usize>&path=<rel>` in `web.rs`: resolve root index against `config.capture_dirs_resolved()` (out of range → 404 `"unknown root"`), snapshot pending from the `BatcherHandle` already on `WebState` (confirm field name at implementation; the status page reads it today), `all_outbound(u32::MAX)` once per request. Route test per existing tower-util pattern: seed one file in each status, assert the JSON statuses + camelCase keys.
- [ ] **Step 5: Run `cargo test -p perseus` → PASS. Commit** — `feat(perseus): library listing API with derived per-file status (0.5.1 T4)`

---

### Task 5: Library tab UI — roots, breadcrumbs, listing, selection

**Files:**
- Modify: `crates/perseus/src/web/index.html` (tab button + `<section id="library">`), `crates/perseus/src/web/app.js`, `crates/perseus/src/web/style.css`

**Interfaces:**
- Consumes: `GET /api/library?root&path` (T4 shape), `GET /api/status` `volumes` (T2).
- Produces (used by T7/T10/T11 UI): `libState = { root: null|number, path: "", selected: Set<string /* rel */>, entries: [] }`; `function libLoad(root, path)`; `function libSelectedItems() -> [{root, rel}]`.

- [ ] **Step 1: Tab scaffold.** Follow the existing two-tab pattern in `index.html`/`app.js` (Transfers/Settings) exactly — add a third nav button `Library` and section. Top level (`libState.root === null`): one row per capture root (name = configured path, free-space chip from `status.volumes` matched by root). Click → `libLoad(idx, "")`.
- [ ] **Step 2: Listing view.** Breadcrumb (`Root name / seg / seg`, each clickable), directory rows (icon + name, click descends), file rows: checkbox, name, size (existing formatter), mtime (`YYYY-MM-DD HH:MM`), status chip. Status→chip mapping: `unsent` neutral, `queued` blue, `sending` blue pulsing (reuse transfer row animation class), `delivered`/`sent` green, `declined` red. `batches > 0` renders a small `×N` suffix with `title="in N batches"`.
- [ ] **Step 3: Selection.** Checkbox per file + one per directory row (directory = "everything under it", resolved server-side in T8/T9 — the UI just sends the dir rel with a `dir: true` flag); footer bar appears when non-empty: `N selected — [Send to…] [Delete] [Preview]` (buttons wired in T7/T9/T10; render disabled until then).
- [ ] **Step 4: Errors.** `libLoad` failure renders an inline error card with the server's message (offline SMB root shows `root unavailable`, spec §7) and a Retry button — never an empty listing.
- [ ] **Step 5: Manual check** (`cargo run -p perseus -- run` against a scratch config with two capture dirs; browse, select, breadcrumbs). **Commit** — `feat(perseus): Library tab — roots, lazy listing, selection (0.5.1 T5)`

---

### Task 6: preview backend — rustafits behind `preview` feature

**Files:**
- Modify: `crates/perseus/Cargo.toml` (dep + feature), `crates/perseus/src/lib.rs`, `crates/perseus/src/web.rs` (route), `crates/perseus/packaging/linux/build_arm64.sh` (toolchain + feature)
- Create: `crates/perseus/src/preview.rs`
- Test: inline in `preview.rs` + route test

**Interfaces:**
- Produces: `pub struct PreviewCache` (`Semaphore(1)` + `Mutex<LruMap>` of ≤8 entries keyed `(String /*root:rel*/, u64 /*size*/, i64 /*mtime*/, u32 /*w*/)` → `Arc<Vec<u8>>`); `pub async fn render_jpeg(cache: &PreviewCache, abs: &Path, w: u32) -> anyhow::Result<(Arc<Vec<u8>>, String /*etag*/)>`.
- Cargo: `[features] default = ["preview"]`, `preview = ["dep:rustafits"]`, `rustafits = { path = "../../rustafits", optional = true }`. The whole module + route are `#[cfg(feature = "preview")]`; without the feature the route returns 404 `"preview not built"` (tiny cfg'd stub so the router shape is stable).

- [ ] **Step 1: Failing test** — render a real small FITS fixture (reuse an existing test FITS from the repo's test data; if none is committed under perseus, generate a minimal 100×100 16-bit FITS in the test via `athenaeum_core`'s FITS writer) at `w=200`, assert: JPEG magic bytes (`FF D8`), second call with same key hits the cache (assert via a render-count hook — an `AtomicUsize` incremented inside the render closure), `w` clamped to 1600.
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement.** Call rustafits' public render API (crate name `astroimage`; find the exact entry the desktop's `rustafits_processor` uses — auto-stretch to 8-bit + JPEG encode at quality ~85, downscale to `w` preserving aspect). Semaphore acquired around the blocking render (`tokio::task::spawn_blocking`); LRU evicts oldest; ETag = hex of the key hash.
- [ ] **Step 4: Route.** `GET /api/library/preview?root&path&w`: T1 guard → stat → key; `If-None-Match` match → 304; else render → `image/jpeg` + `ETag` header. Non-FITS/XISF extension → 415 `"not a renderable frame"`; render error → 422 with the error string. Route test: 200+ETag then 304; 415 arm.
- [ ] **Step 5: arm64 + variants.** `build_arm64.sh`: add `apt-get update && apt-get install -y cmake nasm` inside the container script, and change the deb build to `cargo deb --variant headless -- -p perseus` keeping default features (verify the variant's current `--no-default-features` flag: replace with explicit feature list `--no-default-features --features preview` so tray stays off but preview stays on). Confirm `cargo build -p perseus --no-default-features` still compiles (stub route).
- [ ] **Step 6: Run tests → PASS. Commit** — `feat(perseus): FITS preview endpoint — rustafits JPEG, semaphore(1), LRU+ETag (0.5.1 T6)`

---

### Task 7: preview UI — the pre-blink pane

**Files:**
- Modify: `crates/perseus/src/web/app.js`, `index.html`, `style.css`

**Interfaces:** Consumes T6 route + T5 `libState`.

- [ ] **Step 1: Pane.** Clicking a file name (not the checkbox) or the footer `Preview` button opens an overlay pane: image (`max-width: min(90vw, 1600px)`), filename caption, `←`/`→` buttons + ArrowLeft/ArrowRight/Escape keys walking `libState.entries` files of the current directory (skip dirs). `img.src = /api/library/preview?root=…&path=…&w=1200` — the browser's HTTP cache + server ETag make the blink walk fast.
- [ ] **Step 2: States.** Loading spinner while `img` loads; `onerror` → inline message with the server's status text (415 → "not a renderable frame", 404 after deletion → "file gone" + refresh listing per spec §2).
- [ ] **Step 3: Manual blink check** over a real capture dir (arrow-walk 10 files; second pass must be instant). **Commit** — `feat(perseus): pre-blink preview pane with keyboard walk (0.5.1 T7)`

---

### Task 8: browser send — `POST /api/library/send`

**Files:**
- Modify: `crates/perseus/src/library.rs` (selection expansion), `crates/perseus/src/batcher.rs` (`remove_pending`), `crates/perseus/src/web.rs` (route)
- Test: inline + route test

**Interfaces:**
- Consumes: T1 guard; `run::build_batch_package`, `run::enqueue_package_to_all`, `run::record_seen`, `BatchStore::record`/`record_files` (exact call sequence: copy the batcher's flush body — read `batcher.rs::flush_once` and mirror it, mode string `"browser"`).
- Produces: `BatcherHandle::remove_pending(&self, files: &[(PathBuf, PathBuf)]) -> usize` (removes exact pairs from the pending set, returns how many were removed).
- Produces: `pub fn expand_selection(config: &Config, items: &[SendItem]) -> anyhow::Result<Vec<(PathBuf /*capture_dir*/, PathBuf /*file*/)>>` where `SendItem { root: usize, rel: String, dir: bool }` — a `dir: true` item walks recursively (files only), each resolved through the T1 guard.
- Route body: `{ targets: Vec<String>, items: Vec<SendItem> }` → 200 `{ enqueued: usize, packageRef: String }`, 422 when expansion yields zero files, 400 on invalid target/path.

- [ ] **Step 1: Failing tests** — `remove_pending` removes exactly the named pairs; `expand_selection` walks a dir recursively, applies the guard (hostile rel → Err), returns `(owning capture_dir, file)` pairs; route test: seed two files, one already in pending → after send, pending no longer contains it (spec §1a double-send guard), `perseus_batch` row has mode `browser`, seen rows recorded.
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement** (mirror `flush_once`'s build→enqueue→record-seen→record-batch order and its zero-target requeue EXCEPTION: browser send does NOT requeue into pending on zero targets — it returns 502 `"no target accepted the package"` and deletes the built package dir; the operator retries explicitly).
- [ ] **Step 4: Run tests → PASS. Commit** — `feat(perseus): send-from-library route — explicit batches, pending dedup (0.5.1 T8)`

---

### Task 9: deletion — `POST /api/library/delete` (spec §2 matrix)

**Files:**
- Modify: `crates/perseus/src/library.rs` (delete engine), `crates/perseus/src/web.rs` (route), `crates/perseus/src/run.rs` (expose/reuse the audit-row writer used by `retention_delete_source` with actor `manual-web` — extract a `pub(crate) fn write_deletion_audit(store, path, size, actor)` if it is currently inline)
- Test: inline — ONE TEST PER §2 MATRIX ROW

**Interfaces:**
- Consumes: T1 guard, T8 `expand_selection` + `BatcherHandle::remove_pending`, `SeenStore::mark_deleted`, `BatchStore::packages_for_sources`, outbound rows for in-flight detection.
- Produces:

```rust
#[derive(serde::Serialize)] #[serde(rename_all = "camelCase")]
pub struct DeletePreviewItem { pub rel: String, pub root: usize,
    pub queued: bool, pub in_flight_batches: Vec<String>, pub confirmed_batches: usize }
#[derive(serde::Serialize)] #[serde(rename_all = "camelCase")]
pub enum DeleteOutcomeDto { Deleted, Refused { reason: String }, Error { reason: String } }

pub fn delete_preview(...) -> anyhow::Result<Vec<DeletePreviewItem>>;
pub fn delete_perform(...) -> anyhow::Result<Vec<(String, DeleteOutcomeDto)>>;
```

Route: `{ items, confirm: bool }` — `confirm:false` → preview list (nothing touched); `confirm:true` → outcomes. Refusal arm: any resolved path under `config.data_dir` or the packages dir → `Refused { reason: "perseus-internal path" }` (defense-in-depth on top of the guard).

- [ ] **Step 1: Failing tests, one per matrix row:** (a) queued file → removed from pending BEFORE unlink, pending no longer lists it; (b) file in in-flight batch → preview names the batch, delete succeeds, outbound row untouched, payload copy still present; (c) confirmed-batch file → deleted, `batches_for_source` still returns the ref (history intact); (d) **reappear case**: delete → `mark_deleted` stamped → recreate identical `(size, mtime)` → `should_enqueue` returns TRUE (this is the load-bearing assertion; read `seen.rs::should_enqueue`'s deleted-row handling first and assert its actual contract — if a deleted row does NOT currently re-enqueue, fixing `should_enqueue` is IN SCOPE of this task); (e) directory delete recursive + per-file error continues (make one file read-only-parent on unix to force an unlink error) + dir removed only when emptied; (f) internal-path refusal; (g) audit rows written with actor `manual-web` and visible via the existing retention-log read; (h) vanished-before-delete → `Error { "not found" }`, others proceed.
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement** — order per file: pending-remove → audit-write → unlink → `mark_deleted`; directory post-pass removes now-empty dirs deepest-first.
- [ ] **Step 4: Run tests → PASS. Commit** — `feat(perseus): library deletion — two-step confirm, honest §2 matrix (0.5.1 T9)`

---

### Task 10: Library actions UI — Send-to dialog + Delete confirm

**Files:**
- Modify: `crates/perseus/src/web/app.js`, `index.html`, `style.css`

**Interfaces:** Consumes T8/T9 routes, T5 `libSelectedItems()`, existing `GET /api/targets/options` (device picker source — same list the Settings targets editor uses).

- [ ] **Step 1: Send-to dialog.** Footer `Send to…` → modal: checkbox list of target devices (from `/api/targets/options`, preselect the configured targets), `Send N files` button → `POST /api/library/send` → toast with `enqueued` count, link switches to Transfers tab; listing refreshes (statuses flip to queued/sending).
- [ ] **Step 2: Delete confirm.** Footer `Delete` → call route with `confirm:false` → modal renders the preview: total count, per-warning groups ("3 queued — will leave the send queue", "2 in transfer #X — the transfer completes from its packaged copy", "5 were in confirmed batches"). Confirm button label `Delete N files` → `confirm:true` → per-item outcomes toast (`Deleted 9, 1 error`), listing refreshes.
- [ ] **Step 3: Manual check both dialogs. Commit** — `feat(perseus): library send/delete dialogs with §2 warnings (0.5.1 T10)`

---

### Task 11: send a previous batch to another node

**Files:**
- Modify: `crates/perseus/src/resend.rs` (generalize), `crates/perseus/src/web.rs` (route `POST /api/transfers/send-to`), `crates/perseus/src/web/app.js` (history-row action)
- Test: inline in `resend.rs` + route test

**Interfaces:**
- Consumes: `resend_declined_as_new` internals (`rebuild_package_payloads`, shared-dir probe, `BatchStore::clone_files`, `SeenStore::relink_package`).
- Produces: `pub async fn send_batch_to_target(store, engine /*of the TARGET peer*/, cleanup, config, batches, seen, row: &OutboundRow, target_device: &str) -> Result<i64>` — extract the body of `resend_declined_as_new` minus the `is_declined` gate into this fn; `resend_declined_as_new` becomes a thin wrapper (gate + same-peer call). Missing sources at rebuild → eligible-subset: rebuild what exists, return the new row id + skipped count (`Result<(i64, usize)>` — adjust both signatures consistently).
- Route: `{ id: i64 /*outbound row*/, target: String }` → `{ newId, sent, skipped }`; 409 when 0 sources remain (`"all source files deleted locally"`).

- [ ] **Step 1: Failing tests** — divert a confirmed batch to a second peer: new row minted for the target engine, fresh `batch_uuid` (new dir basename), original row untouched; eligible-subset: delete 1 of 3 sources → `skipped == 1`, package carries 2; all-deleted → Err.
- [ ] **Step 2: Run → FAIL. Step 3: Implement. Step 4: Route + history-row UI:** `Send to device…` menu on terminal rows (picker as in T10), confirm shows `sends N of M` when `skipped > 0` — the eligible-subset copy (owner rule: `(N of M)` counts).
- [ ] **Step 5: Run tests → PASS. Commit** — `feat(perseus): send a recorded batch to another device — new transfer, eligible subset (0.5.1 T11)`

---

### Task 12: scheduler config — `Mode::Scheduled`, next-fire math, `perseus_meta`

**Files:**
- Modify: `crates/perseus/src/config.rs` (Mode + SendCfg + validation + TOML docs in `config_template.toml`), `crates/perseus/src/batch_store.rs` (meta KV)
- Create: `crates/perseus/src/schedule.rs`
- Test: inline in `schedule.rs` + `config.rs`

**Interfaces:**
- Produces (config): `Mode::Scheduled` variant (serde `scheduled`); `SendCfg` grows `pub schedule_times: Vec<(u8, u8)> /*(hour, minute), sorted, deduped*/` and `pub schedule_catchup: bool` (SendCfg stays `Clone`; drop `Copy` if the Vec forces it — update the two `watch::channel` call sites accordingly). Raw config fields: `schedule_times: Vec<String>` (`"HH:MM"`), `schedule_catchup: bool` (default true). Validation: `mode = "scheduled"` requires ≥1 valid time; bad `HH:MM` → config error naming the value.
- Produces (schedule.rs, pure):

```rust
/// Next fire strictly after `now`, in local time. DST-nonexistent local times
/// resolve to the next valid instant; ambiguous ones take the earliest.
pub fn next_fire(times: &[(u8, u8)], now: chrono::DateTime<chrono::Local>) -> chrono::DateTime<chrono::Local>;
/// Catch-up check: the latest schedule point in (last_fire, now], if any.
pub fn missed_point(times: &[(u8, u8)], last_fire: chrono::DateTime<chrono::Local>,
    now: chrono::DateTime<chrono::Local>) -> Option<chrono::DateTime<chrono::Local>>;
```

- Produces (batch_store): `pub fn meta_get(&self, key: &str) -> Result<Option<String>>`, `pub fn meta_set(&self, key: &str, value: &str) -> Result<()>` over `CREATE TABLE IF NOT EXISTS perseus_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)`; key used: `last_scheduled_fire` (RFC-3339).

- [ ] **Step 1: Failing tests** — table-driven `next_fire`: single time before/after now, midnight wrap, two times picks nearer, `["06:00"]` at exactly 06:00 → tomorrow (strictly-after); construct fixed `Local` datetimes via `chrono::TimeZone::with_ymd_and_hms`, and cover the DST arms with `chrono-tz` ONLY inside tests if `Local` proves untestable — otherwise document the local-time dependency and test the pure offset math via a generic `TimeZone` parameter (implementer's call; the assertions above are the contract). `missed_point`: none, one, several-missed→latest-only (fires ONCE, spec §3). Config: parse/validate arms; meta KV roundtrip.
- [ ] **Step 2: Run → FAIL. Step 3: Implement (generic over `TimeZone` for testability: `fn next_fire_in<Tz: TimeZone>(…)`, thin `Local` wrapper). Step 4: Run → PASS.**
- [ ] **Step 5: Commit** — `feat(perseus): scheduled mode config + pure next-fire math + perseus_meta (0.5.1 T12)`

---

### Task 13: batcher third arm + catch-up + status

**Files:**
- Modify: `crates/perseus/src/batcher.rs` (loop arm), `crates/perseus/src/web.rs` (`StatusDto.nextScheduledSend`)
- Test: inline in `batcher.rs` (existing loop tests use paused tokio time — follow them) + status route test

**Interfaces:**
- Consumes: T12 `next_fire`/`missed_point`/meta KV; existing `flush_once` machinery.
- Produces: batch mode string `"scheduled"` on rows the timer flushes; `StatusDto.next_scheduled_send: Option<String>` (RFC-3339, serde camelCase) — `None` unless mode is Scheduled.

- [ ] **Step 1: Failing tests** (paused-time): (a) Scheduled mode: file stabilizes → NOT flushed until the fire instant, then one batch mode `scheduled`, `last_scheduled_fire` stamped; (b) empty pending at fire → no batch row (no-op invariant); (c) **collision**: manual `flush_now` drains 2 files mid-window, a 3rd stabilizes, timer fires → scheduled batch contains ONLY the 3rd (spec §3 owner requirement — by construction, but pin it); (d) catch-up: meta `last_scheduled_fire` = yesterday 05:00, times `["06:00"]`, spawn at 09:00 → one immediate flush, stamp updated; catchup=false → no flush; (e) live mode flip Scheduled→Manual disarms (no fire), →Scheduled re-arms.
- [ ] **Step 2: Run → FAIL. Step 3: Implement** — the loop's `select!` gains a `sleep_until(next_fire)` arm active only in Scheduled mode (recomputed after every fire/mode-change/config-change notification on the existing `send_cfg_rx` watch); catch-up runs once before the first arm. Fire path = the existing drain-and-flush with mode `"scheduled"`.
- [ ] **Step 4: Status:** compute `next_fire` in `api_status` from the live SendCfg. Route test asserts presence in scheduled mode, absence otherwise.
- [ ] **Step 5: Run tests → PASS. Commit** — `feat(perseus): fire-at-time scheduler in the batcher — catch-up once, honest collisions (0.5.1 T13)`

---

### Task 14: scheduler Settings UI

**Files:**
- Modify: `crates/perseus/src/web/app.js`, `index.html`, `crates/perseus/src/config_edit.rs` (PUT handler accepts the new fields), `crates/perseus/src/web.rs` (send-config PUT DTO)

**Interfaces:** Consumes T12 config fields via the existing send-config GET/PUT round-trip (find the current mode/auto_quiet editor in the Settings tab and extend it — same DTO, same live-apply watch path).

- [ ] **Step 1: Mode radio 3-way:** `Immediately (quiet window)` / `On schedule` / `Manually`. `On schedule` reveals a times editor: list of `HH:MM` rows with remove buttons + an add field (`<input type="time">`), client-side dedup/sort; `Catch up a missed send at startup` checkbox. PUT on save; server validation errors render inline (existing pattern).
- [ ] **Step 2: Status surface:** "Next scheduled send: <local time>" line on the status header when mode is scheduled (from `nextScheduledSend`); the pending-files card's button stays "Send N pending now" in every mode. Add the SMB hint text next to the existing poll-interval field: "watching a network share? raise this to reduce NAS load" (spec §7).
- [ ] **Step 3: Manual check: flip modes live, add/remove times, verify next-send updates without restart. Commit** — `feat(perseus): scheduler settings UI — 3-way mode, times editor, next-send (0.5.1 T14)`

---

### Task 15: retention transparency — card + per-file fate

**Files:**
- Modify: `crates/perseus/src/library.rs` (fate line), `crates/perseus/src/web.rs` (retention DTO extension if the config GET lacks a field), `crates/perseus/src/web/app.js` + `index.html` (card)
- Test: fate-line unit tests + card render check

**Interfaces:**
- Consumes: `RetentionConfig` (policy/dry_run/keep_days/disk_max_pct/interval_secs — already served to the web via the existing retention editor DTO; verify and extend with `next_pass: Option<String>` computed from the retention timer's last/next tick if cheaply available, else omit and show "checked hourly" static text — do NOT invent a new timer just for display).
- Produces: `pub fn retention_fate(policy: &RetentionPolicy, dry_run: bool, keep_days: u32, confirmed_at: Option<DateTime<Utc>>, status: &FileStatus) -> Option<String>` in `library.rs`, filling T4's `LibraryEntry.retention`. Contract (spec §8): `KeepEverything` → `None`; `KeepDays` + Delivered/Sent with known `confirmed_at` → `"deletable after <YYYY-MM-DD>"` (`confirmed_at + keep_days`), prefixed `"would be "` when dry-run; `KeepDays` + not-yet-confirmed → `"kept until sent and confirmed"`; `DiskPct` → `"deleted under disk pressure, oldest first"` (same dry-run prefix). `confirmed_at` comes from the newest confirmed outbound row of the file's batches (already loaded in T4's join — thread it through `StatusSources`).

- [ ] **Step 1: Failing tests** — one per contract arm above, incl. dry-run prefixes.
- [ ] **Step 2: Run → FAIL. Step 3: Implement + thread `confirmed_at` through the T4 join. Step 4: Run → PASS.**
- [ ] **Step 5: Settings card** (spec §8 copy, generated from effective config): policy sentence (three variants, exact wording from the spec), mode banner (yellow DRY-RUN / red live-armed with the two-key note), the manual-vs-retention distinction line, link to the existing retention log view. Library rows render `entry.retention` as a muted suffix line.
- [ ] **Step 6: Commit** — `feat(perseus): retention transparency — settings card + per-file fate (0.5.1 T15)`

---

### Task 16: docs, gates, wrap

**Files:**
- Modify: `CLAUDE.md` (Perseus bullet in the Transfers section: library browser, scheduler, preview, diskspace, retention card — 3-5 lines), `RELEASE_NOTES.md` (seed the 0.5.1 "What's New" bullets for these features, EN)
- Test: full gates

- [ ] **Step 1: Docs.** CLAUDE.md bullet + RELEASE_NOTES.md seed (notes file is release-owned; append feature bullets in the established voice).
- [ ] **Step 2: Gates.** `cargo build --workspace` · `cargo build -p perseus --no-default-features` · `cargo test -p perseus` · `cargo test -p athenaeum-core` (no core changes expected — confirm) · `npx tsc --noEmit` (no TS changes expected — confirm).
- [ ] **Step 3: E2E pass** (existing two-node harness in perseus tests): scheduler-fires-during-manual collision (T13c already pins it at unit level; run the full suite), browser re-send of fully-duplicate selection → "already on peer" confirm, send-to-device lands as a new transfer on node B.
- [ ] **Step 4: Commit** — `docs(perseus): 0.5.1 library cycle — CLAUDE.md + release-notes seed (0.5.1 T16)`

---

## Self-review notes (spec → plan coverage)

- §1 listing/status → T4/T5; §1a send → T8/T10; §2 matrix → T9 (one test per row) + T10 dialogs; §3 → T12/T13/T14; §4 → T6/T7; §5 → T2; §6 → T11; §7 guard → T1, SMB watch hole → T3, poll-cost hint → T14, UNC free-space → T2; §8 → T15; §9 API/config → spread across route tasks; §10 testing → embedded per task + T16 e2e. Non-goals honored (no move/rename, no cron, no windows, no alerts).
- Deliberately bound at implementation time (named in-task, not placeholders): the exact `WebState` field for the batcher handle (T4), `seen.rs` deleted-row `should_enqueue` contract (T9d — fixing it is in scope if it fails), rustafits' exact render entrypoint (T6), `SendCfg` Copy→Clone fallout (T12), retention `next_pass` availability (T15).
