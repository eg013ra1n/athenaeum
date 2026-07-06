# Stage I — Personal Sync Implementation Plan (Perseus-first) — 2026-07-07

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship BRD Phase I — observatory→home personal sync over iroh with the Perseus capture agent, hub accounts, retention, and transfer history (milestones M-Perseus-MVP → M-Sync1).

**Architecture:** Track A builds the transport-agnostic sync engine as a feature-gated `athenaeum-core` library (trait-mocked, then iroh), wrapped by the Perseus headless agent validated on the owner's real observatory. Track B builds the `athenaeum-hub` service (email-OTP auth, device registry, relay-map) and the app account layer in parallel. A merge phase replaces ticket pairing with account pairing and ships the Transfers UI. Spec: `../specs/2026-07-06-personal-sync-design.md` (§ references below point there).

**Tech Stack:** Rust (tokio, async-trait, iroh + iroh-blobs, rusqlite, notify, keyring), Axum + sqlx/Postgres (hub), React/TS (Settings/Transfers UI), lettre SMTP (OTP mail).

## Global Constraints

- Every new Tauri command gets `#[tracing::instrument(skip_all, err)]`, a same-name Axum mirror via `core::api`, registration in `invoke_handler![]` + `build_router`, and ts-rs types in `ts_export.rs` (CLAUDE.md "Adding a Tauri Command").
- Serde boundary: `#[serde(rename_all = "camelCase")]`; TS mirrors in `src/types/models.ts` via the generation test.
- Zero `println!`/`eprintln!`; `tracing` only; canonical field names (`frame_id`, `operation_id`, `path`, `bytes`, `outcome`, …) — new field names require a spec update.
- Frontend: design tokens only; `api.listen` cancelled-flag pattern; `notify()` for discrete outcomes only; timestamps via `formatTimestamp`.
- Retention hard invariant (never violated anywhere, tested everywhere): **only `confirmed` files are ever deleted; dry-run is the default until M-Perseus-MVP passes.**
- Commit per task (or finer); commit as the user (no AI co-author); push only to `origin` (GitLab).
- Rust gates per repo reality: `cargo build --workspace --all-targets`, `cargo test -p athenaeum-core`, `npx tsc --noEmit`. Clippy `-D warnings` is NOT a gate.
- iroh crates: pin exact versions at A5 step 1 after checking current 1.x API (docs.rs) — the plan's iroh snippets are shape, not gospel.

## Task index & dependency graph

```
A1 core feature-gate ──► A2 sharing trait+loopback ──► A4 engine ──► A5 iroh ──► A6 Perseus ──► A9 SOAK GATE (M-Perseus-MVP)
                    └──► A3 package/manifest ────────┘         └──► A7 ingest+ack (app) ──┘
                                                                A8 retention(dry-run) ──► A9
B1 hub skeleton ──► B2 OTP auth+devices ──► B3 relay-map+relay deploy
                                        └─► B4 app account layer + Settings UI
A9 + B4 ──► M1 account pairing ──► M2 manual mode ──► M3 Transfers UI ──► M4 retention live ──► M5 E2E (M-Sync1)
```

Tracks A and B are independent — parallelize freely. Single-dev order: A1 A2 A3 A4 | B1 B2 (while A soaks or blocks) | A5 A6 A7 A8 A9 | B3 B4 | M1…M5.

---

## Track A — transport, engine, Perseus

### Task A1: Feature-gate `athenaeum-core` (build without rustafits/solvemyastro)

**Files:**
- Modify: `crates/athenaeum-core/Cargo.toml` (features), `crates/athenaeum-core/src/lib.rs` (module cfg-gates)
- Modify: whatever fails `cargo check --no-default-features` (expect: `rustafits_processor`, `analysis`, `plate_solve`, `registration`, parts of `services`/`api` re-exports)

**Interfaces:**
- Produces: features `render` (rustafits), `solver` (solvemyastro), `default = ["render", "solver"]`. Everything Perseus needs (`db`, `fits_parser`, `models`, `settings` subset, upcoming `sharing`/`package`/`sync`) compiles with `--no-default-features`.

- [ ] **Step 1:** In `Cargo.toml`: `rustafits = { path = ..., optional = true }`, `solvemyastro = { ..., optional = true }`; add `[features] default = ["render", "solver"]; render = ["dep:rustafits"]; solver = ["dep:solvemyastro"]`.
- [ ] **Step 2:** Run `cargo check -p athenaeum-core --no-default-features`; gate each failing module in `lib.rs` with `#[cfg(feature = "render")]` / `#[cfg(feature = "solver")]` (and `api`/`services` re-exports likewise). Do NOT restructure module internals — gate at declaration sites; if a shared type sits in a gated module, move the type to `models`, not the module out of the gate.
- [ ] **Step 3:** Verify both ways: `cargo check -p athenaeum-core --no-default-features` passes; `cargo build --workspace --all-targets` passes (tauri/web use default features implicitly — confirm no `default-features = false` needed there).
- [ ] **Step 4:** Add a CI-friendly guard test note in the workspace CI yml (build matrix line: `cargo check -p athenaeum-core --no-default-features`).
- [ ] **Step 5:** `cargo test -p athenaeum-core` (default features) green. Commit `build(core): feature-gate render/solver deps for headless consumers`.

