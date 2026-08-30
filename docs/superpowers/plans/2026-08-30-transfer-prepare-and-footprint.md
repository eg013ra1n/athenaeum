# Transfer preparation, single-copy footprint, Transfers settings tab — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make *Send to node…* return instantly and show a cancellable `preparing` row with byte progress; hold ONE copy of a package's data on each end (sender: `packages/` + iroh outboards; receiver: the landed file); let the operator choose the outgoing-staging and incoming-working folders on a new Settings → Transfers tab.

**Architecture:** Four phases that each leave the app working. (A) `SyncDirs` splits the identity dir from the data dirs and backs two settings + validation + three commands. (B) The iroh serve import switches to `ImportMode::TryReference` for app hosts and the receiver's export to `ExportMode::TryReference` + hard-link landing. (C) A new `OutboundState::Preparing` row is inserted before any copy; an API-layer worker (semaphore 1) reflink-or-stream-copies + hashes with progress, then flips the row to `queued` and asks the engine to `Drive` it. (D) Transfers rows learn `preparing`/`indexing`/`announced`; Settings gets a Transfers tab.

**Tech Stack:** Rust (athenaeum-core, iroh-blobs 0.103 / iroh 1.0.3, rusqlite, tokio, `reflink-copy` 0.1.30 as a new direct dep), Tauri 2 + Axum mirrors, React/TS (ts-rs generated `src/types/models.ts`).

**Spec:** `docs/superpowers/specs/2026-08-30-transfer-prepare-and-footprint-design.md`

## Global Constraints

- Two backends in sync: every new/changed command in `crates/athenaeum-tauri/src/commands/sync.rs` has its Axum mirror in `crates/athenaeum-web/src/routes/sync.rs`, registered in `invoke_handler![]` (`crates/athenaeum-tauri/src/lib.rs`) and `build_router` (`crates/athenaeum-web/src/routes/mod.rs`); both wear `#[tracing::instrument(skip_all, err)]` (`err(Debug)` on the web side).
- Serde boundary `#[serde(rename_all = "camelCase")]`; every new boundary type is registered in `crates/athenaeum-core/src/ts_export.rs` and `src/types/models.ts` is regenerated with `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` (never hand-edited).
- Never swallow errors: log (`error!`/`warn!`, snake_case fields: `package_id`, `path`, `error`, `count`, `duration_ms`) before returning; message = short stable phrase, data in fields.
- Wire (`Announce3`/`Announce4`, receipts), manifest schema, Perseus behavior (Copy import, own `packages_dir`), collab swarm path (`fetch_collection_multi`, `collab_seed`): untouched.
- `identity_dir` (`<db dir>/sync`, holding `device_key` + `device_key.lock`) never moves.
- Design tokens only in the frontend (`bg-surface`, `text-content-muted`, `text-error`, …); `notify()` for notifications; `api.invoke` only (no `@tauri-apps/*` outside `src/api/`).
- Commit as the repo user (no AI co-author trailer); one commit per task; gates before each commit: `cargo check --workspace`, the task's tests, `npx tsc --noEmit` when TS changed.
- Rust formatting: `rustfmt --edition 2021 <files>` on touched files (not `cargo fmt -p`).

---

## File map

| File | Responsibility after this plan |
| ---- | ---- |
| `crates/athenaeum-core/src/settings/mod.rs` | 3 new keys (`sync.outgoing_staging_dir`, `sync.incoming_working_dir`, `sync.incoming_working_dir_previous`) |
| `crates/athenaeum-core/src/api/sync.rs` | `SyncDirs` + `sync_dirs()`, `validate_transfer_dir`, `TransferPaths` commands, storage report, enqueue split (row first), cancel routing, all `sync_dir.join(…)` sites rewired |
| `crates/athenaeum-core/src/api/sync_prepare.rs` (new) | preparation worker (`spawn_prepare`, `run_prepare`), `heal_interrupted_preparations` |
| `crates/athenaeum-core/src/api/scan_roots.rs` | `check_scan_root_overlap` extracted from `add_scan_root` |
| `crates/athenaeum-core/src/sync/sender.rs` | `PrepareRuntime` (semaphore + cancel flags) on `SyncSenderRuntime` |
| `crates/athenaeum-core/src/sync/models.rs` | `OutboundState::Preparing` |
| `crates/athenaeum-core/src/sync/store.rs` | `insert_outbound_with_files_in_state`, `enqueue_preparing`, `settle_outbound_files_terminal`, `total_bytes` in counts |
| `crates/athenaeum-core/src/sync/status.rs` | display mapping (`preparing` / `announced`), `total_bytes` |
| `crates/athenaeum-core/src/sync/engine.rs` | `Command::Drive`, `SyncEngineHandle::drive`, `ImportProgress` → `indexing` ticks |
| `crates/athenaeum-core/src/sync/receiver.rs` | `bound_working_dir` on `SyncRuntime`, `ImportProgress` no-op arms |
| `crates/athenaeum-core/src/sync/ingest.rs` | `land_payload` hard-link first |
| `crates/athenaeum-core/src/package/writer.rs` | `write_manifest`, `stage_payload` (reflink-or-stream copy+hash) |
| `crates/athenaeum-core/src/sharing/mod.rs` | `TransportEvent::ImportProgress` |
| `crates/athenaeum-core/src/sharing/iroh/node.rs` | `NodeOptions`, `bind_with(identity_dir, working_dir, …)`, serve mode threading, `route_import_progress` |
| `crates/athenaeum-core/src/sharing/iroh/blobs.rs` | `mode` + progress on imports, `TryReference` export, `export_source_vanished` |
| `crates/athenaeum-tauri/src/commands/sync.rs`, `crates/athenaeum-web/src/routes/sync.rs` | `get_transfer_paths`, `set_transfer_paths`, `cleanup_transfer_leftovers`; `cancel_sync_package` gains `ctx` |
| `src/components/transfers/{presentation.ts,TransferRow.tsx,TransfersPanel.tsx}`, `src/hooks/useTransferQueue.ts` | `preparing` / `indexing` / `announced` |
| `src/components/settings/TransfersSection.tsx` (new), `SyncSection.tsx`, `src/pages/Settings.tsx` | Transfers tab |
| `docs/superpowers/open-items.md`, `CLAUDE.md` | smoke list, release-note lines, updated sync bullets |

---

## Phase A — Paths and settings

### Task 1: `SyncDirs` — one resolver for identity / packages / working dirs

**Files:**
- Modify: `crates/athenaeum-core/src/settings/mod.rs` (the `pub mod keys` block, near line 132)
- Modify: `crates/athenaeum-core/src/api/sync.rs:118-131` (beside `sync_paths`)
- Test: `crates/athenaeum-core/src/api/sync.rs` (`mod tests`, uses `test_ctx()` at ~5079)

**Interfaces:**
- Produces: `pub struct SyncDirs { identity_dir, packages_dir, working_dir, db_path: PathBuf }` with `blobs_dir()`, `staging_root()`, `incoming_fallback()`; `pub(crate) fn sync_dirs(ctx: &ServiceContext) -> Result<SyncDirs, ApiError>`; keys `SYNC_OUTGOING_STAGING_DIR`, `SYNC_INCOMING_WORKING_DIR`, `SYNC_INCOMING_WORKING_DIR_PREVIOUS`.
- `sync_paths` stays until Task 4 removes its last caller.

- [ ] **Step 1: Add the settings keys**

In `crates/athenaeum-core/src/settings/mod.rs`, inside `pub mod keys` after `SYNC_MAX_CONCURRENT_RECEIVES`:

```rust
    /// Absolute path of the folder that holds prepared outgoing packages
    /// (`<dir>/<uuid>/…`). Empty/unset = `<identity_dir>/packages`
    /// (transfer-prepare spec §6.1). Applies to the next preparation.
    pub const SYNC_OUTGOING_STAGING_DIR: &str = "sync.outgoing_staging_dir";
    /// Absolute path of the folder that holds the iroh blob store (`blobs/`),
    /// receive staging (`staging/`), the incoming fallback and collab dirs.
    /// Empty/unset = `<identity_dir>`. Applies at the next transport start
    /// (spec §6.4).
    pub const SYNC_INCOMING_WORKING_DIR: &str = "sync.incoming_working_dir";
    /// The previous custom working dir, recorded by `set_transfer_paths` when
    /// the working dir changes, so the storage report can count its leftovers
    /// (spec §6.5). Cleared when nothing is left there.
    pub const SYNC_INCOMING_WORKING_DIR_PREVIOUS: &str = "sync.incoming_working_dir_previous";
```

- [ ] **Step 2: Write the failing tests**

Append to `mod tests` in `crates/athenaeum-core/src/api/sync.rs`:

```rust
    #[test]
    fn sync_dirs_defaults_under_the_db_dir() {
        let (tmp, ctx) = test_ctx();
        let dirs = sync_dirs(&ctx).unwrap();
        let identity = dirs.db_path.parent().unwrap().join("sync");
        assert_eq!(dirs.identity_dir, identity);
        assert_eq!(dirs.packages_dir, identity.join("packages"));
        assert_eq!(dirs.working_dir, identity);
        assert_eq!(dirs.blobs_dir(), identity.join("blobs"));
        assert_eq!(dirs.staging_root(), identity.join("staging"));
        assert_eq!(dirs.incoming_fallback(), identity.join("incoming"));
        drop(tmp);
    }

    #[test]
    fn sync_dirs_honors_both_settings_and_ignores_blank_values() {
        let (tmp, ctx) = test_ctx();
        let out = tmp.path().join("out");
        let work = tmp.path().join("work");
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            crate::db::set_setting(&conn, keys::SYNC_OUTGOING_STAGING_DIR, out.to_str().unwrap())
                .unwrap();
            crate::db::set_setting(&conn, keys::SYNC_INCOMING_WORKING_DIR, work.to_str().unwrap())
                .unwrap();
        }
        let dirs = sync_dirs(&ctx).unwrap();
        assert_eq!(dirs.packages_dir, out);
        assert_eq!(dirs.working_dir, work);
        assert_eq!(dirs.blobs_dir(), work.join("blobs"));
        // identity never follows the working dir
        assert_eq!(dirs.identity_dir, dirs.db_path.parent().unwrap().join("sync"));

        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            crate::db::set_setting(&conn, keys::SYNC_OUTGOING_STAGING_DIR, "   ").unwrap();
        }
        let dirs = sync_dirs(&ctx).unwrap();
        assert_eq!(dirs.packages_dir, dirs.identity_dir.join("packages"), "blank = default");
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p athenaeum-core --lib sync_dirs_ 2>&1 | tail -5`
Expected: compile error `cannot find function sync_dirs`.

- [ ] **Step 4: Implement `SyncDirs`**

In `crates/athenaeum-core/src/api/sync.rs`, directly after `sync_paths`:

```rust
/// Every directory the transfer machinery writes, resolved once per call
/// (transfer-prepare spec §6.2). `identity_dir` (`<db dir>/sync`) holds the
/// device key + its lock and NEVER moves; `packages_dir` and `working_dir`
/// follow the two Settings → Transfers folders, defaulting under `identity_dir`
/// so an install that never touches the tab keeps today's layout.
#[derive(Debug, Clone)]
pub struct SyncDirs {
    pub identity_dir: PathBuf,
    /// Prepared outgoing packages: `<packages_dir>/<uuid>/…`.
    pub packages_dir: PathBuf,
    /// Blob store, receive staging, incoming fallback, collab dirs.
    pub working_dir: PathBuf,
    pub db_path: PathBuf,
}

impl SyncDirs {
    pub fn blobs_dir(&self) -> PathBuf {
        self.working_dir.join("blobs")
    }
    pub fn staging_root(&self) -> PathBuf {
        self.working_dir.join("staging")
    }
    pub fn incoming_fallback(&self) -> PathBuf {
        self.working_dir.join("incoming")
    }
    /// The defaults for the two configurable folders — what "Use default" restores.
    pub fn default_packages_dir(&self) -> PathBuf {
        self.identity_dir.join("packages")
    }
    pub fn default_working_dir(&self) -> PathBuf {
        self.identity_dir.clone()
    }
}

/// A configured folder setting: `Some(path)` when the key holds a non-blank
/// value, else `None` (= default).
fn configured_dir(conn: &rusqlite::Connection, key: &str) -> Result<Option<PathBuf>, ApiError> {
    Ok(crate::db::get_setting(conn, key)?
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from))
}

pub(crate) fn sync_dirs(ctx: &ServiceContext) -> Result<SyncDirs, ApiError> {
    let db = db(ctx)?;
    let db_path = db.path().to_path_buf();
    let identity_dir = db_path
        .parent()
        .map(|p| p.join("sync"))
        .unwrap_or_else(|| PathBuf::from("sync"));
    let conn = db.conn();
    let packages_dir = configured_dir(&conn, keys::SYNC_OUTGOING_STAGING_DIR)?
        .unwrap_or_else(|| identity_dir.join("packages"));
    let working_dir = configured_dir(&conn, keys::SYNC_INCOMING_WORKING_DIR)?
        .unwrap_or_else(|| identity_dir.clone());
    Ok(SyncDirs {
        identity_dir,
        packages_dir,
        working_dir,
        db_path,
    })
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p athenaeum-core --lib sync_dirs_ 2>&1 | tail -5`
Expected: `test result: ok. 2 passed`.

- [ ] **Step 6: Commit**

```bash
rustfmt --edition 2021 crates/athenaeum-core/src/api/sync.rs crates/athenaeum-core/src/settings/mod.rs
git add crates/athenaeum-core/src/api/sync.rs crates/athenaeum-core/src/settings/mod.rs
git commit -m "feat(sync): SyncDirs — identity dir split from configurable packages/working dirs"
```

---

### Task 2: `validate_transfer_dir` + the scan-root overlap helper

**Files:**
- Modify: `crates/athenaeum-core/src/api/scan_roots.rs:286-330` (extract the overlap loop of `add_scan_root`)
- Modify: `crates/athenaeum-core/src/api/sync.rs` (new fn beside `sync_dirs`)
- Test: both files' `mod tests`

**Interfaces:**
- Produces: `pub fn check_scan_root_overlap(conn: &Connection, new_path: &Path) -> Result<(), ApiError>` (scan_roots.rs) and `pub(crate) fn validate_transfer_dir(conn: &Connection, policy: &PathPolicy, raw: &str, label: &str) -> Result<PathBuf, ApiError>` (sync.rs) — returns the normalized, created, writable path.

- [ ] **Step 1: Write the failing tests**

In `crates/athenaeum-core/src/api/sync.rs` `mod tests`:

```rust
    #[test]
    fn validate_transfer_dir_creates_and_returns_a_writable_dir() {
        let (tmp, ctx) = test_ctx();
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let target = tmp.path().join("staging-new");
        let got = validate_transfer_dir(
            &conn,
            &crate::api::PathPolicy::AllowAll,
            target.to_str().unwrap(),
            "Outgoing staging folder",
        )
        .unwrap();
        assert!(got.is_dir(), "created on validation");
        assert!(!got.join(".athenaeum-write-test").exists(), "probe file removed");
    }

    #[test]
    fn validate_transfer_dir_rejects_relative_and_scan_root_overlap() {
        let (tmp, ctx) = test_ctx();
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let err = validate_transfer_dir(&conn, &crate::api::PathPolicy::AllowAll, "relative/x", "X")
            .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "relative: {err:?}");

        // A monitored root: inside it, equal to it, and containing it are all rejected.
        let root = tmp.path().join("lights");
        std::fs::create_dir_all(root.join("night1")).unwrap();
        crate::db::upsert_scan_root(&conn, root.to_str().unwrap(), "normal").unwrap();
        for candidate in [root.join("night1"), root.clone(), tmp.path().to_path_buf()] {
            let err = validate_transfer_dir(
                &conn,
                &crate::api::PathPolicy::AllowAll,
                candidate.to_str().unwrap(),
                "X",
            )
            .unwrap_err();
            assert!(
                matches!(err, ApiError::Invalid(_) | ApiError::Conflict(_)),
                "{}: {err:?}",
                candidate.display()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn validate_transfer_dir_rejects_a_dir_it_cannot_write() {
        use std::os::unix::fs::PermissionsExt;
        let (tmp, ctx) = test_ctx();
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let ro = tmp.path().join("ro");
        std::fs::create_dir_all(&ro).unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o500)).unwrap();
        let err =
            validate_transfer_dir(&conn, &crate::api::PathPolicy::AllowAll, ro.to_str().unwrap(), "X")
                .unwrap_err();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(err, ApiError::Invalid(m) if m.contains("not writable")), "{err:?}");
    }
```

