# Stage 1.5 — Sync Hardening — Design — 2026-07-08

**Status:** Approved in brainstorm 2026-07-08, pending owner review of the written spec. **Owner:** Vilen.
**Inputs:** Stage I design `2026-07-06-personal-sync-design.md` (transport, engine, Perseus, ingest — all shipped on `0.4.0`), Stage II design `2026-07-06-collaboration-projects-design.md` (consumes §2's `collaboration` root), A5 report `.superpowers/sdd/task-A5-report.md` (follow-ups #3/#4 close here). Runs in parallel with the owner's Stage I gates (A9 soak, M-Sync1 runbook); blocks nothing in them and depends on nothing in them.

---

## 1. Scope

Four workstreams, all on the shipped Stage I foundation:

1. **Configurable landing directories** — sync-incoming and collaboration roots managed through the existing scan-roots directory manager.
2. **Blob-store cleanup** — release package blobs on both sides once they are no longer needed; startup sweep for already-accumulated orphans.
3. **Perseus** — multiple capture directories + a minimal local web status page (Syncthing-style) with sent-file status, manual delete of confirmed files, and retention-policy editing.
4. **History/status polish (both sides)** — device names instead of hex node ids, transfer duration/speed, an explicit "confirmed — safe to delete" marker.

**Out of scope (decided in brainstorm):** remote management of Perseus from the primary app (config/commands over hub or iroh control channel — next stage, the web page's API is shaped so it can become that protocol's local backend); TOTP/2FA (email OTP stays; revisit with the Stage II/III portal where sessions are frequent); moving already-landed files out of app-data (they are cataloged; the dual-pane browser moves them safely if wanted).

## 2. Landing directories via scan roots

`scan_roots.kind` gains two values beside the existing `calibration_library`:

- **`sync_incoming`** — where the primary's receiver lands ingested payloads. Layout under the root is unchanged from Stage I: `<root>/<origin_device_short>/<date>/`. Resolution order in `ingest`: designated `sync_incoming` root → fallback to the current `<app-data>/sync/incoming` (Stage I behavior, nothing breaks on upgrade).
- **`collaboration`** — reserved for Stage II (`<CollabRoot>/<project-slug>/<member>/…` per its §7). Stage 1.5 only lets the user designate it in the directory manager and stores it; no consumer yet. Stage II's first-open storage prompt will create/select through this same mechanism instead of inventing its own.

Rules, both kinds: **at most one root per kind**, enforced with the same code-level SELECT-then-INSERT check as `calibration_library` (`api::scan_roots::check_library_root_uniqueness` generalizes to a per-kind check; same benign TOCTOU caveat). Designation UI: the existing directory manager in Settings (kind picker/badge on the root row), no new surface. A `sync_incoming` root is a normal scan root in every other respect — the scanner may watch it, but receiver-ingested files are already cataloged at landing, so a scan pass over them is a no-op by path (same invariant as masters in the Calibration Library root).

Signed-out / unconfigured UX: when no `sync_incoming` root is designated, Settings → Sync shows a persistent hint, and the first landed package after each app start raises one `sync`-kind notification ("files are landing in the app data folder — designate a sync folder in Settings"), deduped by key.

## 3. Blob-store cleanup

Stage I pins every package collection with a permanent tag and never deletes anything (A5 follow-up #4): the capture node keeps a full second copy of everything it ever sent (surviving even retention deletion of the capture file), and the primary keeps a full second copy of everything it ever received. Fix:

- **`SharingTransport::release(package_id)`** — new trait method: "this package's payload data is no longer needed locally". Iroh impl: delete the collection's permanent tag (children become unreachable) and trigger the blob store's GC; also drop the package from the in-memory `served` map (A5 follow-up #3). Loopback impl: no-op (plus a test-observable release log for engine tests).
- **Sender-side call sites:** package enters `confirmed` (the engine's terminal happy state); package abandoned as terminally `failed` (attempts exhausted — the payload files themselves are untouched, only the blob copies go).
- **Receiver-side call site:** after the ack for a fetched package is sent (receipts durable, staging already exported) — the fetched blobs have served their purpose.
- **Startup sweep (both roles):** on transport start, enumerate the store's tags and release every package whose engine/receipt state is terminal. This retroactively cleans the orphans accumulated before this fix on both existing deployments. Sweep failures log `warn` and never block startup.
- Ack-replay compatibility: the replay guard answers re-announces **from the receipt log**, never from blobs, so releasing receiver-side blobs does not break replay. A sender re-announcing an already-released package re-imports from the source files (which retention guarantees still exist for non-confirmed packages; a confirmed package is never re-announced).

## 4. Perseus: multiple capture dirs + local web page

**Config:** `capture_dirs = ["…", "…"]` (array). The old singular `capture_dir` stays readable as an alias (exactly one of the two forms allowed; both → config error). One watcher per directory over the shared engine/seen-store; per-dir `baseline` counts in `status` and the web page.

**Web page:** embedded axum server inside `perseus run`, default `web_bind = "127.0.0.1:8686"`, disabled with `web_bind = ""`. Binding to a non-loopback address requires `web_token` (bearer; requests without it → 401) — refuse to start otherwise. One static HTML page (no build step, vanilla JS polling ~2 s) + JSON API:

- `GET /api/status` — watcher state per directory, engine queue depth, in-flight transfers with progress, retention policy + dry-run flag + next-run time, peer/relay connectivity.
- `GET /api/sent?state=…` — sent files with per-package state (`queued/announced/transferring/confirmed/failed`), bytes, timestamps.
- `GET /api/history?query=…` — `sync_history` rows (see §5: device name, duration, speed, outcome).
- `GET /api/retention/log` — tail of recent retention decisions (what was deleted / would-be-deleted in dry-run).
- `POST /api/delete` `{package_ids}` — manual delete of **confirmed** packages' source files: eligibility goes through the exact same confirmed()-only chokepoint retention uses (never deletable: anything non-confirmed — enforced in core, not in the UI), writes `deleted` history rows, and `release()`s the blobs.
- `GET/PUT /api/retention/policy` — read/edit the retention policy: mode (keep-everything / delete-on-confirm / keep-N-days / disk-max-pct), `keep_days`, `disk_max_pct`, `interval_secs`, `dry_run`. **The two live-deletion keys (`soak_opt_in`, `live_deletion`) are TOML-only and read-only on the web** — shown with an explanatory hint. This preserves the Stage I safety invariant (live deletion requires two explicit config keys; dry-run is the default) while making everything else adjustable from the browser.

**Config write-back:** `PUT /api/retention/policy` rewrites `perseus.toml` via `toml_edit` (comments/layout preserved — the file remains the single source of truth and stays hand-editable over SSH) and pushes the new policy to the running retention service through a `tokio::sync::watch` channel (applies without restart). Concurrent hand-edits: last write wins; the page re-reads on every poll.

## 5. History and statuses (both sides)

- **Device names:** history keeps the hex node id (stable key). Display maps it through the cached hub device list (Perseus already caches it for pairing; the app has it in the account layer). Hub unreachable / unknown id → short hex fallback. No schema change.
- **Duration/speed:** `sync_history` already stores `started_at/finished_at/bytes`; UI (app TransfersPanel + Perseus page) renders duration and derived MB/s. No schema change.
- **"Confirmed — safe to delete":** a confirmed package means every frame was ingested-or-duplicate on the primary; every UI that lists sent files shows this as an explicit badge, and it is the same predicate the retention evaluator and the manual-delete endpoint use (one function, one meaning).

## 6. Testing

- **Blob GC:** loopback engine tests assert `release` fires on confirmed/failed-terminal and never earlier; iroh test: send → confirm → tags empty → provider refuses a re-fetch; receiver test: ingest → ack → local blobs gone → re-announce still ack-replays from receipts; startup-sweep test seeds orphaned tags and asserts cleanup.
- **Delete invariants:** the retention test suite's confirmed()-only property extends to the manual-delete path (attempt to delete a `transferring` package → rejected in core).
- **Perseus config:** array parse, singular alias, both-forms error; multi-dir watcher smoke (two dirs, files in each, both packaged).
- **Web:** handler-level JSON contract tests (status/sent/history shapes); auth test (non-loopback bind without token refuses to start; wrong bearer → 401); policy PUT round-trip preserves TOML comments and updates the live watch value; PUT cannot touch `soak_opt_in`/`live_deletion`.
- **Landing root:** ingest lands under a designated `sync_incoming` root; fallback to app-data when undesignated; per-kind uniqueness check.
- **E2E harness:** extend `sync_e2e.rs` with post-confirm assertions "sender blob store empty, receiver blob store empty".

## 7. Sequencing

1. §3 blob GC (stops the ongoing disk leak on both live deployments — first for a reason) →
2. §2 landing roots (small, unblocks pointing the soak's receive side at real storage) →
3. §4 Perseus multi-dir + web page →
4. §5 history polish (pure UI, rides on existing data) →
5. E2E extensions.

Independent of the owner's Stage I gates; the soak continues on the current build and simply picks these up on its next Perseus update.
