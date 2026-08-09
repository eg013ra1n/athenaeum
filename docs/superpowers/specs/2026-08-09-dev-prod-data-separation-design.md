# Dev/Prod Data Separation — Design

**Date:** 2026-08-09
**Status:** Approved (brainstorm 2026-08-09)

## Problem

On a machine that runs both the installed (release) Athenaeum and `npm run tauri dev`, both processes resolve the same app-data directory (`~/Library/Application Support/com.vsharifov.athenaeum` on macOS) and therefore the same `athenaeum.db`, `sync/` identity, `account/` token, logs, and Gaia catalogs. Every dev launch mutates the production catalog. There is no way to use the production app on the dev machine without dev sessions corrupting it.

## Decision

Debug builds automatically use a **sibling app-data directory with a `.dev` suffix**: `com.vsharifov.athenaeum.dev`. Release builds are byte-for-byte unaffected. No workflow change — `npm run tauri dev` is isolated by construction, the same way debug builds already default to the test hub instead of the production hub (`settings/mod.rs::defaults::ACCOUNT_HUB_URL`).

An env var `ATHENAEUM_APP_DATA_DIR` (explicit directory path) overrides the resolution entirely, in both debug and release — the escape hatch for bug-triage against a user's copied data dir, or for deliberately pointing a debug build at production data.

## Architecture

### 1. Single source of truth in core

New helper in `athenaeum-core` (suggested home: a small `paths` module or alongside `logging`):

- `app_data_dir_name() -> &'static str` — returns `com.vsharifov.athenaeum`, or `com.vsharifov.athenaeum.dev` under `cfg(debug_assertions)`.

Consumers:

- **`core/src/logging/mod.rs::resolve_app_data_dir`** — replaces its three hardcoded `com.vsharifov.athenaeum` literals with the helper. Additionally honors `ATHENAEUM_APP_DATA_DIR` (checked before the platform default; existing `ATHENAEUM_LOG_DIR` > `ATHENAEUM_DB_PATH` precedence above it is unchanged).
- **`athenaeum-tauri`** — new `resolve_app_data_dir(&AppHandle) -> Result<PathBuf, String>` helper: if `ATHENAEUM_APP_DATA_DIR` is set, use it verbatim; otherwise take Tauri's `app_data_dir()` **parent** and join `app_data_dir_name()` (never string-mangle the last component; `set_extension` would corrupt a dotted identifier). Call sites converted (the only three that touch `app_data_dir()` directly):
  - `commands/core.rs::initialize_database`
  - `commands/files.rs::get_database_path`
  - `lib.rs` legacy-cache cleanup (~line 155)

Everything else already follows the DB path's parent and needs **no change**: `sync/` (iroh identity + blobs, `api/sync.rs`), `account/` (token file, `api/account.rs::resolve_config`), Gaia catalogs (`commands/plate_solve.rs` passes `db.path().parent()` down), log dir when `ATHENAEUM_DB_PATH` is set (web/Docker).

### 2. Keychain — no change needed

Verified during brainstorm: debug builds are already **file-only** for the hub token (`api/account.rs::AccountConfig::token_store`, `cfg!(debug_assertions)` branch — ad-hoc code signatures make keychain grants useless in dev). The token file lives in `<db_parent>/account/`, so it moves to the `.dev` tree automatically. A dev sign-out can never touch the production keychain entry, by construction. The `KEYRING_SERVICE` constant stays as-is.

### 3. Sync / account identity

The dev tree starts with no `sync/` and no `account/` → the dev app is a **fresh, signed-out device** with its own iroh identity. This is deliberate: dev appears as its own device on the (test) hub; production device identity is never shared or cloned.

### 4. log-mcp

`crates/log-mcp/src/query.rs` keeps its deliberately-duplicated resolver (dependency-free of core) but learns to enumerate log files from **both** sibling dirs — `…athenaeum/logs` and `…athenaeum.dev/logs` — merging chronologically exactly as it already merges rotated files within one dir. A dev session then sees both the production app's and the dev run's logs. Existing env overrides keep working; a dir that doesn't exist is silently skipped.

### 5. Snapshot script — `npm run dev:db-refresh`

`scripts/dev-db-refresh.mjs` (Node, no new deps; macOS/Linux — Windows may run it but the symlink step is skipped):

1. Resolve the platform prod dir and the `.dev` sibling; create the `.dev` dir if missing.
2. Copy `athenaeum.db` — prefer `sqlite3 <prod> ".backup <dev>"` (WAL-safe); if the `sqlite3` CLI is unavailable, fall back to copying `athenaeum.db` + `-wal` + `-shm` and print a warning to close the production app first.
3. **Wipe identity-bound transfer state** in the dev copy — the same 8 tables the batch-model upgrade reset wipes (`sync_outbound`, `sync_inbound`, `sync_outbound_files`, `sync_inbound_files`, `sync_events`, `sync_receipts`, `sync_sources`, `sync_history`). The dev copy has a fresh device identity; inherited outbound rows would otherwise be resurrected at startup pointing at payload dirs that don't exist in the dev tree. Catalog tables are untouched.
4. Symlink `catalogs/` → the prod `catalogs/` if not already present (Gaia tiers are multi-GB and read-mostly; a dev-triggered tier download writes into the shared dir — acceptable, additive).
5. Never copy `sync/`, `account/`, or `logs/`.

Run on demand; without it the dev app starts with an empty catalog (normal `init_db`).

## Known limitations (accepted)

- **DB separation ≠ file separation.** A snapshot of the prod catalog points at the same FITS files on disk. Move / Archive / Black-Hole / calibration outputs in a dev session operate on the real files. Unchanged from today; the catalog is protected, the files are not.
- **WebView storage stays shared.** The Tauri `identifier` is not changed, so localStorage (notification history, UI state) is keyed identically for the dev run and the installed app. Cosmetic.
- Uninstall scripts are not updated: the `.dev` tree only ever exists on development machines, never on end-user installs (release builds never create it).

## What was verified to not break

- **Release builds**: the suffix exists only under `debug_assertions`; the env override is additive.
- **Web/Docker**: DB path comes from `ATHENAEUM_DB_PATH` (default `/data`) — untouched. A local debug `cargo run -p athenaeum-web` with no env vars only moves its *log* fallback to `.dev/logs`, which nothing depends on.
- **Real-data `#[ignore]` tests** (`blind_gate.rs`, `registration_e2e.rs`, `api/lights.rs` e2e, solvemyastro diagnostics) hardcode the prod path as string literals for locating real fixtures and never call the resolver — unaffected in debug test runs.
- **Frontend**: zero direct `appDataDir` usage in `src/` — all paths come from backend commands.
- **Token store / keychain**: no code path change; verified file-only in debug already.

## Testing

- Unit test on the core helper: debug name carries the `.dev` suffix; `ATHENAEUM_APP_DATA_DIR` wins in the tauri resolver (pure-logic part testable without an AppHandle).
- Existing gates: `cargo build --workspace`, `cargo test -p athenaeum-core`, `npx tsc --noEmit`.
- Manual smoke: `npm run tauri dev` → About/DB path shows the `.dev` dir; production app keeps its catalog; `npm run dev:db-refresh` produces a working dev catalog with empty transfer tables; `log-mcp` returns events from both trees.