(`crate::db::upsert_scan_root(conn, path, kind)` is the existing insert used by `add_scan_root`; if its signature differs, mirror the call `add_scan_root` makes at the end of its body.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p athenaeum-core --lib validate_transfer_dir 2>&1 | tail -5`
Expected: `cannot find function validate_transfer_dir`.

- [ ] **Step 3: Extract the overlap helper in `scan_roots.rs`**

Replace the loop starting at `// 4. Get existing scan roots and check for overlaps` (through the end of that `for root in existing_roots.iter()` block) with a call `check_scan_root_overlap(&conn, &new_path)?;` and add the function (same messages, so existing scan-root tests keep passing):

```rust
/// Reject `new_path` (already canonicalized + normalized) when it equals, sits
/// inside, or contains any existing scan root — the shared guard behind
/// `add_scan_root` and the transfer-folder settings (a folder the scanner
/// watches would ingest transfer copies as duplicates).
pub fn check_scan_root_overlap(conn: &rusqlite::Connection, new_path: &Path) -> Result<(), ApiError> {
    let existing_roots = crate::db::get_scan_roots(conn)?;
    for root in existing_roots.iter() {
        let existing_path = normalize_path(&Path::new(&root.path).canonicalize().map_err(|e| {
            ApiError::Internal(format!("Failed to resolve existing root path: {}", e))
        })?);
        if new_path == existing_path {
            return Err(ApiError::Conflict(
                "This directory is already being monitored".to_string(),
            ));
        }
        if new_path.starts_with(&existing_path) {
            return Err(ApiError::Conflict(format!(
                "Cannot add directory: it is a subdirectory of existing scan root '{}'",
                root.path
            )));
        }
        if existing_path.starts_with(new_path) {
            return Err(ApiError::Conflict(format!(
                "Cannot add directory: existing scan root '{}' is a subdirectory of it",
                root.path
            )));
        }
    }
    Ok(())
}
```

Keep whatever the original loop did AFTER the three checks (if there is more in the loop body, keep it in the helper verbatim).

- [ ] **Step 4: Implement `validate_transfer_dir` in `sync.rs`**

```rust
/// Validate (and create) a transfer folder the operator typed or picked
/// (transfer-prepare spec §6.3). Order: absolute → create → canonicalize →
/// `PathPolicy` → no scan-root overlap → write probe. Returns the normalized
/// path to persist. `label` names the setting in messages.
pub(crate) fn validate_transfer_dir(
    conn: &rusqlite::Connection,
    policy: &crate::api::PathPolicy,
    raw: &str,
    label: &str,
) -> Result<PathBuf, ApiError> {
    let raw = raw.trim();
    let candidate = Path::new(raw);
    if raw.is_empty() || !candidate.is_absolute() {
        return Err(ApiError::Invalid(format!("{label}: enter an absolute path")));
    }
    std::fs::create_dir_all(candidate).map_err(|e| {
        tracing::warn!(path = %candidate.display(), error = %e, "transfer folder create failed");
        ApiError::Invalid(format!("{label}: cannot create folder: {e}"))
    })?;
    let path = crate::api::scan_roots::normalize_path(&candidate.canonicalize().map_err(|e| {
        ApiError::Invalid(format!("{label}: cannot resolve folder: {e}"))
    })?);
    policy.check(&path)?;
    crate::api::scan_roots::check_scan_root_overlap(conn, &path).map_err(|e| match e {
        ApiError::Conflict(_) => ApiError::Invalid(format!(
            "{label}: must not be inside or contain a monitored folder — the scanner would ingest transfer copies"
        )),
        other => other,
    })?;
    let probe = path.join(".athenaeum-write-test");
    if let Err(e) = std::fs::write(&probe, b"probe") {
        tracing::warn!(path = %path.display(), error = %e, "transfer folder write probe failed");
        return Err(ApiError::Invalid(format!("{label}: folder is not writable: {e}")));
    }
    if let Err(e) = std::fs::remove_file(&probe) {
        tracing::warn!(path = %probe.display(), error = %e, "transfer folder probe cleanup failed");
    }
    Ok(path)
}
```

(`normalize_path` is `pub fn` in `scan_roots.rs:58`; make `check_scan_root_overlap` `pub` too.)

- [ ] **Step 5: Run the tests**

Run: `cargo test -p athenaeum-core --lib validate_transfer_dir 2>&1 | tail -5` and `cargo test -p athenaeum-core --lib scan_roots 2>&1 | tail -3`
Expected: all pass (the scan-root tests prove the extraction kept behavior).

- [ ] **Step 6: Commit**

```bash
rustfmt --edition 2021 crates/athenaeum-core/src/api/sync.rs crates/athenaeum-core/src/api/scan_roots.rs
git add crates/athenaeum-core/src/api/sync.rs crates/athenaeum-core/src/api/scan_roots.rs
git commit -m "feat(sync): validate_transfer_dir — absolute, policy-checked, outside scan roots, writable"
```

---

### Task 3: `get_transfer_paths` / `set_transfer_paths` / `cleanup_transfer_leftovers` + storage report (both backends)

**Files:**
- Modify: `crates/athenaeum-core/src/api/sync.rs` (new types + fns; `TransferStorage` + `get_transfer_storage` at ~4660-4708)
- Modify: `crates/athenaeum-core/src/sync/receiver.rs:3591-3700` (`SyncRuntime` gets `bound_working_dir`)
- Modify: `crates/athenaeum-core/src/ts_export.rs:201` (register `TransferPaths`, `PathSetting`)
- Modify: `crates/athenaeum-tauri/src/commands/sync.rs` (after `cleanup_finished_transfers` ~258), `crates/athenaeum-tauri/src/lib.rs:450`
- Modify: `crates/athenaeum-web/src/routes/sync.rs` (after `cleanup_finished_transfers` ~350), `crates/athenaeum-web/src/routes/mod.rs:276`
- Test: `crates/athenaeum-core/src/api/sync.rs` `mod tests`

**Interfaces:**
- Produces:
  ```rust
  pub struct PathSetting { pub configured: Option<String>, pub effective: String, pub default: String, pub restart_required: bool }
  pub struct TransferPaths { pub outgoing: PathSetting, pub working: PathSetting }
  pub async fn get_transfer_paths(ctx: &ServiceContext, sync: &SyncRuntime) -> Result<TransferPaths, ApiError>
  pub async fn set_transfer_paths(ctx: &ServiceContext, sync: &SyncRuntime, policy: &PathPolicy, outgoing: Option<String>, working: Option<String>) -> Result<TransferPaths, ApiError>
  pub async fn cleanup_transfer_leftovers(ctx: &ServiceContext, sync: &SyncRuntime) -> Result<u64, ApiError>
  impl SyncRuntime { pub async fn bound_working_dir(&self) -> Option<PathBuf>; pub(crate) async fn set_bound_working_dir(&self, p: PathBuf) }
  ```
  `TransferStorage` gains `packages_dir: String`, `working_dir: String`, `leftover_bytes: u64`.
- Consumes: `sync_dirs`, `validate_transfer_dir` (Tasks 1–2).

- [ ] **Step 1: Write the failing tests**

```rust
    #[tokio::test]
    async fn transfer_paths_roundtrip_defaults_and_custom() {
        let (tmp, ctx) = test_ctx();
        let sync = crate::sync::SyncRuntime::new();
        let paths = get_transfer_paths(&ctx, &sync).await.unwrap();
        assert!(paths.outgoing.configured.is_none());
        assert_eq!(paths.outgoing.effective, paths.outgoing.default);
        assert!(!paths.working.restart_required, "nothing bound, nothing configured");

        let out = tmp.path().join("custom-out");
        let work = tmp.path().join("custom-work");
        let paths = set_transfer_paths(
            &ctx,
            &sync,
            &crate::api::PathPolicy::AllowAll,
            Some(out.to_str().unwrap().to_string()),
            Some(work.to_str().unwrap().to_string()),
        )
        .await
        .unwrap();
        assert_eq!(paths.outgoing.configured.as_deref(), Some(out.canonicalize().unwrap().to_str().unwrap()));
        assert_eq!(PathBuf::from(&paths.working.effective), work.canonicalize().unwrap());
        // Not bound yet → a configured working dir does not demand a restart.
        assert!(!paths.working.restart_required);

        // Bound elsewhere → restart required.
        sync.set_bound_working_dir(tmp.path().join("bound-elsewhere")).await;
        let paths = get_transfer_paths(&ctx, &sync).await.unwrap();
        assert!(paths.working.restart_required);

        // Reset to default clears the key and records the previous working dir.
        let paths = set_transfer_paths(&ctx, &sync, &crate::api::PathPolicy::AllowAll, None, None)
            .await
            .unwrap();
        assert!(paths.outgoing.configured.is_none());
        assert!(paths.working.configured.is_none());
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        assert_eq!(
            crate::db::get_setting(&conn, keys::SYNC_INCOMING_WORKING_DIR_PREVIOUS).unwrap().as_deref(),
            Some(work.canonicalize().unwrap().to_str().unwrap())
        );
    }

    #[tokio::test]
    async fn set_transfer_paths_rejects_working_inside_outgoing() {
        let (tmp, ctx) = test_ctx();
        let sync = crate::sync::SyncRuntime::new();
        let out = tmp.path().join("out");
        let err = set_transfer_paths(
            &ctx,
            &sync,
            &crate::api::PathPolicy::AllowAll,
            Some(out.to_str().unwrap().to_string()),
            Some(out.join("work").to_str().unwrap().to_string()),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(m) if m.contains("inside")), "{err:?}");
    }

    #[tokio::test]
    async fn storage_report_counts_leftovers_after_a_move() {
        let (tmp, ctx) = test_ctx();
        let sync = crate::sync::SyncRuntime::new();
        let dirs = sync_dirs(&ctx).unwrap();
        // Data left in the DEFAULT working dir…
        std::fs::create_dir_all(dirs.identity_dir.join("blobs")).unwrap();
        std::fs::write(dirs.identity_dir.join("blobs").join("x.data"), vec![7u8; 4096]).unwrap();
        // …after the operator moved the working dir elsewhere.
        let work = tmp.path().join("work");
        set_transfer_paths(&ctx, &sync, &crate::api::PathPolicy::AllowAll, None, Some(work.to_str().unwrap().to_string()))
            .await
            .unwrap();
        let report = get_transfer_storage(&ctx).unwrap();
        assert_eq!(report.leftover_bytes, 4096);
        assert_eq!(PathBuf::from(&report.working_dir), work.canonicalize().unwrap());

        // Cleanup refuses while bound there, then frees when not.
        sync.set_bound_working_dir(dirs.identity_dir.clone()).await;
        assert!(matches!(
            cleanup_transfer_leftovers(&ctx, &sync).await.unwrap_err(),
            ApiError::Conflict(_)
        ));
        sync.set_bound_working_dir(work.clone()).await;
        let freed = cleanup_transfer_leftovers(&ctx, &sync).await.unwrap();
        assert_eq!(freed, 4096);
        assert!(!dirs.identity_dir.join("blobs").exists());
        assert_eq!(get_transfer_storage(&ctx).unwrap().leftover_bytes, 0);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p athenaeum-core --lib transfer_paths 2>&1 | tail -5`
Expected: compile errors for the missing functions.

- [ ] **Step 3: `bound_working_dir` on `SyncRuntime`**

In `crates/athenaeum-core/src/sync/receiver.rs`, add a field to `SyncRuntime` (initialized in `new()`) and two methods:

```rust
    /// The working dir the running node bound its blob store under (transfer-
    /// prepare spec §6.4) — `None` until `ensure_started` binds. Compared with
    /// the configured setting to raise the "restart required" badge.
    bound_working_dir: tokio::sync::Mutex<Option<PathBuf>>,
```
```rust
    pub async fn bound_working_dir(&self) -> Option<PathBuf> {
        self.bound_working_dir.lock().await.clone()
    }
    pub async fn set_bound_working_dir(&self, dir: PathBuf) {
        *self.bound_working_dir.lock().await = Some(dir);
    }
```

In `ensure_started`, right after `std::fs::create_dir_all(&sync_dir)…?;` add `*self.bound_working_dir.lock().await = Some(sync_dir.clone());` (Task 4 renames the parameter to `working_dir`; the value is the same).

- [ ] **Step 4: Implement the types, the commands and the storage fields in `sync.rs`**

Beside `get_transfer_storage`:

```rust
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PathSetting {
    /// The persisted value, `None` = default.
    pub configured: Option<String>,
    /// What is in effect for the NEXT use (packages: next preparation; working:
    /// next transport start).
    pub effective: String,
    pub default: String,
    /// Working dir only: the running node bound a different dir than the one in
    /// effect, so the change waits for a restart. Always `false` for outgoing.
    pub restart_required: bool,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct TransferPaths {
    pub outgoing: PathSetting,
    pub working: PathSetting,
}

fn path_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

pub async fn get_transfer_paths(ctx: &ServiceContext, sync: &SyncRuntime) -> Result<TransferPaths, ApiError> {
    let dirs = sync_dirs(ctx)?;
    let (out_cfg, work_cfg) = {
        let db = db(ctx)?;
        let conn = db.conn();
        (
            configured_dir(&conn, keys::SYNC_OUTGOING_STAGING_DIR)?,
            configured_dir(&conn, keys::SYNC_INCOMING_WORKING_DIR)?,
        )
    };
    let bound = sync.bound_working_dir().await;
    let restart_required = matches!(&bound, Some(b) if b != &dirs.working_dir);
    Ok(TransferPaths {
        outgoing: PathSetting {
            configured: out_cfg.as_deref().map(path_string),
            effective: path_string(&dirs.packages_dir),
            default: path_string(&dirs.default_packages_dir()),
            restart_required: false,
        },
        working: PathSetting {
            configured: work_cfg.as_deref().map(path_string),
            effective: path_string(&dirs.working_dir),
            default: path_string(&dirs.default_working_dir()),
            restart_required,
        },
    })
}

/// Persist the two folders (`None` = reset to default) after §6.3 validation.
/// Nothing is written unless BOTH values validate. A working-dir change records
/// the previous custom dir for the leftovers report (§6.5).
pub async fn set_transfer_paths(
    ctx: &ServiceContext,
    sync: &SyncRuntime,
    policy: &crate::api::PathPolicy,
    outgoing: Option<String>,
    working: Option<String>,
) -> Result<TransferPaths, ApiError> {
    let before = sync_dirs(ctx)?;
    {
        let db = db(ctx)?;
        let conn = db.conn();
        let out_path = match outgoing.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(raw) => Some(validate_transfer_dir(&conn, policy, raw, "Outgoing staging folder")?),
            None => None,
        };
        let work_path = match working.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(raw) => Some(validate_transfer_dir(&conn, policy, raw, "Incoming working folder")?),
            None => None,
        };
        let eff_out = out_path.clone().unwrap_or_else(|| before.default_packages_dir());
        let eff_work = work_path.clone().unwrap_or_else(|| before.default_working_dir());
        if eff_work != eff_out && eff_work.starts_with(&eff_out) {
            return Err(ApiError::Invalid(
                "Incoming working folder: must not be inside the outgoing staging folder".into(),
            ));
        }
        let prev_work = configured_dir(&conn, keys::SYNC_INCOMING_WORKING_DIR)?;
        if prev_work.as_ref() != work_path.as_ref() {
            if let Some(prev) = prev_work {
                crate::db::set_setting(&conn, keys::SYNC_INCOMING_WORKING_DIR_PREVIOUS, &path_string(&prev))?;
            }
        }
        crate::db::set_setting(
            &conn,
            keys::SYNC_OUTGOING_STAGING_DIR,
            &out_path.as_deref().map(path_string).unwrap_or_default(),
        )?;
        crate::db::set_setting(
            &conn,
            keys::SYNC_INCOMING_WORKING_DIR,
            &work_path.as_deref().map(path_string).unwrap_or_default(),
        )?;
        tracing::info!(
            outgoing = %eff_out.display(),
            working = %eff_work.display(),
            "transfer folders updated"
        );
    }
    get_transfer_paths(ctx, sync).await
}

/// The dirs whose contents are leftovers: the default trio under `identity_dir`
/// when it is not the effective working/packages dir, plus the same trio under
/// the recorded previous custom working dir.
fn leftover_dirs(ctx: &ServiceContext, dirs: &SyncDirs) -> Result<Vec<PathBuf>, ApiError> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if dirs.working_dir != dirs.identity_dir {
        roots.push(dirs.identity_dir.clone());
    }
    let prev = {
        let db = db(ctx)?;
        let conn = db.conn();
        configured_dir(&conn, keys::SYNC_INCOMING_WORKING_DIR_PREVIOUS)?
    };
    if let Some(p) = prev {
        if p != dirs.working_dir && p != dirs.identity_dir {
            roots.push(p);
        }
    }
    let mut out = Vec::new();
    for root in roots {
        for name in ["blobs", "staging", "packages"] {
            let d = root.join(name);
            // The default packages dir is a leftover only if packages moved away.
            if name == "packages" && d == dirs.packages_dir {
                continue;
            }
            if d.is_dir() {
                out.push(d);
            }
        }
    }
    Ok(out)
}

pub async fn cleanup_transfer_leftovers(ctx: &ServiceContext, sync: &SyncRuntime) -> Result<u64, ApiError> {
    let dirs = sync_dirs(ctx)?;
    let targets = leftover_dirs(ctx, &dirs)?;
    if let Some(bound) = sync.bound_working_dir().await {
        if targets.iter().any(|t| t.starts_with(&bound)) {
            return Err(ApiError::Conflict(
                "The transport is still using the previous folder — restart Athenaeum first".into(),
            ));
        }
    }
    let mut freed = 0u64;
    for t in &targets {
        let bytes = dir_size_bytes(t);
        match std::fs::remove_dir_all(t) {
            Ok(()) => freed = freed.saturating_add(bytes),
            Err(e) => {
                tracing::error!(path = %t.display(), error = %e, "leftover cleanup failed");
                return Err(ApiError::Internal(format!("remove {}: {e}", t.display())));
            }
        }
    }
    let db = db(ctx)?;
    let conn = db.conn();
    if leftover_dirs(ctx, &dirs)?.is_empty() {
        crate::db::set_setting(&conn, keys::SYNC_INCOMING_WORKING_DIR_PREVIOUS, "")?;
    }
    tracing::info!(freed_bytes = freed, count = targets.len(), "transfer leftovers removed");
    Ok(freed)
}
```

Extend `TransferStorage` with three fields and fill them in `get_transfer_storage` (which now reads `sync_dirs`):

```rust
    /// Effective outgoing staging folder (display).
    pub packages_dir: String,
    /// Effective incoming working folder (display).
    pub working_dir: String,
    /// Bytes still sitting in the default / previous folders after a move
    /// (transfer-prepare spec §6.5); 0 when nothing was moved.
    pub leftover_bytes: u64,
```
```rust
pub fn get_transfer_storage(ctx: &ServiceContext) -> Result<TransferStorage, ApiError> {
    let dirs = sync_dirs(ctx)?;
    let packages_dir = dirs.packages_dir.clone();
    let blobs_dir = dirs.blobs_dir();
    // … existing walk of packages_dir + blobs_dir unchanged …
    let staging_bytes = dir_size_bytes(&dirs.staging_root());
    let leftover_bytes = leftover_dirs(ctx, &dirs)?.iter().map(|d| dir_size_bytes(d)).sum();
    Ok(TransferStorage {
        packages_bytes,
        packages_count,
        blobs_bytes,
        staging_bytes,
        packages_dir: path_string(&packages_dir),
        working_dir: path_string(&dirs.working_dir),
        leftover_bytes,
    })
}
```

Register in `ts_export.rs` next to `TransferStorage`: `crate::api::sync::TransferPaths, crate::api::sync::PathSetting,`.

- [ ] **Step 5: Tauri + Axum wrappers**

`crates/athenaeum-tauri/src/commands/sync.rs` (after `cleanup_finished_transfers`):

```rust
/// The two Settings → Transfers folders with their effective/default values.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_transfer_paths(state: State<'_, AppState>) -> Result<TransferPaths, String> {
    api::get_transfer_paths(&state.ctx, &state.sync).await.map_err(|e| e.to_string())
}

/// Persist the two folders (`null` = default); validated before anything is written.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_transfer_paths(
    state: State<'_, AppState>,
    outgoing: Option<String>,
    working: Option<String>,
) -> Result<TransferPaths, String> {
    api::set_transfer_paths(&state.ctx, &state.sync, &PathPolicy::AllowAll, outgoing, working)
        .await
        .map_err(|e| e.to_string())
}

/// Remove transfer data left in the default / previous folders after a move.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn cleanup_transfer_leftovers(state: State<'_, AppState>) -> Result<u64, String> {
    api::cleanup_transfer_leftovers(&state.ctx, &state.sync).await.map_err(|e| e.to_string())
}
```
(add `use athenaeum_core::api::{PathPolicy, sync::TransferPaths};` to the imports.) Register the three in `invoke_handler![]` after `commands::cleanup_finished_transfers,`.

`crates/athenaeum-web/src/routes/sync.rs` (after `cleanup_finished_transfers`):

```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetTransferPathsArgs {
    pub outgoing: Option<String>,
    pub working: Option<String>,
}

/// POST /api/get_transfer_paths
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_transfer_paths(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<TransferPaths>, (StatusCode, String)> {
    api::get_transfer_paths(&state.ctx, &state.sync).await.map(Json).map_err(api_err)
}

/// POST /api/set_transfer_paths — `allowed_paths` sandbox applies (same policy
/// construction as `scan_roots::allowed_roots_policy`).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_transfer_paths(
    State(state): State<WebAppState>,
    Json(args): Json<SetTransferPathsArgs>,
) -> Result<Json<TransferPaths>, (StatusCode, String)> {
    let policy = if state.allowed_paths.is_empty() {
        PathPolicy::AllowAll
    } else {
        PathPolicy::AllowedRoots(
            state
                .allowed_paths
                .iter()
                .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
                .collect(),
        )
    };
    api::set_transfer_paths(&state.ctx, &state.sync, &policy, args.outgoing, args.working)
        .await
        .map(Json)
        .map_err(api_err)
}

/// POST /api/cleanup_transfer_leftovers
#[tracing::instrument(skip_all, err(Debug))]
pub async fn cleanup_transfer_leftovers(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<u64>, (StatusCode, String)> {
    api::cleanup_transfer_leftovers(&state.ctx, &state.sync).await.map(Json).map_err(api_err)
}
```
(imports: `use athenaeum_core::api::PathPolicy; use athenaeum_core::api::sync::TransferPaths;`). Register in `routes/mod.rs` after the `cleanup_finished_transfers` route:
```rust
        .route("/api/get_transfer_paths", post(sync::get_transfer_paths))
        .route("/api/set_transfer_paths", post(sync::set_transfer_paths))
        .route("/api/cleanup_transfer_leftovers", post(sync::cleanup_transfer_leftovers))
```

- [ ] **Step 6: Run tests, regenerate TS, check workspace**

Run: `cargo test -p athenaeum-core --lib transfer_paths storage_report 2>&1 | tail -5`; `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract 2>&1 | tail -3`; `cargo check --workspace 2>&1 | tail -3`; `npx tsc --noEmit`.
Expected: tests pass; `models.ts` gains `PathSetting`, `TransferPaths`, and the three `TransferStorage` fields; tsc clean (no consumer reads the new fields yet).

- [ ] **Step 7: Commit**

```bash
rustfmt --edition 2021 crates/athenaeum-core/src/api/sync.rs crates/athenaeum-core/src/sync/receiver.rs crates/athenaeum-tauri/src/commands/sync.rs crates/athenaeum-web/src/routes/sync.rs
git add -A crates/athenaeum-core/src crates/athenaeum-tauri/src crates/athenaeum-web/src src/types/models.ts
git commit -m "feat(sync): transfer folder settings — get/set_transfer_paths, leftovers report + cleanup (both backends)"
```

---

### Task 4: Rewire every data-dir call site to `SyncDirs`; split the node bind

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/iroh/node.rs:900-1000` (`bind` → `bind_with`)
- Modify: `crates/athenaeum-core/src/api/sync.rs` at lines 751, 990-1016, 1099-1100, 1148, 1391, 1453-1454, 1503, 2323, 2385, 2553-2557, 2596, 3550, 3901, 4363-4420, 4489-4490, 4687
- Modify: `crates/athenaeum-core/src/api/collab_exchange.rs` (every `sync_paths(` use — `grep -n "sync_paths(" crates/athenaeum-core/src/api/collab_exchange.rs`)
- Modify: `crates/athenaeum-core/src/sync/receiver.rs:3684-3745` (parameter rename `sync_dir` → `working_dir`)
- Test: `crates/athenaeum-core/src/sharing/iroh/tests.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy)]
  pub struct NodeOptions { pub serve_import_mode: ImportMode }   // Default = Copy
  impl SharedIrohNode {
      pub async fn bind(sync_dir: &Path, relay_mode: RelayMode) -> Result<Arc<Self>>            // = bind_with(sync_dir, sync_dir, relay_mode, NodeOptions::default())
      pub async fn bind_with(identity_dir: &Path, working_dir: &Path, relay_mode: RelayMode, opts: NodeOptions) -> Result<Arc<Self>>
      pub fn working_dir(&self) -> &Path
      pub fn serve_import_mode(&self) -> ImportMode
  }
  ```
- `sync_paths` is deleted; every caller uses `sync_dirs`.

- [ ] **Step 1: Write the failing test** (`crates/athenaeum-core/src/sharing/iroh/tests.rs`)

```rust
#[tokio::test]
async fn bind_with_keeps_identity_in_identity_dir_and_blobs_in_working_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let identity = tmp.path().join("identity");
    let working = tmp.path().join("working");
    let node = SharedIrohNode::bind_with(&identity, &working, iroh::RelayMode::Disabled, NodeOptions::default())
        .await
        .unwrap();
    assert!(identity.join("device_key").is_file(), "key under identity dir");
    assert!(!working.join("device_key").exists(), "no key under working dir");
    assert!(working.join("blobs").join("blobs.db").is_file(), "store under working dir");
    assert_eq!(node.working_dir(), working.as_path());
    assert_eq!(node.serve_import_mode(), iroh_blobs::api::blobs::ImportMode::Copy);
    node.shutdown().await;
}
```
(If the node has no `shutdown()`, drop it instead — check how sibling tests in that file tear down a node and copy that.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p athenaeum-core --lib bind_with_keeps 2>&1 | tail -3` → `no function bind_with`.

