# Sync & Collaboration — Stage 0 (enablers) + Stage I (personal sync) Task Plan — 2026-07-05

> **SUPERSEDED (2026-07-06):** replaced by the Perseus-first design (`../specs/2026-07-06-personal-sync-design.md`) and its forthcoming implementation plan. W1/W2 shipped with roadmap Phase 1; W3/W4 (S-1/S-2 spikes) were closed at the desk (iroh / email OTP) with validation folded into the Perseus MVP track; Perseus moved from I10 to the head of the sequence. Task labels (Wn/In) remain referenced from the new docs for traceability.

Implementation plan derived from `../specs/2026-07-05-sync-collaboration-components.md` (components C-n, decisions D-n) and the BRD (`../specs/2026-07-05-sync-collaboration-brd.md`, requirements A/B). Same format as `2026-07-02-phase0-hygiene.md`: each task has location, change, tests, effort. Runs **in parallel with the in-flight Phase 2 calibration work** — no task below touches calibration modules.

Scope fence: Stage I ends at BRD Phase I (personal multi-machine sync). Nothing here builds projects, portal pages, quality gates, or Discord — that's Stage II, planned after Phase 2 lands.

---

## Stage 0 — Enablers & spikes (~3–4 weeks, parallelizable)

### W1 — Schema foundation (collab-readiness Stage 1) — *the long-lead item, start first*

**Where:** `crates/athenaeum-core/src/db/schema.rs::init_db()` + write paths.

1. `catalog_meta` one-row table: `catalog_uuid` (generated once), `schema_version`, `created_at`.
2. `uuid TEXT NOT NULL` + `updated_at TEXT` on `files`, `frames`, `frames_set`, `sessions`, `calibration_set`, `tags` — idempotent `ALTER TABLE` migration with backfill (`randomblob`-based UUIDv4), UNIQUE index on each `uuid`.
3. `updated_at` bump via triggers (`AFTER UPDATE` per table) — write-path discipline is too easy to miss across ~190 commands.

**Tests:** fresh DB + migrated legacy DB both carry uuids everywhere; UPDATE bumps `updated_at`; existing workspace tests green. **Effort:** 2–3 days. **Blocks:** W7, everything in Stage I ingest/dedupe.

### W2 — Contract hardening (collab-readiness Stage 4)

**Where:** workspace + `src/types/models.ts`.

1. ts-rs derive on the models exposed to the frontend; `cargo test` that regenerates `models.ts` and diffs (fails on drift).
2. Shared command-helper layer so one function serves both the Tauri command and the Axum route (pilot on `files` + `settings` modules; Stage I adds ~15–20 new commands and must use it from day one).

**Tests:** deliberate Rust field rename fails the diff test; pilot modules behave identically over both transports. **Effort:** 3–5 days. **Blocks:** nothing hard, but do before Stage I command work.

### W3 — S-1 transport spike → close D-1 (BRD Q9)

**Where:** standalone playground repo (not the app workspace).

Script the B4/B5 scenario against **both** candidates (iroh embedded; Syncthing sidecar via REST): two machines (or two containers), ~10 GB FITS set, mid-transfer network kill, resume, hash-confirmed delivery signal back to sender, delete-at-source only after confirmation; then a self-hosted relay behind NAT. Measure/record: integration surface, resume behavior, confirmation latency, relay ops effort.

**Exit:** decision record `docs/superpowers/specs/2026-07-XX-transport-decision.md` (D-1 closed). **Effort:** ~1 week. **Blocks:** I5.

### W4 — S-2 auth spike → close D-2 (BRD Q1)

Prototype the desktop sign-in flow (device-code style vs embedded webview vs email+magic-link), token refresh, OS-keychain storage (macOS Keychain / Windows Credential Manager / Secret Service). Pick account provider approach (own email+password vs OAuth).

**Exit:** decision record (D-2 closed) + flow diagram. **Effort:** 2–3 days. **Blocks:** W5 final auth, I1.

### W5 — S-3 hub walking skeleton (C-1 minimal)

**Where:** new repo `athenaeum-hub` (D-4), Axum + Postgres (D-3).

Endpoints only for Stage I: account signup/signin (per D-2), device register/list/revoke (device pubkey = transport node id, D-5), `GET /relay-map` (static list for now), aggregate stats ingest stub. Single-binary deploy + migrations; deployed once to a real host end-to-end.

**Tests:** API integration tests; one manual end-to-end sign-in from a dev build of the app (I1 stub). **Effort:** ~1 week. **Blocks:** I1, I2.