**Acceptance:** core compiles headless; zero behavior change with default features. **Effort:** 1–2 days (surface unknown until Step 2 — this is why A1 is first).

### Task A2: `sharing` module — `SharingTransport` trait + `LoopbackTransport`

**Files:**
- Create: `crates/athenaeum-core/src/sharing/mod.rs`, `sharing/types.rs`, `sharing/loopback.rs`
- Modify: `crates/athenaeum-core/src/lib.rs` (declare `pub mod sharing;` — ungated)
- Test: inline `#[cfg(test)]` in `loopback.rs` + `sharing/tests.rs`

**Interfaces (produces — the contract every later task builds on):**

```rust
// sharing/types.rs
pub type NodeId = [u8; 32];                       // ed25519 pubkey; == iroh node id
pub struct PackageId(pub String);                  // uuid v4 string
#[derive(Serialize, Deserialize, Clone)]
pub struct PackageAnnounce { pub package_id: PackageId, pub root_hash: String,
    pub byte_size: u64, pub frame_count: u32 }
#[derive(Serialize, Deserialize, Clone)]
pub struct FrameReceipt { pub frame_uuid: String, pub xxh3: String, pub outcome: ReceiptOutcome }
#[derive(Serialize, Deserialize, Clone)] pub enum ReceiptOutcome { Ingested, Duplicate, Rejected(String) }
pub enum TransportEvent {
    AnnounceReceived { from: NodeId, announce: PackageAnnounce },
    AckReceived { from: NodeId, package_id: PackageId, receipts: Vec<FrameReceipt> },
    FetchProgress { package_id: PackageId, bytes_done: u64, bytes_total: u64 },
}

// sharing/mod.rs
#[async_trait]
pub trait SharingTransport: Send + Sync {
    async fn start(&self) -> anyhow::Result<StartInfo>;          // StartInfo { node_id, pairing_ticket: String }
    async fn announce(&self, to: NodeId, a: &PackageAnnounce) -> anyhow::Result<()>;
    /// Pull a package (manifest + blobs) from `from` into `dest_dir`. Verified, resumable.
    async fn fetch(&self, from: NodeId, pkg: &PackageAnnounce, dest_dir: &Path) -> anyhow::Result<()>;
    /// Register the local package directory that peers may fetch from (provider side).
    async fn serve(&self, pkg: &PackageAnnounce, src_dir: &Path) -> anyhow::Result<()>;
    async fn ack(&self, to: NodeId, package_id: &PackageId, receipts: Vec<FrameReceipt>) -> anyhow::Result<()>;
    async fn events(&self) -> tokio::sync::mpsc::Receiver<TransportEvent>;
}
```