- [ ] **Step 3: Implement `NodeOptions` + `bind_with`**

In `node.rs`, above `impl SharedIrohNode`:

```rust
/// Host-chosen node behavior (transfer-prepare spec §4.1).
#[derive(Debug, Clone, Copy)]
pub struct NodeOptions {
    /// How `serve` imports a package dir into the blob store. `TryReference`
    /// (the app: `packages/<uuid>` is an immutable snapshot) references the
    /// payload in place and stores only the outboard; `Copy` (Perseus: a resend
    /// rewrites its payloads in place) copies it into the store.
    pub serve_import_mode: ImportMode,
}

impl Default for NodeOptions {
    fn default() -> Self {
        Self { serve_import_mode: ImportMode::Copy }
    }
}
```

Add fields `working_dir: PathBuf` and `serve_import_mode: ImportMode` to `SharedIrohNode`; rename the existing `bind` body to `bind_with(identity_dir, working_dir, relay_mode, opts)`: `DeviceKey::load_or_create_in(identity_dir)`, `device_key_lock_path(identity_dir)`, `let blob_dir = working_dir.join("blobs");`, and set the two new fields in the `Self { … }` construction. Then:

```rust
    /// Single-dir bind: identity and data under the same `sync_dir`, Copy
    /// imports — Perseus and every existing test keep this shape.
    pub async fn bind(sync_dir: &Path, relay_mode: RelayMode) -> Result<Arc<Self>> {
        Self::bind_with(sync_dir, sync_dir, relay_mode, NodeOptions::default()).await
    }

    pub fn working_dir(&self) -> &Path {
        &self.working_dir
    }

    pub fn serve_import_mode(&self) -> ImportMode {
        self.serve_import_mode
    }
```

- [ ] **Step 4: Rewire the app call sites**

Replace `sync_paths` with `sync_dirs` everywhere (the function is deleted at the end of this step):

| Site | Before | After |
| ---- | ---- | ---- |
| `api/sync.rs:751` `has_pending_rows_for` | `let Ok((_sync_dir, db_path)) = sync_paths(ctx)` | `let Ok(dirs) = sync_dirs(ctx)` … `CatalogSyncStore::open(&dirs.db_path)` |
| `:990-1016` `ensure_iroh_node` | `let (sync_dir, _) = sync_paths(ctx)?; create_dir_all(&sync_dir); SharedIrohNode::bind(&sync_dir, relay_mode)` … `cleanup_orphan_blob_stores(&sync_dir)` | `let dirs = sync_dirs(ctx)?; create_dir_all(&dirs.identity_dir)?; create_dir_all(&dirs.working_dir)?; SharedIrohNode::bind_with(&dirs.identity_dir, &dirs.working_dir, relay_mode, NodeOptions { serve_import_mode: ImportMode::TryReference })` … `cleanup_orphan_blob_stores(&dirs.working_dir)` — **use `ImportMode::Copy` here until Task 5 lands** (Task 5 flips it). |
| `:1099-1100, 1148` `autostart_if_enabled` | `sync_paths` + `sync_dir.join("incoming")` + `ensure_started(node, sync_dir, db_path, …)` | `let dirs = sync_dirs(ctx)?; incoming_resolver(ctx, dirs.incoming_fallback())?; ensure_started(node, dirs.working_dir.clone(), dirs.db_path.clone(), …)` |
| `:1391` `resurrect_pending_senders` | `Ok((_sync_dir, db_path)) => db_path` | `Ok(dirs) => dirs.db_path` |
| `:1453-1454, 1503` `start_sync` (same shape as autostart) | as above | as above |
| `:2323, 2385` delete-history staging path | `sync_dir.join("staging").join(&row.package_id)` | `sync_dirs(ctx)?.staging_root().join(&row.package_id)` |
| `:2553-2557` `sender_packages_dir` | `sync_dir.join("packages")` | `Ok(sync_dirs(ctx)?.packages_dir)` |
| `:2596, 3550, 3901` | `let (_sync_dir, db_path) = sync_paths(ctx)?` | `let db_path = sync_dirs(ctx)?.db_path` |
| `:4363-4420` `sweep_orphan_payload_dirs` | `sender_packages_dir(ctx)` | unchanged (it already goes through `sender_packages_dir`) |
| `:4489-4490` `remove_terminal_staging_dirs` | `sync_dir.join("staging")` | `sync_dirs(ctx)?.staging_root()` |
| `:4687` | `sync_dir.join("packages")` | `sync_dirs(ctx)?.packages_dir` |
| `collab_exchange.rs` | each `sync_paths(` | `sync_dirs(` with `.db_path` / `.working_dir` as the site needs (collab dirs live under the working dir) |

In `receiver.rs::ensure_started` rename the `sync_dir` parameter to `working_dir` (doc: "the data dir: blob store, staging, collab") and pass `working_dir.clone()` where `sync_dir.clone()` was (the staging root at :3739). Delete `sync_paths`.

- [ ] **Step 5: Gates**

Run: `cargo check --workspace 2>&1 | tail -3`; `cargo test -p athenaeum-core --lib sharing::iroh 2>&1 | tail -3`; `cargo test -p athenaeum-core --lib api::sync 2>&1 | tail -3`; `cargo build -p perseus 2>&1 | tail -2`.
Expected: all green; Perseus untouched (`bind` wrapper).

- [ ] **Step 6: Commit**

```bash
rustfmt --edition 2021 crates/athenaeum-core/src/api/sync.rs crates/athenaeum-core/src/api/collab_exchange.rs crates/athenaeum-core/src/sharing/iroh/node.rs crates/athenaeum-core/src/sync/receiver.rs
git add -A crates/athenaeum-core/src
git commit -m "refactor(sync): route every transfer data dir through SyncDirs; bind_with(identity_dir, working_dir)"
```

---

## Phase B — Single copy

### Task 5: Sender serve import = `TryReference` on app hosts (want-subset too)

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/iroh/blobs.rs:218-330` (`import_subset_collection` gains `mode`)
- Modify: `crates/athenaeum-core/src/sharing/iroh/node.rs:2118-2175` (`role_serve` threads `self.serve_import_mode`)
- Modify: `crates/athenaeum-core/src/api/sync.rs` `ensure_iroh_node` (`ImportMode::TryReference`)
- Test: `crates/athenaeum-core/src/sharing/iroh/tests.rs`

**Interfaces:**
- Produces: `pub async fn import_subset_collection(store, pkg_dir, want, tag, mode: ImportMode)`.

- [ ] **Step 1: Write the failing tests**

```rust
/// A payload above the inline threshold, so the store must either copy or reference it.
fn write_test_package(root: &std::path::Path, files: &[(&str, usize)]) -> std::path::PathBuf {
    use crate::package::{write_package, ManifestRecord, PayloadKind, MANIFEST_VERSION};
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut records = Vec::new();
    for (name, size) in files {
        let p = src.join(name);
        let bytes: Vec<u8> = (0..*size).map(|i| (i % 253) as u8).collect();
        std::fs::write(&p, &bytes).unwrap();
        records.push((
            p.clone(),
            ManifestRecord {
                v: MANIFEST_VERSION,
                frame_uuid: format!("u-{name}"),
                origin_catalog_uuid: format!("u-{name}"),
                origin_device: "dev".into(),
                payload_kind: PayloadKind::RawFrame,
                rel_path: name.to_string(),
                byte_size: *size as u64,
                xxh3: crate::package::xxh3_full_file(&p).unwrap(),
                frame_meta: serde_json::json!({}),
                analysis: None,
                app_version: "test".into(),
                project: None,
            },
        ));
    }
    let pkg = root.join("pkg");
    write_package(&pkg, records).unwrap();
    pkg
}

fn store_holds_payload_copy(blob_dir: &std::path::Path, size: u64) -> bool {
    walkdir::WalkDir::new(blob_dir)
        .into_iter()
        .flatten()
        .any(|e| e.file_type().is_file() && e.metadata().map(|m| m.len() == size).unwrap_or(false))
}

#[tokio::test]
async fn try_reference_import_yields_same_hash_and_no_store_copy() {
    use crate::sharing::iroh::blobs::{import_package_collection_with_mode, import_subset_collection};
    use iroh_blobs::api::blobs::ImportMode;
    let tmp = tempfile::tempdir().unwrap();
    let pkg = write_test_package(tmp.path(), &[("a.fits", 300_000), ("b.fits", 300_000)]);

    let copy_store = iroh_blobs::store::fs::FsStore::load(tmp.path().join("copy")).await.unwrap();
    let ref_store = iroh_blobs::store::fs::FsStore::load(tmp.path().join("reference")).await.unwrap();
    let (h_copy, _) = import_package_collection_with_mode(&copy_store, &pkg, "t", ImportMode::Copy).await.unwrap();
    let (h_ref, _) = import_package_collection_with_mode(&ref_store, &pkg, "t", ImportMode::TryReference).await.unwrap();
    assert_eq!(h_copy, h_ref, "mode never changes the collection hash");
    assert!(store_holds_payload_copy(&tmp.path().join("copy"), 300_000));
    assert!(!store_holds_payload_copy(&tmp.path().join("reference"), 300_000), "reference: no 300 000-byte file in the store");

    // The want-subset import honors the mode too (it used to call add_path = Copy).
    let sub_store = iroh_blobs::store::fs::FsStore::load(tmp.path().join("subset")).await.unwrap();
    let want: std::collections::HashSet<String> = ["a.fits".to_string()].into_iter().collect();
    import_subset_collection(&sub_store, &pkg, &want, "t", ImportMode::TryReference).await.unwrap();
    assert!(!store_holds_payload_copy(&tmp.path().join("subset"), 300_000));
}

#[tokio::test]
async fn reimport_of_a_known_hash_from_a_new_path_serves_from_the_new_path() {
    use iroh_blobs::api::blobs::{AddPathOptions, ImportMode};
    let tmp = tempfile::tempdir().unwrap();
    let store = iroh_blobs::store::fs::FsStore::load(tmp.path().join("store")).await.unwrap();
    let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let p1 = tmp.path().join("p1.bin");
    let p2 = tmp.path().join("p2.bin");
    std::fs::write(&p1, &bytes).unwrap();
    let tag1 = store.blobs().add_path_with_opts(AddPathOptions { path: p1.clone(), format: iroh_blobs::BlobFormat::Raw, mode: ImportMode::TryReference }).temp_tag().await.unwrap();
    std::fs::remove_file(&p1).unwrap();
    std::fs::write(&p2, &bytes).unwrap();
    let tag2 = store.blobs().add_path_with_opts(AddPathOptions { path: p2.clone(), format: iroh_blobs::BlobFormat::Raw, mode: ImportMode::TryReference }).temp_tag().await.unwrap();
    assert_eq!(tag1.hash(), tag2.hash());
    let out = tmp.path().join("out.bin");
    store.blobs().export(tag2.hash(), &out).await.unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), bytes, "re-pointed to p2, readable after p1 vanished");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p athenaeum-core --lib try_reference_import 2>&1 | tail -3` → arity error on `import_subset_collection`.

- [ ] **Step 3: Implement**

`blobs.rs` — add `mode: ImportMode` to `import_subset_collection` and replace the payload branch:

```rust
            Src::Payload(abs) => {
                let size = tokio::fs::metadata(&abs)
                    .await
                    .with_context(|| format!("stat {}", abs.display()))?
                    .len();
                let tt = store
                    .blobs()
                    .add_path_with_opts(AddPathOptions {
                        path: abs.clone(),
                        format: BlobFormat::Raw,
                        mode,
                    })
                    .temp_tag()
                    .await
                    .with_context(|| format!("import blob {}", abs.display()))?;
                (tt, size)
            }