### W6 — C-4 `SharingTransport` trait + mock

**Where:** `crates/athenaeum-core/src/sharing/` (new module).

Define the trait per the components doc: `send(package, to_device) → TransferHandle`, delivery-confirmation callback (content-hash-verified), resume-after-restart enumeration, progress wired to `ProgressEmitter`, relay-map injection. Ship a `LoopbackTransport` mock (in-process, fault-injectable: mid-transfer abort, duplicated delivery, delayed ack) so the whole Stage-I sync engine is testable without the real transport.

**Tests:** mock round-trip + fault-injection unit tests. **Effort:** 2–3 days. **Blocks:** I3–I7 (they build against the trait, not the impl).

### W7 — Package/manifest format v1 (pulled forward from roadmap Phase 4)

**Where:** `crates/athenaeum-core/src/package/` (new module).

`manifest.ndjson` schema: one record per frame — frame `uuid`, origin `catalog_uuid`, rel path, xxh3 content hash, full frames-row metadata + analysis summary; package = manifest + payload files (dir or zip via existing `zip_writer`). Writer from a frame/file selection; reader + validation. This is the transfer unit for personal sync AND future projects (D-6) — version the schema (`"v": 1`).

**Tests:** round-trip export→import on a fixture catalog; idempotent re-import (uuid dedupe) — needs W1. **Effort:** 3–4 days. **Blocks:** I4, I6.

## Stage I — Personal sync (BRD Phase I; after D-1/D-2 close)

### I1 — App account layer (C-3)

**Where:** `crates/athenaeum-core/src/account/` (hub client: reqwest, token refresh) + `Settings → Account` UI + keychain storage per W4 decision. Device keypair generated at first sign-in; registered with hub (W5). Signed-out state hides sync surfaces only (BRD A2 — hard requirement; add an explicit test that all pre-existing pages render signed-out).

**Effort:** ~1 week. Commands via the W2 helper; web mirrors included.

### I2 — Machine roles & pairing

**Where:** core `sync/config` + Settings UI. Designate this install *primary* or *capture node*; capture node picks its target primary from the account's device list (hub). Store role + peer in settings. Guard: exactly one primary per account (hub-enforced; clear error otherwise).

**Effort:** 2–3 days. **Depends:** I1.

### I3 — Sync engine schema & queue (C-5 core)

**Where:** `crates/athenaeum-core/src/sync/` + `db/schema.rs`.

Tables: `sync_outbound (id, file_id, package_ref, state: queued|transferring|delivered|confirmed|failed, attempts, created_at, confirmed_at)`, `sync_history (both directions: frame uuid, filename, object, peer_device, direction, bytes, started/finished, outcome)` — append-only, indexed by filename/object/date. Queue worker on the existing `operation_queue` (`OperationKind::SyncTransfer`), cooperative cancellation like archive.

**D-7 structural constraint:** build the engine as a library layer decoupled from the catalog (queue/packaging/delivery/retention/history behind a small storage trait, its own SQLite file when embedded in Perseus) — the full app and the Perseus agent (I10) are two shells over the same crate-level module.

**Tests:** state-machine unit tests over `LoopbackTransport` incl. crash-resume (re-enumerate `transferring` on startup). **Effort:** ~1 week. **Depends:** W1, W6.

### I4 — Auto & manual send modes

**Where:** auto: hook the existing scanner/monitor "scan finished" completion path on capture nodes — newly ingested files enqueue automatically (per-node toggle; all frame types, BRD B2). Manual: `enqueue_sync_selection` command + selection UI on Objects/Browse pages ("Send to primary" action, BRD B3). Both produce W7 packages (metadata travels, B8).

**Tests:** scan N files in auto mode → N queue entries; manual selection → exactly the selection. **Effort:** 3–4 days. **Depends:** I3, W7.

### I5 — Real transport implementation (D-1 winner)

**Where:** `crates/athenaeum-core/src/sharing/<impl>/`.

Implement `SharingTransport` per S-1's decision record: connection via device keys, relay map from hub, verified resumable transfer, delivery confirmation. If Syncthing won: sidecar lifecycle management (detect/spawn/configure via REST) is part of this task. If iroh won: embed endpoint in-process; relay-map plumbing.

**Tests:** re-run the S-1 scenario script against the production impl; soak test with the 10 GB fixture. **Effort:** 1–2 weeks (the risk item of Stage I). **Depends:** W3, W6, I1.

### I6 — Receiver ingest & ack (primary side)

**Where:** `crates/athenaeum-core/src/sync/ingest.rs`.