- [ ] **Step 1:** Write failing tests first (`sharing/tests.rs`): `loopback_announce_fetch_ack_roundtrip` (peer A serves fixture dir, announces to B; B receives event, fetches, acks; A receives receipts), `loopback_fault_abort_mid_fetch_then_resume` (fault knob `abort_after_bytes`; second fetch succeeds and file content hash-verifies), `loopback_duplicate_ack_delivered_once_ok` (dup ack does not panic and is observable twice — idempotence is the *engine's* job, transport just delivers).
- [ ] **Step 2:** `cargo test -p athenaeum-core sharing` → FAIL (module missing).
- [ ] **Step 3:** Implement `LoopbackTransport`: registry `Arc<Mutex<HashMap<NodeId, PeerInbox>>>` shared by construction (`LoopbackNetwork::new().endpoint()` → two linked instances); `fetch` = tokio fs copy loop honoring fault knobs `FaultPlan { abort_after_bytes: Option<u64>, duplicate_ack: bool, delay_ack: Option<Duration> }`.
- [ ] **Step 4:** Tests green. Commit `feat(sharing): SharingTransport trait + fault-injectable LoopbackTransport`.

**Acceptance:** trait + mock merged, no consumer yet. **Effort:** 2 days. **Depends:** A1 (module ungated but compiles headless).

### Task A3: `package` module — manifest v1 + writer/reader

**Files:**
- Create: `crates/athenaeum-core/src/package/{mod.rs,manifest.rs,writer.rs,reader.rs}`
- Test: `package/tests.rs` with a small real FITS fixture (reuse an existing test fixture from `fits_parser` tests)

**Interfaces:**

```rust
// manifest.rs — one NDJSON line per payload file
#[derive(Serialize, Deserialize)]
pub struct ManifestRecord {
    pub v: u32,                                   // = 1
    pub frame_uuid: String,
    pub origin_catalog_uuid: String,
    pub origin_device: String,                    // hex NodeId
    pub payload_kind: PayloadKind,                // RawFrame | CalibratedLight | Master | Other
    pub rel_path: String,
    pub byte_size: u64,
    pub xxh3: String,                             // reuse duplicates::hash helpers
    pub frame_meta: serde_json::Value,            // frames-row snapshot (models::Frame serialized)
    pub analysis: Option<serde_json::Value>,      // frame_analysis summary when present
    pub app_version: String,
}
// writer.rs
pub fn write_package(dest_dir: &Path, records: Vec<(PathBuf /*src file*/, ManifestRecord)>) -> Result<PackageAnnounce>;
// reader.rs
pub fn read_manifest(dir: &Path) -> Result<Vec<ManifestRecord>>;
pub fn validate_package(dir: &Path) -> Result<()>;   // per-record: file exists, size, xxh3
```

- [ ] **Step 1:** Failing tests: `package_roundtrip_manifest_matches` (write from fixture FITS + fabricated meta → read → records equal, files copied, xxh3 recomputes), `validate_catches_corruption` (flip a byte → `validate_package` errs naming the rel_path), `manifest_forward_compat_unknown_field_ok` (extra JSON key parses fine — serde default tolerance).
- [ ] **Step 2:** Run → FAIL. Implement. `write_package` computes `root_hash` = xxh3 over sorted record hashes (placeholder until A5 swaps in the iroh collection hash — keep the field a string, producer-defined).
- [ ] **Step 3:** Tests green; commit `feat(package): manifest v1 + package writer/reader/validation`.

**Acceptance:** round-trip + validation on real FITS. **Effort:** 2–3 days. **Depends:** A1.

### Task A4: sync engine — `SyncStore` + sender state machine + worker

**Files:**
- Create: `crates/athenaeum-core/src/sync/{mod.rs,store.rs,engine.rs,models.rs}`
- Test: `sync/engine_tests.rs` (over `LoopbackTransport` + standalone store in tempdir)

**Interfaces:**

```rust
// models.rs
pub enum OutboundState { Queued, Announced, Transferring, Delivered, Confirmed, Failed }
pub struct OutboundRow { pub id: i64, pub package_ref: String /*dir path*/, pub peer: NodeId,
    pub state: OutboundState, pub attempts: u32, pub created_at: String, pub confirmed_at: Option<String> }
pub struct HistoryRow { pub frame_uuid: String, pub filename: String, pub object: Option<String>,
    pub peer_device: String, pub direction: Direction, pub bytes: u64,
    pub started_at: String, pub finished_at: Option<String>, pub outcome: String }

// store.rs — SQL defined ONCE as consts, used by both impls
pub trait SyncStore: Send + Sync {           // sync (rusqlite) trait, called from worker thread
    fn enqueue(&self, package_ref: &str, peer: NodeId) -> Result<i64>;
    fn set_state(&self, id: i64, s: OutboundState) -> Result<()>;
    fn bump_attempts(&self, id: i64) -> Result<u32>;
    fn non_terminal(&self) -> Result<Vec<OutboundRow>>;      // crash-resume enumeration
    fn confirm(&self, id: i64, receipts: &[FrameReceipt]) -> Result<()>;
    fn append_history(&self, h: HistoryRow) -> Result<()>;
    fn search_history(&self, q: HistoryQuery) -> Result<Vec<HistoryRow>>;
}
pub struct StandaloneSyncStore { /* own SQLite file (Perseus, tests) */ }
pub struct CatalogSyncStore    { /* app catalog DB — same DDL, created in schema.rs by M2 */ }
// engine.rs
pub struct SyncEngine { /* store + transport + config */ }
impl SyncEngine {
    pub fn spawn(store: Arc<dyn SyncStore>, transport: Arc<dyn SharingTransport>, peer: NodeId) -> SyncEngineHandle;
    // handle: enqueue_package(dir), cancel(id), status_snapshot(), shutdown()
}
```

DDL (both stores, `store.rs` consts): `sync_outbound (id INTEGER PK, package_ref TEXT NOT NULL, peer TEXT NOT NULL, state TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL, confirmed_at TEXT)`; `sync_history (id INTEGER PK, frame_uuid TEXT, filename TEXT, object TEXT, peer_device TEXT, direction TEXT, bytes INTEGER, started_at TEXT, finished_at TEXT, outcome TEXT)` + indexes on `filename`, `object`, `started_at`.

- [ ] **Step 1:** Failing state-machine tests: `happy_path_reaches_confirmed_and_history_has_both_events`, `mid_transfer_abort_leaves_transferring_then_resume_completes` (drop engine, new engine over same store re-enumerates `non_terminal()` and finishes), `ack_lost_then_duplicate_ack_confirms_once`, `failed_after_max_attempts_with_error_outcome_in_history`, `cancel_moves_to_failed_cancelled`.
- [ ] **Step 2:** Run → FAIL. Implement `StandaloneSyncStore` (rusqlite, WAL) + engine worker: tokio task loop — `Queued→announce→Announced`; on peer fetch completion event… note: loopback `fetch` is receiver-driven; sender learns completion via `AckReceived` only → `Announced→Transferring` on first `FetchProgress`, `→Delivered` unused in v1 if no delivered signal exists — **collapse to `Queued|Announced|Transferring|Confirmed|Failed` and document `Delivered` as reserved** (keeps DDL stable, engine simpler; spec's five states preserved in the enum).
- [ ] **Step 3:** Tests green over loopback incl. fault plans from A2.
- [ ] **Step 4:** `tracing` events per transition (`info!(package_id, state, "sync state")`; errors `error!` — never swallowed). Commit `feat(sync): engine state machine + SyncStore (standalone impl) over LoopbackTransport`.

**Acceptance:** engine survives kill/resume/dup-ack purely on the mock. **Effort:** ~1 week. **Depends:** A2, A3.

### Task A5: iroh transport implementation + validation gate

**Files:**
- Create: `crates/athenaeum-core/src/sharing/iroh/{mod.rs,proto.rs,blobs.rs}`
- Create: `crates/athenaeum-core/examples/sync_validation.rs` (manual two-machine B4/B5 scenario)
- Modify: `crates/athenaeum-core/Cargo.toml` (deps `iroh`, `iroh-blobs` — pin exact 1.x versions here)
- Test: `sharing/iroh/tests.rs` (two in-process endpoints over localhost)

**Interfaces:**
- Consumes: `SharingTransport` (A2), package dir layout (A3).
- Produces: `IrohTransport::new(secret_key, relay_mode) -> Self` implementing the trait; `pairing_ticket` = serialized NodeTicket string; `root_hash` = iroh-blobs collection hash (replaces A3's placeholder at write time via a `HashProvider` closure param on `write_package` — small A3 API touch, do it here).

- [ ] **Step 1:** Check current iroh 1.x API on docs.rs; pin versions. Custom ALPN `b"athenaeum/sync/1"`; `proto.rs`: postcard-encoded `enum Msg { Announce(PackageAnnounce), Ack { package_id, receipts } }` over a uni QUIC stream; `blobs.rs`: fs blob store at `<data_dir>/sync_blobs`, package dir → collection (import blobs + manifest as first entry), fetch = download collection into dest dir.
- [ ] **Step 2:** In-process tests: `iroh_roundtrip_two_endpoints_localhost` (same assertions as loopback round-trip), `iroh_resume_after_endpoint_restart` (drop receiving endpoint mid-download, recreate, re-fetch completes — iroh-blobs verified ranges make this cheap), `engine_suite_over_iroh` (re-run the A4 test set parameterized over the iroh transport, localhost).
- [ ] **Step 3:** `examples/sync_validation.rs` — CLI with `serve <dir>` / `fetch <ticket> <dest>` modes + printed instructions. **VALIDATION GATE (manual, 2–3 days):** two real machines both behind NAT, ~10 GB FITS; kill network mid-transfer → resume; hash-confirmed delivery; delete-at-source only after ack; then repeat through self-hosted `iroh-relay` on the VPS (B3 task provides it — if B3 not done yet, run relay ad-hoc from the iroh-relay binary). Record results in `docs/superpowers/research/2026-07-XX-iroh-validation.md`.
- [ ] **Step 4:** GATE decision: pass → proceed; fail → STOP, escalate to owner (Syncthing fallback per spec §2 — plan revision required).
- [ ] **Step 5:** Commit `feat(sharing): iroh transport (blobs collections + announce/ack protocol) + validation harness`.

**Acceptance:** engine tests green over iroh; validation doc written; gate passed. **Effort:** 1–2 weeks (risk item). **Depends:** A2–A4.

### Task A6: Perseus agent MVP

**Files:**
- Create: `crates/perseus/` (workspace member): `src/main.rs`, `src/config.rs`, `src/watcher.rs`, `src/run.rs`, `dist/perseus.service` (systemd), `dist/com.athenaeum.perseus.plist` (launchd), `README.md`
- Modify: workspace `Cargo.toml` members
- Test: `crates/perseus/tests/e2e_loopback.rs`

**Interfaces:**
- Consumes: `athenaeum_core::{sharing::iroh, sync, package, fits_parser}` with `default-features = false`.
- Produces: single binary; TOML config:

```toml
# perseus.toml
capture_dir = "/data/capture"
data_dir = "/var/lib/perseus"            # SQLite store + blob store + logs
pairing_ticket = "<paste from primary Settings → Sync (dev)>"
mode = "auto"                             # only value in MVP
[retention]
policy = "keep_everything"                # keep_everything | on_confirm | keep_days | disk_pct
dry_run = true                            # MUST stay true until M-Perseus-MVP sign-off
```

- [ ] **Step 1:** Failing e2e test: temp capture dir + loopback transport wired via a test-only constructor; drop two fixture FITS in → both enqueued once (stability window respected), packages built with header-derived `frame_meta`, engine reaches `Confirmed` against an in-test receiver stub; unclean shutdown mid-transfer (abort task) → restart resumes.
- [ ] **Step 2:** Implement: `config.rs` (serde TOML + validation), `watcher.rs` (`notify` watcher + **write-stability check**: enqueue only after size+mtime stable for `stability_secs = 10` — capture software writes progressively; copy the monitor module's pattern), `run.rs` (StandaloneSyncStore at `data_dir`, IrohTransport from a persisted device key file `0600`, ticket → peer NodeId, tracing JSONL to `data_dir/logs` with `perseus.*` prefix), `main.rs` (clap: `run`, `status`, `enqueue-backlog <dir>` for pre-existing files).
- [ ] **Step 3:** Build for targets: `cargo build -p perseus --release` on macOS + `cross`/CI check for `aarch64-unknown-linux-gnu`. Service files documented in README (install paths, restart-on-failure).
- [ ] **Step 4:** Tests green; commit `feat(perseus): headless capture agent MVP (ticket pairing, auto mode, dry-run retention)`.

**Acceptance:** e2e test green; binary runs against a real dir. **Effort:** ~1 week. **Depends:** A4, A5 (A3 via engine).

### Task A7: Primary-side receiver — ingest + ack (app)

**Files:**
- Create: `crates/athenaeum-core/src/sync/ingest.rs`, `crates/athenaeum-core/src/api/sync.rs`
- Modify: `crates/athenaeum-core/src/db/schema.rs` (add `sync_receipts (package_id TEXT, frame_uuid TEXT, xxh3 TEXT, outcome TEXT, received_at TEXT, PRIMARY KEY(package_id, frame_uuid))` + the A4 DDL for `CatalogSyncStore`), `crates/athenaeum-tauri/src/commands/` + `crates/athenaeum-web/src/routes/` (new `sync` domain, registered)
- Test: `sync/ingest_tests.rs`

**Interfaces:**
- Consumes: `TransportEvent::AnnounceReceived`, `package::read_manifest/validate_package`, existing `db::insert_file/insert_frame` + scanner helpers, `frames.uuid` unique index (Phase 1).
- Produces: receiver service `SyncReceiver::spawn(ctx: &ServiceContext, transport, incoming_root: PathBuf)`; commands `get_sync_pairing_ticket() -> String` (dev-flagged: settings key `sync.dev_ticket_pairing = true`), `get_sync_status()`, `list_sync_history(query) -> Vec<HistoryRow>`; events `sync-progress` / `sync-finished { package_id, outcome, ok_count, failed }` via `ProgressEmitter`.

- [ ] **Step 1:** Failing tests: `ingest_lands_files_and_rows` (fixture package → files under `<incoming_root>/<origin_device>/<date>/`, `files`+`frames`+`fits_header` rows created with manifest metadata, history rows written), `duplicate_delivery_single_row_but_acked` (same package twice → one catalog row per frame, second ack receipts say `Duplicate`), `primary_wins_metadata` (pre-edit frame on primary with newer `updated_at`; re-delivered older meta does NOT overwrite; history notes `outcome="skipped_older"`), `ack_replay_from_receipt_log` (receipts persisted; replayed ack identical without re-ingest).
- [ ] **Step 2:** Run → FAIL. Implement ingest pipeline per spec §9 order: validate → land (tmp + atomic rename) → per-frame tx: dedupe by `frames.uuid` then xxh3 (`files` hash lookup via duplicates helpers) → insert or skip → receipt row → history row; after batch: `ack` with all receipts; `notify`-worthy outcome via `sync-finished` event only (frontend decides notification in M3).
- [ ] **Step 3:** Wire commands per Global Constraints (core::api + two thin wrappers + registration + ts types). `get_sync_pairing_ticket` starts the transport lazily and returns the ticket; receiver spawn happens behind the same dev flag at app start.
- [ ] **Step 4:** Tests green; `cargo build --workspace --all-targets`; `npx tsc --noEmit`. Commit `feat(sync): primary receiver — verified ingest, uuid dedupe, primary-wins merge, idempotent ack`.

**Acceptance:** Perseus (A6) → app primary works end-to-end on one machine (manual smoke: real Perseus + dev app, loopback network). **Effort:** ~1 week. **Depends:** A4, A5; A6 for the smoke.

### Task A8: Retention service (dry-run first)

**Files:**
- Create: `crates/athenaeum-core/src/sync/retention.rs`
- Modify: `crates/perseus/src/run.rs` (hourly tick), `crates/perseus/src/config.rs` (already has the block)
- Test: `sync/retention_tests.rs`

**Interfaces:**

```rust
pub enum RetentionPolicy { KeepEverything, OnConfirm, KeepDays(u32), DiskPct { max_pct: u8 } }
pub struct RetentionOutcome { pub eligible: Vec<PathBuf>, pub deleted: Vec<PathBuf>, pub dry_run: bool }
/// `deleter` abstracts the actual removal (Perseus: fs remove + history row; app shell later: file_op pipeline).
pub fn evaluate_and_apply(store: &dyn SyncStore, policy: &RetentionPolicy, dry_run: bool,
    disk_probe: &dyn Fn() -> u8, deleter: &mut dyn FnMut(&Path) -> Result<()>) -> Result<RetentionOutcome>;
```

- [ ] **Step 1:** Failing tests — the BRD acceptance sketch verbatim: `untransferred_never_eligible_even_on_full_disk` (disk_probe=99%, nothing confirmed → eligible empty + would-warn flag), `on_confirm_deletes_only_confirmed`, `keep_days_respects_confirmed_at`, `disk_pct_deletes_oldest_confirmed_first_until_under_threshold`, `dry_run_deletes_nothing_but_reports` (deleter must NOT be called).
- [ ] **Step 2:** Run → FAIL. Implement; every would-delete in dry-run logs `warn!(path, policy, "retention dry-run: would delete")`; real deletes log `info!` + history row `outcome="retention_deleted"`.
- [ ] **Step 3:** Wire the Perseus tick (config-driven). Tests green. Commit `feat(sync): retention policies with hard never-delete-untransferred invariant (dry-run default)`.

**Acceptance:** invariant tests green; Perseus logs dry-run decisions. **Effort:** 3–4 days. **Depends:** A4.

### Task A9: M-Perseus-MVP soak — MANUAL GATE

**Files:** none (ops + sign-off). Record: `docs/superpowers/research/2026-07-XX-perseus-soak.md`

- [ ] **Step 1:** Deploy Perseus at the observatory (systemd, real capture dir, ticket from home primary, dry-run retention, dev-flag receiver on the home app).
- [ ] **Step 2:** Soak ≥1 week of real sessions. Watch via logs + `sync_history`.
- [ ] **Step 3:** Verify exit criteria (spec §7): all captures arrived with metadata; interruptions resumed; re-runs deduped (zero duplicate catalog rows — SQL check `SELECT uuid, COUNT(*) FROM frames GROUP BY uuid HAVING COUNT(*) > 1`); dry-run retention log correct against reality; history complete both ends.
- [ ] **Step 4:** Owner sign-off recorded in the soak doc → **M-Perseus-MVP done**. Any failure → fix-and-extend soak, do not proceed to M4 (retention live) without this gate.

**Effort:** 1 week elapsed (low active). **Depends:** A6, A7, A8.

---

## Track B — hub, auth, relay, account layer

> New repo `athenaeum-hub` (owner creates the GitLab project — manual step). Paths below are repo-relative. Stack: Axum + sqlx (Postgres) + `sqlx migrate`, config via env, Dockerfile + compose for local dev, GitLab CI (build, test, deploy via the VPS pattern used by artfrom-space).

### Task B1: Hub skeleton

**Files (athenaeum-hub repo):**
- Create: `Cargo.toml`, `src/main.rs`, `src/config.rs`, `src/db.rs`, `src/routes/mod.rs`, `migrations/0001_init.sql`, `Dockerfile`, `docker-compose.yml`, `.gitlab-ci.yml`, `README.md`

- [ ] **Step 1:** Scaffold: Axum router with `GET /api/v1/health` → `{"status":"ok","version":...}`; sqlx pool from `DATABASE_URL`; `sqlx migrate run` on boot; tracing JSON logs; graceful shutdown.
- [ ] **Step 2:** Integration test (`tests/health.rs`, `#[sqlx::test]`): health returns 200.
- [ ] **Step 3:** compose file: postgres:16 + hub; README quickstart. CI: `cargo test` with a service Postgres.
- [ ] **Step 4:** Deploy once to the VPS behind nginx at `projects.artfrom.space/api/v1/health` (nginx location block documented in README; TLS via existing certbot setup). Commit(s) in the hub repo.

**Acceptance:** health check answers over HTTPS from the VPS. **Effort:** 2–3 days.

### Task B2: OTP auth + device registry

**Files (athenaeum-hub):**
- Create: `src/routes/auth.rs`, `src/routes/devices.rs`, `src/mailer.rs`, `src/auth_mw.rs`, `migrations/0002_accounts.sql`
- Test: `tests/auth_flow.rs`

**Interfaces (produces — the client contract for B4):**

```
POST /api/v1/auth/otp     {email}                                    → 204 (always, no user enumeration)
POST /api/v1/auth/verify  {email, code, devicePubkey, deviceName}    → 200 {deviceToken, deviceId} | 401
GET  /api/v1/devices                          (Bearer deviceToken)   → [{id, name, pubkey, role, createdAt, lastSeenAt}]
POST /api/v1/devices/{id}/revoke              (Bearer)               → 204
POST /api/v1/devices/{id}/role {role, peerDeviceId?}  role: "primary"|"capture" → 200 | 409 "primary exists"
```

Migration `0002`: `accounts(id uuid PK default gen_random_uuid(), email text unique not null, created_at timestamptz)`; `otp_codes(email text, code_hash text, expires_at timestamptz, consumed bool default false)`; `devices(id uuid PK, account_id uuid references accounts, pubkey bytea unique not null, name text, role text, peer_device_id uuid, token_hash text not null, created_at timestamptz, last_seen_at timestamptz, revoked_at timestamptz)`; `CREATE UNIQUE INDEX one_primary_per_account ON devices(account_id) WHERE role = 'primary' AND revoked_at IS NULL;`

- [ ] **Step 1:** Failing integration tests: `otp_flow_creates_account_and_device` (otp → mailer stub captures code → verify → token works on /devices), `wrong_code_401_and_rate_limited` (6 rapid otp requests for one email → 429), `second_primary_409`, `revoked_token_rejected`, `otp_single_use`.
- [ ] **Step 2:** Implement: codes = 6 digits from `rand`, stored argon2-hashed, TTL 10 min; device token = 32 random bytes, returned base64, stored SHA-256-hashed; `mailer.rs` trait `Mailer { send_code(email, code) }` with `SmtpMailer` (lettre, env-configured host/creds) + `LogMailer` for dev/tests; rate limits via a small `otp_requests` counter table (per email + per IP, window 15 min).
- [ ] **Step 3:** Tests green; deploy; manual smoke with a real mailbox through the configured SMTP relay. Commit(s).

**Acceptance:** full flow works against the deployed hub with real email. **Effort:** ~1 week. **Depends:** B1.

### Task B3: Relay-map + relay auth callback + relay deployment

**Files (athenaeum-hub):** `src/routes/relay.rs`, `migrations/0003_relays.sql`, `docs/ops/relay.md`

- [ ] **Step 1:** `GET /api/v1/relay-map` (Bearer) → `{relays: ["https://relay1.artfrom.space"]}` from a `relays` table (url, active). `POST /api/v1/relay-auth` — **check iroh-relay's actual `access.http.url` request/response contract from its docs at implementation time**; semantic: body carries the connecting node id → 200 if pubkey ∈ non-revoked `devices` else 403. Integration tests for both (auth'd device passes, revoked fails).
- [ ] **Step 2:** Ops per `docs/ops/relay.md`: install `iroh-relay` on the VPS (or a second cheap VPS from day one if load-fear), DNS `relay1.artfrom.space`, TLS, config `access.http.url = https://projects.artfrom.space/api/v1/relay-auth` with bearer secret; systemd unit.
- [ ] **Step 3:** Manual verification: two NATed machines connect via the relay (reuse A5's validation example with relay-map pointing at relay1); revoke a device → connection refused.
- [ ] **Step 4:** Commit(s) + ops doc.

**Acceptance:** self-hosted authorized relay serving real traffic. **Effort:** 3–4 days. **Depends:** B2 (+A5 example for verification).

### Task B4: App account layer + Settings → Account UI

**Files (athenaeum repo):**
- Create: `crates/athenaeum-core/src/account/{mod.rs,client.rs,keys.rs,token_store.rs}`, `crates/athenaeum-core/src/api/account.rs`
- Modify: tauri commands + web routes (new `account` domain), `ts_export.rs`, `src/pages/Settings…` (Account section component `src/components/settings/AccountSection.tsx`)
- Test: core unit tests with a mocked hub (wiremock); frontend signed-out render test

**Interfaces:**
- Consumes: B2 endpoint contract verbatim.
- Produces: commands `account_sign_in_start(email)`, `account_sign_in_verify(email, code) -> AccountStatus`, `account_status() -> AccountStatus { signed_in, email?, device_id?, role? }`, `account_sign_out()`, `list_account_devices()`, `revoke_account_device(device_id)`, `set_machine_role(role, peer_device_id?)`; `keys.rs`: ed25519 keypair generated once, persisted; **pubkey == iroh node id** (use iroh's `SecretKey` type so there is exactly one key format); `token_store.rs`: `keyring` crate with file-0600 fallback.

- [ ] **Step 1:** Failing tests: client against wiremock (`sign_in_flow_stores_token`, `revoked_token_maps_to_signed_out`, `second_primary_conflict_surfaces_409_message`); keys (`keypair_persisted_once_stable_node_id`).
- [ ] **Step 2:** Implement client (reqwest, base URL from settings key `account.hub_url`, default `https://projects.artfrom.space`), token in keyring under service `com.vsharifov.athenaeum`, commands via core::api + wrappers + registration + ts types.
- [ ] **Step 3:** `AccountSection.tsx`: email field → code field → signed-in card (email, device list with revoke, machine-role selector with peer picker for capture role). Design tokens; errors surfaced via `notify()` tone `warning`.
- [ ] **Step 4:** **A2 guard:** frontend test (or scripted checklist) — with `account_status = signed_out`, all existing pages render; sync surfaces hidden. `npx tsc --noEmit` + workspace build green. Commit `feat(account): hub client, device keys, keychain token store, Settings → Account`.

**Acceptance:** sign-in from a dev build against the deployed hub; device appears in `GET /devices`. **Effort:** ~1 week. **Depends:** B2 (deployed).

---

## Merge phase (after A9 gate + B4)

### Task M1: Account pairing replaces tickets

**Files:** Modify `crates/athenaeum-core/src/sync/mod.rs` (peer resolution), `crates/perseus/src/{config.rs,run.rs}`, `crates/athenaeum-core/src/api/sync.rs`

- [ ] **Step 1:** Peer resolution order: account present → capture node's `peer_device_id` (from hub device list) resolves the primary's pubkey = NodeId; else dev flag `sync.dev_ticket_pairing` → ticket (kept for tests/offline dev). Perseus config gains `[account] email/hub_url` (sign-in via `perseus login` subcommand doing the OTP flow on the CLI) — ticket block becomes optional.
- [ ] **Step 2:** Relay-map plumbed: transport constructed with relays from `GET /relay-map` (fallback: iroh defaults under dev flag). Tests: unit for resolution order; manual: Perseus signs in, picks primary from device list, transfers via relay1.
- [ ] **Step 3:** Commit `feat(sync): account-based pairing (tickets behind dev flag), hub relay-map`.

**Effort:** 3–4 days. **Depends:** A9, B4.

### Task M2: Manual send mode (full-app capture node)

**Files:** Create `crates/athenaeum-core/src/api/sync.rs::enqueue_sync_selection`; Modify `db/schema.rs` (ensure A7 DDL in catalog store applies to capture-role installs), Objects/Browse selection toolbar (`src/components/...` per existing bulk-action pattern), tauri/web wrappers

- [ ] **Step 1:** Core test: `enqueue_sync_selection(frame_ids)` builds one package from exactly the selection (manifest from catalog rows incl. analysis summaries — B8), enqueues to the configured primary; ineligible files (missing on disk) reported back, not silently dropped.
- [ ] **Step 2:** UI: "Send to primary" bulk action visible only when `machine_role = capture` and signed in; mixed selections run on the eligible subset with `(N of M)` count (owner convention).
- [ ] **Step 3:** Auto mode for full-app capture nodes: hook the scanner "scan finished" completion path — new files enqueue when `sync.auto_mode = true` (per-node toggle in Settings → Sync). Test: scan N fixture files in auto mode → N queue entries.
- [ ] **Step 4:** Commit `feat(sync): manual send-to-primary + full-app auto mode`.

**Effort:** 3–4 days. **Depends:** M1.

### Task M3: Transfers UI + TransferIndicator + notifications

**Files:** Create `src/components/transfers/{TransferIndicator.tsx,TransfersPanel.tsx}`, `src/hooks/useSyncStatus.ts`; Modify `Layout.tsx` (panel at root, indicator in sidebar), `NotificationPanel.tsx` + kind union (`sync`), core `api/sync.rs` (status/history queries already from A7 — extend with `direction`/`peer` filters)

- [ ] **Step 1:** `useSyncStatus`: listens `sync-progress`/`sync-finished` (cancelled-flag pattern), polls `get_sync_status()` on mount; exposes `{active: TransferItem[], up_bps, down_bps, queued}`.
- [ ] **Step 2:** `TransferIndicator` next to `ComputeQueueIndicator`: ↑/↓ rates + queue badge; click opens `TransfersPanel` (slide-over at app root — NotificationPanel pattern): Active tab (per-item state/progress/cancel) + History tab (search by filename/object/peer, `formatTimestamp`).
- [ ] **Step 3:** Notifications on discrete outcomes from the existing completion handlers (NOT a new listener in NotificationContext): "N frames arrived from <device>" (`kind:'sync'`, dedupeKey = package id), "transfer failed", "retention blocked: disk full, nothing eligible". Icon added to the kind map.
- [ ] **Step 4:** `npx tsc --noEmit`; manual smoke over loopback dev mode. Commit `feat(ui): transfers panel + sidebar indicator + sync notifications`.

**Effort:** ~1 week. **Depends:** A7 events, M2.

### Task M4: Retention goes live

**Files:** Modify `crates/perseus/src/config.rs` (dry_run default stays true; docs state the opt-in), `crates/athenaeum-core/src/sync/retention.rs` (app-shell deleter = `file_op` delete pipeline), Settings → Sync (retention policy editor for full-app capture nodes)

- [ ] **Step 1:** App-shell deleter routes through `file_op` (catalog-consistent, history row appended) — test: retention delete removes file AND catalog row via the pipeline, history shows transfer + deletion as two events (BRD B6 acceptance).
- [ ] **Step 2:** Perseus: `dry_run = false` allowed only when config also sets `i_have_verified_the_soak = true` (explicit, greppable). Docs updated.
- [ ] **Step 3:** Re-run A8 invariant suite in live mode (deleter counts calls). Manual: observatory config flipped after A9 sign-off; first real policy deletion observed in history on both ends.
- [ ] **Step 4:** Commit `feat(sync): live retention (file_op-integrated in app; explicit opt-in in Perseus)`.

**Effort:** 2–3 days. **Depends:** A9 (gate), M1.

### Task M5: Two-instance E2E harness → M-Sync1

**Files:** Create `crates/athenaeum-core/tests/sync_e2e.rs` (loopback, CI) + `scripts/sync_e2e_manual.md` (real-transport runbook)

- [ ] **Step 1:** CI test: two `ServiceContext`s with separate temp DBs/data dirs; capture ctx auto-enqueues 50 fixture frames → primary ingests all with metadata; re-run is dedupe-safe (row counts stable); retention (live mode, tempdir) deletes at source per `on_confirm`; history complete on both; assert via SQL.
- [ ] **Step 2:** Manual runbook (M-Sync1 proof, spec §11): Perseus auto-mode 50-frame fixture run over real iroh + relay1 + hub pairing; full-app manual-send variant; hub device list shows both devices; record the demo script + results in the runbook doc.
- [ ] **Step 3:** Commit `test(sync): two-instance E2E harness + M-Sync1 runbook`. **M-Sync1 sign-off** recorded → Stage I done; update roadmap checkboxes.

**Effort:** ~1 week. **Depends:** everything above.

---

## Spec-coverage check (self-review)

- §2 D-1/D-2 rationale → A5 gate, B2 flow ✓; §3 tracks → task graph ✓; §4 trait/relays/validation → A2/A5/B3 ✓; §5 package v1 → A3 (+A5 root-hash swap) ✓; §6 engine/library/feature-gate/modes → A1/A4/M2 ✓; §7 Perseus MVP+product → A6/M1 ✓; §8 hub+account → B1/B2/B4 ✓ (aggregate-stats ingest stub: **deferred to Stage II accounting** — hub stores nothing Stage I needs; noted deviation from spec §8, harmless); §9 ingest/ack → A7 ✓; §10 UI → M3 ✓; §11 sequencing/milestones → A9/M5 ✓; §12 deferrals honored ✓.
- Known deviations recorded: `Delivered` state reserved-not-used (A4 step 2); stats-ingest stub dropped from B-track (above). Both are spec-compatible simplifications.