```
and add `reference = matches!(mode, ImportMode::TryReference)` to its final `debug!`.

`node.rs::role_serve`:
```rust
        let mode = self.serve_import_mode;
        let (hash, entries) = match want {
            None => blobs::import_package_collection_with_mode(&self.store, src_dir, &tag, mode).await?,
            Some(w) => blobs::import_subset_collection(&self.store, src_dir, w, &tag, mode).await?,
        };
```
Update the `import_package_collection` doc comment (the Perseus sentence stays true — it is now the *reason Perseus keeps Copy*, not the reason the app does).

`api/sync.rs::ensure_iroh_node`: `NodeOptions { serve_import_mode: ImportMode::TryReference }` with a comment pointing at spec §4.2 (packages dir immutable until post-confirm cleanup).

- [ ] **Step 4: Run tests + the existing serve/D3 suites**

Run: `cargo test -p athenaeum-core --lib sharing::iroh 2>&1 | tail -3`; `cargo test -p athenaeum-core --lib collab 2>&1 | tail -3`; `cargo test -p athenaeum-core --lib sync::engine 2>&1 | tail -3`.
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add -A crates/athenaeum-core/src
git commit -m "feat(sync): app serve import references packages in place (TryReference), subset import honors the mode"
```

---

### Task 6: Import progress → `indexing` ticks

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/mod.rs` (`TransportEvent::ImportProgress`)
- Modify: `crates/athenaeum-core/src/sharing/iroh/node.rs` (`EventDemux::route_import_progress` beside `route_serve_progress` at ~552; `role_serve` passes a progress callback)
- Modify: `crates/athenaeum-core/src/sharing/iroh/blobs.rs:130-180` (`import_package_collection_with_mode` streams `AddProgressItem`)
- Modify: `crates/athenaeum-core/src/sync/engine.rs` (`on_import_progress`)
- Modify: every exhaustive `match` on `TransportEvent` (compiler lists them; the receiver's `event_peer` + router get a no-op arm)
- Test: `crates/athenaeum-core/src/sharing/iroh/tests.rs`

**Interfaces:**
- Produces: `TransportEvent::ImportProgress { package_id: PackageId, bytes_done: u64, bytes_total: u64 }`; `pub type ImportProgressSink = Arc<dyn Fn(u64, u64) + Send + Sync>`; `import_package_collection_with_mode(store, pkg_dir, tag, mode, progress: Option<ImportProgressSink>)`; engine emits `sync-progress { stage: "indexing", bytes_done, bytes_total }`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn import_reports_byte_progress_reaching_the_total() {
    use crate::sharing::iroh::blobs::import_package_collection_with_mode;
    use iroh_blobs::api::blobs::ImportMode;
    let tmp = tempfile::tempdir().unwrap();
    let pkg = write_test_package(tmp.path(), &[("a.fits", 2_000_000), ("b.fits", 2_000_000)]);
    let store = iroh_blobs::store::fs::FsStore::load(tmp.path().join("s")).await.unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(u64, u64)>::new()));
    let sink = {
        let seen = seen.clone();
        std::sync::Arc::new(move |done: u64, total: u64| seen.lock().unwrap().push((done, total)))
    };
    import_package_collection_with_mode(&store, &pkg, "t", ImportMode::TryReference, Some(sink)).await.unwrap();
    let ticks = seen.lock().unwrap().clone();
    let last = ticks.last().copied().expect("at least the terminal tick");
    assert_eq!(last.1, 4_000_000 + std::fs::metadata(pkg.join("manifest.ndjson")).unwrap().len());
    assert_eq!(last.0, last.1, "terminal tick pins done == total");
}
```

- [ ] **Step 2: Run to verify failure** — arity error.

- [ ] **Step 3: Implement**

`blobs.rs`:
```rust
pub type ImportProgressSink = Arc<dyn Fn(u64, u64) + Send + Sync>;

pub async fn import_package_collection_with_mode(
    store: &Store,
    pkg_dir: &Path,
    tag: &str,
    mode: ImportMode,
    progress: Option<ImportProgressSink>,
) -> Result<(Hash, Vec<(String, u64)>)> {
    let files = collect_files(pkg_dir)?;
    let count = files.len();
    let bytes_total: u64 = files.iter().map(|f| f.len).sum();
    let mut bytes_before: u64 = 0;
    let mut last_tick = Instant::now();
    // … existing vectors …
    for f in &files {
        let mut stream = store
            .blobs()
            .add_path_with_opts(AddPathOptions { path: f.abs.clone(), format: BlobFormat::Raw, mode })
            .stream()
            .await;
        let mut tt: Option<TempTag> = None;
        while let Some(item) = stream.next().await {
            match item {
                AddProgressItem::CopyProgress(off) | AddProgressItem::OutboardProgress(off) => {
                    if let Some(p) = &progress {
                        if last_tick.elapsed() >= SERVE_PROGRESS_THROTTLE_IMPORT {
                            last_tick = Instant::now();
                            p(bytes_before + off.min(f.len), bytes_total);
                        }
                    }
                }
                AddProgressItem::Done(t) => tt = Some(t),
                AddProgressItem::Error(e) => {
                    return Err(anyhow::Error::from(e)).with_context(|| format!("import blob {}", f.abs.display()));
                }
                AddProgressItem::Size(_) | AddProgressItem::CopyDone => {}
            }
        }
        let tt = tt.with_context(|| format!("import blob {}: stream ended without Done", f.abs.display()))?;
        bytes_before = bytes_before.saturating_add(f.len);
        items.push((f.name.clone(), tt.hash()));
        entries.push((f.name.clone(), f.len));
        child_tags.push(tt);
    }
    if let Some(p) = &progress {
        p(bytes_total, bytes_total);
    }
    // … store_and_tag_collection + debug! unchanged …
}
const SERVE_PROGRESS_THROTTLE_IMPORT: Duration = Duration::from_millis(300);
```
Check the exact `AddProgressItem` variant names/payloads in `iroh-blobs-0.103.0/src/api/proto.rs:496-530` (`CopyProgress(u64)`, `Size(u64)`, `CopyDone`, `OutboardProgress(u64)`, `Done(TempTag)`, `Error(…)`) and adjust the arms to match exactly. `import_package_collection` (the Copy wrapper) passes `None`. The D3 seeding call site passes `None`.

`sharing/mod.rs`: add the variant with a doc ("sender-side only: the serve import is hashing the package; `bytes_total` is the package's payload bytes").

`node.rs` `EventDemux`:
```rust
    pub(crate) fn route_import_progress(&self, package_id: &PackageId, bytes_done: u64, bytes_total: u64) {
        let inner = self.inner.lock().expect("demux mutex poisoned");
        for ((_, pid), (_, tx)) in inner.claims.iter() {
            if pid == package_id {
                let _ = tx.try_send(TransportEvent::ImportProgress {
                    package_id: package_id.clone(),
                    bytes_done,
                    bytes_total,
                });
            }
        }
    }
```
`role_serve` builds `let demux = Arc::clone(&self.demux); let pid = pkg.package_id.clone(); let sink: ImportProgressSink = Arc::new(move |d, t| demux.route_import_progress(&pid, d, t));` and passes `Some(sink)` to both import fns (add the same `progress` parameter to `import_subset_collection`, streaming its payload branch the same way; the in-memory manifest blob counts as 0 bytes).

`engine.rs`: in the transport-event match add
```rust
                TransportEvent::ImportProgress { package_id, bytes_done, bytes_total } => {
                    self.on_import_progress(package_id, bytes_done, bytes_total)
                }
```
```rust
    /// Serve-import (outboard hashing) progress while the row is still `Queued`
    /// (transfer-prepare spec §4.4) — surfaced as the `indexing` stage.
    fn on_import_progress(&mut self, package_id: PackageId, bytes_done: u64, bytes_total: u64) {
        let slot = self.pending.iter().find_map(|(k, p)| match &p.announce {
            Some(a) if a.package_id == package_id => Some((*k, a.frame_count)),
            _ => None,
        });
        let Some((id, frame_count)) = slot else {
            return;
        };
        self.emit_progress_bytes(id, "indexing", frame_count, bytes_done, bytes_total);
    }
```
(If `announce` is not yet set on the slot when serve runs, key the lookup on the tag → id map the engine keeps for serve; use whatever `on_serve_progress` uses to resolve `id` — copy that resolution verbatim.) Loopback transport and the receiver router: add `TransportEvent::ImportProgress { .. } => {}` arms where the compiler demands.

- [ ] **Step 4: Gates**

Run: `cargo check --workspace 2>&1 | tail -3`; `cargo test -p athenaeum-core --lib sharing 2>&1 | tail -3`; `cargo test -p athenaeum-core --lib sync:: 2>&1 | tail -3`.

- [ ] **Step 5: Commit**

```bash
git add -A crates/athenaeum-core/src
git commit -m "feat(sync): serve-import progress as an indexing stage tick"
```

---

### Task 7: Receiver export moves out of the store (`ExportMode::TryReference`); vanished-source is transfer-class

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/iroh/blobs.rs:560-600` (export loop in `fetch_collection_to_dir`)
- Test: `crates/athenaeum-core/src/sharing/iroh/tests.rs`

**Interfaces:**
- Produces: `pub(crate) fn export_source_vanished(err: &iroh_blobs::api::RequestError) -> bool`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn export_try_reference_leaves_no_owned_copy_in_the_store() {
    use iroh_blobs::api::blobs::{ExportMode, ExportOptions};
    let tmp = tempfile::tempdir().unwrap();
    let store = iroh_blobs::store::fs::FsStore::load(tmp.path().join("s")).await.unwrap();
    let bytes: Vec<u8> = (0..500_000u32).map(|i| (i % 249) as u8).collect();
    let tag = store.blobs().add_bytes(bytes.clone()).temp_tag().await.unwrap();
    assert!(store_holds_payload_copy(&tmp.path().join("s"), 500_000), "owned before export");
    let target = tmp.path().join("staging").join("a.fits");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    store.blobs().export_with_opts(ExportOptions { hash: tag.hash(), mode: ExportMode::TryReference, target: target.clone() }).finish().await.unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), bytes);
    assert!(!store_holds_payload_copy(&tmp.path().join("s"), 500_000), "moved out: the store no longer owns a copy");
    // A second export of the same hash copies FROM the external path.
    let target2 = tmp.path().join("staging").join("b.fits");
    store.blobs().export_with_opts(ExportOptions { hash: tag.hash(), mode: ExportMode::TryReference, target: target2.clone() }).finish().await.unwrap();
    assert_eq!(std::fs::read(&target2).unwrap(), bytes);
    // And once that external path is gone, the export fails with a vanished source.
    std::fs::remove_file(&target).unwrap();
    std::fs::remove_file(&target2).unwrap();
    let target3 = tmp.path().join("staging").join("c.fits");
    let err = store.blobs().export_with_opts(ExportOptions { hash: tag.hash(), mode: ExportMode::TryReference, target: target3 }).finish().await.unwrap_err();
    assert!(crate::sharing::iroh::blobs::export_source_vanished(&err), "{err:?}");
}
```

- [ ] **Step 2: Run to verify failure** — `export_source_vanished` missing.

- [ ] **Step 3: Implement**

In `blobs.rs` replace the export loop body:

```rust
    for (name, blob_hash) in collection.iter() {
        let target = dest_dir.join(name);
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                LocalFault(anyhow::Error::new(e).context(format!("create dir {}", parent.display())))
            })?;
        }
        // Transfer-prepare spec §5.1: MOVE the store-owned data file into staging
        // (rename; EXDEV → copy, then iroh deletes its own copy either way). The
        // store then references the staged file, so the receiver holds one copy.
        let res = store
            .blobs()
            .export_with_opts(ExportOptions {
                hash: *blob_hash,
                mode: ExportMode::TryReference,
                target: target.clone(),
            })
            .finish()
            .await;
        if let Err(e) = res {
            if export_source_vanished(&e) {
                // §5.3: a same-hash sibling package's staged file was cleaned before
                // this export ran and GC has not yet dropped the dead entry. NOT a
                // local fault: the row parks Waiting, the sender re-announces, GC
                // (≤ 15 min) purges the entry and the retry re-downloads the blob.
                tracing::warn!(
                    hash = %blob_hash,
                    path = %target.display(),
                    error = %format!("{e:#}"),
                    "export source vanished; waiting for GC before retry"
                );
                return Err(anyhow::Error::new(e).context(format!("export {name}: source vanished")));
            }
            return Err(LocalFault(
                anyhow::Error::new(e).context(format!("export {name} -> {}", target.display())),
            )
            .into());
        }
    }
```
```rust
/// An export that failed because the blob's external data file is gone — the
/// only export failure that is a transfer-class error, never `LocalFault`.
pub(crate) fn export_source_vanished(err: &iroh_blobs::api::RequestError) -> bool {
    match err {
        iroh_blobs::api::RequestError::Inner { source } => {
            source.downcast_ref::<std::io::Error>().map(|e| e.kind() == std::io::ErrorKind::NotFound).unwrap_or(false)
                || source.to_string().contains("No such file")
        }
        iroh_blobs::api::RequestError::Rpc { .. } => false,
    }
}
```
Imports: `use iroh_blobs::api::blobs::{AddPathOptions, ExportMode, ExportOptions, ImportMode};`. If `RequestError::Inner.source` is not an `anyhow::Error` (check `iroh-blobs-0.103.0/src/api.rs:38-48` — `source: Error` there is iroh-blobs' own `api::Error`), match its io variant / use `to_string().contains("No such file") || contains("os error 2")` and pin the behavior with the test above — the test is the contract, the predicate adapts to the type.

- [ ] **Step 4: Gates**

Run: `cargo test -p athenaeum-core --lib sharing::iroh 2>&1 | tail -3`; `cargo test -p athenaeum-core --lib receiver 2>&1 | tail -3`.

- [ ] **Step 5: Commit**

```bash
git add -A crates/athenaeum-core/src
git commit -m "feat(sync): receiver export moves blobs out of the store; vanished-source export waits for GC"
```

---

### Task 8: Landing links instead of copying

**Files:**
- Modify: `crates/athenaeum-core/src/sync/ingest.rs:789-812` (`land_payload`)
- Test: `crates/athenaeum-core/src/sync/ingest_tests.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(unix)]
#[test]
fn land_payload_hard_links_on_the_same_volume_and_keeps_staging() {
    use std::os::unix::fs::MetadataExt;
    let tmp = tempfile::tempdir().unwrap();
    let staged = tmp.path().join("staging").join("x.fits");
    std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
    std::fs::write(&staged, b"payload").unwrap();
    let landing = tmp.path().join("incoming");
    let record = crate::package::ManifestRecord {
        v: crate::package::MANIFEST_VERSION,
        frame_uuid: "u".into(),
        origin_catalog_uuid: "u".into(),
        origin_device: "d".into(),
        payload_kind: crate::package::PayloadKind::RawFrame,
        rel_path: "sub/x.fits".into(),
        byte_size: 7,
        xxh3: crate::package::xxh3_full_file(&staged).unwrap(),
        frame_meta: serde_json::json!({}),
        analysis: None,
        app_version: "t".into(),
        project: None,
    };
    let landed = super::land_payload(&landing, &staged, &record).unwrap();
    assert_eq!(landed, landing.join("sub").join("x.fits"));
    assert!(staged.exists(), "staging copy left in place until the package epilogue");
    assert_eq!(
        std::fs::metadata(&staged).unwrap().ino(),
        std::fs::metadata(&landed).unwrap().ino(),
        "same inode: linked, not copied"
    );
    assert!(!landing.join("sub").join("x.fits.tmp").exists());
}

#[test]
fn land_payload_falls_back_to_copy_when_linking_fails() {
    // A link target on a path whose parent is a FILE cannot be linked or created
    // — use the copy fallback seam instead: link to a dest inside a read-only
    // dir is platform-dependent, so exercise the fallback through the helper.
    let tmp = tempfile::tempdir().unwrap();
    let staged = tmp.path().join("x.fits");
    std::fs::write(&staged, b"payload").unwrap();
    let tmp_dest = tmp.path().join("x.fits.tmp");
    super::link_or_copy(&staged, &tmp_dest, true).unwrap();
    assert_eq!(std::fs::read(&tmp_dest).unwrap(), b"payload");
}
```
(`land_payload` is private; the tests module is inside `ingest.rs`' crate so `super::` reaches it — put these in `ingest.rs`' own `#[cfg(test)] mod` if `ingest_tests.rs` cannot see private items.)

- [ ] **Step 2: Run to verify failure** — `link_or_copy` missing / inode mismatch.

- [ ] **Step 3: Implement**

```rust
/// Link `src` to `dest` (same volume: zero-copy, shares the inode) or, when the
/// platform/volume refuses (`EXDEV`, SMB/NFS/exFAT, permission), copy. `force_copy`
/// is the test seam for the fallback branch. Never fails a landing for a link
/// refusal — copy was the behavior before transfer-prepare spec §5.2.
fn link_or_copy(src: &Path, dest: &Path, force_copy: bool) -> Result<()> {
    if !force_copy {
        match std::fs::hard_link(src, dest) {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::debug!(
                    src = %src.display(),
                    dest = %dest.display(),
                    error = %e,
                    "hard link refused; copying instead"
                );
            }
        }
    }
    std::fs::copy(src, dest)
        .with_context(|| format!("copy payload to {}", dest.display()))?;
    Ok(())
}

fn land_payload(landing_base: &Path, payload: &Path, record: &ManifestRecord) -> Result<PathBuf> {
    let dest = unique_path(&landing_base.join(Path::new(&record.rel_path)));
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create landing dir {}", parent.display()))?;
    }
    // Link (or copy) to a sibling temp, then rename into place. The staged file
    // stays until the package epilogue removes staging — the blob store references
    // it (transfer-prepare spec §5.2), so it must outlive the collection's tag.
    let tmp = dest.with_extension(format!(
        "{}.tmp",
        dest.extension().and_then(|e| e.to_str()).unwrap_or("part")
    ));
    let _ = std::fs::remove_file(&tmp);
    link_or_copy(payload, &tmp, false)?;
    std::fs::rename(&tmp, &dest)
        .with_context(|| format!("rename landed file into {}", dest.display()))?;
    Ok(dest)
}
```

- [ ] **Step 4: Gates**

Run: `cargo test -p athenaeum-core --lib ingest 2>&1 | tail -3`; `cargo test -p athenaeum-core --lib receiver 2>&1 | tail -3`.

- [ ] **Step 5: Commit**

```bash
git add -A crates/athenaeum-core/src
git commit -m "feat(sync): land received files by hard link, copy only when the volume refuses"
```

---

## Phase C — Async preparation

### Task 9: `OutboundState::Preparing`, display mapping, `total_bytes`, preparing insert, terminal settle

**Files:**
- Modify: `crates/athenaeum-core/src/sync/models.rs:24-93`
- Modify: `crates/athenaeum-core/src/sync/status.rs` (struct at ~27; mapping at ~220; tests at ~712, ~757)
- Modify: `crates/athenaeum-core/src/sync/store.rs` (`grouped_file_counts` ~2028; `insert_outbound_with_files` ~2403; `CatalogSyncStore` impl ~2794)
- Modify: `crates/athenaeum-core/src/api/sync.rs:1699-1745` (`outbound_summary` fallback)
- Modify: `src/components/transfers/TransferRow.tsx:150-153` (`stageProgress`: add `announced`, keep `preparing`) — keeps tsc green after regen
- Test: `store.rs`/`status.rs` `mod tests`

**Interfaces:**
- Produces: `OutboundState::Preparing` (`"preparing"`, non-terminal); `outbound_display_state(Preparing) == "preparing"`, `(Announced) == "announced"`; `TransferFileCounts.total_bytes: u64`; `pub fn insert_outbound_with_files_in_state(conn, package_ref, peer_hex, display_name, files, layout, state) -> Result<i64>`; `impl CatalogSyncStore { pub fn enqueue_preparing(&self, package_ref, peer, display_name, files, layout) -> Result<i64> }`; `pub fn settle_outbound_files_terminal(conn, outbound_id, outcome, culprit: Option<(&str, &str)>) -> Result<()>` (free fn, `CatalogSyncStore::settle_files_terminal` wrapper).

- [ ] **Step 1: Write the failing tests**

`models.rs`/`status.rs`:
```rust
    #[test]
    fn preparing_roundtrips_and_is_not_terminal() {
        assert_eq!(OutboundState::Preparing.as_str(), "preparing");
        assert_eq!(OutboundState::from_db("preparing").unwrap(), OutboundState::Preparing);
        assert!(!OutboundState::Preparing.is_terminal());
    }
```
In `status.rs` update `outbound_display_state_maps_every_raw_state_without_retry` cases: add `(OutboundState::Preparing, "preparing")`, change `(OutboundState::Announced, "preparing")` → `"announced"`; in the retry test change the `Announced` expectation to `"announced"`.

`store.rs`:
```rust
    #[test]
    fn enqueue_preparing_inserts_row_and_files_in_preparing() {
        let store = CatalogSyncStore::open_in_memory_for_test();
        let files = vec![
            AnnounceFileEntry { rel_path: "a.fits".into(), byte_size: 10, frame_uuid: "ua".into() },
            AnnounceFileEntry { rel_path: "b.fits".into(), byte_size: 30, frame_uuid: "ub".into() },
        ];
        let peer = test_node_id(1);
        let id = store.enqueue_preparing("/pkg/x", peer, Some("Batch"), &files, PackageLayout::Batch).unwrap();
        let row = store.get_outbound(id).unwrap().unwrap();
        assert_eq!(row.state, OutboundState::Preparing);
        assert!(store.non_terminal().unwrap().iter().any(|r| r.id == id));
        let conn = store.conn_for_test();
        let counts = outbound_file_counts(&conn, &[id]).unwrap();
        assert_eq!(counts[&id].total, 2);
        assert_eq!(counts[&id].total_bytes, 40);
    }

    #[test]
    fn settle_outbound_files_terminal_marks_culprit_failed_and_rest_cancelled() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(DDL_OUTBOUND_FILES, []).unwrap();
        let row = |rel: &str| OutboundFileRow {
            outbound_id: 1, rel_path: rel.into(), byte_size: 10, frame_uuid: format!("u-{rel}"),
            state: OutboundFileState::Pending, bytes_done: 0, outcome: None, error: None,
            updated_at: "2026-08-30T00:00:00.000Z".into(),
        };
        replace_outbound_files(&conn, 1, &[row("a.fits"), row("b.fits")]).unwrap();
        settle_outbound_files_terminal(&conn, 1, "failed", Some(("b.fits", "read error"))).unwrap();
        let rows = list_outbound_files(&conn, 1).unwrap();
        let a = rows.iter().find(|r| r.rel_path == "a.fits").unwrap();
        let b = rows.iter().find(|r| r.rel_path == "b.fits").unwrap();
        assert_eq!((a.state, a.outcome.as_deref()), (OutboundFileState::Done, Some("failed")));
        assert_eq!((b.state, b.error.as_deref()), (OutboundFileState::Failed, Some("read error")));
        let c = outbound_file_counts(&conn, &[1]).unwrap();
        assert_eq!((c[&1].done, c[&1].failed), (1, 1));
    }
```
(`CatalogSyncStore::open_in_memory_for_test` / `conn_for_test` / `test_node_id`: use whatever helpers the existing `CatalogSyncStore` tests in `store.rs` use — grep `impl CatalogSyncStore` tests for `open(` on a tempfile path and a `NodeId` fixture, and mirror them.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p athenaeum-core --lib preparing 2>&1 | tail -3` → missing variant/functions.

- [ ] **Step 3: Implement**

`models.rs`: add `Preparing` as the first variant (doc: "Row inserted, payload not yet staged: the preparation worker is copying + hashing into `package_ref` (transfer-prepare spec §3). Cancellable; a restart heals it to `Failed`."), `as_str`/`from_db` arms `"preparing"`. `is_terminal` unchanged (it lists terminals only).

`status.rs`: `OutboundState::Preparing => "preparing", OutboundState::Announced => "announced"` and the `TransferFileCounts` field:
```rust
    /// `byte_size` sum of every row — the manifest-free total a `preparing`
    /// row's summary falls back to (spec §3.8).
    pub total_bytes: u64,
```
Update the struct doc's field list. `grouped_file_counts`: add `SUM(byte_size)` as column 6 (after `duplicate_bytes`), map it, fill `total_bytes`. Extend both existing count tests with `assert_eq!(p1.total_bytes, 70)` (7 rows × 10) and `assert_eq!(c.total_bytes, 70)`.

`store.rs`:
```rust
pub fn insert_outbound_with_files(conn, package_ref, peer_hex, display_name, files, layout) -> Result<i64> {
    insert_outbound_with_files_in_state(conn, package_ref, peer_hex, display_name, files, layout, OutboundState::Queued)
}

pub fn insert_outbound_with_files_in_state(
    conn: &Connection,
    package_ref: &str,
    peer_hex: &str,
    display_name: Option<&str>,
    files: &[AnnounceFileEntry],
    layout: PackageLayout,
    state: OutboundState,
) -> Result<i64> {
    // body of the old fn with `state.as_str()` in place of `OutboundState::Queued.as_str()`
}

/// Batch-terminal settle callable from the API layer (a preparation that
/// failed or was cancelled never reached the engine): every row not yet `done`
/// gets `done`/`outcome`; `culprit = (rel_path, error)` instead gets `failed`
/// with the error. Rows already `done` keep their verdict.
pub fn settle_outbound_files_terminal(
    conn: &Connection,
    outbound_id: i64,
    outcome: &str,
    culprit: Option<(&str, &str)>,
) -> Result<()> {
    for row in list_outbound_files(conn, outbound_id)? {
        if row.state == OutboundFileState::Done {
            continue;
        }
        match culprit {
            Some((rel, err)) if rel == row.rel_path => set_outbound_file_state(
                conn, outbound_id, &row.rel_path, OutboundFileState::Failed, row.bytes_done, Some("failed"), Some(err),
            )?,
            _ => set_outbound_file_state(
                conn, outbound_id, &row.rel_path, OutboundFileState::Done, row.bytes_done, Some(outcome), None,
            )?,
        }
    }
    Ok(())
}
```
(`set_outbound_file_state` is the free fn the trait impl delegates to — check its exact parameter order at its definition and match it.) On `CatalogSyncStore`:
```rust
    pub fn enqueue_preparing(&self, package_ref: &str, peer: NodeId, display_name: Option<&str>, files: &[AnnounceFileEntry], layout: PackageLayout) -> Result<i64> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        insert_outbound_with_files_in_state(&conn, package_ref, &node_id_hex(&peer), display_name, files, layout, OutboundState::Preparing)
    }
    pub fn settle_files_terminal(&self, id: i64, outcome: &str, culprit: Option<(&str, &str)>) -> Result<()> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        settle_outbound_files_terminal(&conn, id, outcome, culprit)
    }
```

`api/sync.rs::outbound_summary`:
```rust
    let (file_count, byte_size) = match package_totals(Path::new(&row.package_ref)) {
        (0, 0) => {
            let c = file_counts.get(&row.id).copied().unwrap_or_default();
            (c.total, c.total_bytes)
        }
        totals => totals,
    };
```

`TransferRow.tsx::stageProgress`: keep `case 'preparing': return 0.05;` and add `case 'announced':` to the 0.08 group (it is already listed there — verify; if so nothing to do).

- [ ] **Step 4: Gates**

Run: `cargo test -p athenaeum-core --lib sync:: 2>&1 | tail -3`; `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract 2>&1 | tail -2`; `npx tsc --noEmit`.
Expected: green; `models.ts` `TransferFileCounts` gains `totalBytes` (the two synthesized literals in `useTransferQueue.ts` at ~433 and ~731 need `totalBytes: 0` — add it).

- [ ] **Step 5: Commit**

```bash
git add -A crates/athenaeum-core/src src/types/models.ts src/hooks/useTransferQueue.ts src/components/transfers/TransferRow.tsx
git commit -m "feat(sync): OutboundState::Preparing, announced label, total_bytes, preparing insert + terminal settle"
```

---

### Task 10: Package writer split — `write_manifest` + `stage_payload` (reflink or one-pass copy+hash)

**Files:**
- Modify: `crates/athenaeum-core/Cargo.toml` (add `reflink-copy = "0.1"`)
- Modify: `crates/athenaeum-core/src/package/writer.rs`
- Modify: `crates/athenaeum-core/src/package/mod.rs` (re-export)
- Test: `crates/athenaeum-core/src/package/tests.rs`

**Interfaces:**
- Produces:
  ```rust
  pub fn write_manifest(dest_dir: &Path, records: &[ManifestRecord]) -> Result<()>;
  pub struct StagedPayload { pub xxh3: String, pub bytes: u64 }
  #[derive(Debug)] pub struct StageCancelled;   // Display "preparation cancelled"; std::error::Error
  pub fn stage_payload(src: &Path, dest: &Path, expected_size: u64, cancelled: &dyn Fn() -> bool, on_progress: &mut dyn FnMut(u64)) -> Result<StagedPayload>;
  ```
  `write_package_with_root_hash` keeps its signature and now calls `write_manifest`.

- [ ] **Step 1: Write the failing tests** (`package/tests.rs`)

```rust
#[test]
fn stage_payload_copies_hashes_and_reports_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.bin");
    let bytes: Vec<u8> = (0..3_000_000u32).map(|i| (i % 241) as u8).collect();
    std::fs::write(&src, &bytes).unwrap();
    let dest = tmp.path().join("pkg").join("sub").join("dst.bin");
    let mut ticks = Vec::new();
    let staged = stage_payload(&src, &dest, bytes.len() as u64, &|| false, &mut |done| ticks.push(done)).unwrap();
    assert_eq!(staged.bytes, bytes.len() as u64);
    assert_eq!(staged.xxh3, xxh3_full_file(&src).unwrap());
    assert_eq!(std::fs::read(&dest).unwrap(), bytes);
    assert_eq!(*ticks.last().unwrap(), bytes.len() as u64, "terminal tick == size");
}

#[test]
fn stage_payload_rejects_a_size_drift_and_removes_the_partial_file() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.bin");
    std::fs::write(&src, vec![1u8; 100]).unwrap();
    let dest = tmp.path().join("dst.bin");
    let err = stage_payload(&src, &dest, 200, &|| false, &mut |_| {}).unwrap_err();
    assert!(err.to_string().contains("size mismatch"), "{err:#}");
    assert!(!dest.exists());
}

#[test]
fn stage_payload_honors_cancellation() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.bin");
    std::fs::write(&src, vec![9u8; 5_000_000]).unwrap();
    let dest = tmp.path().join("dst.bin");
    let err = stage_payload(&src, &dest, 5_000_000, &|| true, &mut |_| {}).unwrap_err();
    assert!(err.downcast_ref::<StageCancelled>().is_some(), "{err:#}");
    assert!(!dest.exists());
}

#[test]
fn write_package_still_writes_the_same_manifest() {
    // existing write_package tests keep passing; this pins write_manifest alone
    let tmp = tempfile::tempdir().unwrap();
    let rec = sample_record("a.fits", 3);   // reuse the file's existing record fixture
    write_manifest(tmp.path(), std::slice::from_ref(&rec)).unwrap();
    let back = read_manifest(tmp.path()).unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].rel_path, "a.fits");
}
```

- [ ] **Step 2: Run to verify failure** — missing symbols.

- [ ] **Step 3: Implement**

`Cargo.toml` `[dependencies]`: `reflink-copy = "0.1"` (0.1.30 is already in the lock).

`writer.rs`:
```rust
use std::io::{Read, Write};

/// One-pass staging of a payload into the package (transfer-prepare spec §3.3):
/// reflink when the filesystem can (APFS/Btrfs/XFS/ReFS — then hash the clone,
/// one read), else stream-copy while hashing (one read, one write). Verifies
/// the size, removes `dest` on any failure, checks `cancelled` every 64 MiB.
pub fn stage_payload(
    src: &Path,
    dest: &Path,
    expected_size: u64,
    cancelled: &dyn Fn() -> bool,
    on_progress: &mut dyn FnMut(u64),
) -> Result<StagedPayload> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create payload dir {}", parent.display()))?;
    }
    let result = stage_payload_inner(src, dest, expected_size, cancelled, on_progress);
    if result.is_err() {
        let _ = fs::remove_file(dest);
    }
    result
}

fn stage_payload_inner(
    src: &Path,
    dest: &Path,
    expected_size: u64,
    cancelled: &dyn Fn() -> bool,
    on_progress: &mut dyn FnMut(u64),
) -> Result<StagedPayload> {
    const CHUNK: usize = 4 * 1024 * 1024;
    const CANCEL_EVERY: u64 = 64 * 1024 * 1024;
    if cancelled() {
        return Err(StageCancelled.into());
    }
    let _ = fs::remove_file(dest);
    let mut hasher = Xxh3::new();
    let mut buf = vec![0u8; CHUNK];
    let mut done: u64 = 0;
    let mut next_cancel_check = CANCEL_EVERY;
    let reflinked = reflink_copy::reflink(src, dest).is_ok();
    let mut input = fs::File::open(if reflinked { dest } else { src })
        .with_context(|| format!("open {} for staging", src.display()))?;
    let mut output = if reflinked {
        None
    } else {
        Some(fs::File::create(dest).with_context(|| format!("create {}", dest.display()))?)
    };
    loop {
        let n = input.read(&mut buf).with_context(|| format!("read {}", src.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        if let Some(out) = output.as_mut() {
            out.write_all(&buf[..n]).with_context(|| format!("write {}", dest.display()))?;
        }
        done += n as u64;
        on_progress(done);
        if done >= next_cancel_check {
            next_cancel_check += CANCEL_EVERY;
            if cancelled() {
                return Err(StageCancelled.into());
            }
        }
    }
    if let Some(out) = output.as_mut() {
        out.sync_data().ok();
    }
    if done != expected_size {
        anyhow::bail!(
            "package copy size mismatch for {}: staged {} bytes, expected {}",
            src.display(), done, expected_size
        );
    }
    Ok(StagedPayload { xxh3: format!("{:016x}", hasher.digest()), bytes: done })
}

pub struct StagedPayload { pub xxh3: String, pub bytes: u64 }

#[derive(Debug)]
pub struct StageCancelled;
impl std::fmt::Display for StageCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("preparation cancelled") }
}
impl std::error::Error for StageCancelled {}

/// `manifest.ndjson`: one compact record per line. Split out of
/// `write_package_with_root_hash` so the preparation worker can write it after
/// staging the payloads itself.
pub fn write_manifest(dest_dir: &Path, records: &[ManifestRecord]) -> Result<()> {
    fs::create_dir_all(dest_dir).with_context(|| format!("create package dir {}", dest_dir.display()))?;
    let manifest_path = dest_dir.join(MANIFEST_FILENAME);
    let mut buf = String::new();
    for r in records {
        let line = serde_json::to_string(r).context("serialize manifest record")?;
        buf.push_str(&line);
        buf.push('\n');
    }
    fs::write(&manifest_path, buf.as_bytes()).with_context(|| format!("write manifest {}", manifest_path.display()))
}
```
Make `write_package_with_root_hash` call `write_manifest(dest_dir, &manifest_records)?` instead of its inline NDJSON block. `xxh3_full_file`'s hex format is `format!("{:016x}", hasher.digest())` — confirm in `package/mod.rs:144-160` and use the identical formatting. Re-export `stage_payload`, `write_manifest`, `StagedPayload`, `StageCancelled` from `package/mod.rs`.

- [ ] **Step 4: Gates**

Run: `cargo test -p athenaeum-core --lib package 2>&1 | tail -3`; `cargo build -p perseus 2>&1 | tail -2` (Perseus uses `write_package`).

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/Cargo.toml Cargo.lock crates/athenaeum-core/src/package
git commit -m "feat(package): stage_payload (reflink or one-pass copy+hash) and write_manifest split"
```

---

### Task 11: Engine `Command::Drive` + `SyncEngineHandle::drive`

**Files:**
- Modify: `crates/athenaeum-core/src/sync/engine.rs:241-300` (enum), `~681-730` (handle), `~1021` (dispatch), `~2165-2205` (`resend_package` → shared `drive_package`)
- Test: `crates/athenaeum-core/src/sync/engine_tests.rs`

