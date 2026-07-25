# D3 — Multi-source Project Distribution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Project packages download from ALL their holders at once (per-frame fan-out, byte-level failover), every `send_receive` member auto-replicates published contributions (per-project toggle, default ON), and every device that holds a package seeds it at near-zero disk cost.

**Architecture:** Swarm fetch = `SplitStrategy::Split` over the hub holder list; the real collection hash reaches the hub at publish through the EXISTING `root_hash` pipe (only the value changes); seeding = `ImportMode::TryReference` under the reserved `project/…` tag namespace; auto-replication = a local need-diff worker over the hub announcement list. **Zero hub-side work.**

**Spec:** `docs/superpowers/specs/2026-07-26-multi-source-project-distribution-design.md` (incl. the §3.2 amendment) — read it before Task 1.

**Tech Stack:** Rust (iroh 1.0.2, iroh-blobs 0.103 — vendored at `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`), rusqlite, React/TS.

## Global Constraints

- Branch `0.5.0`; commit as repo default identity (eg013ra1n), NO Co-Authored-By/attribution.
- Two backends for every new command (Tauri `commands/collab*.rs` + web `routes/collab*.rs`, registered both sides, same commit).
- TDD per task: red → green → mutation check where the logic warrants → full gates (`cargo test -p athenaeum-core --lib`, `--test sync_e2e`, plus targeted suites) → commit.
- `rustfmt --edition 2021` scoped to OWN hunks only; NEVER format a `mod.rs` (cascades through the module tree).
- tracing style: message = short stable phrase, snake_case fields; never swallow errors.
- The fallback path (today's `ProjectRequest`-to-one) must remain byte-identical — every existing collab test stays green untouched; a red there is a finding, not a fixup.

## Verified anchors (checked against the tree at plan time — re-locate by grep, line numbers drift)

- Sequential holder loop: `api/collab_exchange.rs:1008` (probe 5 s `:488`, local-complete poll 2 s/90 s `:482-483`); candidates built `:967-976`; `report_have` calls `:1063`, `:818`.
- Single-provider choke: `sharing/iroh/blobs.rs:328` (scalar param), `Shuffled::new(vec![provider])` `:347`/`:442`; provider progress items dropped at `:466`.
- Vendored downloader: `download_with_opts` + `DownloadRequest { request, providers, strategy }` (`downloader.rs:296-343`, `:404-421`); Split = per-child requests `buffered_unordered(32)` (`:180-195`, `:440-482`); `execute_get` per-provider resume (`:486-555`); `ContentDiscovery` blanket impl for `Vec<EndpointId>`-likes (`:557-572`).
- Publish hash placeholder: `api/collab.rs:1299-1303` (BLAKE3 of manifest bytes, self-described identifier); hub POST at `:1354` region; row upsert `:1400-1410`; push-seed step 8 `:1448-1485`.
- `project_packages.root_hash` NOT NULL (`db/schema.rs:1893`), synced at poll (`api/collab_exchange.rs:615`).
- Tag namespace contract: `sharing/iroh/node.rs:274-284` (`recv_in_flight_package_id`); foreign-tag sweep-survival test seeds `"project/seed/deadbeef"` (`node.rs:~3035`).
- `ImportMode::TryReference`: vendored `api/proto.rs:630-644`; `add_path_with_opts(AddPathOptions { mode, .. })` (`api/blobs.rs:230`, `:265-268`, `:656-659`); honored by `store/fs/import.rs`. Our `import_package_collection` (`blobs.rs:105`) currently uses plain `add_path` (= Copy).
- Serve-dir reconstruction for re-serving a downloaded package: `handle_project_request` (`api/collab_exchange.rs:346-383`).
- Real-QUIC two-endpoint test harness: `sharing/iroh/tests.rs` (`mem_transport()`, `start_and_pair`, `build_package`; throttle/wall-clock tests are the style donors).
- `ReceiveGate`: `control.receive_gate.acquire()` via `SyncRuntime::inbound_control()`; project path today holds a bare permit (`receiver.rs:~2934`).
- W2's `IngestConn::Shared`; `ingest_project_package` (`sync/project_ingest.rs`).
- `local_status` lifecycle: `db/collab_exchange.rs:224-227` (`set_local_status`), values `none|downloading|complete|failed`.

---

### Task 1: `fetch_collection_multi` — the transport-level swarm fetch

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/iroh/blobs.rs` (new fn beside `fetch_collection_to_dir`)
- Modify: `crates/athenaeum-core/src/sharing/iroh/node.rs` (`role_fetch`-style wrapper), `crates/athenaeum-core/src/sharing/mod.rs` (transport surface)
- Test: `crates/athenaeum-core/src/sharing/iroh/tests.rs`

**Interfaces:**
- Produces: `SharingTransport::fetch_collection_multi(providers: Vec<NodeId>, root_hash: &str, byte_size: u64, dest_dir: &Path, sink: FetchSink, telemetry: ProviderTelemetrySink) -> Result<()>` where `ProviderTelemetrySink = Arc<dyn Fn(ProviderEvent) + Send + Sync>` and `enum ProviderEvent { Trying(NodeId), Failed(NodeId), ActiveCount(usize) }` (shape yours to what the progress items actually carry — read `DownloadProgressItem` variants in the vendored source first; `ActiveCount` may be derived, not native). Default trait impl returns `bail!("transport does not support multi-source fetch")` so the loopback mock and Perseus compile untouched.
- Consumes: nothing new.

Mechanics: same two-phase shape as the scalar fn — phase 1 (root+meta) via `download_with_opts` with the FULL provider set and `SplitStrategy::None` (meta is one tiny request; Split adds nothing), entry-name validation, in-flight tag set; phase 2 via `download_with_opts(DownloadRequest { request: GetRequest hash-seq all, providers: Shuffled::new(providers), strategy: SplitStrategy::Split })`; consume the progress stream routing provider items into `telemetry` instead of `_ => {}`; per-file observers, batch sink, permanent tag + in-flight retire, export loop — all identical to the scalar fn (LocalFault marking included). Do NOT refactor the scalar fn to delegate — copy deliberately and note why (the scalar fn is the fallback's load-bearing path; entangling them risks both).

- [ ] **Step 1: red tests.** Two real-QUIC tests in `tests.rs`:
  - `multi_fetch_uses_both_providers` — providers A and B `serve` the SAME package (build one package dir, serve on both endpoints; identical bytes ⇒ identical collection hash — assert `serve` returns/registers the same root hash on both, or fetch the hash via the announce of one). Puller C calls `fetch_collection_multi([A,B], hash, …)` with a recording telemetry sink. Assert: content lands + xxh3 matches; telemetry saw `Trying` for BOTH provider ids (with ≥8 children the probability both are hit is overwhelming; if flaky in 3 runs, raise the file count).
  - `multi_fetch_survives_a_provider_dying_mid_transfer` — pace provider A's serving? (No serve-side pacing exists — instead: SHUT DOWN provider A after the fetch starts: spawn the fetch, sleep ~200 ms, `a.shutdown()`; assert the fetch still completes and telemetry recorded `Failed(A)` or at minimum completion well under any timeout.) Red = the fn doesn't exist (compile error acceptable for a new API).
- [ ] **Step 2: implement** as above; run both tests → green; 3 consecutive runs for flake.
- [ ] **Step 3: gates + commit** `feat(sharing): fetch_collection_multi — Split fan-out across providers with per-provider telemetry (D3 T1)`.

### Task 2: publish sends the real collection hash (and becomes seed №1)

**Files:**
- Modify: `crates/athenaeum-core/src/api/collab.rs` (`publish_collab_frames`, the `:1295-1310` region + push-seed epilogue), `crates/athenaeum-core/src/sharing/iroh/blobs.rs` (`import_package_collection` gains an `ImportMode` parameter or a `_with_mode` sibling), `crates/athenaeum-core/src/sharing/iroh/node.rs` (a public `seed_project_collection(project_id, package_id, dir) -> Result<Hash>` that imports TryReference under `project/<project_id>/<package_id>` and records the served-collection entry so incoming GETs resolve).
- Test: `sharing/iroh/tests.rs`, `api/collab.rs` tests.

**Interfaces:**
- Produces: `SharedIrohNode::seed_project_collection(...) -> Hash` (also used by Task 4). Tag format: `project/<project_id>/<package_id>` — document in the namespace-contract comment at `node.rs:274-284` (the contract fn needs NO change — it already yields None for these).

Mechanics in `publish_collab_frames`: after `write_package`, BEFORE the hub POST: `let root_hash = node.seed_project_collection(&project_id, &package_id, &pkg_dir)?.to_hex()` (node via `ensure_iroh_node`; if the node cannot bind — offline publish is already impossible, the hub POST requires network — propagate the error exactly as a hub failure is handled today). Replace the manifest-BLAKE3 placeholder with it. Keep the old placeholder computation DELETED, not commented. TryReference targets the retained package dir (`local_dir` — verify it is never cleaned for collab packages; state the finding).

- [ ] **Step 1: red.** `publish_root_hash_is_the_collection_hash` (api/collab tests — find how existing publish tests stub the hub; assert the POSTed/stored `root_hash` equals a direct `import_package_collection` of the same dir) + `seed_tag_lands_and_survives_the_sweep` (extend the existing foreign-tag sweep test with a REAL seeded collection instead of the synthetic `project/seed/deadbeef`).
- [ ] **Step 2: implement**, green, plus: `try_reference_does_not_copy_payloads` — import a dir with a ~1 MB payload, assert the blob-store dir grew by ≪ payload size (give generous slack for metadata; the payload itself must not be duplicated).
- [ ] **Step 3: gates + commit** `feat(collab): publish seeds the collection and sends the real root hash (D3 T2)`.

### Task 3: the swarm download path

**Files:**
- Modify: `crates/athenaeum-core/src/api/collab_exchange.rs` (`download_project_package`), factoring the swarm attempt into a testable `fn swarm_fetch_plan(holders, own_node, root_hash_known, swarm_unfit) -> Option<Vec<NodeId>>` + the executing async fn.
- Test: `api/collab_exchange.rs` tests + one real-QUIC e2e in `sharing/iroh/tests.rs` or `tests/` (see Step 3).

Mechanics: in `download_project_package`, BEFORE the sequential loop: if the package row's `root_hash` parses as a plausible collection hash AND not in the session's `swarm_unfit` set (in-memory `Mutex<HashSet<String>>` keyed by package_id, on the ctx-adjacent state — pick the least invasive carrier and say which) AND ≥1 non-self holder: take ONE `ReceiveGate` permit (via `sync.inbound_control()`; receiver not started → skip gate, proceed — document), add relay-only dial hints per holder, `fetch_collection_multi` into the SAME staging layout the fallback uses, then `ingest_project_package` on `spawn_blocking` with `IngestConn::Shared`, `set_local_status` transitions exactly as the fallback does (`downloading` → `complete`/`failed`), `report_have` on success. NO ack on this path (spec §3.1.4 — comment it). On swarm failure (all providers exhausted): insert into `swarm_unfit`, `warn!`, FALL THROUGH to the sequential loop in the same call (not next cycle — the user pressed Download). Telemetry: journal per provider transition (reuse the collab package journal if one exists — grep; else `tracing` + the progress event) and emit a progress event carrying `sources: usize` (find the existing project-download progress event the UI listens to — if none exists, add `project-download-progress { packageId, sources }`, emitted on ActiveCount changes, and note the UI task consumes it).

- [ ] **Step 1: red.** Unit tests on `swarm_fetch_plan`: no holders → None; only self → None; unfit-cached → None; normal → Some(list minus self). Integration red: `swarm_download_falls_back_when_providers_lack_the_hash` — stub holders that don't serve the hash (loopback endpoints with nothing served), assert the sequential fallback still delivers (reuse an existing download test fixture — find how current download tests drive `download_project_package`; if none exists end-to-end, pin the fallback at the `swarm_unfit` + plan level and state so).
- [ ] **Step 2: implement**, green.
- [ ] **Step 3: the crown e2e** (real-QUIC, 3 endpoints): A publishes+seeds (T2 path), B swarm-fetches from [A] and seeds (T4 must exist for B-as-provider — if T4 not yet landed, write this test in T4 instead and say so here), C swarm-fetches from [A, B]; assert C's telemetry tried both. Place where the harness lives; mark `#[ignore]`-free but generous on timeouts.
- [ ] **Step 4: gates + commit** `feat(collab): swarm download — Split across all holders with graceful fallback (D3 T3)`.

### Task 4: downloaders seed after ingest

**Files:**
- Modify: `crates/athenaeum-core/src/api/collab_exchange.rs` (post-ingest hook path — both the swarm path's completion and the existing `report_have_after_ingest` (`:818`) flow), reusing `handle_project_request`'s serve-dir reconstruction (`:346-383`) + `SharedIrohNode::seed_project_collection` (T2).
- Modify: the project-data deletion site(s) — grep `delete` in api/collab*.rs / db/collab*.rs (leave-project, package delete, contribution cleanup — enumerate what exists) — each must also delete `project/<project_id>/<package_id>` tags (a `SharedIrohNode::unseed_project(project_id)` / per-package variant).
- Test: sharing/iroh tests + api tests.

Mechanics: after a successful ingest (swarm or fallback — ONE shared hook), reconstruct the serve dir the way `handle_project_request` does, `seed_project_collection` (TryReference — references the LANDED files; verify the reconstruction points at landed contribution paths, not staging, or seeding dies with the staging cleanup), THEN `report_have` (order matters: never advertise before the blobs are servable — comment it).

- [ ] **Step 1: red.** `downloader_seeds_and_serves_after_ingest` — loopback-or-real-QUIC: B ingests a package (drive the fallback path in a test fixture), assert `project/…` tag exists and a THIRD endpoint can fetch the collection from B (real-QUIC needed for the actual serve — combine with T3's crown e2e if written there).
- [ ] **Step 2: implement**, green. Deletion: `unseeding_removes_project_tags` — seed, delete the project's local data through the real deletion fn, assert tags gone AND unrelated `project/<other>/…` tags survive.
- [ ] **Step 3: gates + commit** `feat(collab): every downloader becomes a seed — TryReference under project tags (D3 T4)`.

### Task 5: auto-replication worker + toggle

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs` (guarded ALTER: `collab_projects.auto_replicate INTEGER NOT NULL DEFAULT 1`), `db/collab.rs` accessors, `api/collab_exchange.rs` (need diff + worker), `api/collab.rs` or `collab_exchange.rs` (`set_project_auto_replicate`, `sync_project_now` commands), host wrappers ×2 each.
- Test: pure-fn + worker tests in api/collab_exchange.rs.

Mechanics:
- `fn replication_need(packages: &[ProjectPackageRow], role_allows: bool, auto_on: bool) -> Vec<package_id>` — pure: `state == "published" && !superseded && origin != "mine" && local_status != "complete"`, empty when `!role_allows || !auto_on`. `failed` re-enters (retry by cadence).
- Worker: spawned once per app session alongside the other sync plumbing (find where post-`ensure_started` spawns live — the resurrect/orphan-sweep site in api/sync.rs is the pattern); loop every `COLLAB_AUTO_SYNC_INTERVAL: Duration = 20 min` + a `Notify` kicked after any `refresh_project_packages` that changed rows + by `sync_project_now`. Per pass: for each auto-enabled project (skip `data_role == "send"`), refresh announcements, compute need, download each via T3's path ONE at a time. Errors: per-package `warn!` + continue; never kill the loop.
- [ ] **Step 1: red** — the pure-fn tests (each clause), then worker tests: toggle-off → no downloads; send-role → none; two missing → both downloaded serially (stub the download call via a test seam — factor the worker body to take a `download_fn` closure).
- [ ] **Step 2: implement**, green. Commands + both hosts + registration.
- [ ] **Step 3: gates (workspace build — hosts touched) + commit** `feat(collab): auto-replication — published contributions download themselves (D3 T5)`.

### Task 6: UI (frontend-dev dispatch)

**Files:** the Projects page components (locate: `src/pages/Projects.tsx` + project detail components; read before briefing).
- Per-project: auto-replication toggle (default reflects `auto_replicate`) + published-bytes total (sum of package `byte_size` already in the local rows) + "Sync now" button (`sync_project_now`).
- Per-package: "downloading from N sources" line driven by the T3 progress event (StrictMode-safe listener pattern per CLAUDE.md); W2's `queued` chip for worker-queued packages (if the page's package rows have a status chip — reuse its idiom).
- Gate: `npx tsc --noEmit`. Commit `feat(collab): auto-replication toggle, sources figure, sync-now (D3 T6 UI)`.

### Task 7: docs + release note (orchestrator, inline)

- CLAUDE.md collab/transfers section: swarm fetch, real root hash at publish, `project/…` seeding now real (update the "future collab seeding" wording in the namespace contract comment reference), auto-replication + toggle, fallback semantics.
- RELEASE_NOTES.md: user-facing — projects download from everyone at once; joining a project auto-syncs published contributions (toggle per project); every member helps distribute; uploads stay under the device speed limit.
- Commit `docs: D3 — swarm distribution notes (T7)`.

### Task 8: whole-diff seam review (one reviewer agent)

Hunt list at minimum: staging-vs-landed path in T4's reconstruction (seeding staging would die with cleanup); TryReference file-mutation exposure (what happens on user edits — availability-only per spec §9, verify no panic path); the `swarm_unfit` cache lifetime vs publish-then-download-own-package edge; ReceiveGate interplay (worker serial + gate — can auto-replication starve personal sync or vice versa); dial-hint S1 (relay-only) on every new add_peer; `report_have` ordering vs seed readiness; fallback byte-identity (diff the sequential loop against pre-D3); double-ingest guard if swarm and a stray push-announce race for the same package (the lane path and the swarm path can both land the same package — walk it: `local_status` transitions + `ingest_project_package` idempotency).

## Landing order

T1 → T2 → T3 → T4 → T5 → T6 → T7 → T8. T2 is independent of T1 (can swap if a dispatch stalls). The crown e2e lands with whichever of T3/T4 completes second.

## Verification

- Per task: full `cargo test -p athenaeum-core --lib` + `--test sync_e2e`; real-QUIC tests 3 consecutive runs; workspace build where hosts touched; tsc for T6.
- Owner smoke afterwards: 3-device project — publish on A; B and C auto-download (watch "from N sources" on the second of them); kill A mid-download on C → C completes from B; toggle off on B → nothing new downloads; legacy package (published pre-D3) still downloads via fallback.

## Critical files

`crates/athenaeum-core/src/sharing/iroh/{blobs.rs,node.rs,tests.rs}` · `crates/athenaeum-core/src/sharing/mod.rs` · `crates/athenaeum-core/src/api/{collab.rs,collab_exchange.rs}` · `crates/athenaeum-core/src/db/{schema.rs,collab.rs,collab_exchange.rs}` · host command/route files ×4 · Projects page components
