# Stage 1.5.1 — Perseus Follow-ups Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Close the second sender-side disk leak (package payload copies persist forever), add failed-package retry and capture-dirs editing to the Perseus web page, and fix two Sent/audit cosmetics — all found during the owner's live verification of Stage 1.5.

**Architecture:** Payload cleanup lives in the shared engine (covers Perseus AND the app's capture role): on `confirmed`, delete the package dir's payload files but KEEP `manifest.ndjson` (audit rows and Sent-row naming need it; it's ~1KB). Failed packages keep payloads — that's what retry re-serves. Startup heal cleans what live deployments already accumulated. Retry = re-enqueue of the same `package_ref` dir (receiver dedups by frame uuid; a new outbound row is the honest model). Capture-dirs editing reuses the T8 `toml_edit` machinery with restart-to-apply semantics (watchers spawn at startup; the page derives a "restart pending" banner from saved-config ≠ runtime dirs).

**Tech Stack:** unchanged (no new deps).

## Global Constraints

Same as `2026-07-08-stage15-sync-hardening.md`: tracing-only logging (stable message + snake_case fields); never swallow; headless `--no-default-features` compiles; DTOs camelCase; deletion invariant sacred (confirmed-only via the shared chokepoint — payload cleanup is NOT source deletion and must never touch capture files); commits as `eg013ra1n`, no AI trailers; gates per task = `cargo test -p perseus`, `cargo test -p athenaeum-core sync::` where core touched, full suites at the end.

---

### Task 1: Package payload cleanup on confirm + startup heal (core engine)

**Files:** Modify `crates/athenaeum-core/src/sync/engine.rs`; Test `crates/athenaeum-core/src/sync/engine_tests.rs`.

**Interfaces:** Produces `fn cleanup_package_payloads(dir: &Path) -> anyhow::Result<u64>` (private engine helper, returns freed bytes): removes every regular file in the package dir EXCEPT `manifest.ndjson` (non-recursive is fine — packages are flat; if subdirs exist, walk them and remove emptied dirs). Consumed by both call sites below.

- [ ] Failing test A: drive a package to `confirmed` over loopback (existing fixture); assert the package dir afterwards contains ONLY `manifest.ndjson`, and the payload file is gone. Poll-based (cleanup runs in the confirm path after `append_confirmed_history`).
- [ ] Failing test B (startup heal): seed a store with a `confirmed` outbound row whose `package_ref` dir still holds a payload + manifest; spawn the engine; assert the payload is removed and the manifest survives. (Heal runs once in `Worker::run` startup, alongside crash-resume: iterate `store.confirmed()`, best-effort clean each dir, `warn!` on per-dir errors, never block startup. Log one `info!(count, freed_bytes, "package payload heal")` when count > 0.)
- [ ] Failing test C (failed keeps payloads): drive a package to terminal `failed`; assert payloads REMAIN (retry depends on them).
- [ ] Implement: call `cleanup_package_payloads` in `on_ack` immediately AFTER `append_confirmed_history` (which reads the manifest — order matters) with `info!(package_id, freed_bytes, "package payloads cleaned")`; failure = `warn!`, never fails the confirm. Add the startup heal loop. Do NOT touch `fail_package`/`cancel_package`.
- [ ] Perseus audit compatibility check: `retention_delete_source`'s audit reads the manifest (kept) — run `cargo test -p perseus` to prove the fallback-audit tests still pass unchanged.
- [ ] Gates: `cargo test -p athenaeum-core sync::`, `cargo test -p perseus`, headless check. Commit: `feat(sync): free package payload copies on confirm (+ startup heal); keep manifest for audit`

### Task 2: Retry failed packages (Perseus web)

**Files:** Modify `crates/perseus/src/web.rs`, `crates/perseus/src/web/index.html`; Test in `web.rs`.

**Interfaces:** `POST /api/retry` body `{ "ids": [i64] }` → `{ "retried": [{oldId, newId}], "rejected": [{id, reason}] }` (camelCase). Consumes `SyncEngineHandle::enqueue_package(dir)`.

- [ ] Failing tests: (a) failed row with intact package dir → 200, new row exists in `queued`+ state, response maps old→new id; (b) confirmed/transferring id → rejected `"not failed"`; (c) failed row whose dir lacks `manifest.ndjson` or has no payload files → rejected `"package data missing"` (honest — nothing to re-send).
- [ ] Implement handler: per id — look up row (`all_outbound` or targeted query), verify `state == "failed"`, verify dir exists with manifest + ≥1 payload file, `engine.enqueue_package(dir).await` → new id. `tracing::info!(old_id, new_id, "failed package re-enqueued via web")`.
- [ ] UI: amber **Retry** button on `state === 'failed'` rows in the Sent table (same row-action pattern as Delete); on success refresh; per-id rejection reasons surfaced in the status line.
- [ ] Gates + commit: `feat(perseus): retry failed packages from the web page`

### Task 3: Capture-dirs editor on the web page (restart-to-apply)

**Files:** Modify `crates/perseus/src/config_edit.rs`, `crates/perseus/src/web.rs`, `crates/perseus/src/web/index.html`; Test in `config_edit.rs` + `web.rs`.

**Interfaces:** `pub fn apply_capture_dirs_edit(config_path: &Path, dirs: &[String]) -> anyhow::Result<Config>` — same contract as `apply_retention_edit` (edit-on-copy, whole-config re-validate, tmp+atomic rename, byte-identical on reject). Writes `capture_dirs` as a TOML array AND **removes the legacy `capture_dir` key** (both-forms is a validation error). `GET/PUT /api/capture-dirs`.

- [ ] Failing config_edit tests: round-trip preserves comments elsewhere; singular `capture_dir` key removed when array written; nonexistent dir → Err + file byte-identical; empty list → Err ("at least one capture directory").
- [ ] Failing web tests: PUT valid → 200 + file updated; PUT nonexistent dir → 422 byte-identical; GET returns `{ configured: [...], runtime: [...], restartPending: bool }` (configured = from `state.config`, runtime = `state.capture_dirs` snapshot; `restartPending = configured != runtime`).
- [ ] Implement. PUT success also updates `state.config` (RwLock) — runtime dirs stay the spawn-time snapshot, which is exactly what makes `restartPending` honest.
- [ ] UI: Capture Directories card in Status section — list with remove buttons + add-row input + Save; after save, persistent amber banner `saved — restart Perseus to apply` driven by `restartPending` (survives reloads; clears itself after the restart because runtime == configured again).
- [ ] Gates + commit: `feat(perseus): capture-dirs editor on the web page (restart-to-apply)`

### Task 4: Sent/audit cosmetics

**Files:** Modify `crates/perseus/src/web.rs` (SentDto), `crates/perseus/src/run.rs` (audit peer), `crates/perseus/src/web/index.html`; Test in `web.rs` + existing audit tests.

- [ ] `SentDto` gains `files: Vec<String>` (filenames from `read_manifest(package_ref)`, capped at 5 + `"+N more"` handled client-side; manifest unreadable → empty vec, UI falls back to the dir basename). Sent table renders filenames prominently, full path in a muted sub-line/title attr.
- [ ] Audit `peer` fix: `deleted_manual`/`retention_deleted` history rows currently stamp the agent's OWN node id in `peer_device` (screenshot evidence: `1781053e` = self); stamp the configured sync peer (same value transfer rows use) — locate where `build_retention_history_rows` gets its peer and pass the engine's peer hex. Adjust the existing audit tests' expectations.
- [ ] Gates + commit: `fix(perseus): sent rows show filenames; audit rows stamp the sync peer, not self`

---

Final: full gates (workspace build, core+perseus suites, headless, tsc — tsc only if src/ touched, it is not), short whole-wave review, ledger + memory update. The owner's runbook (`scripts/stage15_manual_verification.md`) gains a §5 addendum: retry button, capture-dirs editor, and "package payload copies are freed on confirm — `data/packages/<uuid>/` shrinks to manifest-only".