**Interfaces:**
- Produces: `Command::Drive(i64)`; `pub async fn drive(&self, id: i64) -> Result<()>` — "a `queued` row exists on disk and in the DB; read it and drive it like a crash-resume".

- [ ] **Step 1: Write the failing test** (`engine_tests.rs`, using that file's loopback harness — `build_package` at :48 and whatever `spawn_pair`/loopback helper the neighbouring tests use)

```rust
#[tokio::test]
async fn drive_picks_up_a_pre_inserted_queued_row_and_confirms_it() {
    let tmp = tempfile::tempdir().unwrap();
    let pkg = build_package(&tmp.path().join("src"), "uuid-1", "a.fits", "M31", 4096);
    // The API layer inserted the row itself (as the preparation worker will).
    let (sender_store, engine, _receiver) = loopback_pair(&tmp).await;   // this file's existing pair helper
    let files = vec![AnnounceFileEntry { rel_path: "a.fits".into(), byte_size: 4096, frame_uuid: "uuid-1".into() }];
    let id = sender_store.enqueue(&pkg.to_string_lossy(), engine_peer(&engine), Some("Batch"), &files, PackageLayout::Batch).unwrap();
    engine.drive(id).await.unwrap();
    wait_for_state(&sender_store, id, OutboundState::Confirmed).await;   // this file's polling helper
}
```
Adapt the helper names to the ones that exist in `engine_tests.rs` (the file already spawns loopback engines and polls states — reuse those exact helpers; do not invent new ones).

- [ ] **Step 2: Run to verify failure** — `no method drive`.

- [ ] **Step 3: Implement**

```rust
    /// Drive a row the API layer already inserted as `Queued` (a finished
    /// preparation, transfer-prepare spec §3.3): read it and start it like a
    /// crash-resume. The row is NOT in the in-memory `pending` map (it never
    /// passed through `Process`), so a `Kick` could not reach it.
    Drive(i64),
```
```rust
    pub async fn drive(&self, id: i64) -> Result<()> {
        self.cmd_tx
            .send(Command::Drive(id))
            .await
            .map_err(|_| anyhow!("sync engine worker stopped"))?;
        Ok(())
    }
```
Dispatch: `Some(Command::Drive(id)) => self.drive_package(id, "drive").await, Some(Command::Resend(id)) => self.drive_package(id, "resend").await,` and rename `resend_package` to `drive_package(&mut self, id: i64, why: &'static str)`, using `why` in its log lines (`tracing::warn!(package_id = id, why, "outbound row vanished; ignoring")` etc.).

- [ ] **Step 4: Gates**

Run: `cargo test -p athenaeum-core --lib sync::engine 2>&1 | tail -3`.

- [ ] **Step 5: Commit**

```bash
git add -A crates/athenaeum-core/src
git commit -m "feat(sync): Command::Drive — drive a pre-inserted queued row"
```

---

### Task 12: Preparation worker + enqueue split (row first, copy in the background)

**Files:**
- Modify: `crates/athenaeum-core/src/sync/sender.rs` (`PrepareRuntime` + field on `SyncSenderRuntime`)
- Create: `crates/athenaeum-core/src/api/sync_prepare.rs`; register `pub mod sync_prepare;` in `crates/athenaeum-core/src/api/mod.rs`
- Modify: `crates/athenaeum-core/src/api/sync.rs:3040-3300` (`build_selection_package` → `plan_selection_package`; `build_and_enqueue_selection`, `enqueue_built`, `enqueue_frame_set_send`)
- Test: `crates/athenaeum-core/src/api/sync.rs` `mod tests` (loopback engine via `test_ctx` + the existing `build_and_enqueue_selection` tests as the model)

**Interfaces:**
- Produces:
  ```rust
  // sync/sender.rs
  pub struct PrepareRuntime { slot: Arc<tokio::sync::Semaphore>, cancels: std::sync::Mutex<HashMap<i64, Arc<AtomicBool>>> }
  impl PrepareRuntime { pub fn new() -> Self; pub fn register(&self, id: i64) -> Arc<AtomicBool>; pub fn cancel(&self, id: i64) -> bool; pub fn finish(&self, id: i64); pub fn is_preparing(&self, id: i64) -> bool; pub fn slot(&self) -> Arc<Semaphore> }
  impl SyncSenderRuntime { pub fn prepare(&self) -> &Arc<PrepareRuntime> }
  // api/sync_prepare.rs
  pub struct PrepareJob { pub id: i64, pub peer: NodeId, pub pkg_dir: PathBuf, pub records: Vec<(PathBuf, ManifestRecord)> /* xxh3 = "" */, pub bank: Vec<BankCandidate>, pub engine: Arc<SyncEngineHandle>, pub emitter: Option<Arc<dyn ProgressEmitter>> }
  pub struct BankCandidate { pub file_id: i64, pub path: PathBuf, pub size: i64, pub modified_at: String, pub rel_path: String }
  pub fn spawn_prepare(ctx: Arc<ServiceContext>, sender: Arc<SyncSenderRuntime>, job: PrepareJob)
  pub(crate) fn fail_preparing_row(store: &CatalogSyncStore, id: i64, peer: NodeId, pkg_dir: &Path, reason: &str, culprit: Option<(&str, &str)>, emitter: Option<&dyn ProgressEmitter>)
  ```
- Consumes: `stage_payload`, `write_manifest` (Task 10); `enqueue_preparing`, `settle_files_terminal` (Task 9); `engine.drive` (Task 11); `sync_dirs` (Task 1).

- [ ] **Step 1: Write the failing tests** (`api/sync.rs` `mod tests`; model them on the existing `build_and_enqueue_selection` loopback tests in the same module — reuse their engine/peer fixtures)

```rust
    /// Poll the catalog store until the row reaches `want` (or 10 s pass).
    async fn wait_state(ctx: &ServiceContext, id: i64, want: OutboundState) -> OutboundRow {
        let db_path = sync_dirs(ctx).unwrap().db_path;
        let store = CatalogSyncStore::open(&db_path).unwrap();
        for _ in 0..200 {
            let row = store.get_outbound(id).unwrap().unwrap();
            if row.state == want { return row; }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("row {id} never reached {want:?}");
    }

    #[tokio::test]
    async fn enqueue_returns_a_preparing_row_then_prepares_and_confirms() {
        let (tmp, ctx, engine, peer) = loopback_ctx_with_engine().await;   // the module's existing fixture
        let frame_ids = seed_two_frames_on_disk(&ctx, tmp.path(), 1_500_000);   // the module's existing seeding helper
        let started = std::time::Instant::now();
        let result = build_and_enqueue_selection(&ctx, &engine, "origin", &sync_dirs(&ctx).unwrap().packages_dir, &frame_ids, Some("Batch"), None, peer, None, Arc::new(SyncSenderRuntime::new()))
            .await
            .unwrap();
        assert_eq!(result.enqueued_count, 2);
        assert!(started.elapsed() < std::time::Duration::from_millis(500), "returns before copying");
        let id = result.outbound_id.expect("row id");
        let row = CatalogSyncStore::open(&sync_dirs(&ctx).unwrap().db_path).unwrap().get_outbound(id).unwrap().unwrap();
        assert!(matches!(row.state, OutboundState::Preparing | OutboundState::Queued | OutboundState::Announced | OutboundState::Transferring | OutboundState::Confirmed));
        let row = wait_state(&ctx, id, OutboundState::Confirmed).await;
        let dir = PathBuf::from(&row.package_ref);
        assert!(dir.join("manifest.ndjson").is_file());
        let manifest = crate::package::read_manifest(&dir).unwrap();
        assert_eq!(manifest.len(), 2);
        for r in &manifest {
            assert_eq!(r.xxh3.len(), 16, "hash filled by the worker");
        }
    }

    #[tokio::test]
    async fn cancelling_a_preparing_row_removes_the_dir_and_settles_files() {
        let (tmp, ctx, engine, peer) = loopback_ctx_with_engine().await;
        let frame_ids = seed_two_frames_on_disk(&ctx, tmp.path(), 200_000_000);   // big enough to be mid-copy
        let sender = Arc::new(SyncSenderRuntime::new());
        let result = build_and_enqueue_selection(&ctx, &engine, "origin", &sync_dirs(&ctx).unwrap().packages_dir, &frame_ids, Some("Batch"), None, peer, None, Arc::clone(&sender))
            .await
            .unwrap();
        let id = result.outbound_id.unwrap();
        assert!(sender.prepare().cancel(id), "cancel flag set while preparing");
        let row = wait_state(&ctx, id, OutboundState::Cancelled).await;
        assert!(!Path::new(&row.package_ref).exists(), "partial dir removed");
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let c = outbound_file_counts(&conn, &[id]).unwrap()[&id];
        assert_eq!(c.done, c.total, "files settled cancelled");
    }

    #[tokio::test]
    async fn a_source_that_vanishes_mid_preparation_fails_the_row_honestly() {
        let (tmp, ctx, engine, peer) = loopback_ctx_with_engine().await;
        let frame_ids = seed_two_frames_on_disk(&ctx, tmp.path(), 1_000_000);
        // Delete the SECOND source after the stat pre-flight but before the worker reaches it:
        // remove it right after enqueue returns (the worker starts with the first file).
        let sender = Arc::new(SyncSenderRuntime::new());
        let second_path = source_path_of(&ctx, frame_ids[1]);
        let result = build_and_enqueue_selection(&ctx, &engine, "origin", &sync_dirs(&ctx).unwrap().packages_dir, &frame_ids, Some("Batch"), None, peer, None, Arc::clone(&sender))
            .await
            .unwrap();
        std::fs::remove_file(&second_path).unwrap();
        let id = result.outbound_id.unwrap();
        let row = wait_state(&ctx, id, OutboundState::Failed).await;
        assert!(row.last_error.as_deref().unwrap_or("").starts_with("preparation failed:"), "{:?}", row.last_error);
        assert!(!Path::new(&row.package_ref).exists());
    }
```
`build_and_enqueue_selection` grows three parameters (`peer: NodeId`, `emitter: Option<Arc<dyn ProgressEmitter>>`, `sender: Arc<SyncSenderRuntime>`) and `EnqueueSelectionResult` grows `outbound_id: Option<i64>` (`#[serde(skip_serializing_if = "Option::is_none")]`, additive for the frontend). If the module's loopback fixtures have different names, use the existing ones (grep `fn loopback` / `spawn` in `api/sync.rs` tests) — the assertions are the contract. A flaky "returns before copying" bound: 500 ms is generous for two `stat`s + one insert; keep it.

- [ ] **Step 2: Run to verify failure** — signature mismatch / missing `outbound_id`.

- [ ] **Step 3: `PrepareRuntime` in `sync/sender.rs`**

```rust
use std::sync::atomic::{AtomicBool, Ordering};

/// Preparation admission + cancel flags (transfer-prepare spec §3.3/§3.4):
/// one package stages at a time (`slot`), each in-flight preparation owns a
/// cancel flag keyed by its outbound row id.
pub struct PrepareRuntime {
    slot: Arc<tokio::sync::Semaphore>,
    cancels: std::sync::Mutex<HashMap<i64, Arc<AtomicBool>>>,
}

impl PrepareRuntime {
    pub fn new() -> Self {
        Self { slot: Arc::new(tokio::sync::Semaphore::new(1)), cancels: std::sync::Mutex::new(HashMap::new()) }
    }
    pub fn slot(&self) -> Arc<tokio::sync::Semaphore> { Arc::clone(&self.slot) }
    pub fn register(&self, id: i64) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        self.cancels.lock().expect("prepare mutex poisoned").insert(id, Arc::clone(&flag));
        flag
    }
    /// Raise the flag; `false` when `id` is not preparing (already handed to the engine or terminal).
    pub fn cancel(&self, id: i64) -> bool {
        match self.cancels.lock().expect("prepare mutex poisoned").get(&id) {
            Some(f) => { f.store(true, Ordering::SeqCst); true }
            None => false,
        }
    }
    pub fn is_preparing(&self, id: i64) -> bool {
        self.cancels.lock().expect("prepare mutex poisoned").contains_key(&id)
    }
    pub fn finish(&self, id: i64) {
        self.cancels.lock().expect("prepare mutex poisoned").remove(&id);
    }
}

impl Default for PrepareRuntime { fn default() -> Self { Self::new() } }
```
Add `prepare: Arc<PrepareRuntime>` to `SyncSenderRuntime` (init in `new()`), `pub fn prepare(&self) -> &Arc<PrepareRuntime>`. Export `PrepareRuntime` from `sync/mod.rs`.

- [ ] **Step 4: `api/sync_prepare.rs`**

```rust
//! Preparation worker (transfer-prepare spec §3): stages a planned package into
//! `packages/<uuid>`, hashes it, writes the manifest, then hands the `queued`
//! row to the engine. One package at a time; cancellable; honest terminal on
//! failure; healed to `failed` after a restart.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::api::{db, ApiError};
use crate::events::{emit_event, ProgressEmitter};
use crate::package::{stage_payload, write_manifest, ManifestRecord, StageCancelled};
use crate::services::ServiceContext;
use crate::sharing::types::NodeId;
use crate::sync::engine::SyncEngineHandle;
use crate::sync::receiver::{SyncFileProgressEvent, SyncFinishedEvent, SyncProgressEvent};
use crate::sync::store::{CatalogSyncStore, SyncStore};
use crate::sync::{node_id_hex, Direction, OutboundState, SyncSenderRuntime};

const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(300);

pub struct BankCandidate {
    pub file_id: i64,
    pub path: PathBuf,
    pub size: i64,
    pub modified_at: String,
    pub rel_path: String,
}

pub struct PrepareJob {
    pub id: i64,
    pub peer: NodeId,
    pub pkg_dir: PathBuf,
    /// `(source, record)` with `record.xxh3` empty — filled by the worker.
    pub records: Vec<(PathBuf, ManifestRecord)>,
    pub bank: Vec<BankCandidate>,
    pub engine: Arc<SyncEngineHandle>,
    pub emitter: Option<Arc<dyn ProgressEmitter>>,
}

enum PrepareError {
    Cancelled,
    Failed { reason: String, culprit: Option<(String, String)> },
}

/// Fire-and-forget: acquires the single preparation slot, stages on a blocking
/// thread, then flips the row and drives it — or terminalizes it.
pub fn spawn_prepare(ctx: Arc<ServiceContext>, sender: Arc<SyncSenderRuntime>, job: PrepareJob) {
    let flag = sender.prepare().register(job.id);
    let slot = sender.prepare().slot();
    tokio::spawn(async move {
        let _permit = match slot.acquire_owned().await {
            Ok(p) => p,
            Err(_) => {
                tracing::error!(package_id = job.id, "prepare slot closed");
                sender.prepare().finish(job.id);
                return;
            }
        };
        let id = job.id;
        let peer = job.peer;
        let pkg_dir = job.pkg_dir.clone();
        let engine = Arc::clone(&job.engine);
        let emitter = job.emitter.clone();
        let ctx2 = Arc::clone(&ctx);
        let flag2 = Arc::clone(&flag);
        let started = Instant::now();
        let outcome = tokio::task::spawn_blocking(move || run_prepare(&ctx2, job, flag2)).await;
        sender.prepare().finish(id);
        let store = match sync_store(&ctx) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(package_id = id, error = %format!("{e:#}"), "prepare: open store failed");
                return;
            }
        };
        match outcome {
            Ok(Ok(stats)) => {
                if let Err(e) = store.set_state(id, OutboundState::Queued) {
                    tracing::error!(package_id = id, error = %format!("{e:#}"), "prepare: set queued failed");
                    return;
                }
                let _ = store.append_sync_event(
                    Direction::Sent,
                    &id.to_string(),
                    "prepared",
                    Some(&format!("files={} bytes={} duration_ms={}", stats.files, stats.bytes, started.elapsed().as_millis())),
                );
                tracing::info!(package_id = id, files = stats.files, bytes = stats.bytes, duration_ms = started.elapsed().as_millis() as u64, "package prepared");
                if let Err(e) = engine.drive(id).await {
                    tracing::error!(package_id = id, error = %format!("{e:#}"), "prepare: drive failed");
                }
            }
            Ok(Err(PrepareError::Cancelled)) => {
                terminalize(&store, id, peer, &pkg_dir, OutboundState::Cancelled, None, "cancelled", None, emitter.as_deref());
                tracing::info!(package_id = id, "preparation cancelled");
            }
            Ok(Err(PrepareError::Failed { reason, culprit })) => {
                let msg = format!("preparation failed: {reason}");
                tracing::error!(package_id = id, error = %reason, "preparation failed");
                let culprit_ref = culprit.as_ref().map(|(r, e)| (r.as_str(), e.as_str()));
                terminalize(&store, id, peer, &pkg_dir, OutboundState::Failed, Some(&msg), "failed", culprit_ref, emitter.as_deref());
            }
            Err(join) => {
                let msg = format!("preparation failed: worker panicked: {join}");
                tracing::error!(package_id = id, error = %join, "preparation worker panicked");
                terminalize(&store, id, peer, &pkg_dir, OutboundState::Failed, Some(&msg), "failed", None, emitter.as_deref());
            }
        }
    });
}

struct PrepareStats { files: usize, bytes: u64 }

fn run_prepare(ctx: &ServiceContext, mut job: PrepareJob, flag: Arc<AtomicBool>) -> Result<PrepareStats, PrepareError> {
    let cancelled = || flag.load(Ordering::SeqCst);
    let total: u64 = job.records.iter().map(|(_, r)| r.byte_size).sum();
    let frame_count = job.records.len() as u32;
    let peer_hex = node_id_hex(&job.peer);
    let mut done_before: u64 = 0;
    let mut last_tick = Instant::now() - PROGRESS_MIN_INTERVAL;
    let emit_batch = |emitter: &Option<Arc<dyn ProgressEmitter>>, bytes_done: u64| {
        if let Some(em) = emitter {
            emit_event(em.as_ref(), "sync-progress", &SyncProgressEvent {
                package_id: job.id.to_string(),
                direction: Direction::Sent,
                stage: "preparing".to_string(),
                peer_device: peer_hex.clone(),
                frame_count,
                project_id: None,
                bytes_done: Some(bytes_done),
                bytes_total: Some(total),
            });
        }
    };
    std::fs::create_dir_all(&job.pkg_dir).map_err(|e| PrepareError::Failed { reason: format!("create {}: {e}", job.pkg_dir.display()), culprit: None })?;
    let mut hashes: Vec<String> = Vec::with_capacity(job.records.len());
    for (src, record) in job.records.iter() {
        let dest = job.pkg_dir.join(&record.rel_path);
        let mut file_last = Instant::now() - PROGRESS_MIN_INTERVAL;
        let emitter = job.emitter.clone();
        let rel = record.rel_path.clone();
        let size = record.byte_size;
        let id = job.id;
        let peer_hex2 = peer_hex.clone();
        let mut on_progress = |file_done: u64| {
            if last_tick.elapsed() >= PROGRESS_MIN_INTERVAL || file_done == size {
                last_tick = Instant::now();
                emit_batch(&emitter, done_before + file_done);
            }
            if let Some(em) = &emitter {
                if file_last.elapsed() >= PROGRESS_MIN_INTERVAL || file_done == size {
                    file_last = Instant::now();
                    emit_event(em.as_ref(), "sync-file-progress", &SyncFileProgressEvent {
                        package_id: id.to_string(),
                        peer_device: peer_hex2.clone(),
                        file: rel.clone(),
                        bytes_done: file_done,
                        bytes_total: size,
                    });
                }
            }
        };
        let staged = stage_payload(src, &dest, size, &cancelled, &mut on_progress).map_err(|e| {
            if e.downcast_ref::<StageCancelled>().is_some() {
                PrepareError::Cancelled
            } else {
                PrepareError::Failed { reason: format!("{}: {e:#}", record.rel_path), culprit: Some((record.rel_path.clone(), format!("{e:#}"))) }
            }
        })?;
        done_before += staged.bytes;
        hashes.push(staged.xxh3);
    }
    for ((_, record), h) in job.records.iter_mut().zip(hashes.iter()) {
        record.xxh3 = h.clone();
    }
    let records: Vec<ManifestRecord> = job.records.iter().map(|(_, r)| r.clone()).collect();
    write_manifest(&job.pkg_dir, &records).map_err(|e| PrepareError::Failed { reason: format!("manifest: {e:#}"), culprit: None })?;
    // Bank the full hashes as `files.strong_hash` where the disk still matches the row.
    let by_rel: std::collections::HashMap<&str, &str> = records.iter().map(|r| (r.rel_path.as_str(), r.xxh3.as_str())).collect();
    let bank: Vec<(i64, String)> = job.bank.iter().filter_map(|c| {
        let h = by_rel.get(c.rel_path.as_str())?;
        crate::duplicates::backfill::disk_matches_row(&c.path, c.size, &c.modified_at).then(|| (c.file_id, (*h).to_string()))
    }).collect();
    if let Ok(db) = db(ctx) {
        crate::api::sync::bank_manifest_hashes(&db.conn(), &bank);
    }
    emit_batch(&job.emitter, total);
    Ok(PrepareStats { files: records.len(), bytes: total })
}

fn sync_store(ctx: &ServiceContext) -> Result<CatalogSyncStore, ApiError> {
    let dirs = crate::api::sync::sync_dirs(ctx)?;
    CatalogSyncStore::open(&dirs.db_path).map_err(|e| ApiError::Internal(format!("open catalog sync store: {e:#}")))
}

/// Terminalize a row that never reached the engine: remove the partial dir,
/// stamp the state + reason, settle files, journal, emit `sync-finished`.
#[allow(clippy::too_many_arguments)]
fn terminalize(
    store: &CatalogSyncStore,
    id: i64,
    peer: NodeId,
    pkg_dir: &Path,
    state: OutboundState,
    last_error: Option<&str>,
    outcome: &str,
    culprit: Option<(&str, &str)>,
    emitter: Option<&dyn ProgressEmitter>,
) {
    if let Err(e) = std::fs::remove_dir_all(pkg_dir) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(package_id = id, path = %pkg_dir.display(), error = %e, "remove partial package dir failed");
        }
    }
    if let Err(e) = store.set_state(id, state) { tracing::error!(package_id = id, error = %format!("{e:#}"), "prepare: set terminal state failed"); }
    if let Err(e) = store.set_last_error(id, last_error) { tracing::warn!(package_id = id, error = %format!("{e:#}"), "prepare: set last_error failed"); }
    if let Err(e) = store.settle_files_terminal(id, outcome, culprit) { tracing::warn!(package_id = id, error = %format!("{e:#}"), "prepare: settle files failed"); }
    let _ = store.append_sync_event(Direction::Sent, &id.to_string(), if state == OutboundState::Cancelled { "cancelled" } else { "prepare_failed" }, last_error);
    if let Some(em) = emitter {
        emit_event(em, "sync-finished", &SyncFinishedEvent {
            package_id: id.to_string(),
            direction: Direction::Sent,
            outcome: outcome.to_string(),
            peer_device: node_id_hex(&peer),
            ok_count: 0,
            failed: Vec::new(),
            new_count: 0,
            duplicate_count: 0,
            project_id: None,
        });
    }
}

/// Startup heal (spec §3.6): every `preparing` row → `failed`, partial dir removed.
pub fn heal_interrupted_preparations(ctx: &ServiceContext) -> Result<usize, ApiError> {
    let store = sync_store(ctx)?;
    let rows = store.non_terminal().map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    let mut healed = 0usize;
    for row in rows.into_iter().filter(|r| r.state == OutboundState::Preparing) {
        tracing::warn!(package_id = row.id, path = %row.package_ref, "preparation interrupted by a restart; failing the row");
        terminalize(&store, row.id, row.peer, Path::new(&row.package_ref), OutboundState::Failed,
            Some("preparation interrupted by a restart — send again"), "failed", None, None);
        healed += 1;
    }
    Ok(healed)
}
```
`bank_manifest_hashes` becomes `pub(crate)`. Make sure `CatalogSyncStore` exposes `set_state`/`set_last_error`/`append_sync_event`/`non_terminal` — they are `SyncStore` trait methods, so `use crate::sync::store::SyncStore` brings them in.

- [ ] **Step 5: Enqueue split in `api/sync.rs`**

Rename `build_selection_package` → `plan_selection_package` and change its per-entry body: keep `stat`, `byte_size`, `frame_meta`, `analysis`, `frame_uuid`, `rel_path` dedup; **remove** the `xxh3_full_file` call (`xxh3: String::new()`) and the `disk_matches_row` bank push, collecting instead
```rust
        if is_catalog_file {
            bank.push(BankCandidate { file_id, path: path.to_path_buf(), size: file.size, modified_at: file.modified_at.to_rfc3339(), rel_path: rel_path.clone() });
        }
```
Remove the `write_package` call and the `bank_manifest_hashes` call at the end; return
```rust
pub(crate) struct PlannedSelection {
    pub(crate) pkg_dir: Option<PathBuf>,
    pub(crate) records: Vec<(PathBuf, ManifestRecord)>,
    pub(crate) bank: Vec<BankCandidate>,
    pub(crate) eligible: Vec<i64>,
    pub(crate) ineligible: Vec<IneligibleFrame>,
    pub(crate) total: usize,
    pub(crate) display_name: Option<String>,
    pub(crate) files: Vec<AnnounceFileEntry>,
}
```
(`files` built from `records` as before, `pkg_dir = Some(packages_dir.join(uuid))` when records are non-empty). Replace `enqueue_built` with:
```rust
/// Insert the `preparing` row (name + per-file rows in one tx) and hand the
/// staging to the worker. Returns the row id, or `None` when nothing was eligible.
async fn enqueue_planned(
    ctx: &Arc<ServiceContext>,
    sender: &Arc<SyncSenderRuntime>,
    engine: &Arc<SyncEngineHandle>,
    peer: NodeId,
    planned: PlannedSelection,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<Option<i64>, ApiError> {
    let Some(pkg_dir) = planned.pkg_dir.clone() else {
        return Ok(None);
    };
    let store = {
        let dirs = sync_dirs(ctx)?;
        CatalogSyncStore::open(&dirs.db_path).map_err(|e| ApiError::Internal(format!("open catalog sync store: {e:#}")))?
    };
    let id = store
        .enqueue_preparing(&pkg_dir.to_string_lossy(), peer, planned.display_name.as_deref(), &planned.files, PackageLayout::Batch)
        .map_err(|e| ApiError::Internal(format!("insert preparing row: {e:#}")))?;
    let byte_size: u64 = planned.files.iter().map(|f| f.byte_size).sum();
    let _ = store.append_sync_event(Direction::Sent, &id.to_string(), "enqueued", Some(&format!("frames={} bytes={}", planned.files.len(), byte_size)));
    tracing::info!(package_id = id, state = "preparing", files = planned.files.len(), bytes = byte_size, "sync state");
    crate::api::sync_prepare::spawn_prepare(
        Arc::clone(ctx),
        Arc::clone(sender),
        crate::api::sync_prepare::PrepareJob { id, peer, pkg_dir, records: planned.records, bank: planned.bank, engine: Arc::clone(engine), emitter },
    );
    Ok(Some(id))
}
```
(`PackageLayout::Batch` is what `enqueue_built` passed for app sends — keep whatever it passed.) `build_and_enqueue_selection` and `enqueue_frame_set_send` call `plan_selection_package` under the DB borrow, then `enqueue_planned`, and put the id into `EnqueueSelectionResult.outbound_id`. Both public commands pass `&sender` / `dest.node` / `emitter` through (the Tauri/Axum wrappers already have `state.sync_sender`; add the argument).

Add `pub outbound_id: Option<i64>` (`#[serde(skip_serializing_if = "Option::is_none")]`) to `EnqueueSelectionResult`; regenerate TS.

- [ ] **Step 6: Gates**

Run: `cargo test -p athenaeum-core --lib api::sync 2>&1 | tail -5` (the three new tests + every existing enqueue test); `cargo check --workspace`; `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`; `npx tsc --noEmit`.

- [ ] **Step 7: Commit**

```bash
git add -A crates/athenaeum-core/src crates/athenaeum-tauri/src crates/athenaeum-web/src src/types/models.ts
git commit -m "feat(sync): async preparation — preparing row first, staged + hashed in a worker with progress, cancel, honest failure"
```

---

### Task 13: Cancel routing + startup heal wired into both hosts

**Files:**
- Modify: `crates/athenaeum-core/src/api/sync.rs:3844-3852` (`cancel_sync_package`), `:1082-1100` (`autostart_if_enabled` head)
- Modify: `crates/athenaeum-tauri/src/commands/sync.rs:211`, `crates/athenaeum-web/src/routes/sync.rs:283`
- Test: `crates/athenaeum-core/src/api/sync.rs` `mod tests`

**Interfaces:**
- `pub async fn cancel_sync_package(ctx: &ServiceContext, sender: &Arc<SyncSenderRuntime>, id: i64) -> Result<(), ApiError>`.

- [ ] **Step 1: Write the failing tests**

```rust
    #[tokio::test]
    async fn heal_marks_a_preparing_row_failed_and_removes_its_dir() {
        let (tmp, ctx) = test_ctx();
        let dirs = sync_dirs(&ctx).unwrap();
        let pkg = dirs.packages_dir.join("half-done");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("a.fits"), b"partial").unwrap();
        let store = CatalogSyncStore::open(&dirs.db_path).unwrap();
        let files = vec![AnnounceFileEntry { rel_path: "a.fits".into(), byte_size: 7, frame_uuid: "u".into() }];
        let id = store.enqueue_preparing(&pkg.to_string_lossy(), test_peer(), Some("B"), &files, PackageLayout::Batch).unwrap();
        assert_eq!(crate::api::sync_prepare::heal_interrupted_preparations(&ctx).unwrap(), 1);
        let row = store.get_outbound(id).unwrap().unwrap();
        assert_eq!(row.state, OutboundState::Failed);
        assert!(row.last_error.unwrap().contains("interrupted"));
        assert!(!pkg.exists());
        drop(tmp);
    }

    #[tokio::test]
    async fn cancel_routes_to_the_prepare_flag_while_preparing() {
        let (_tmp, ctx) = test_ctx();
        let sender = Arc::new(SyncSenderRuntime::new());
        let flag = sender.prepare().register(42);
        cancel_sync_package(&ctx, &sender, 42).await.unwrap();
        assert!(flag.load(std::sync::atomic::Ordering::SeqCst));
    }
```

- [ ] **Step 2: Run to verify failure** — arity error on `cancel_sync_package`.

- [ ] **Step 3: Implement**

```rust
pub async fn cancel_sync_package(ctx: &ServiceContext, sender: &Arc<SyncSenderRuntime>, id: i64) -> Result<(), ApiError> {
    // A preparing row never reached an engine: raise its flag; the worker
    // terminalizes it (transfer-prepare spec §3.4).
    if sender.prepare().cancel(id) {
        tracing::info!(package_id = id, "sync package cancel requested (preparing)");
        return Ok(());
    }
    let engine = active_engine_for_row(sender, id).await?;
    engine.cancel(id).await.map_err(|e| ApiError::Internal(format!("cancel package {id}: {e:#}")))?;
    tracing::info!(package_id = id, "sync package cancel requested");
    let _ = ctx;
    Ok(())
}
```
(`ctx` is reserved for the row lookup the engine path may need later; if clippy objects to the unused binding, drop the parameter and the wrapper arg.)

`autostart_if_enabled`: before `let dev = dev_pairing_enabled(ctx)?;` add
```rust
    match crate::api::sync_prepare::heal_interrupted_preparations(ctx) {
        Ok(0) => {}
        Ok(n) => tracing::warn!(count = n, "healed interrupted preparations"),
        Err(e) => tracing::error!(error = %format!("{e:#}"), "heal interrupted preparations failed"),
    }
```
(before the autostart gate, so it runs on every boot).

Wrappers: Tauri `api::cancel_sync_package(&state.ctx, &state.sync_sender, id)`; Axum `api::cancel_sync_package(&state.ctx, &state.sync_sender, args.id)`.

- [ ] **Step 4: Gates**

Run: `cargo test -p athenaeum-core --lib api::sync 2>&1 | tail -3`; `cargo check --workspace`.

- [ ] **Step 5: Commit**

```bash
git add -A crates/athenaeum-core/src crates/athenaeum-tauri/src crates/athenaeum-web/src
git commit -m "feat(sync): cancel reaches a preparing row; interrupted preparations heal to failed at startup"
```

---

## Phase D — UI

### Task 14: Transfers rows — `preparing`, `indexing`, `announced`

**Files:**
- Modify: `src/components/transfers/presentation.ts:160-200` (chip map), `:216-245` (sublines)
- Modify: `src/components/transfers/TransferRow.tsx:216-240` (counts/bytes), `:352-380` (line), `:150-175` (`stageProgress`)
- Modify: `src/components/transfers/TransfersPanel.tsx:352-372`
- Modify: `src/hooks/useTransferQueue.ts:505-520` (`isTransferring`, `speedBps` gates)

- [ ] **Step 1: `presentation.ts`**

Chip map: keep `case 'preparing'` (label `preparing`, `CHIP_MUTED`) and `case 'announced'`; add
```ts
    case 'indexing':
      return { label: 'preparing', className: CHIP_MUTED };
```
Sublines (`displayStateSubline`):
```ts
  if (displayState === 'indexing') return 'indexing — hashing the package for transfer';
  if (displayState === 'announced' && kind === 'outbound') return 'waiting for the peer to start pulling';
```

- [ ] **Step 2: `useTransferQueue.ts`** — in the active-outbound mapping (~line 505):

```ts
        // Transfer-prepare spec §7.1: a `queued` row whose last live tick was the
        // serve import renders as `indexing` (same chip as preparing).
        displayState:
          s.displayState === 'queued' && liveOutboundStage.get(s.id) === 'indexing'
            ? 'indexing'
            : s.displayState,
        speedBps:
          s.state === 'transferring' || s.state === 'preparing' || liveOutboundStage.get(s.id) === 'indexing'
            ? (live?.speedBps ?? null)
            : null,
        isTransferring:
          s.state === 'transferring' || s.state === 'preparing' || liveOutboundStage.get(s.id) === 'indexing',
```
(`OutboundState` on the TS side is the generated union — `'preparing'` exists after Task 9's regen.)

- [ ] **Step 3: `TransferRow.tsx`**

`stageProgress`: add `case 'indexing': return 0.05;` next to `preparing`. In `LiveRowBody`:
```ts
  // Preparing/indexing: no file has moved yet — show the file total + bytes, not "N of M".
  const preparingLike = row.displayState === 'preparing' || row.displayState === 'indexing';
  const compactCounts = row.terminal || row.fileCounts.total === 0;
```
and in the line:
```tsx
            {compactCounts ? (
              …unchanged…
            ) : preparingLike ? (
              <>
                <span className="tabular-nums">
                  {travelFiles} file{travelFiles === 1 ? '' : 's'}
                </span>
                <span aria-hidden="true">·</span>
                <span className="tabular-nums">
                  {formatBytes(row.bytesDone)} / {formatBytes(travelBytes)}
                </span>
              </>
            ) : (
              …existing N of M branch…
            )}
```
The Cancel affordance already covers "ANY non-terminal row" (comment at :287) — `preparing` is non-terminal; verify the button renders for it (no code change expected).

- [ ] **Step 4: `TransfersPanel.tsx`** — outbound mini-row: when `row.displayState === 'preparing' || row.displayState === 'indexing'` render `{formatBytes(row.bytesDone)} / {formatBytes(row.byteSize)}` in place of `N of M` (import `formatBytes` from wherever `TransferRow` gets it).

- [ ] **Step 5: Gate**

Run: `npx tsc --noEmit`. Then `npm run dev:web` against `cargo run -p athenaeum-web` (or `npm run tauri dev`) and send a ≥ 1 GB selection: the row appears at once with the `preparing` chip and a moving byte bar; Cancel removes it; after `queued` the `indexing` subline shows briefly; then `announced` / `transferring` as before.

- [ ] **Step 6: Commit**

```bash
git add src
git commit -m "feat(ui): Transfers rows show preparing / indexing / announced with byte progress"
```

---

### Task 15: Settings → Transfers tab

**Files:**
- Create: `src/components/settings/TransfersSection.tsx`
- Modify: `src/components/settings/SyncSection.tsx` (remove the storage card :435-470, upload-limit card :472-505, concurrent-receives card :507-560 and their state/handlers; keep status + pairing)
- Modify: `src/pages/Settings.tsx` (tab union, button, tab body)

- [ ] **Step 1: Move the three cards**

Cut from `SyncSection.tsx` into `TransfersSection.tsx`, unchanged in behavior: the `storage`/`cleaning` state + `refreshStorage` + `handleCleanup` (lines ~80-84, 129-137, 188-220), the upload-limit state/helpers/handler (`BYTES_PER_MB`, `MIN_LIMIT_MB`, `bytesToMbInput`, lines ~85-91, 228-282) and card (~472-505), the concurrent-receives state/helpers/handler (`MIN_RECEIVES`…`receivesToInput`, ~96-99, 285-320) and card (~507-560), and the storage card (~435-470). `SyncSection` keeps its account/status/ticket parts; delete the now-unused imports (`TransferStorage`, `TransferCleanup`, `formatBytes` if unused).

- [ ] **Step 2: Add the Folders cards to `TransfersSection.tsx`**

```tsx
import { useCallback, useEffect, useRef, useState } from 'react';
import { FolderOpen, RotateCcw, AlertTriangle } from 'lucide-react';
import { api } from '../../api';
import { pickDirectory } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { FolderBrowserModal } from '../FolderBrowserModal';
import { useNotifications } from '../../contexts/NotificationContext';
import type { TransferPaths, PathSetting } from '../../types/models';

function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** One folder card: effective path, default hint, Choose… / Use default, restart badge. */
function FolderCard({
  title,
  hint,
  setting,
  onChoose,
  onReset,
  error,
  busy,
}: {
  title: string;
  hint: string;
  setting: PathSetting;
  onChoose: () => void;
  onReset: () => void;
  error: string | null;
  busy: boolean;
}) {
  return (
    <div className="rounded-lg border border-border bg-surface p-3">
      <div className="flex items-center justify-between gap-3">
        <h4 className="text-sm font-medium text-content-secondary">{title}</h4>
        {setting.restartRequired && (
          <span className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-medium bg-warning/15 text-warning">
            <AlertTriangle size={11} /> Restart Athenaeum to apply
          </span>
        )}
      </div>
      <p className="mt-1 font-mono text-xs text-content break-all" title={setting.effective}>
        {setting.effective}
      </p>
      <p className="mt-1 text-[11px] text-content-muted">
        {setting.configured ? `Default: ${setting.default}` : 'Default location'} · {hint}
      </p>
      {error && <p className="mt-1 text-[11px] text-error">{error}</p>}
      <div className="mt-2 flex items-center gap-2">
        <button
          type="button"
          onClick={onChoose}
          disabled={busy}
          className="inline-flex items-center gap-1 rounded border border-border bg-surface-elevated px-2 py-1 text-xs text-content hover:bg-surface disabled:opacity-50"
        >
          <FolderOpen size={12} /> Choose…
        </button>
        {setting.configured && (
          <button
            type="button"
            onClick={onReset}
            disabled={busy}
            className="inline-flex items-center gap-1 rounded px-2 py-1 text-xs text-content-muted hover:text-content disabled:opacity-50"
          >
            <RotateCcw size={12} /> Use default
          </button>
        )}
      </div>
    </div>
  );
}

export default function TransfersSection() {
  const { notify } = useNotifications();
  const mounted = useRef(true);
  const [paths, setPaths] = useState<TransferPaths | null>(null);
  const [pathError, setPathError] = useState<{ outgoing: string | null; working: string | null }>({ outgoing: null, working: null });
  const [savingPaths, setSavingPaths] = useState(false);
  const [browsing, setBrowsing] = useState<'outgoing' | 'working' | null>(null);
  // … moved state from SyncSection (storage, cleaning, uploadMb…, receives…) …

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const refreshPaths = useCallback(async () => {
    try {
      const p = await api.invoke<TransferPaths>('get_transfer_paths');
      if (mounted.current) setPaths(p);
    } catch (err) {
      console.error('[transfers] get_transfer_paths failed:', err);
    }
  }, []);

  useEffect(() => {
    refreshPaths();
    // … refreshStorage() + the moved setting loads …
  }, [refreshPaths]);

  const applyPaths = async (outgoing: string | null | undefined, working: string | null | undefined) => {
    if (!paths) return;
    setSavingPaths(true);
    setPathError({ outgoing: null, working: null });
    try {
      const next = await api.invoke<TransferPaths>('set_transfer_paths', {
        outgoing: outgoing === undefined ? paths.outgoing.configured : outgoing,
        working: working === undefined ? paths.working.configured : working,
      });
      if (!mounted.current) return;
      setPaths(next);
      notify({
        kind: 'generic',
        tone: 'success',
        title: 'Transfer folders saved',
        detail: next.working.restartRequired ? 'The working folder applies after a restart.' : undefined,
      });
      // refreshStorage();
    } catch (err) {
      console.error('[transfers] set_transfer_paths failed:', err);
      const msg = errMsg(err);
      if (mounted.current) {
        setPathError(
          msg.startsWith('Incoming working folder')
            ? { outgoing: null, working: msg }
            : { outgoing: msg, working: null },
        );
      }
    } finally {
      if (mounted.current) setSavingPaths(false);
    }
  };

  const choose = async (which: 'outgoing' | 'working') => {
    if (isTauri()) {
      const picked = await pickDirectory();
      if (!picked) return;
      await applyPaths(which === 'outgoing' ? picked : undefined, which === 'working' ? picked : undefined);
    } else {
      setBrowsing(which);
    }
  };

  return (
    <div className="space-y-6">
      <div>
        <h4 className="text-sm font-medium text-content-secondary mb-2">Folders</h4>
        <div className="space-y-3">
          {paths && (
            <>
              <FolderCard
                title="Outgoing staging folder"
                hint="Prepared sends are staged here until the receiver confirms them."
                setting={paths.outgoing}
                onChoose={() => choose('outgoing')}
                onReset={() => applyPaths(null, undefined)}
                error={pathError.outgoing}
                busy={savingPaths}
              />
              <FolderCard
                title="Incoming working folder"
                hint="Downloads are verified here before landing in your Incoming folder. Same disk as Incoming = no extra copy."
                setting={paths.working}
                onChoose={() => choose('working')}
                onReset={() => applyPaths(undefined, null)}
                error={pathError.working}
                busy={savingPaths}
              />
            </>
          )}
        </div>
      </div>
      {/* Bandwidth card (moved) */}
      {/* Receiving card (moved) */}
      {/* Storage card (moved) + leftovers row: */}
      {/* {storage && storage.leftoverBytes > 0 && (
            <div className="mt-2 flex items-center justify-between gap-3 text-xs text-content-muted">
              <span>Leftovers in previous folders: <span className="text-content-secondary">{formatBytes(storage.leftoverBytes)}</span></span>
              <button type="button" onClick={handleCleanupLeftovers} className="…">Clean up</button>
            </div>
          )} */}
      {browsing && (
        <FolderBrowserModal
          onSelect={(path) => {
            const which = browsing;
            setBrowsing(null);
            void applyPaths(which === 'outgoing' ? path : undefined, which === 'working' ? path : undefined);
          }}
          onClose={() => setBrowsing(null)}
        />
      )}
    </div>
  );
}
```
Replace the three `{/* … (moved) */}` placeholders with the cut JSX; `handleCleanupLeftovers` calls `api.invoke<number>('cleanup_transfer_leftovers')`, notifies `Freed X`, then `refreshStorage()`; on a `Conflict` error it shows the backend message (the restart hint). Check `FolderBrowserModal`'s actual props (`src/components/FolderBrowserModal.tsx:16-30`) and pass exactly those.

- [ ] **Step 3: `Settings.tsx`**

`type SettingsTab = 'general' | 'transfers' | 'calibration' | 'analysis' | 'plate_solving';`, add `'transfers'` to `validTabs`, a tab button after General (icon `ArrowLeftRight` from lucide, label `Transfers`), and the body:
```tsx
      {activeTab === 'transfers' && (
        <div className="mb-6 bg-surface-elevated rounded-lg p-6">
          <h3 className="text-lg font-semibold mb-4 flex items-center gap-2">
            <ArrowLeftRight size={20} />
            Transfers
          </h3>
          <p className="text-xs text-content-muted mb-4">
            Where transfers keep their working data, how fast they may upload, how many may arrive at once.
          </p>
          <TransfersSection />
        </div>
      )}
```
Update the General Sync card's blurb to drop the moved knobs.

- [ ] **Step 4: Gates**

Run: `npx tsc --noEmit`; run the app; Settings → Transfers: both cards show the defaults from the §6.1 table for this OS; choosing a folder inside a monitored root is rejected inline; choosing a fresh folder saves; the working card shows the restart badge until relaunch; Storage shows per-folder sizes; General keeps Account + pairing only.

- [ ] **Step 5: Commit**

```bash
git add src
git commit -m "feat(ui): Settings → Transfers tab — staging/working folders, bandwidth, receiving, storage"
```

---

### Task 16: Ledger, docs, CLAUDE.md

**Files:**
- Modify: `docs/superpowers/open-items.md` (new "Unverified by hand" block, release-note lines)
- Modify: `CLAUDE.md` (the "Transfers / Personal Sync" bullets: `packages/` rationale, TryReference, prepare state, folder settings)
- Modify: `crates/athenaeum-tauri/scripts/uninstall-macos.sh`, `uninstall-linux.sh` (comment: custom transfer folders are not removed)

- [ ] **Step 1: open-items block** (newest first, above the counter block)

```markdown
### Transfer preparation + single-copy footprint (2026-08-30)

Spec `docs/superpowers/specs/2026-08-30-transfer-prepare-and-footprint-design.md`.

- Send a ≥ 20 GB object from this Mac: the dialog closes in < 1 s, the row reads
  `preparing · 300 files · X / Y · speed`, Cancel mid-way removes the row's dir,
  a fresh send prepares while a second one waits in `preparing` at 0 B.
- After confirm, `<packages>/<uuid>` is manifest-only and `blobs/` on the Mac
  stays in the tens of MB (outboards only).
- Receive on the pod (ext4): `du blobs/` drops to KB right after export, landed
  files share inodes with `staging/` until confirm, storage card matches `du`.
- Kill the app mid-preparation: on relaunch the row is `failed — preparation
  interrupted`, its dir is gone.
- Settings → Transfers: move both folders, restart, send + receive again;
  Storage shows the leftovers in the old folder and Clean up frees them.
- Perseus resend against the same receiver still lands (Copy path untouched).
```
Release-note lines: "Send returns instantly with a visible preparing row", "one copy per transfer on both ends", "Settings → Transfers tab with configurable folders".

- [ ] **Step 2: CLAUDE.md** — in the Transfers bullet list, update the frame-set-send bullet's neighbors: replace "iroh-blobs serve imports with `ImportMode::Copy` deliberately" wording (wherever the packages rationale is stated) with the §4 stance, add one bullet for `OutboundState::Preparing` + the worker, one for the two folders + `SyncDirs`.

- [ ] **Step 3: Commit**

```bash
git add docs CLAUDE.md crates/athenaeum-tauri/scripts
git commit -m "docs: transfer-prepare cycle — smoke list, release-note lines, CLAUDE.md sync bullets"
```

---

### Task 17: ETA from the median speed, not the live EMA (owner request, 2026-08-30)

**Files:**
- Modify: `src/hooks/useTransferQueue.ts:60-100` (`LiveBytes`, `SpeedTrackerEntry`, `trackBytes`), the `sync-progress` listener (~line 340), the row-model type (~line 172) and every `speedBps:` literal (~515, 567, 611, 649, 689, 747)
- Modify: `src/components/transfers/TransferRow.tsx:234-236`
- Modify: `src/components/transfers/presentation.ts:37-48` (`formatEta` doc only)

**Interfaces:**
- Produces: `LiveBytes.etaBps: number | null` (median of the recent instantaneous rates; `null` until `SPEED_MEDIAN_MIN_SAMPLES` samples exist); `TransferRowModel.etaBps: number | null`. `speedBps` (EMA) keeps driving the speed label unchanged.
- Rationale: the EMA over ~3 samples is right for the *speed* label (it should move) but wrong for the ETA — a single-stream QUIC transfer swings 0.5–5 MB/s minute to minute (measured on the LDN 1272 send), so a 3-sample ETA jumps between "40m" and "4h". A median over a longer window is robust to those swings and to the zero-progress ticks between files.

- [ ] **Step 1: Extend the tracker** (`useTransferQueue.ts`)

```ts
/** How many instantaneous-rate samples feed the ETA median. At the backend's
 *  ~300 ms tick cadence this is roughly the last 15–40 s of transfer. */
const SPEED_MEDIAN_WINDOW = 48;
/** Below this many samples the median is meaningless; the ETA stays hidden. */
const SPEED_MEDIAN_MIN_SAMPLES = 6;

interface LiveBytes {
  bytesDone: number;
  bytesTotal: number;
  /** Smoothed bytes/sec for the speed label (EMA, ~3 samples). */
  speedBps: number | null;
  /** Median bytes/sec over the recent window — the ETA's basis. `null` until
   *  `SPEED_MEDIAN_MIN_SAMPLES` increasing samples have arrived. */
  etaBps: number | null;
}

interface SpeedTrackerEntry {
  lastTs: number;
  lastBytes: number;
  ema: number | null;
  /** Ring of the last `SPEED_MEDIAN_WINDOW` instantaneous rates (bytes/sec). */
  samples: number[];
}

/** Median of a non-empty array (copy + sort; the window is tiny). */
function medianOf(samples: number[]): number {
  const sorted = [...samples].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 1 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

function trackBytes(
  store: Map<string, SpeedTrackerEntry>,
  key: string,
  bytesDone: number,
  bytesTotal: number,
): LiveBytes {
  const now = Date.now();
  const prev = store.get(key);
  let ema = prev?.ema ?? null;
  const samples = prev?.samples ?? [];
  if (prev && bytesDone > prev.lastBytes && now > prev.lastTs) {
    const rate = ((bytesDone - prev.lastBytes) / (now - prev.lastTs)) * 1000;
    ema = ema == null ? rate : SPEED_EMA_ALPHA * rate + (1 - SPEED_EMA_ALPHA) * ema;
    samples.push(rate);
    if (samples.length > SPEED_MEDIAN_WINDOW) samples.splice(0, samples.length - SPEED_MEDIAN_WINDOW);
  }
  store.set(key, { lastTs: now, lastBytes: bytesDone, ema, samples });
  const etaBps = samples.length >= SPEED_MEDIAN_MIN_SAMPLES ? medianOf(samples) : null;
  return { bytesDone, bytesTotal, speedBps: ema, etaBps };
}
```

- [ ] **Step 2: Reset the window on a stage change**

In the `sync-progress` listener, before calling `trackBytes` for an outbound package, compare `p.stage` with the last recorded stage (`liveOutboundStage`); when it differs (`preparing` → `indexing` → `transferring` are different pipes with different speeds), `outSpeedRef.current.delete(`out:${id}`)` first so the median restarts. Inbound has one stage (`fetching`); no reset needed there.

- [ ] **Step 3: Thread `etaBps` through the row model**

Add `etaBps: number | null` to `TransferRowModel` next to `speedBps`, and in every row construction set it the same way `speedBps` is set (`live?.etaBps ?? null` where `speedBps` reads `live?.speedBps`, `null` where `speedBps` is `null`).

- [ ] **Step 4: Use it for the ETA only** (`TransferRow.tsx`)

```ts
  const speedLabel = row.isTransferring ? formatSpeed(row.speedBps) : null;
  const remaining = Math.max(0, travelBytes - row.bytesDone);
  // ETA from the median (robust to the minute-scale swings of one QUIC stream);
  // hidden until the window has enough samples — never an "∞" or a wild first guess.
  const eta =
    row.isTransferring && remaining > 0 && row.etaBps != null
      ? formatEta(remaining, row.etaBps)
      : null;
```
Update `formatEta`'s doc comment in `presentation.ts` to say the caller passes the median rate.

- [ ] **Step 5: Gate**

Run: `npx tsc --noEmit`. Run the app and send/receive a multi-GB batch: the speed label keeps moving; the ETA appears after ~6 progress ticks and stays within a narrow band instead of jumping with every burst; after a stage change (`preparing` → `transferring`) the ETA disappears and returns once the new window fills.

- [ ] **Step 6: Commit**

```bash
git add src
git commit -m "feat(ui): transfer ETA from the median speed window instead of the live EMA"
```

---

## Self-review

**Spec coverage.** §3.1 → T9; §3.2 → T12 (`plan_selection_package`, `enqueue_planned`); §3.3 worker/reflink/progress → T10 + T12; §3.4 cancel → T12/T13; §3.5 failure → T12 (`terminalize`); §3.6 heal → T12/T13; §3.7 (no change); §3.8 `total_bytes` → T9; §4.1 mode/subset → T5; §4.2 lifecycle (no code); §4.3 Perseus (bind wrapper, T4); §4.4 indexing → T6; §5.1 → T7; §5.2 → T8; §5.3 → T7; §5.5 (no change); §6.1–6.2 → T1; §6.3 → T2; §6.4 restart flag → T3; §6.5 leftovers → T3; §6.6 commands → T3; §7.1 → T14; §7.2 (dialog unchanged; notification title tweak folded into T14 if desired); §7.3 → T15; §8 compat → T9/T3; §9 → across tasks; §10 tests → each task; §11 files → file map; §12 decisions → no code.

**Placeholder scan.** The three `{/* … (moved) */}` markers in T15 point at concrete existing JSX by line range; T11/T12 name the existing loopback fixtures to reuse rather than inventing them — executors must grep the named helpers first. No TBD/TODO.

**Type consistency.** `sync_dirs` (T1) used by T3/T4/T12/T13; `validate_transfer_dir` (T2) by T3; `enqueue_preparing` / `settle_files_terminal` (T9) by T12; `stage_payload` / `write_manifest` / `StageCancelled` (T10) by T12; `drive` (T11) by T12; `PrepareRuntime::{register,cancel,finish,slot}` (T12) by T13; `total_bytes` (T9) by T12's tests; `import_package_collection_with_mode(…, mode, progress)` (T6) — T5's tests call it with four args and must be updated to pass `None` when T6 lands (T6 step 3 note). `PathSetting.restartRequired` (T3) read by T15.