Receive package → verify content hashes → import: file lands under a configured "incoming" scan-root folder (per capture node, template-organized); manifest metadata applied; **dedupe by frame uuid then content hash** (same frame twice = one catalog row, B7); **primary-wins** metadata merge (B9: never overwrite newer primary edits — compare `updated_at`); ack (frame uuid + hash) back to sender → sender flips `confirmed`. Provenance: `origin device` recorded on history rows.

**Tests:** duplicate delivery → single row; conflicting metadata edit on primary survives ingest; ack loss → sender retries, receiver ack idempotent. **Effort:** ~1 week. **Depends:** I3, W7, W1.

### I7 — Retention service (capture node)

**Where:** `crates/athenaeum-core/src/sync/retention.rs`.

Policy config per capture node (BRD B5): delete-on-confirm / keep N days / disk ≥ X% oldest-confirmed-first. Evaluation loop piggybacks on the monitor tick. Hard invariants, enforced in one place and unit-tested: only `confirmed` files are ever deleted; deletion goes through the existing `file_op` delete pipeline (catalog-consistent, history row appended); untransferred files are untouchable regardless of disk pressure — disk-full with nothing eligible → `notify()` warning instead.

**Tests:** the BRD acceptance sketch verbatim — network kill mid-transfer → no delete; restore → transfer → confirm → policy delete + two history events; disk over threshold → only confirmed, oldest first. **Effort:** 3–4 days. **Depends:** I3, I6.

### I8 — History & status UI + notifications

**Where:** frontend. Transfer queue/status page (per-file state, progress via SSE/Tauri events), history view on both machine roles (searchable, B6), `notify()` on discrete outcomes (new `NotificationKind: 'sync'` → union + icon map per convention). Commands mirrored web-side via W2 helper.

**Effort:** ~1 week. **Depends:** I3–I7.

### I10 — Perseus agent MVP (C-10, BRD B1a)

**Where:** new crate `crates/perseus/` (or separate repo — decide with D-4 review; workspace crate is simpler while the sync library stabilizes).

Thin shell over the I3 library: account sign-in (I1 client subset) + device registration as capture node, target-primary selection, capture-directory watcher (`notify`-based, reusing the monitor pattern), auto-enqueue of new FITS/XISF (header parse via `fits_parser` for manifest metadata; no analysis fields), retention (I7 rules), local history. Config: minimal local web page (bind localhost) or config file — no Tauri, no React app. Ships as a single binary + service unit (systemd/launchd/Windows service); build targets include Linux ARM64.

**Tests:** Perseus → full-app-primary E2E on the I9 harness; unclean shutdown mid-transfer resumes. **Effort:** ~1 week on top of the library (the point of D-7 is that Perseus adds packaging/watcher glue only). **Depends:** I1 (client), I3, I5, I7.

### I9 — Two-instance E2E harness + exit milestone

**Where:** test harness: two app instances, separate DBs/data dirs, on one machine (loopback transport for CI; real transport for the manual run).

**Milestone M-Sync1 (Stage I exit):** **Perseus** on the capture side in auto mode receives 50 fixture frames → all arrive on the full-app primary with metadata, dedupe-safe re-run, retention deletes at source per policy, history complete on both ends, hub shows both devices + aggregate stats. Second variant of the same run with a full app as capture node (manual send path). Demo script recorded in the doc.

**Effort:** ~1 week. **Depends:** all of Stage I.

## Sequencing

```
W1 ──► W7 ──► I4/I6
W2 ──────────► (all Stage-I commands)
W3 ──► D-1 ──► I5
W4 ──► D-2 ──► W5 ──► I1 ──► I2
W6 ──────────► I3 ──► I4 ──► I7 ──► I8 ──► I9
                        └───► I6 ──┘
```

Stage 0 tracks are independent of each other; a single developer order: W1 → W6 → W3 → W4 → W5 → W2 → W7, then Stage I as I1 → I2 → I3 → I4 → I6 → I5 → I7 → I10 → I8 → I9 (I5 late = engine matured against the mock first; I10 Perseus right after the library is proven end-to-end). Rough total: Stage 0 ~3–4 wks, Stage I ~7–9 wks single-developer.

## Explicitly out of scope (Stage II+)

Projects/membership/roles (C-7), quality gate (C-6 — waits for Phase 2 calibration), portal beyond hub skeleton (C-2), Discord (C-9), private-relay paid tier (C-8$), bandwidth scheduling (B10), coordinator flows. Change journal (collab-readiness Stage 3) is NOT needed for one-way personal sync — deferred to Stage II where project metadata re-import requires it.
