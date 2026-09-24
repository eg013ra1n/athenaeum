# Collab v3 — Wave 2: app per-frame exchange — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The app publishes and receives collaboration data frame by frame
against the wave-1 hub. Every machine keeps exactly ONE copy of each project
frame, and that same file is both what the user sees and what the machine
seeds. A contributor calibrates each passing light once into its own project
folder. A processor replicates what its policy asks for. Holders are reported
from the disk, and a 15 s version poll drives everything. The package layer is
removed, and an outdated hub API shows "update required".

**Architecture — reuse first (owner instruction 2026-09-24: reuse the
exchange code that exists, re-verify what is bad, no extra copies).** The
existing code was audited component by component; the table and ledger are in
§"Reuse audit" below. The transport core is kept:
- the one-per-process auto-sync worker (`spawn_collab_auto_sync` →
  `auto_sync_loop_inner` → `run_auto_sync_pass`);
- the assignment fetch engine (`sharing/iroh/assign.rs`);
- the one-copy landing primitive `blobs::export_child`;
- the in-flight tag scheme and the vanished-source handling;
- the reference import (`add_path_child` + `ensure_child_readable`);
- the seed-tag namespace `project/…` and its prefix unseed;
- the role guard and dial hints;
- the scanner's known/moved/duplicate/unknown reconcile;
- the project WBPP collector.

These are **adapted** to frames: the need set, the held-set report (now from
the disk), the publish body and the landing transaction.

The package layer is **removed**, because it is what produces the extra
copies and the push-seed model the spec drops:
- package dirs and staging, `collab_pub`, `collab_serve`, `collab_seed` and
  `collab_swarm`;
- `write_package`/`stamp_extra_card` copies;
- `land_project_payload`'s `fs::copy`;
- push-serve and push-seed, and the collab sender engine;
- the swarm-unfit cache.

**One genuinely new piece** is the collaboration blob store under
`<Collab>/.athenaeum/blobs` (spec R6). It is built from existing parts:
`open_fs_store` is factored out of `node.rs`, `GatedBlobs` is reused as is,
and the provider-event consumer is factored and shares the one `UploadPacer`.
It is served on a second ALPN. The ledger below shows why nothing short of it
reaches one copy on every OS.

**Tech Stack:** Rust 2021, iroh `=1.2.0`, iroh-blobs `=0.103.0`, rusqlite,
wiremock (tests), React/TS + Vitest, ts-rs mirror (`src/types/models.ts`,
regenerated, never hand-edited).

**Spec:** `docs/superpowers/specs/2026-09-23-collab-v3-per-frame-model-design.md`
covers this wave in §5 (5.1–5.6), §3.2–3.3, §10, §11 and §14 wave 2, and
rulings R5, R6, R7, R12, R16, R17, R18 and R19. The hub contract is the hub
`main` at `6127951`, summarised below. Where the hub and the spec differ, the
hub wins.

## Global Constraints

- **Branch.** App repo `/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum`,
  branch `collab-v3-wave2` from `main` `49c3c171` (the plan commit on top).
  The hub is NOT touched in this wave.
- **Two backends in sync.** Every new or changed command gets its Tauri
  wrapper (`crates/athenaeum-tauri/src/commands/collab.rs`) and its Axum
  mirror (`crates/athenaeum-web/src/routes/collab.rs`) in the same task. Logic
  goes in core. Every wrapper wears `#[tracing::instrument(skip_all, err)]`
  (web: `err(Debug)` for `(StatusCode, String)`).
- **Frontend access.** No `@tauri-apps/*` outside `src/api/`, and the
  frontend calls only `api.invoke`. Use design tokens, never raw colours.
  Outcomes go through `notify()`, never on progress events.
- **TS mirror.** `src/types/models.ts` is generated. After any change to a
  ts-rs type, run
  `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` and commit
  the regenerated file.
- **Errors and logging.** Never swallow an error: every `Err` is logged
  (`error!`/`warn!`) before it is returned or folded. Log messages are short
  stable phrases with data in snake_case fields (`project_id`, `frame_uuid`,
  `path`, `count`, `error`, `outcome`, `duration_ms`). A new field name needs
  a spec update of the logging dictionary
  (`docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`, "Unified
  event schema"). This wave adds `project_id`, `frame_uuid`,
  `content_version`, `bytes` and `holders` there in Task 12.
- **No print macros.** `println!`/`eprintln!` are forbidden in production code.
- **No third-party project names** in code, comments, docs or commit messages.
- **Hub rule S1 is unchanged.** Cross-account dialing uses the holder's
  `relayUrl` only (`pairing::peer_dial_addr(.., relay_only = true)`).
- **Frozen wire.** The `Msg` postcard variants (`AnnounceProject`,
  `RequestProject`, …) stay in the enum, and the golden pins in
  `sharing/wire_golden_tests.rs` must stay green. Only their handling changes.
- **Personal sync must not change behaviour.** The personal-sync store
  `<working_dir>/blobs`, its tags, its roles and its sweeps stay as they are.
  `cargo test -p athenaeum-core` covers them.
- **Test gates.**
  - Each task ends green on
    `cargo test -p athenaeum-core --all-targets <filter>` for its own tests.
  - Every task that touches Rust also runs
    `cargo check --workspace --all-targets`.
  - Every task that touches the frontend also runs `npx tsc --noEmit` and
    `npx vitest run`.
  - The final task runs the FULL `cargo test -p athenaeum-core` (all targets,
    no filter), `cargo test --workspace`, and
    `cargo check -p athenaeum-core --no-default-features`.
- **Commits.** Commit as `eg013ra1n <vilen.sharifov@gmail.com>`. End each
  message with:
  ```
  Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01RYd9tanU4ehWLDXkPv159r
  ```
  No push, no deploy.
- **Formatting.** Run `rustfmt <files>` on the files you touched, not
  `cargo fmt -p`. Clippy is not a gate.

---

## Hub contract (hub `main` 6127951 — what the app codes against)

All paths are under `/api/v1`. Auth is `Authorization: Bearer <deviceToken>`.
Errors come back as `{"error": "..."}` or as an empty body. 401 and 403 always
have empty bodies.

| Call | Request | Response |
| ---- | ---- | ---- |
| `GET /me/project-versions` | — | `[{projectId, version}]` |
| `GET /me/projects` | — | `[{id, slug, title, dataRole, coordinator, requireApproval, pendingFrames, pendingAnnouncements(deprecated), govCaps[]}]` |
| `GET /projects/{id}/manifest?since&after&limit` | limit ≤ 1000 | `{projectVersion, rows: FrameView[], hasMore, next: {since, after} \| null}` ordered by `(manifestVersion, frameUuid)` |
| `POST /projects/{id}/frames` | `{frames: FrameIn[]}` (1..=500, 8 MB) | `{state, projectVersion, announced}` |
| `POST /projects/{id}/frames/{uuid}/version` | `{blake3, byteSize, xxh3}` | `{contentVersion, projectVersion}` |
| `POST /projects/{id}/frames/{uuid}/approve` | `{trust?: bool}` (body required, send `{}`) | `{published}` |
| `POST /projects/{id}/frames/{uuid}/reject` | `{reason}` 1..=500 | `{state:"rejected"}` |
| `PUT /projects/{id}/holders/self` | `{full?, add?: [{frameUuid, contentVersion}], remove?: [uuid]}` (≤10 000 each) | 204 |
| `GET /projects/{id}/frames/{uuid}/holders` | — | `[{pubkey, displayName, lastSeenAt?, relayUrl?}]` (fresh only) |
| `GET /projects/{id}/dictionary` | — | `{current: {version, entries: [{canonical, aliases[], kind}], createdAt} \| null, history}` |
| `GET /projects/{id}/thresholds` | — | `{current: {version, rules, createdAt} \| null, history}` (unchanged) |
| Retired: `…/announcements*`, `…/have` | any | **409 `{"error":"collab_api_outdated"}`** |

`FrameIn` has these fields:
- `frameUuid`
- `fileName`, with the hub rule: non-empty, at most 255 bytes, no
  surrounding whitespace, not `.` or `..`, and none of `/ \ :` NUL or
  control characters
- `blake3`, 64 lowercase hex characters
- `byteSize` > 0
- `xxh3`, 16 lowercase hex characters
- `filterRaw`, 1..=80 bytes after trimming
- `filterCanonical`, which must be a `canonical` of the current dictionary,
  exact and case-sensitive
- `channel`, one of `mono`, `osc`, `osc-r`, `osc-g`, `osc-b`
- `exptimeSec`, in (0, 86400]
- `dateObs?`, RFC 3339
- `gateVersion`, which must equal the current thresholds version, or 0 when
  the project has none
- `meta`, a JSON object of at most 8192 bytes

A batch is atomic. The hub refuses a whole batch in these cases:
- **stale `gateVersion`:** 409 `gate version X is stale, current is Y`;
- **unknown filter:** 400;
- **uuid already announced:** 409
  `frame {uuid} already announced; use /version …`;
- **project closed:** 409.

Announcing inserts the announcing device as a holder. That hold is still
subject to the 75-minute freshness rule, so it has to be refreshed.

`FrameView` fields:
- `frameUuid`, `publisherAccountId`, `publisherDisplayName` ("former member"
  after account deletion), `own`
- `fileName`, `contentVersion`, `blake3`, `byteSize`, `xxh3`
- `filterRaw`, `filterCanonical`, `channel`, `exptimeSec`, `dateObs?`, `meta`
- `gateVersion`, `accepted`, `acceptedReason?`
- `state` (`pending` | `published` | `rejected`), `rejectReason?`
- `manifestVersion`, `createdAt`, `holderCount`

Two visibility rules:
- **Pending and rejected rows** are visible only to their publisher and to
  holders of `data.moderate`.
- **No tombstones.** Rows are never deleted.

**Client caps rule (hub README).** When a project version moves, compare your
own `govCaps` and `coordinator` flag from `/me/projects` with the values you
saw last. On any change, re-fetch the manifest with `since=0` and prune every
cached row the fresh fetch no longer returns.

Holder permissions are filtered per row, and the hub silently drops rows that
fail. `send` members may hold only their own frames. `send_receive` members
and moderators may hold any published frame. Moderators may also hold
pending frames.

---

## Reuse audit (2026-09-24, verified in code; cite as "audit")

Paths are relative to `crates/athenaeum-core/src/`. In this section:
- `CE` = `api/collab_exchange.rs`
- `PI` = `sync/project_ingest.rs`
- `C` = `api/collab.rs`
- `N` = `sharing/iroh/node.rs`
- `B` = `sharing/iroh/blobs.rs`
- `A` = `sharing/iroh/assign.rs`
- `M` = `sharing/iroh/mod.rs`

### Verdicts

| Component | Verdict | Wave-2 change |
| ---- | ---- | ---- |
| `spawn_collab_auto_sync` CE:2854, `run_collab_auto_sync_loop` CE:2790, `auto_sync_loop_inner` CE:2830, `AUTO_SYNC_KICK` | KEEP, retune | The 15 s version tick kicks a full pass only when a version moved. The 20-minute interval stays as the retry and disk-truth cadence (Task 8). |
| `run_auto_sync_pass` CE:2612 | ADAPT | Keep: signed-out skip, per-project isolation, report-before-gate, the injected `download` seam. Change: manifest delta instead of the package refresh, per-frame need set, one batched fetch per project (Task 9). |
| `replication_need` CE:2579 | ADAPT | Spec §5.3 predicate over frame rows (Task 9). |
| `role_allows_replication` CE:2565 | KEEP | — |
| `download_project_package` CE:1745 | ADAPT, mostly remove | Keep: fail-closed role guard, per-provider dial hints (`peer_dial_addr(.., true)`, CE:2132-2149), `ReceiveGate` permit. Remove: sequential holder loop, `request_project`, `wait_for_local_complete`, `probe_holder`, `rearm_for_fallback`, `SWARM_UNFIT` and its helpers (Tasks 9, 12). |
| `try_swarm_download` CE:2104 | ADAPT | Becomes the per-project frame batch. Staging, ingest and `release` go away (Task 9). |
| `PackagePullClaim` | ADAPT | Same RAII claim, keyed by `(project_id, frame_uuid)` (Task 9). |
| `report_held_set` CE:1150 | ADAPT | Keep the 403→`Ok(0)` fold. The source becomes disk truth, not `held_package_ids_for_project`. This fixes the phantom-holder bug (Task 9). |
| `report_have_after_ingest`, `seed_ingested_package`, `seed_approved_announcement` | REMOVE | Replaced by "export → seed tag → holder delta" per frame (Task 12). |
| `reconstruct_seed_dir`, `reconstruct_serve_dir`, `materialize_package_dir`, `manifest_fully_local`, `package_fully_held` | REMOVE | Package dirs only. `materialize_package_dir` is a permanent cross-volume second copy (CE:393-408) (Task 12). |
| `handle_project_request`, `authorize_and_reconstruct_serve`, `ensure_collab_sender_engine`, `CollabCleanupSink`, collab sender runtime | REMOVE | Push-serve and push-seed are dropped by the spec. The `Msg` variants stay, and the receiver arms drop them with a `warn!` (Task 12). |
| `unseed_package_local_data`, `unseed_project_local_data` | ADAPT | Per-frame tag delete, plus the existing project-prefix delete (Tasks 5, 12). |
| `ingest_project_package` PI:77 | REMOVE | Package anchor and staging. Its silent `<working>/collaboration` fallback (PI:132-139) is the defect spec §5.1 names (Task 12). |
| `process_project_frame` PI:254 | ADAPT (parts) | Keep: `validate_rel_path` on the peer-supplied name, one tx per frame with orphan-file removal on tx error, the `sync_history` row, and version supersede (drop the old file and row). Moved into the landing step (Task 9). |
| `land_project_payload` PI:412 | REMOVE | Always `std::fs::copy` (PI:422): two copies even on one volume. Replaced by `export_child` (Task 9). |
| `publish_collab_frames` C:1150 | ADAPT heavily | Keep: gate, uuid handling, unseed on announce failure (F5), unseed of superseded content. Remove: the stamped staging copy (C:1239-1241), the `write_package` copy (C:1306), push-seed and `select_seed_target` (Task 7). |
| `seed_project_collection` N:2731 | ADAPT | Add a single-file raw sibling `seed_project_frame`. Fix: probe readability before reusing a cached seed (N:2740, unlike `role_serve`) (Task 5). |
| `unseed_project_package`, `unseed_project` N:2791/2814 | KEEP | Tag name `project/<pid>/<uuid>/<ver>`. The prefix delete is unchanged (Task 5). |
| `fetch_collection_multi` B:1064 | KEEP (personal and legacy use) | Not used per frame. It opens a pool per call and runs a phase-1 meta GET on the hard-wired ALPN. |
| `fetch_children_assigned` A:503 | ADAPT | Items carry their own `GetRequest`, hash, provider list and size. Per-item results. The ALPN is a parameter. Hedge back-half is `GetRequest::blob_ranges` for raw items. The collection caller keeps fail-fast (Task 4). |
| `export_child` B:653, `in_flight_tag` B:587, `export_source_vanished`/`on_export_source_vanished` B:610/711 | KEEP | Used as is per frame (Tasks 4, 9). |
| `add_path_child` B:204, `ensure_child_readable` B:265 | KEEP, one flag | The collab caller disables the `Copy` repair. A dead path is reported, never turned into a second owned copy (Task 5). |
| store open N:1009-1022, `GatedBlobs` M:1945, provider-event consumer M:~575, `UploadPacer` | KEEP, factor | `open_fs_store(dir)` and `spawn_provider_events(rx, pacer)` are shared by both stores (Task 3). |
| scanner `reconcile_project_contribution` (`scanner/mod.rs:628`) | ADAPT | Same four branches over `project_frames_local` (Task 12). |
| `export/project_collector.rs` | ADAPT | Reads `project_frames_local` (Task 12). |

### Disk-copy ledger for one frame

| Side | Today (package code) | Wave 2 |
| ---- | ---- | ---- |
| Publisher, peak | 3 calibrated copies: artifact + stamped staging (C:1241) + `write_package` copy (`package/writer.rs:73`) | 1 + the generator's sibling temp during its atomic rename |
| Publisher, steady | 2 | **1** (the file in `<Collab>/<project>/<me>/`, referenced by the store; outboard ≈ 0.4 %) |
| Receiver, peak | 2 (store data, then staged export, then `fs::copy` to the landing path, PI:422) | 1 per in-flight frame (store-owned data, until the export renames it) |
| Receiver, steady, same volume | 1 | **1** |
| Receiver, steady, Collaboration root on another volume | 2 (landing + `collab_seed` copy, CE:393-408) | **1** (the collab store sits on the root's volume, so the export is a rename) |

**Why a second store and not the one at `<working_dir>/blobs`** (audit D,
verified in iroh-blobs 0.103 `store/fs.rs:1284-1320`):
- `ExportMode::TryReference` renames, and falls back to copy-then-delete only
  on raw os error 18 (EXDEV).
- On Windows, a cross-volume rename returns `ERROR_NOT_SAME_DEVICE` (17), and
  the export **fails**.
- On Unix it survives, at the cost of a double write and a non-atomic copy
  into a scanned folder.
- Workarounds bring the second copy back:
  - staging and then copying brings back the dead-first-path problem, F2;
  - `Copy` export plus re-import leaves `Owned` winning the path union.

Reference import (publisher) works across volumes. Only landing needs the
same volume. One store serving both roles keeps a single seed namespace, so
the collab store holds own AND replica frames.

### Bugs this wave removes (audit F, verified)

- **F1.** `land_project_payload` always copies (PI:422).
- **F2.** A receiver seed stays readable only because `collab_seed` sorts
  before `collab_swarm`/`staging`, which is `paths.first()` after the store's
  `sort()`.
- **F3.** Cross-volume seed dirs are permanent duplicates (CE:393-408).
- **F4.** A missing Collaboration root silently falls back to the working
  dir (PI:132-139).
- **F5.** The held set is reported from the DB (CE:302), so a deleted file
  becomes a phantom holder.
- **F6.** `seed_project_collection` reuses a cached seed without a
  readability probe (N:2740).
- **F7.** Windows cross-volume export would be a hard error.
- **F9.** The push receive holds a gate slot with no abort
  (`sync/receiver.rs` ~3450).
- **F10.** Publish never runs, because of decision C (C:1181-1195).

## Plan rulings (decided here; cite as P1…)

- **P1 — Collab blob store.** `<Collab>/.athenaeum/blobs`, opened with the
  same code as the personal store: `open_fs_store(dir)`, factored out of
  `N:1009-1022`, GC on, `GC_INTERVAL` 900 s, never `FsStore::load`.
  - **Serving.** It is served by the existing `GatedBlobs` type, unchanged,
    wrapping `BlobsProtocol::new(&collab_store, Some(events2))` under
    `COLLAB_BLOBS_ALPN = b"athenaeum/collab-blobs/1"`, mounted in the same
    `build_router` (M:798).
  - **Events.** `events2` gets its own drain consumer from the factored
    `spawn_provider_events`, sharing the ONE `UploadPacer`. The device-wide
    upload cap therefore covers both stores, and every throttle request is
    still answered (the M:~575 rule).
  - **Root changes.** A router cannot add protocols after spawn. Setting,
    changing or clearing the Collaboration root rebuilds the router through
    the path relay changes already use (`apply_relay_change`, N:1641). The
    store handle lives in an `Arc<RwLock<Option<Store>>>` slot on the node;
    `None` means the ALPN is not mounted.
  - **Shutdown** flushes both stores.
  - **Verified in iroh-blobs 0.103:**
    - `BlobsProtocol::new(&Store, Option<EventSender>)`;
    - `EventSender: Clone`;
    - `util::connection_pool::ConnectionPool::new(endpoint, alpn, opts)`
      takes any ALPN;
    - `Remote::execute_get(conn, req)` does not care which ALPN the
      connection used.
- **P2 — `ATH_CVER` stays the calibration engine version.** The generator
  already writes it. The content version is not stamped, because the hashes
  are the content identity.
- **P3 — Filter mapping in wave 2 is automatic.** The trimmed `FILTER` is
  matched case-insensitively against each dictionary entry's `canonical` and
  `aliases`. No match fails the gate with
  `filter "<raw>" is not in the project dictionary`. The normaliser, the
  `filter_mappings` table and the modal belong to wave 3.
- **P4 — Gate header fallbacks stay for scale and centre.** The plate-solved
  precondition is wave 3.
  - **With a `plate_solves` row:** the header WCS cards are stripped from
    `GenerationSpec.cards` and replaced by `fits_writer::wcs::wcs_cards(&solve)`,
    and `meta.wcs` is filled with FITS 1-based `crpix`.
  - **Without one:** the header WCS is copied through and `meta.wcs` is
    omitted.
- **P5 — No zero point in wave 2.** That is wave 3.
- **P6 — Publish calibration options.** `CalibratedLightOptions::default()`
  with `debayer_osc = false` (R21, `splitOsc = false`: OSC ships as a CFA
  float FITS) and `format = Fits`.
  - `channel` is `"osc"` when the frame's `BAYERPAT` is vouched for by the
    catalog, else `"mono"`.
- **P7 — Per-frame "calibrated" verdict uses the one gate.** A frame is
  `Calibrated` when this is `Ok`:
  ```
  api::lights::check_mode_ready(&compute_export_readiness_for_frames(conn, set_id, &[frame_id])?, ExportMode::CalibratedLights)
  ```
  Otherwise the blocker text is the failure. This replaces decision C's
  constant (C:400).
- **P8 — Holder reports come from the disk.**
  - **Full report on every 20-minute pass,** after disk truth. Above 10 000
    held frames the first chunk goes `full=true` and the rest as delta `add`.
  - **Delta `add`** after every landing and every publish or version.
  - **Delta `remove`** for every frame disk truth drops.
  - **Supersedes spec §4.2.** The spec's "full every 6 h" is superseded by
    the hub's 75-minute freshness.
- **P9 — Manifest cursor.**
  - **Cursor value.** It is the highest `manifestVersion` applied.
  - **Paging.** Follow `next` until `hasMore = false`.
  - **Caps rule.** A caps or `coordinator` change resets the cursor to 0,
    and the refetch prunes non-own rows that no longer come back.
- **P10 — Publisher folder names.**
  - **Replicas.** The first landing from a publisher uses
    `sync::ingest::sanitize_slug(display name)`, made unique against other
    publishers' folders in the project. Later frames of that publisher reuse
    that folder, found through `publisher_account_id`.
  - **Own frames.** They use my own display name, uniquified by the same
    rule.
  - **Renames.** Renaming an account never moves files.
- **P11 — Fetch through the generalized assignment engine.** One call per
  project batch.
  - **Items.** Each item is `GetRequest::blob(hash)` with that frame's
    providers: fresh holders from `GET …/frames/{uuid}/holders` minus self,
    dialled relay-only.
  - **Reused behaviour.** Hedging, the stall watchdog, per-provider backoff
    and goodput stay as they are.
  - **Concurrency.** One `ReceiveGate` permit per project batch, which is
    the unit of lane accounting, as today's package was.
  - **Order.** Rarest-first by `holderCount`, then oldest `createdAt`.
  - **Failure isolation.** One frame failing never aborts its siblings.
- **P12 — Old wire variants.** The receiver arms `ProjectAnnounceReceived`
  and `ProjectRequestReceived` log
  `warn!("retired collab message ignored")` and drop. The variants stay.
- **P13 — Local preferences.** `collab_projects.auto_publish` (default 1,
  R16) and `auto_replicate` are local and never overwritten by a hub
  refresh.
- **P14 — Loss guard.**
  - **Trip condition.** Disk truth finds missing **replicas** above
    `collab.loss_guard_fraction` (0.10) of the held replicas, OR above
    `collab.loss_guard_bytes` (10737418240).
  - **On trip.** It sets `replication_paused = 1` and emits one
    `collab-replication-paused` event, with no re-fetch.
  - **Restore.** Rescan the Collaboration root (the scanner's "moved" branch
    repairs paths), run disk truth again, then unpause.
  - **Stop holding.** Set `locally_declined = 1` on the missing rows, then
    unpause.
  - **Settings.** The keys land now. Their Settings UI is wave 4.
- **P15 — Old tests.**
  - **Deleted.** The 9 `#[ignore]`d publish tests and
    `decision_c_blocks_publish_of_gate_eligible_frames` assert package,
    push-seed or decision-C semantics. They are deleted and replaced by the
    per-frame tests in Tasks 7 and 13.
  - **open-items.** The entries "Collab publish rework — its own cycle" and
    the Windows-coverage "9 further collab tests" note are updated.
- **P16 — E2E harness.** An in-process test with three `ServiceContext`s and
  three real iroh nodes, relay disabled (the existing `bind_node_into`),
  talking to one stateful fake hub. The fake hub is built on wiremock with
  custom `Respond` impls over `Arc<Mutex<FakeHubState>>`. The three-machine
  run across the test relay is an acceptance step after the test-hub deploy,
  written into open-items.
- **P17 — Outdated hub API.** `409 {"error":"collab_api_outdated"}` →
  `AccountClientError::CollabApiOutdated` →
  `ApiError::Conflict(COLLAB_API_OUTDATED_MSG)`. The frontend matches the
  prefix `collab_api_outdated` and shows "Update required" with a button that
  opens the updates dialog.
- **P18 — Frame uuid = `frames.uuid`.** It is also `ATH_CSRC`. A frame with
  an empty uuid fails the gate with `frame has no uuid`.
- **P19 — "Update pending".** For an own frame, `recipe_hash` is the xxh3 of
  these, in order:
  - the resolved master (path, strong hash or `size:mtime`) pairs;
  - `LIGHT_CAL_ENGINE_VERSION`;
  - the source `size:mtime`;
  - the P6 options JSON.

  A publish run that computes a different hash regenerates the frame and
  posts `…/version`.
- **P20 — Dead entries (iroh-blobs 0.103 has no public blob delete; a
  re-fetch over a vanished external file panics, B:703-709).**
  - **Probe first.** Before fetching or seeding a hash whose entry is
    `Complete`, run `blobs::probe_first_byte` (B:124).
  - **On a failed probe:** drop the frame's tags, set `awaiting_gc = 1`,
    skip it this pass, and never `Copy`-repair.
  - **Re-fetch.** The need set includes it again once
    `store.blobs().status(hash)` is `NotFound`.
  - **Timing.** The collab store's GC runs every 900 s, so a vanished
    replica is re-fetched within two passes.
  - **Pinned by a test.** A shorter collab GC interval is NOT part of this
    wave; the 900 s slack protects the unprotected windows and a change
    needs its own measurement.
- **P21 — Landing = `export_child` straight to the final path.** The target
  is `sync::ingest::unique_path(<Collab>/<project>/<publisher>/<fileName>)`.
  Never export to a temp and rename, because the store would then reference
  a dead path.
  - **Order:** in-flight tag → fetch → `export_child` → set the permanent
    seed tag `project/<pid>/<uuid>/<ver>` (`HashAndFormat::raw`) → delete
    the in-flight tag → one DB tx (row + `sync_history`) → holder delta
    `add`.
  - **A failing DB tx** removes the landed file, as `process_project_frame`
    does today, and drops the seed tag.
- **P22 — In-flight tags.** `in-flight/project/<pid>/<uuid>/<ver>`, set
  before any bytes move and deleted only on success. Opening the collab
  store sweeps stale `in-flight/project/` tags.
- **P23 — Scanner.** The walk of the Collaboration root skips
  `.athenaeum/`: a `filter_entry` in `scanner/mod.rs:155-162`, and in the
  batch walker too.
- **P24 — Byte-identical frames share an entry.** If `status(hash)` is
  already `Complete` under another frame's tag and its probe passes, the
  landing links or copies from that frame's landed path
  (`sync::ingest::link_or_copy`) and logs
  `warn!("identical frame content in project")`.
- **P25 — The Collaboration root is required for any receive or publish.**
  - **No silent fallback.** Without it, publish and replication refuse with
    `ApiError::Invalid("set a Collaboration folder in File Manager → Folders first")`,
    and the worker logs one `warn!` per pass and skips.
  - **Validation.** `validate_transfer_dir` gains `OverlapRule::Skip`, used
    only by `set_collaboration_dir`: the Collaboration root IS a scan root.
    It keeps the absolute-path, policy and write-probe checks.

- **P26 — The path of a project frame comes ONLY from `project_frames_local`**
  (owner ruling 2026-09-24, spec amendment A1). Every consumer takes the
  file path from the table. None infers it from the folder layout, and none
  from a header card. The consumers are disk truth, holder reports,
  seeding, stacking (wave 5), the WBPP export and the scanner.
  - **Own frames can live anywhere.** An own frame's `landed_path` may lie
    OUTSIDE the Collaboration root. The wave-3 externally calibrated
    original is seeded in place by reference: no copy, no stamps. Import by
    reference works across volumes, and only a receiver's landing needs the
    same volume.
  - **Scanner rule in the Collaboration root:**
    - The scanner never creates `files`/`frames` rows for any file under
      the Collaboration root, whatever its header says. A replica of an
      externally calibrated original carries no `ATH_PRJ` and no `CALSTAT`.
    - Each file there is reconciled against `project_frames_local`: first
      by `landed_path` (known), then by `(project, xxh3)` (moved or
      duplicate), else it is unknown and inert (R18).
    - Outside the Collaboration root the existing `ATH_PRJ` divert stays
      as defence in depth.
  - **Calibration status.** Being a project frame means "calibrated". The
    project stacking plan and the export decide it from the table (spec §7
    amendment), never from `CALSTAT`. Frame metadata (WCS, filter,
    exposure) comes from the manifest row.

---

## File structure

**Core (`crates/athenaeum-core/src/`):**
- `collab/hub_client.rs` — v3 calls and wire types (Task 1).
- `db/schema.rs`, `db/collab.rs`, new `db/collab_frames.rs` — the local
  table and project columns (Task 2).
- `sharing/iroh/node.rs`, `sharing/iroh/mod.rs` — `open_fs_store`,
  `spawn_provider_events`, the collab store slot, `COLLAB_BLOBS_ALPN` mount,
  router rebuild on root change (Task 3); `seed_project_frame` and
  `unseed_project_frame` (Task 5).
- `sharing/iroh/assign.rs`, `sharing/iroh/blobs.rs` — generalized items and
  `fetch_blobs_assigned` (Task 4); the raw single-file import (Task 5).
- `collab/gate.rs`, `api/collab.rs` (`frame_gate_inputs`), new
  `collab/filters.rs`, new `collab/frame_meta.rs` (Task 6).
- `api/collab.rs` `publish_collab_frames` — rewritten in place (Task 7).
- `api/collab_exchange.rs` — the worker and pass adapted in place (Tasks 8
  and 9). The package functions are deleted in Task 12. The file keeps its
  name to preserve blame and history.
- new `api/collab_autopublish.rs` (Task 10).
- `scanner/mod.rs`, `export/project_collector.rs`, `sync/receiver.rs`,
  `api/sync.rs` (Task 12).
- `settings/mod.rs` — two keys (Task 9).
- `ts_export.rs` (Tasks 2 and 11).
- new `collab/fake_hub.rs` (`#[cfg(test)]`, Task 8, grown in Tasks 9 and 13).

**Deleted in Task 12:**
- `sync/project_ingest.rs`
- `api/collab_e2e_tests.rs` (replaced by `api/collab_v3_e2e_tests.rs` in
  Task 13)
- the package functions of `api/collab_exchange.rs` and
  `db/collab_exchange.rs`
- the collab sender runtime
- the tables `project_packages` and `project_contributions`
- the folders `collab_pub`, `collab_serve`, `collab_seed` and
  `collab_swarm`, plus the legacy `project/` and `collab/` tags in the
  personal store

**Hosts:**
- `crates/athenaeum-tauri/src/commands/{collab,mod}.rs`, `lib.rs`
- `crates/athenaeum-web/src/routes/{collab,mod}.rs`

**Frontend:**
- `src/pages/{ProjectDetail,Projects}.tsx`
- `src/hooks/useProjects.ts`
- `src/components/collab/{ReceiveTab,ModerationQueue,AutoReplicateBar}.tsx`
- new `src/components/collab/UpdateRequired.tsx`
- `src/types/models.ts` (generated)

**Docs:**
- `docs/transfers/README.md`
- `docs/superpowers/open-items.md`
- the logging spec dictionary
- `CLAUDE.md` (Transfers/collab summary and the command-surface line)

---

### Task 1: Hub client speaks v3 — wire types, new calls, `CollabApiOutdated`

**Files:**
- Modify: `crates/athenaeum-core/src/collab/hub_client.rs`, `crates/athenaeum-core/src/account/client.rs` (enum variant + `Display`), `crates/athenaeum-core/src/api/collab.rs` (the `AccountClientError → ApiError` mapping, `COLLAB_API_OUTDATED_MSG`).
- Test: the `#[cfg(test)]` module of `hub_client.rs` (wiremock, extend).

**Interfaces:**
- Produces (all `#[serde(rename_all = "camelCase")]`):
  ```rust
  // account/client.rs
  pub enum AccountClientError { /* … existing … */ CollabApiOutdated }
  // collab/hub_client.rs
  pub struct MyProjectWire { pub id: String, pub slug: String, pub title: String, pub data_role: String,
      pub coordinator: bool, pub require_approval: bool, #[serde(default)] pub pending_frames: i64,
      #[serde(default)] pub gov_caps: Vec<String> }            // pendingAnnouncements no longer read
  pub struct ProjectVersionWire { pub project_id: String, pub version: i64 }
  pub struct FrameViewWire { pub frame_uuid: String, pub publisher_account_id: String,
      pub publisher_display_name: String, pub own: bool, pub file_name: String, pub content_version: i32,
      pub blake3: String, pub byte_size: i64, pub xxh3: String, pub filter_raw: String,
      pub filter_canonical: String, pub channel: String, pub exptime_sec: f64,
      pub date_obs: Option<String>, #[serde(default)] pub meta: serde_json::Value, pub gate_version: i32,
      pub accepted: bool, pub accepted_reason: Option<String>, pub state: String,
      pub reject_reason: Option<String>, pub manifest_version: i64, pub created_at: String,
      #[serde(default)] pub holder_count: i64 }
  pub struct ManifestCursorWire { pub since: i64, pub after: String }
  pub struct ManifestPageWire { pub project_version: i64, pub rows: Vec<FrameViewWire>,
      pub has_more: bool, pub next: Option<ManifestCursorWire> }
  #[derive(Serialize)] pub struct FrameInWire { pub frame_uuid: String, pub file_name: String,
      pub blake3: String, pub byte_size: i64, pub xxh3: String, pub filter_raw: String,
      pub filter_canonical: String, pub channel: String, pub exptime_sec: f64,
      #[serde(skip_serializing_if = "Option::is_none")] pub date_obs: Option<String>,
      pub gate_version: i32, pub meta: serde_json::Value }
  pub struct AnnounceFramesWire { pub state: String, pub project_version: i64, pub announced: usize }
  pub struct NewVersionWire { pub content_version: i32, pub project_version: i64 }
  #[derive(Serialize)] pub struct HolderRefWire { pub frame_uuid: String, pub content_version: i32 }
  pub struct DictionaryEntryWire { pub canonical: String, #[serde(default)] pub aliases: Vec<String>, pub kind: String }
  pub struct DictionarySetWire { pub version: i32, pub entries: Vec<DictionaryEntryWire> }
  pub struct DictionaryWire { pub current: Option<DictionarySetWire> }
  impl CollabClient {
      pub async fn project_versions(&self, token: &str) -> Result<Vec<ProjectVersionWire>, AccountClientError>;
      pub async fn manifest_page(&self, token: &str, project_id: &str, since: i64, after: Option<&str>, limit: u32) -> Result<ManifestPageWire, AccountClientError>;
      pub async fn announce_frames(&self, token: &str, project_id: &str, frames: &[FrameInWire]) -> Result<AnnounceFramesWire, AccountClientError>;
      pub async fn new_frame_version(&self, token: &str, project_id: &str, frame_uuid: &str, blake3: &str, byte_size: i64, xxh3: &str) -> Result<NewVersionWire, AccountClientError>;
      pub async fn approve_frame(&self, token: &str, project_id: &str, frame_uuid: &str, trust: bool) -> Result<u64, AccountClientError>;
      pub async fn reject_frame(&self, token: &str, project_id: &str, frame_uuid: &str, reason: &str) -> Result<(), AccountClientError>;
      pub async fn put_holders(&self, token: &str, project_id: &str, full: bool, add: &[HolderRefWire], remove: &[String]) -> Result<(), AccountClientError>;
      pub async fn frame_holders(&self, token: &str, project_id: &str, frame_uuid: &str) -> Result<Vec<HolderWire>, AccountClientError>;
      pub async fn dictionary(&self, token: &str, project_id: &str) -> Result<DictionaryWire, AccountClientError>;
  }
  // api/collab.rs
  pub const COLLAB_API_OUTDATED_MSG: &str = "collab_api_outdated: this hub needs a newer Athenaeum — update to keep collaborating";
  ```
- Consumes: nothing new.

The old package methods (`announce`, `list_announcements`,
`approve_announcement`, `reject_announcement`, `report_have`,
`report_have_set`) and their wire structs stay until Task 11, because callers
still compile against them. Mark each one
`#[deprecated(note = "collab v3: removed in wave 2 Task 11")]` and add
`#[allow(deprecated)]` at the call sites, so no new caller appears.

- [ ] **Step 1: Write the failing tests** (in `hub_client.rs` tests, wiremock):

```rust
#[tokio::test]
async fn outdated_api_409_is_typed_on_get_and_post() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/v1/me/project-versions"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({"error":"collab_api_outdated"})))
        .mount(&server).await;
    Mock::given(method("POST")).and(path("/api/v1/projects/p1/frames"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({"error":"collab_api_outdated"})))
        .mount(&server).await;
    let c = CollabClient::new(server.uri()).unwrap();
    assert!(matches!(c.project_versions("t").await, Err(AccountClientError::CollabApiOutdated)));
    assert!(matches!(c.announce_frames("t", "p1", &[]).await, Err(AccountClientError::CollabApiOutdated)));
}

#[tokio::test]
async fn other_409_keeps_the_hub_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/v1/projects/p1/frames"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({"error":"gate version 1 is stale, current is 2"})))
        .mount(&server).await;
    let c = CollabClient::new(server.uri()).unwrap();
    let err = c.announce_frames("t", "p1", &[]).await.unwrap_err();
    assert!(err.to_string().contains("gate version 1 is stale"), "{err}");
}

#[tokio::test]
async fn manifest_page_passes_cursor_and_decodes_rows() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/v1/projects/p1/manifest"))
        .and(query_param("since", "7")).and(query_param("after", "u9")).and(query_param("limit", "1000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "projectVersion": 9, "hasMore": false, "next": null,
            "rows": [{ "frameUuid":"u10","publisherAccountId":"a","publisherDisplayName":"Ann","own":false,
              "fileName":"c_x.fits","contentVersion":1,"blake3":"b".repeat(64),"byteSize":10,
              "xxh3":"0123456789abcdef","filterRaw":"Red","filterCanonical":"R","channel":"mono",
              "exptimeSec":300.0,"dateObs":null,"meta":{},"gateVersion":0,"accepted":true,
              "acceptedReason":null,"state":"published","rejectReason":null,"manifestVersion":9,
              "createdAt":"2026-09-24T00:00:00Z","holderCount":2 }]})))
        .mount(&server).await;
    let c = CollabClient::new(server.uri()).unwrap();
    let page = c.manifest_page("t", "p1", 7, Some("u9"), 1000).await.unwrap();
    assert_eq!(page.project_version, 9);
    assert_eq!(page.rows[0].filter_canonical, "R");
    assert_eq!(page.rows[0].holder_count, 2);
}

#[tokio::test]
async fn my_projects_reads_pending_frames_and_caps_without_the_alias() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/v1/me/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id":"p1","slug":"m31","title":"M31","dataRole":"send_receive","coordinator":false,
            "requireApproval":true,"pendingFrames":3,"govCaps":["data.moderate"]}])))
        .mount(&server).await;
    let c = CollabClient::new(server.uri()).unwrap();
    let p = &c.my_projects("t").await.unwrap()[0];
    assert_eq!(p.pending_frames, 3);
    assert_eq!(p.gov_caps, vec!["data.moderate".to_string()]);
}

#[tokio::test]
async fn put_holders_sends_full_add_remove() {
    let server = MockServer::start().await;
    Mock::given(method("PUT")).and(path("/api/v1/projects/p1/holders/self"))
        .and(body_json(json!({"full": true, "add":[{"frameUuid":"u1","contentVersion":2}], "remove": []})))
        .respond_with(ResponseTemplate::new(204)).expect(1).mount(&server).await;
    let c = CollabClient::new(server.uri()).unwrap();
    c.put_holders("t", "p1", true, &[HolderRefWire{frame_uuid:"u1".into(), content_version:2}], &[]).await.unwrap();
}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib collab::hub_client`. The tests should fail to compile, because the new items do not exist yet.

- [ ] **Step 3: Implement.**
  - **One status classifier.** Add a single private
    `async fn classify(status, resp, what) -> AccountClientError` used by
    `get_json` AND by every POST/PUT/PATCH path:
    - `401` → `Unauthorized`;
    - `403` → `Forbidden`;
    - `409` whose body `error` equals `"collab_api_outdated"` →
      `CollabApiOutdated`;
    - everything else → `Network(format!("hub returned {status} ({what}): {msg}"))`,
      with the body message included.
    - `get_json` loses its old "unexpected status" arm.
  - **New error variant.** Add
    `AccountClientError::CollabApiOutdated` with
    `Display = "collab_api_outdated"`.
  - **Map it in the api layer.** In `api/collab.rs`, extend the existing
    error-mapping helper (search for `AccountClientError::Unauthorized =>`)
    so `CollabApiOutdated` → `ApiError::Conflict(COLLAB_API_OUTDATED_MSG.into())`.
    Log it with `warn!(outcome = "collab_api_outdated", "hub refused an outdated collab api")`
    the first time per process, using a `static OnceLock<()>` so the 15 s
    poll does not flood the log.
  - **New methods.** Implement them. `approve_frame` always sends
    `{"trust": trust}`.
  - **Parse, not trust.** `manifest_page` builds the query with
    `reqwest`'s `.query(&[...])`, and `limit` is clamped to 1..=1000 by the
    caller. `put_holders` serialises `{"full", "add", "remove"}`, always all
    three keys.
  - **Mirror it.** Add `ProjectWire.version: Option<i64>` (serde default)
    for the public page.

- [ ] **Step 4: Run** `cargo test -p athenaeum-core --lib collab::hub_client`. Expect PASS. Then run `cargo check --workspace --all-targets`.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/collab/hub_client.rs crates/athenaeum-core/src/account/client.rs crates/athenaeum-core/src/api/collab.rs
git commit -m "feat(collab): hub client speaks the per-frame api; collab_api_outdated is typed"
```

---

### Task 2: Local schema — `project_frames_local`, project columns, DB module

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs` (inside `init_db`, after the `collab_projects` block ~:2272–2306)
- Modify: `crates/athenaeum-core/src/db/collab.rs` (`SELECT_COLS`, `CollabProjectRow`, `row_from_sql`, `upsert_project`)
- Modify: `crates/athenaeum-core/src/db/mod.rs` (`pub mod collab_frames;`)
- Create: `crates/athenaeum-core/src/db/collab_frames.rs`
- Test: unit tests in `db/collab_frames.rs` and `db/collab.rs`; the schema idempotency test in `db/schema.rs` (extend the existing `init_db_is_idempotent`-style test; search `fn init_db_twice`)

**Interfaces:**
- Produces:
  ```rust
  // db/collab_frames.rs
  #[derive(Debug, Clone, PartialEq)]
  pub struct LocalFrameRow {
      pub project_id: String, pub frame_uuid: String, pub content_version: i32,
      pub origin: FrameOrigin,              // Own | Replica
      pub publisher_account_id: String, pub publisher_display: String,
      pub file_name: String, pub filter_canonical: String, pub state: String, pub accepted: bool,
      pub byte_size: i64, pub xxh3: String, pub blake3: String, pub holder_count: i64,
      pub manifest_version: i64, pub manifest_json: String,
      pub landed_path: Option<String>,      // None until landed (replica) / written (own)
      pub size_mtime_seen: Option<String>,  // "size:mtime_secs" at the last verified hash
      pub on_disk: bool, pub locally_declined: bool, pub awaiting_gc: bool,
      pub source_frame_id: Option<i64>,     // own only
      pub recipe_hash: Option<String>,      // own only (P19)
      pub last_error: Option<String>, pub updated_at: String,
  }
  pub enum FrameOrigin { Own, Replica }    // stored as 'own' | 'replica'
  pub fn upsert_from_manifest(conn: &Connection, project_id: &str, v: &FrameViewWire) -> Result<()>; // never touches landed_path/on_disk/locally_declined/awaiting_gc/source_frame_id/recipe_hash
  pub fn get(conn: &Connection, project_id: &str, frame_uuid: &str) -> Result<Option<LocalFrameRow>>;
  pub fn list_for_project(conn: &Connection, project_id: &str) -> Result<Vec<LocalFrameRow>>;
  pub fn delete_not_in(conn: &Connection, project_id: &str, keep: &HashSet<String>) -> Result<usize>; // caps-rule prune; never deletes origin='own'
  pub fn record_own(conn: &Connection, row: &LocalFrameRow) -> Result<()>;      // INSERT OR REPLACE for origin own
  pub fn set_landed(conn: &Connection, project_id: &str, frame_uuid: &str, landed_path: &str, size_mtime: &str) -> Result<()>; // on_disk=1, awaiting_gc=0, last_error NULL
  pub fn set_missing(conn: &Connection, project_id: &str, frame_uuid: &str, awaiting_gc: bool) -> Result<()>; // on_disk=0
  pub fn set_declined(conn: &Connection, project_id: &str, frame_uuids: &[String], declined: bool) -> Result<()>;
  pub fn set_error(conn: &Connection, project_id: &str, frame_uuid: &str, error: Option<&str>) -> Result<()>;
  pub fn find_by_landed_path(conn: &Connection, path: &str) -> Result<Option<LocalFrameRow>>;
  pub fn find_by_project_and_xxh3(conn: &Connection, project_id: &str, xxh3: &str) -> Result<Vec<LocalFrameRow>>;
  pub fn update_landed_path(conn: &Connection, project_id: &str, frame_uuid: &str, path: &str) -> Result<()>;
  pub fn own_by_source_frame(conn: &Connection, project_id: &str) -> Result<HashMap<i64, LocalFrameRow>>;
  pub fn publisher_dir(conn: &Connection, project_id: &str, publisher_account_id: &str) -> Result<Option<PathBuf>>; // parent of any landed row of that publisher (P10)
  // db/collab.rs — CollabProjectRow gains (all LOCAL unless noted):
  pub pending_frames: i64,          // hub (renamed column, was pending_announcements)
  pub gov_caps_json: String,        // hub, '[]' default; current caps + a "coordinator" element when is_coordinator
  pub synced_caps_json: String,     // caps at the last successful manifest sync (P9), '[]' default
  pub hub_version: i64,             // last projects.version fully applied, 0 default
  pub manifest_cursor: i64,         // P9, 0 default
  pub dictionary_version: Option<i32>, pub dictionary_json: Option<String>, // hub
  pub policy_json: String,          // LOCAL, default '{"mode":"all"}'
  pub replication_paused: bool,     // LOCAL (P14)
  pub auto_publish: bool,           // LOCAL (P13), default 1
  pub fn set_sync_state(conn: &Connection, project_id: &str, hub_version: i64, manifest_cursor: i64, synced_caps_json: &str) -> Result<()>;
  pub fn set_dictionary(conn: &Connection, project_id: &str, version: Option<i32>, entries_json: Option<&str>) -> Result<()>;
  pub fn set_policy(conn: &Connection, project_id: &str, policy_json: &str) -> Result<()>;
  pub fn set_replication_paused(conn: &Connection, project_id: &str, paused: bool) -> Result<()>;
  pub fn set_auto_publish(conn: &Connection, project_id: &str, on: bool) -> Result<()>;
  ```
- Consumes: `FrameViewWire` (Task 1).

DDL (in `init_db`, idempotent):

```sql
CREATE TABLE IF NOT EXISTS project_frames_local (
    project_id           TEXT NOT NULL,
    frame_uuid           TEXT NOT NULL,
    content_version      INTEGER NOT NULL,
    origin               TEXT NOT NULL CHECK (origin IN ('own','replica')),
    publisher_account_id TEXT NOT NULL,
    publisher_display    TEXT NOT NULL,
    file_name            TEXT NOT NULL,
    filter_canonical     TEXT NOT NULL,
    state                TEXT NOT NULL,
    accepted             INTEGER NOT NULL DEFAULT 1,
    byte_size            INTEGER NOT NULL,
    xxh3                 TEXT NOT NULL,
    blake3               TEXT NOT NULL,
    holder_count         INTEGER NOT NULL DEFAULT 0,
    manifest_version     INTEGER NOT NULL DEFAULT 0,
    manifest_json        TEXT NOT NULL DEFAULT '{}',
    landed_path          TEXT UNIQUE,
    size_mtime_seen      TEXT,
    on_disk              INTEGER NOT NULL DEFAULT 0,
    locally_declined     INTEGER NOT NULL DEFAULT 0,
    awaiting_gc          INTEGER NOT NULL DEFAULT 0,
    source_frame_id      INTEGER,
    recipe_hash          TEXT,
    last_error           TEXT,
    updated_at           TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (project_id, frame_uuid),
    FOREIGN KEY (project_id) REFERENCES collab_projects(project_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_project_frames_local_project_xxh3 ON project_frames_local(project_id, xxh3);
CREATE INDEX IF NOT EXISTS idx_project_frames_local_source ON project_frames_local(source_frame_id);
```

Notes on the DDL:
- **Foreign-key index.** The `project_id` child column is the PK prefix, so
  `every_foreign_key_child_column_is_indexed` is satisfied. Check that the
  test accepts a PK prefix. If it does not, add
  `idx_project_frames_local_project` to its list.
- **Why `source_frame_id` has no FK.** A deleted source frame must not
  delete an own published frame (R12). The column is nulled by
  `ON DELETE`-free code: frame deletion leaves it dangling on purpose, and
  the publish run treats a dangling id as "source gone".

Column changes on `collab_projects`, each guarded by `column_exists`:
- `ALTER TABLE collab_projects RENAME COLUMN pending_announcements TO pending_frames`
  (only when the old name exists and the new one does not);
- then `ADD COLUMN` for `gov_caps_json TEXT NOT NULL DEFAULT '[]'`,
  `synced_caps_json TEXT NOT NULL DEFAULT '[]'`,
  `hub_version INTEGER NOT NULL DEFAULT 0`,
  `manifest_cursor INTEGER NOT NULL DEFAULT 0`,
  `dictionary_version INTEGER`, `dictionary_json TEXT`,
  `policy_json TEXT NOT NULL DEFAULT '{"mode":"all"}'`,
  `replication_paused INTEGER NOT NULL DEFAULT 0` and
  `auto_publish INTEGER NOT NULL DEFAULT 1`.

`upsert_project` still leaves out every LOCAL column (`auto_replicate`,
`policy_json`, `replication_paused`, `auto_publish`) and the sync-state
columns. It writes `pending_frames`. Update the other readers of the
`pending_announcements` field to use `pending_frames`:
- `collab/authz.rs:194`
- `api/collab_exchange.rs:3193` (test code; it compiles until Task 11)
- `api/collab.rs` `ProjectCard.pending_announcements`, renamed
  `pending_frames`. This regenerates TS in Task 10, not here: keep the ts-rs
  export compiling by renaming now and regenerating `models.ts` in this task
  (`TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`). Then fix
  the only TS reader, `src/pages/Projects.tsx:62–64`, to `pendingFrames` so
  `npx tsc --noEmit` stays green.

- [ ] **Step 1: Write the failing tests** (`db/collab_frames.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    fn conn() -> Connection { let c = Connection::open_in_memory().unwrap(); crate::db::schema::init_db(&c).unwrap();
        c.execute("INSERT INTO collab_projects (project_id, slug, title, data_role) VALUES ('p1','m31','M31','send_receive')", []).unwrap(); c }
    fn view(uuid: &str, mv: i64) -> FrameViewWire { serde_json::from_value(serde_json::json!({
        "frameUuid": uuid, "publisherAccountId":"a1","publisherDisplayName":"Ann","own":false,
        "fileName": format!("c_{uuid}.fits"),"contentVersion":1,"blake3":"b".repeat(64),"byteSize":100,
        "xxh3":"0123456789abcdef","filterRaw":"Red","filterCanonical":"R","channel":"mono","exptimeSec":300.0,
        "meta":{},"gateVersion":0,"accepted":true,"state":"published","manifestVersion":mv,
        "createdAt":"2026-09-24T00:00:00Z","holderCount":1})).unwrap() }

    #[test]
    fn manifest_upsert_keeps_local_disk_state() {
        let c = conn();
        upsert_from_manifest(&c, "p1", &view("u1", 1)).unwrap();
        set_landed(&c, "p1", "u1", "/collab/m31/ann/c_u1.fits", "100:1700000000").unwrap();
        let mut v = view("u1", 2); v.accepted = false; v.accepted_reason = Some("clouds".into());
        upsert_from_manifest(&c, "p1", &v).unwrap();
        let r = get(&c, "p1", "u1").unwrap().unwrap();
        assert!(r.on_disk && !r.accepted);
        assert_eq!(r.landed_path.as_deref(), Some("/collab/m31/ann/c_u1.fits"));
        assert_eq!(r.manifest_version, 2);
    }

    #[test]
    fn prune_never_deletes_own_rows() {
        let c = conn();
        upsert_from_manifest(&c, "p1", &view("u1", 1)).unwrap();
        upsert_from_manifest(&c, "p1", &view("u2", 1)).unwrap();
        let mut own = get(&c, "p1", "u2").unwrap().unwrap(); own.origin = FrameOrigin::Own; record_own(&c, &own).unwrap();
        let n = delete_not_in(&c, "p1", &HashSet::new()).unwrap();
        assert_eq!(n, 1);
        assert!(get(&c, "p1", "u2").unwrap().is_some());
    }

    #[test]
    fn publisher_dir_is_the_parent_of_an_existing_landing() {
        let c = conn();
        upsert_from_manifest(&c, "p1", &view("u1", 1)).unwrap();
        set_landed(&c, "p1", "u1", "/collab/m31/ann/c_u1.fits", "100:1").unwrap();
        assert_eq!(publisher_dir(&c, "p1", "a1").unwrap(), Some(PathBuf::from("/collab/m31/ann")));
        assert_eq!(publisher_dir(&c, "p1", "zz").unwrap(), None);
    }

    #[test]
    fn project_delete_cascades() {
        let c = conn(); c.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        upsert_from_manifest(&c, "p1", &view("u1", 1)).unwrap();
        c.execute("DELETE FROM collab_projects WHERE project_id='p1'", []).unwrap();
        assert!(list_for_project(&c, "p1").unwrap().is_empty());
    }
}
```

Add to `db/collab.rs` tests:
- `upsert_project_preserves_local_columns`: set `auto_publish = 0`,
  `policy_json` and `replication_paused`, then upsert again and assert all
  three are unchanged.
- `rename_pending_announcements_is_idempotent`: build a DB with the OLD
  column by executing the old `collab_projects` DDL, run `init_db` twice,
  and assert `pending_frames` exists and `pending_announcements` does not.

- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib db::collab`. Expect FAIL.
- [ ] **Step 3: Implement** the DDL, the migration guards and the functions.
  `upsert_from_manifest` stores `manifest_json = serde_json::to_string(v)`
  and fills the extracted columns. Its `ON CONFLICT(project_id, frame_uuid) DO UPDATE`
  sets only the manifest-derived columns plus `updated_at`. When
  `content_version` increases, the same statement also sets
  `on_disk = 0, size_mtime_seen = NULL` for `origin = 'replica'`, because a
  new version is new content that must be fetched. Use a `CASE`
  expression for that.
- [ ] **Step 4: Run** `cargo test -p athenaeum-core --lib db::` and `cargo test -p athenaeum-core --test ts_contract`. Expect PASS. Then run `npx tsc --noEmit`.
- [ ] **Step 5: Commit** `feat(collab): project_frames_local and the v3 project columns`.

---

### Task 3: The collab blob store on the one node (P1, P22, P23, P25)

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/iroh/node.rs`
  - `N:1009-1022`: extract `open_fs_store`;
  - add the `collab_store` slot and `collab_store()`;
  - `set_collab_root`;
  - `shutdown`;
  - the startup in-flight sweep.
- Modify: `crates/athenaeum-core/src/sharing/iroh/mod.rs`
  - `build_router` M:514/798 takes an optional second `(Store, EventSender)`;
  - extract `spawn_provider_events(rx, pacer, …)` from the consumer at
    M:~575;
  - add `pub const COLLAB_BLOBS_ALPN: &[u8] = b"athenaeum/collab-blobs/1";`.
- Modify: `crates/athenaeum-core/src/api/sync.rs` (`OverlapRule::Skip`,
  `validate_transfer_dir` :231).
- Modify: `crates/athenaeum-core/src/api/scan_roots.rs`
  (`set_collaboration_dir` → `OverlapRule::Skip` validation, then
  `node.set_collab_root(Some(path))`; `clear_collaboration_dir` →
  `set_collab_root(None)`).
- Modify: `crates/athenaeum-core/src/api/sync.rs` `ensure_iroh_node`
  (~:1174): after bind, if a Collaboration root is configured, call
  `set_collab_root`.
- Modify: `crates/athenaeum-core/src/scanner/mod.rs` — skip `.athenaeum/`
  in both walkers (P23).
- Test: `sharing/iroh/node.rs` tests (real nodes, relay disabled) and
  `api/scan_roots.rs` tests.

**Interfaces:**
- Produces:
  ```rust
  // sharing/iroh/node.rs
  pub(crate) async fn open_fs_store(dir: &Path) -> anyhow::Result<Store>;             // GC on, GC_INTERVAL
  impl SharedIrohNode {
      pub async fn set_collab_root(&self, root: Option<&Path>) -> anyhow::Result<()>; // opens <root>/.athenaeum/blobs, sweeps in-flight/project/, rebuilds the router; None unmounts and shuts the old store
      pub fn collab_store(&self) -> Option<Store>;                                     // clone of the slot
      pub fn endpoint(&self) -> Endpoint;                                              // if not already public
  }
  // sharing/iroh/mod.rs
  pub const COLLAB_BLOBS_ALPN: &[u8];
  // api/sync.rs
  pub(crate) enum OverlapRule { Reject, Warn, Skip }
  // api/collab.rs (or a small helper module)
  pub(crate) fn require_collaboration_root(ctx: &ServiceContext) -> Result<PathBuf, ApiError>; // P25 message
  ```

- [ ] **Step 1: Write the failing tests.**
  - `collab_store_is_served_on_its_own_alpn` (node tests):
    1. Bind two nodes A and B with `bind_node_into` and relay disabled.
    2. On A, call `set_collab_root(Some(tmp))` and add a 64 KiB file to
       A's collab store with `add_path_with_opts(ImportMode::TryReference)`,
       tagged `project/p/u/1`.
    3. From B, open a `ConnectionPool::new(B.endpoint(), COLLAB_BLOBS_ALPN, default)`
       and run `remote.execute_get(conn, GetRequest::blob(hash))` into B's
       personal store.
    4. Assert that B then has the blob.
    5. Assert that the same GET on `iroh_blobs::ALPN` does NOT find it:
       the personal store has no such hash, so it returns a not-found or
       empty stats error.
  - `collab_alpn_honours_the_connect_gate`: install a gate on A that refuses
    B, and assert that the collab GET fails.
  - `collab_root_change_remounts_and_sweeps_in_flight`:
    1. Set root R1 and set tag `in-flight/project/p/u/1` in its store.
    2. Set root R2, then R1 again.
    3. Assert the in-flight tag is gone after re-open.
    4. Assert that `collab_store()` is `Some` after each set and `None`
       after `set_collab_root(None)`.
  - `upload_pacer_covers_the_collab_store`: set the device upload cap to a
    small rate, as the existing pacer tests do (search `set_rate` in
    `sharing/iroh/tests.rs`), and assert that a collab GET of 1 MiB takes at
    least the paced time. Reuse the helper of the existing pacer test.
  - `set_collaboration_dir_accepts_a_scan_root_path` (scan_roots tests): a
    path already registered as a normal scan root parent is accepted with
    `Skip`. It is still refused when relative or when the write probe fails.
  - `scanner_skips_the_collab_store_dir`: a `.fits`-named file under
    `<root>/.athenaeum/blobs/` is not visited. Assert on the scan result
    counts.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib sharing::iroh::node collab` and `cargo test -p athenaeum-core --lib api::scan_roots`. Expect FAIL.
- [ ] **Step 3: Implement.**
  - **`open_fs_store`.** The body is the current `N:1009-1022` block moved
    verbatim, with the personal-store call site replaced by
    `open_fs_store(&working_dir.join("blobs"))`. This is a pure refactor
    with identical behaviour.
  - **`spawn_provider_events`.** Move the consumer loop body verbatim into
    the function. The personal store calls it exactly as before. The collab
    `EventSender` gets a second consumer from the same function with the
    SAME `Arc<UploadPacer>`.
  - **`build_router`.** It takes
    `collab: Option<(Store, EventSender)>`. When it is `Some`, add
    `.accept(COLLAB_BLOBS_ALPN, GatedBlobs { inner: BlobsProtocol::new(&store, Some(events)), gate: gate.clone() })`.
  - **Router rebuild.** Every place that rebuilds the router,
    `apply_relay_change` N:1641 included, passes the current slot. Put the
    slot read inside the rebuild helper so no call site can forget it.
  - **`set_collab_root`:**
    1. Take the node's rebuild lock (the one `apply_relay_change` uses).
    2. `open_fs_store(root.join(".athenaeum/blobs"))`.
    3. Delete every tag with prefix `in-flight/project/`.
    4. Swap the slot.
    5. Rebuild the router.
    6. Shut the old store down.
    7. Log
       `info!(path = %root.display(), "collab store mounted")` or
       `info!("collab store unmounted")`.
  - **Errors.** On error, keep the previous slot, and log and return the
    error.
  - **`shutdown`** shuts the collab store down after the router.
  - **`require_collaboration_root`** reads
    `db::scan_root_path_of_kind(conn, "collaboration")` and returns
    `Invalid(P25 message)` when it is absent.
- [ ] **Step 4: Run.**
  - The new tests, plus the whole `sharing::iroh` module:
    `cargo test -p athenaeum-core --lib sharing::iroh`. All personal-sync
    iroh tests must stay green.
  - `cargo test -p athenaeum-core --lib sharing::wire_golden`.
  - `cargo check --workspace --all-targets`.
- [ ] **Step 5: Commit** `feat(collab): collaboration blob store under the Collaboration root, served on its own ALPN`.

---

### Task 4: Generalize the assignment engine to independent blobs (P11)

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/iroh/assign.rs`
  - `fetch_children_assigned` A:503;
  - `run_child` A:635;
  - the hedge back-half A:1197;
  - `claim_provider`/`pick_provider` A:1314/1352;
  - the fail-fast arm A:573;
  - the pool construction A:520.
- Modify: `crates/athenaeum-core/src/sharing/iroh/blobs.rs` — the existing
  collection caller of `fetch_children_assigned`, plus a new
  `fetch_blobs_assigned`.
- Test: `crates/athenaeum-core/src/sharing/iroh/tests.rs` (existing assign
  tests must pass unchanged) plus new tests in the same file.

**Interfaces:**
- Produces:
  ```rust
  // assign.rs
  pub(crate) struct FetchItem {
      pub key: String,                         // caller's id (frame uuid); echoed in results
      pub request: GetRequest,                 // GetRequest::blob(h) or builder().child(i).build(root)
      pub hash: Hash,
      pub size: u64,
      pub providers: Arc<Vec<EndpointId>>,     // per item
  }
  pub(crate) enum FailMode { FailFast, Isolate }
  pub(crate) struct AssignmentOptions { /* existing fields */ pub alpn: &'static [u8], pub fail_mode: FailMode }
  pub(crate) async fn fetch_items_assigned(store: &Store, endpoint: &Endpoint, items: Vec<FetchItem>, opts: AssignmentOptions)
      -> Result<(AssignmentReport, Vec<(String, Result<(), anyhow::Error>)>)>;
  // the old entry point stays as a thin wrapper (collection callers untouched):
  pub(crate) async fn fetch_children_assigned(store, endpoint, providers, root, children, opts) -> Result<AssignmentReport>;
  //   = items with request = child(i).build(root), providers shared, alpn = iroh_blobs::ALPN, FailMode::FailFast
  // blobs.rs
  pub(crate) struct FrameFetch { pub key: String, pub hash: Hash, pub size: u64, pub providers: Vec<EndpointId>, pub in_flight_tag: String }
  pub(crate) async fn fetch_blobs_assigned(store: &Store, endpoint: &Endpoint, frames: Vec<FrameFetch>, telemetry: …)
      -> Result<Vec<(String, Result<(), anyhow::Error>)>>;
  //   sets each in_flight_tag (HashAndFormat::raw) BEFORE the loop, alpn = COLLAB_BLOBS_ALPN, FailMode::Isolate;
  //   leaves in-flight tags in place (the caller exports + retags + deletes them per success, P21/P22)
  ```

- [ ] **Step 1: Write the failing tests** (`sharing/iroh/tests.rs`, real
  nodes, relay disabled, following the existing assign tests' helpers):
  - `raw_items_fetch_from_per_item_providers`: nodes P1 and P2 each hold a
    different blob in their collab stores. Receiver R fetches both in one
    call with item-specific providers (`[P1]` for blob 1, `[P2]` for blob 2)
    and gets both results `Ok`.
  - `isolate_mode_keeps_siblings_alive`: item 1's only provider does not
    hold its hash. Item 1 is `Err` and item 2 is `Ok`.
  - `fail_fast_still_applies_to_collections`: the existing collection test
    with one missing child still returns `Err` for the whole call. Keep the
    existing test name if one covers this; otherwise add it.
  - `raw_item_hedge_uses_blob_ranges`: a unit test on the helper that builds
    the back-half request. For a raw item it equals
    `GetRequest::blob_ranges(h, split.back)`, and for a child item it equals
    the old builder output.
  - `one_pool_per_call`: an assert through a test hook or a counter that one
    `fetch_items_assigned` call with 20 raw items opens ONE pool. Use a
    `#[cfg(test)] static POOLS_OPENED: AtomicUsize`.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib sharing::iroh::tests::`. The new tests FAIL. The old ones still pass.
- [ ] **Step 3: Implement.**
  - **Carry the item.** Replace `(index, hash)` with `FetchItem` all the way
    through `run_child`, which now uses `item.request` instead of building
    `child(index).build(root)`, and through `claim_provider`/`pick_provider`,
    which choose among `item.providers` while the per-provider `States`
    stays global.
  - **Hedge back-half.** It branches on whether the request is a raw blob
    request: raw → `GetRequest::blob_ranges(item.hash, split.back)`,
    collection → the old builder.
  - **Pool and results.** The pool uses `opts.alpn`. Results are collected
    per `key`. `FailMode::FailFast` keeps the old early return, and
    `FailMode::Isolate` records the error and continues.
  - **Resume.** It already uses
    `blobs.local_for_request(item.request.clone())`, so it works for raw
    items unchanged.
  - **Logging.** Per item:
    - `debug!(frame_uuid = %key, bytes, outcome, "blob fetch finished")`;
    - `warn!(frame_uuid = %key, error = %e, "blob fetch failed")` on an
      isolated failure.
- [ ] **Step 4: Run** the whole `sharing::iroh` test module (all green) and `cargo check --workspace --all-targets`.
- [ ] **Step 5: Commit** `refactor(sharing): assignment engine fetches independent blobs with per-item providers`.

---

### Task 5: Seed and unseed one frame by reference (audit: `seed_project_collection` ADAPT, F6)

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/iroh/blobs.rs`
  - `add_path_child` B:204 and `ensure_child_readable` B:265 gain a
    `repair: CopyRepair` parameter (`Allow` | `Refuse`). Every existing
    caller passes `Allow`, so its behaviour is unchanged.
- Modify: `crates/athenaeum-core/src/sharing/iroh/node.rs`
  - new `seed_project_frame`, `unseed_project_frame`,
    `project_frame_tag`;
  - `unseed_project` N:2814 is kept.
- Test: `sharing/iroh/node.rs` tests.

**Interfaces:**
- Produces:
  ```rust
  pub(crate) enum CopyRepair { Allow, Refuse }
  pub fn project_frame_tag(project_id: &str, frame_uuid: &str, content_version: i32) -> String; // "project/<pid>/<uuid>/<ver>"
  impl SharedIrohNode {
      /// Import `path` into the COLLAB store by reference (never a copy), probe it, tag it. Returns the BLAKE3.
      /// Err when no collab store is mounted, when the file is unreadable, or when the probe fails (CopyRepair::Refuse).
      pub async fn seed_project_frame(&self, project_id: &str, frame_uuid: &str, content_version: i32, path: &Path) -> anyhow::Result<Hash>;
      /// Delete that frame's tag(s): every tag with prefix "project/<pid>/<uuid>/". Best-effort per tag, each failure logged.
      pub async fn unseed_project_frame(&self, project_id: &str, frame_uuid: &str) -> anyhow::Result<usize>;
      /// Probe a Complete entry: Ok(true) readable, Ok(false) dead external path, Ok(None-like) → use `BlobHealth`.
      pub async fn collab_blob_health(&self, hash: Hash) -> anyhow::Result<BlobHealth>;
  }
  pub enum BlobHealth { Missing /* status NotFound */, Partial, Readable, Dead /* Complete but probe failed */ }
  ```
- Consumes: Task 3 (`collab_store()`).

- [ ] **Step 1: Write the failing tests.**
  - `seed_frame_references_and_does_not_copy`:
    1. Write a 2 MiB file in `<root>/p/me/c_x.fits`.
    2. Call `seed_project_frame`.
    3. Assert that `du` of `<root>/.athenaeum/blobs` grew by less than
       64 KiB. Sum the sizes of the store dir's files before and after; the
       outboard is about 8 KiB.
    4. Assert the tag `project/p/u1/1` exists and that the returned hash
       equals `iroh_blobs::Hash::new(bytes)`.
  - `seed_frame_refuses_a_dead_path_instead_of_copying`:
    1. Seed.
    2. Delete the file.
    3. Seed again with the same uuid and version at the same path.
    4. Expect `Err`.
    5. Assert the store dir did not grow by 2 MiB, which proves no `Copy`
       repair ran.
  - `blob_health_reports_dead_after_delete`: Readable after seeding. Dead
    after the file is deleted. Missing for an unknown hash.
  - `unseed_frame_deletes_only_that_frame`: seed u1 and u2 and unseed u1.
    The tag for u2 remains.
  - The existing `seed_project_collection` tests stay green, which shows the
    `Allow` path is unchanged.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib sharing::iroh::node::tests::seed`. Expect FAIL.
- [ ] **Step 3: Implement.**
  - **`seed_project_frame`:**
    1. `store.blobs().add_path_with_opts(AddPathOptions { path, format: BlobFormat::Raw, mode: ImportMode::TryReference })`,
       through the existing `add_path_child` with `CopyRepair::Refuse`.
    2. `probe_first_byte`.
    3. `tags().set(project_frame_tag(..), HashAndFormat::raw(h))`.
    4. Log
       `debug!(project_id, frame_uuid, content_version, path = %path.display(), "frame seeded")`.
  - **`BlobHealth`.** `collab_blob_health` maps `status()` to
    `Missing` or `Partial`. For a Complete entry it runs
    `probe_first_byte` and returns `Readable` or `Dead`.
- [ ] **Step 4: Run** the whole `sharing::iroh` module. Expect green.
- [ ] **Step 5: Commit** `feat(collab): seed one frame by reference; dead paths are refused, never copy-repaired`.

---

### Task 6: Gate inputs — per-frame calibrated verdict, dictionary filter, manifest meta (P3, P4, P6, P7, P18)

**Files:**
- Create: `crates/athenaeum-core/src/collab/filters.rs`,
  `crates/athenaeum-core/src/collab/frame_meta.rs`. Register both in
  `collab/mod.rs`.
- Modify: `crates/athenaeum-core/src/collab/gate.rs`
  - `GateFrameInput` gains `filter_raw: String`,
    `filter_canonical: Option<String>`, `uuid: String`,
    `cal_blocker: Option<String>`;
  - `evaluate_frame` adds the failures `frame has no uuid` and
    `filter "<raw>" is not in the project dictionary`;
  - `not calibrated` carries `cal_blocker`.
- Modify: `crates/athenaeum-core/src/api/collab.rs`
  - `frame_gate_inputs` C:281: drop decision C's constant at C:400 and
    compute P7;
  - `evaluate_project_gate` C:524: load the cached dictionary.
- Test: unit tests in `filters.rs` and `frame_meta.rs`; `gate.rs` tests;
  `api/collab.rs` tests. The current
  `decision_c_blocks_publish_of_gate_eligible_frames` C:3105 is rewritten as
  `gate_passes_a_frame_whose_set_has_usable_masters` plus
  `gate_blocks_a_frame_whose_set_lacks_masters_with_the_readiness_sentence`.

**Interfaces:**
- Produces:
  ```rust
  // collab/filters.rs
  #[derive(Debug, Clone, Deserialize, Serialize, PartialEq)] #[serde(rename_all = "camelCase")]
  pub struct DictionaryEntry { pub canonical: String, #[serde(default)] pub aliases: Vec<String>, pub kind: String }
  pub fn match_filter(raw: &str, dict: &[DictionaryEntry]) -> Option<String>; // trimmed, case-insensitive vs canonical and aliases; returns the canonical spelling
  // collab/frame_meta.rs
  pub struct FrameMeta { pub filter_raw: String, pub channel: String, pub exptime_sec: f64, pub date_obs: Option<String>, pub meta: serde_json::Value }
  pub fn build_frame_meta(conn: &Connection, frame_id: i64) -> anyhow::Result<FrameMeta>;
  // meta keys (camelCase, omitted when unknown): instrume, telescope, xbinning, naxis1, naxis2, pixelScaleArcsec,
  // bayerpat, focalLen, fwhmArcsec, eccentricity, starsDetected, medianSnr, snrWeight, frameSnr,
  // wcs {crval1,crval2,crpix1,crpix2,cd:[4],sip?{order,a,b,ap,bp}} (crpix 1-based = DB + 1)
  // api/collab.rs
  pub(crate) fn frame_cal_verdict(conn: &Connection, set_id: i64, frame_id: i64) -> Result<(), String>; // P7
  pub(crate) fn publish_options() -> CalibratedLightOptions; // P6
  ```
- Consumes: `db::collab::CollabProjectRow.dictionary_json` (Task 2). The
  dictionary is FILLED by the poll in Task 8. Until then tests seed it
  directly.

- [ ] **Step 1: Write the failing tests.**

```rust
// collab/filters.rs
#[test]
fn matches_canonical_and_aliases_case_insensitively() {
    let d = vec![
        DictionaryEntry { canonical: "R".into(), aliases: vec!["Red".into()], kind: "broadband".into() },
        DictionaryEntry { canonical: "Ha".into(), aliases: vec!["H-alpha".into(), "Halpha".into()], kind: "narrowband".into() },
    ];
    assert_eq!(match_filter(" red ", &d).as_deref(), Some("R"));
    assert_eq!(match_filter("r", &d).as_deref(), Some("R"));
    assert_eq!(match_filter("H-ALPHA", &d).as_deref(), Some("Ha"));
    assert_eq!(match_filter("OIII", &d), None);
    assert_eq!(match_filter("", &d), None);
}
```

  Also:
  - **`frame_meta.rs` tests.** Use the in-memory DB with one frame, one
    `frame_analysis` row and one `plate_solves` row.
    `wcs.crpix1 == db_crpix1 + 1.0`. `fwhmArcsec == median_fwhm * pixel_scale_arcsec`.
    `channel == "osc"` with `bayerpat = 'RGGB'` and `"mono"` without it.
    Without a solve there is no `wcs` key. The serialized `meta` is at most
    8192 bytes for a full SIP order-5 solve (hub limit): build one with 5th
    order coefficient arrays and assert on the length.
  - **`gate.rs` tests.** `unmapped_filter_fails_with_its_name`,
    `empty_uuid_fails`, `cal_blocker_sentence_is_the_failure_text`.
  - **`api/collab.rs` tests.** Reuse the `seed_publishable_set` fixture
    (C:2508). The masters case needs library masters linked. Reuse the
    calibrated-export fixture helpers used by `api/lights.rs` tests: search
    `fn seed_set_with_masters` or the helper `compute_export_readiness`'s
    tests use. For the pass case, assert
    `report.frames[0].publishable == true`. For the fail case, assert the
    failure equals the `check_mode_ready` sentence.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib collab:: api::collab::tests::gate`. Expect FAIL.
- [ ] **Step 3: Implement.**
  - **Gate inputs.** `frame_gate_inputs` resolves the set id per frame (it
    already has `linked_set_ids`), calls `frame_cal_verdict`, reads
    `frames.uuid` and `frames.filter`, and runs `match_filter` against the
    project's parsed `dictionary_json`. A NULL dictionary maps to an empty
    list, so every frame fails with the filter sentence and the reason is
    visible.
  - **`publish_options()`** returns `CalibratedLightOptions { debayer_osc: false, format: OutputFormat::Fits, ..Default::default() }`.
  - **`build_frame_meta`** reads `frames`, `frame_analysis` (through
    `db::analysis::get_frame_analyses_by_ids`) and
    `plate_solve::storage::get_plate_solve`.
- [ ] **Step 4: Run** the tests above plus `cargo test -p athenaeum-core --test ts_contract`. `GateReport` and `FrameGateRow` may change; if the contract fails, regenerate with `TS_RS_WRITE=1`. Then run `npx tsc --noEmit`.
- [ ] **Step 5: Commit** `feat(collab): gate calibrated-verdict from the export gate; dictionary filter match; manifest meta`.

---

### Task 7: Publish per frame — write once, seed by reference, announce (§5.2; audit: `publish_collab_frames` ADAPT)

**Files:**
- Modify: `crates/athenaeum-core/src/api/collab.rs` — `publish_collab_frames`
  C:1150 is rewritten in place. Remove `PublishFrame`, the staging and
  `write_package` block, the push-seed block and `select_seed_target`.
  `percentile`/`sorted_f64` are removed if unused.
- Modify: `crates/athenaeum-core/src/api/collab.rs` — `PublishResult`
  (ts-rs) becomes:
  ```rust
  #[derive(Serialize, TS)] #[serde(rename_all = "camelCase")]
  pub struct PublishResult { pub announced: usize, pub updated: usize, pub state: Option<String>, // "published" | "pending" | None when nothing was sent
      pub held_back: Vec<HeldBackFrame>, pub unchanged: usize }
  #[derive(Serialize, TS)] #[serde(rename_all = "camelCase")]
  pub struct HeldBackFrame { pub frame_id: i64, pub filename: String, pub reasons: Vec<String> }
  ```
- Modify: the Tauri and web wrappers of `publish_collab_package`. They keep
  the command name, which the frontend switch renames in Task 11. Drop the
  `collab_sender` argument from the core call; the host arguments go in
  Task 12.
- Delete: the 7 ignored tests in `api/collab.rs` (2666, 2768, 2851, 2899,
  2949, 3038, 3496) per P15. They are replaced below.
- Test: `api/collab.rs` tests. Use a real node with a collab root through
  `bind_node_into` and a wiremock hub (fake-hub helpers from Task 8 are not
  yet available: mount the three routes directly).

**Interfaces:**
- Produces:
  ```rust
  pub async fn publish_collab_frames(ctx: &ServiceContext, project_id: &str, emitter: Option<Arc<dyn ProgressEmitter>>) -> Result<PublishResult, ApiError>;
  pub(crate) fn recipe_hash(conn: &Connection, spec: &GenerationSpec, source_path: &Path) -> anyhow::Result<String>; // P19
  ```
- Consumes:
  - Task 1: `announce_frames`, `new_frame_version`, `put_holders`,
    `FrameInWire`.
  - Task 2: `record_own`, `own_by_source_frame`.
  - Task 3: `require_collaboration_root`.
  - Task 5: `seed_project_frame`, `unseed_project_frame`.
  - Task 6: `evaluate_project_gate`, `build_frame_meta`, `publish_options`.
  - The generator: `resolve_generation`, `execute_generation`.
  - The queue: `ComputeQueue::acquire(ComputeJobKind::LightCalibration, …)`.

**Algorithm** (one run per project, the caller holds no DB lock across
awaits):
1. Run `require_collaboration_root` and the gate. If nothing is publishable
   and nothing is `Update pending`, return
   `PublishResult { announced: 0, … held_back }`. Otherwise go on. This
   replaces `Invalid("no publishable frames")`: an empty run is an outcome,
   not an error.
2. Split the publishable frames using `own_by_source_frame` into:
   - **`new`:** no own row;
   - **`update`:** an own row whose `recipe_hash` ≠ the recomputed one, P19;
   - **`unchanged`**.
3. Acquire ONE `ComputeQueue` permit
   (`LightCalibration`, label `collab publish <title>`) for the generation
   phase, following the `sync_prepare::open_generation` pattern: resolve all
   specs in one connection borrow, stat `resolved_master_paths`, keep the
   `hot_maps` cache, use `ctx.image_pool`.
4. For each frame, choose the target path:
   - `new`: `unique_path(<Collab>/<slug(project.slug)>/<own dir, P10>/<calibrated_output_filename(src)>)`;
   - `update`: the existing `landed_path`.

   Then:
   - append the cards `ATH_PRJ = project_id` and
     `ATH_FILT = filter_canonical`, and apply P4's WCS swap in
     `spec.cards`;
   - `execute_generation(spec, target, …)`, which writes atomically;
   - `xxh3 = package::xxh3_full_file(target)`;
   - `hash = node.seed_project_frame(pid, uuid, ver, target)`, with
     `ver = 1` for `new` and `own.content_version + 1` for `update`;
   - for `update`, first `unseed_project_frame(pid, uuid)` so the old tag
     does not pin the old content.

   After generation the permit is dropped.
5. Announce the `new` frames in batches of at most 500 (`FrameInWire`,
   `gate_version = project.thresholds_version.unwrap_or(0)`):
   - **On `CollabApiOutdated`:** unseed everything seeded in this run and
     return the P17 conflict.
   - **On a 409 "gate version … stale":** refresh the project's thresholds
     once (Task 8's `refresh_project`, or in this task a direct
     `thresholds()` call plus cache write), re-run the gate for the batch,
     and retry the batch once.
   - **Any other failure:** unseed that batch's frames (F5 kept). The
     written files stay on disk and are re-announced by the next run: own
     rows are recorded only after a successful announce, so they are
     `new` again.
6. For each `update`, call `new_frame_version`.
7. `record_own` for each announced or updated frame:
   - `origin = own`, `landed_path = target`, `on_disk = 1`;
   - `size_mtime_seen`, `source_frame_id`, `recipe_hash`;
   - `content_version`;
   - manifest fields from the `FrameInWire` with `state` from the response.

   Then call `put_holders(full=false, add=[…])`. Announce already inserted
   this device for version 1, but the refresh keeps it fresh. For updates,
   the hub re-inserted the caller.
8. Emit one `collab-published` event
   `{projectId, announced, updated, heldBack}` for the frontend's
   notification. Log
   `info!(project_id, count = announced, updated, held_back = n, "frames published")`.

- [ ] **Step 1: Write the failing tests** (`api/collab.rs`):
  - `publish_writes_once_into_the_collab_folder_and_seeds_by_reference`: two
    publishable frames. After the run:
    - exactly 2 files under `<Collab>/m31/<me>/`, named `c_<stem>.fits`;
    - no `collab_pub` dir exists under the working dir;
    - the collab store dir grew by less than 1 % of the files' bytes;
    - the tags `project/p1/<uuid>/1` are present;
    - the announce body carries `filterCanonical`, `gateVersion` and
      `meta.fwhmArcsec`;
    - own rows are recorded with `on_disk = 1`.
  - `pending_state_is_returned_as_is`: the hub answers `state: pending`,
    and the result and rows say pending.
  - `failed_announce_unseeds_and_records_nothing`: the hub answers 500.
    No `project/p1/` tags, no own rows, and the files are still on disk.
    Replaces the old `failed_announce_leaves_no_seed_tag`.
  - `stale_gate_version_refreshes_thresholds_and_retries_once`: the first
    announce returns 409 "gate version 0 is stale, current is 1" and the
    thresholds route returns version 1. The second announce carries
    `gateVersion: 1`.
  - `changed_master_publishes_a_new_version`:
    1. Publish.
    2. Touch the master file (mtime change).
    3. Publish again.
    4. Expect `POST …/frames/{uuid}/version` called once, the own row at
       `content_version = 2`, the tag `project/p1/<uuid>/2` present and
       `…/1` gone.
    5. The file path is unchanged.
  - `unchanged_frames_are_not_regenerated`: a second run with no change
    gives `unchanged = 2`, no generator call (assert the file mtime is
    unchanged) and no hub call.
  - `no_collab_root_refuses_before_any_work`: `Invalid` with the P25
    sentence.
  - `outdated_hub_is_a_conflict_with_the_stable_prefix`.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib api::collab::tests::publish`. Expect FAIL.
- [ ] **Step 3: Implement** the algorithm above.
- [ ] **Step 4: Run.**
  - `cargo test -p athenaeum-core --lib api::collab`.
  - `cargo test -p athenaeum-core --test ts_contract` (regenerate
    `PublishResult`).
  - `npx tsc --noEmit`. `ProjectDetail.tsx:174` reads
    `res.seedTarget`/`res.packageId`; change that notification to
    `announced`/`updated`/`heldBack.length` now, so tsc stays green.
  - `cargo check --workspace --all-targets`.
- [ ] **Step 5: Commit** `feat(collab): per-frame publish — calibrate once into the project folder, seed by reference, announce`.

---

### Task 8: Version poll, project refresh, manifest delta — inside the existing worker (R19, P9; audit: worker KEEP/retune)

**Files:**
- Modify: `crates/athenaeum-core/src/api/collab.rs`
  - `refresh_projects` C:955 also stores `pending_frames`, `gov_caps_json`
    and the dictionary (a `dictionary()` call per project whose
    `dictionary_version` is unknown or whose hub version moved);
  - for a project the caller lost, it calls
    `node.unseed_project(project_id)` on the collab store and deletes its
    `project_frames_local` rows except own ones. Own files stay on disk.
- Modify: `crates/athenaeum-core/src/api/collab_exchange.rs`
  - new `sync_manifest`, `poll_versions_once`;
  - `auto_sync_loop_inner` CE:2830 gains the 15 s version tick;
  - `COLLAB_AUTO_SYNC_INTERVAL` stays 20 minutes as the pass cadence;
  - add `COLLAB_VERSION_POLL_INTERVAL = 15 s`.
- Create: `crates/athenaeum-core/src/collab/fake_hub.rs` (`#[cfg(test)]`,
  P16), registered in `collab/mod.rs` under `#[cfg(test)]`.
- Modify: `crates/athenaeum-core/src/ts_export.rs` — register
  `CollabFramesChange`.
- Test: `api/collab_exchange.rs` tests with the fake hub.

**Interfaces:**
- Produces:
  ```rust
  // collab/fake_hub.rs (test only) — ONE hub shared by up to three apps
  pub struct FakeHub { pub server: MockServer, pub state: Arc<Mutex<FakeHubState>> }
  pub struct FakeHubState { pub projects: HashMap<String, FakeProject>, pub tokens: HashMap<String, FakeAccount> /* token -> account+device */ }
  pub struct FakeProject { pub version: i64, pub thresholds_version: i32, pub dictionary: Vec<DictionaryEntry>,
      pub frames: BTreeMap<String, FrameViewWire>, pub holders: HashMap<String, HashMap<String /*device*/, (i32, Instant)>>,
      pub members: Vec<FakeMember>, pub require_approval: bool }
  impl FakeHub {
      pub async fn start() -> Self;                                   // mounts: /me/project-versions, /me/projects, /projects/{id}/manifest,
                                                                      // POST /frames, POST /frames/{u}/version, approve, reject,
                                                                      // PUT /holders/self, GET /frames/{u}/holders, /dictionary, /thresholds, /membership, /collab/pubkey
      pub fn add_account(&self, token: &str, account_id: &str, display: &str, device_pubkey_b64: &str, relay_url: Option<&str>);
      pub fn add_project(&self, id: &str, slug: &str, members: &[(&str /*account*/, &str /*dataRole*/, bool /*coordinator*/)], require_approval: bool);
      pub fn bump(&self, project_id: &str);                           // version += 1
      pub fn frame(&self, project_id: &str, uuid: &str) -> Option<FrameViewWire>;
      pub fn holders_of(&self, project_id: &str, uuid: &str) -> Vec<String>; // device pubkeys, fresh only
      pub fn set_accepted(&self, project_id: &str, uuid: &str, accepted: bool, reason: Option<&str>); // bumps
  }
  // The responders implement the hub's rules that the app depends on: gateVersion equality (409 text verbatim),
  // dictionary membership (400), duplicate uuid (409), batch atomicity, state published/pending by require_approval/trust,
  // visibility of pending rows (publisher + moderators), holder permission filtering (send may hold only own), version bumps,
  // manifest ordering by (manifestVersion, frameUuid) with `next` paging (page size overridable for tests), holderCount.
  // It signs the membership snapshot with a test keypair exactly as the existing e2e helpers do (reuse `member_json`/`seed_project` logic from api/collab_e2e_tests.rs:134-190 — move what is needed here before Task 12 deletes that file).

  // api/collab_exchange.rs
  #[derive(Serialize, TS)] #[serde(rename_all = "camelCase")]
  pub struct CollabFramesChange { pub project_id: String, pub kind: FramesChangeKind, pub count: usize }
  #[derive(Serialize, TS)] #[serde(rename_all = "camelCase")]
  pub enum FramesChangeKind { NewFrames, PendingFrames, Approved, Rejected, Excluded, NewVersions }
  pub async fn sync_manifest(ctx: &ServiceContext, project_id: &str) -> Result<Vec<CollabFramesChange>, ApiError>;
  pub async fn poll_versions_once(ctx: &ServiceContext) -> Result<Vec<String /*moved project ids*/>, ApiError>;
  pub(crate) struct VersionPollHooks { pub on_thresholds_or_dictionary_moved: Box<dyn Fn(&str) + Send + Sync> } // Task 10 plugs auto-publish here; default no-op
  ```
- Consumes: Tasks 1 and 2.

**`sync_manifest` rules:**
1. Read `manifest_cursor`. If `gov_caps_json != synced_caps_json` (both
   from Task 2), the cursor is 0 and `prune = true`.
2. Page through with `limit = 1000` until `has_more == false`. Upsert each
   row with `upsert_from_manifest`. Compare the previous local row to
   classify the change kind:
   - new published not-mine → `NewFrames`;
   - pending visible to a moderator → `PendingFrames`;
   - own pending → published → `Approved`;
   - own → rejected → `Rejected`;
   - accepted true → false → `Excluded`;
   - content_version up, not mine → `NewVersions`.
3. If `prune`, call `delete_not_in(project_id, seen_uuids)`.
4. Call `set_sync_state(hub_version = page.project_version of the LAST page, manifest_cursor = max(manifest_version seen, old cursor), synced_caps_json = gov_caps_json)`.
5. Emit `collab-frames-changed` once per non-zero kind. Log
   `info!(project_id, count, kind, "manifest delta applied")`.

**`poll_versions_once` rules:**
1. Call `project_versions`.
2. Any id whose version ≠ `collab_projects.hub_version`, or that is unknown
   locally, or a cached id missing from the list, triggers ONE
   `refresh_projects` (it handles joins and losses), then `sync_manifest`
   for each moved id.
3. If the moved project's `thresholds_version` or `dictionary_version`
   changed during the refresh, call `on_thresholds_or_dictionary_moved`.
4. Return the moved ids. The caller kicks `AUTO_SYNC_KICK` when the list is
   non-empty.
5. Signed out → `Ok(vec![])`, as the pass does today.
6. `CollabApiOutdated` → the P17 conflict, logged once.

**Worker:**
- `auto_sync_loop_inner` now `select!`s over three arms:
  - the 15 s tick → `poll_versions_once`, which kicks the pass when a
    version moved;
  - the kick;
  - the 20-minute pass interval.
- The pass (Task 9) runs on kick or interval.
- The 90 s startup delay stays for the pass. The poll starts after 5 s.
- Log the poll at `debug` only. It fires 5 760 times a day.

- [ ] **Step 1: Write the failing tests** (fake hub, one app context from
  the existing `test_ctx` CE:3957, with `wire_hub` pointed at the fake):
  - `version_poll_is_quiet_when_nothing_moved`: two polls with no change.
    The second poll makes exactly one hub request, to
    `/me/project-versions`. Assert with wiremock's `received_requests()`.
  - `moved_version_pulls_only_the_delta`: 3 frames exist, the cursor is
    synced, then 2 more are added and bumped. The next manifest request
    carries `since=<old cursor>` and the local rows grow by 2.
  - `paging_follows_next`: the fake page size is 2 and there are 5 frames.
    All 5 are stored after one `sync_manifest`.
  - `caps_change_refetches_from_zero_and_prunes`: the moderator sees a
    pending frame of another member. `data.moderate` is revoked (fake
    `/me/projects` drops the cap) and the version is bumped. The
    `since=0` refetch runs and the pending row is pruned locally. Own rows
    survive.
  - `lost_project_unseeds_and_keeps_own_files`: the project disappears from
    `/me/project-versions` and `/me/projects`. Its collab-store tags are
    gone and its replica rows are deleted. The own file is still on disk.
  - `threshold_move_calls_the_hook`: the hook fires once when the thresholds
    version changes.
  - `change_kinds_are_classified`: one test drives each `FramesChangeKind`
    through the fake and asserts the emitted events, using the test
    emitter used by the existing `auto_pass_*` tests.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib api::collab_exchange::tests::poll api::collab_exchange::tests::manifest`. Expect FAIL.
- [ ] **Step 3: Implement.**
  - **Detecting a caps change.** `refresh_projects` writes the hub's
    current caps, plus a `"coordinator"` element when `is_coordinator`,
    into `gov_caps_json`. `set_sync_state` copies them into
    `synced_caps_json`.
  - **ts-rs.** Register `CollabFramesChange` and `FramesChangeKind` in
    `ts_export.rs`.
- [ ] **Step 4: Run** the new tests, the existing `auto_pass_*` and `sync_now_*` tests (these still pass through the package path until Task 9), `cargo test -p athenaeum-core --test ts_contract` (regenerate), and `cargo check --workspace --all-targets`.
- [ ] **Step 5: Commit** `feat(collab): 15 s version poll and paged manifest delta in the auto-sync worker`.

---

### Task 9: The replication pass — disk truth, holders, need set, fetch, land, loss guard, policy (§5.3, §5.5, R7, R17; P8, P11, P14, P20–P22, P24, P26)

**Files:**
- Modify: `crates/athenaeum-core/src/api/collab_exchange.rs`
  - `run_auto_sync_pass` CE:2612 body;
  - `replication_need` CE:2579 → `frame_need`;
  - `report_held_set` CE:1150 → `report_holders`;
  - `PackagePullClaim` → `FramePullClaim`;
  - `try_swarm_download` → `fetch_frames`;
  - `sync_project_now` CE:2904 keeps its signature;
  - new `disk_truth`, `land_frame`, `loss_guard`, policy functions.
- Modify: `crates/athenaeum-core/src/settings/mod.rs` — keys
  `COLLAB_LOSS_GUARD_FRACTION = "collab.loss_guard_fraction"` (default
  `"0.10"`) and `COLLAB_LOSS_GUARD_BYTES = "collab.loss_guard_bytes"`
  (default `"10737418240"`), listed in `defaults::all()`.
- Modify: `crates/athenaeum-core/src/sync/ingest.rs` — make `unique_path`,
  `sanitize_slug` and `link_or_copy` `pub(crate)` if they are not already.
- Modify: the Tauri and web collab wrappers, adding the new commands below.
- Test: `api/collab_exchange.rs` tests with the fake hub and REAL iroh nodes
  (`bind_node_into` CE:5079) for the fetch/land tests.

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq)] #[serde(rename_all = "camelCase")]
  pub struct ReplicationPolicy { #[serde(default)] pub filters: Vec<String>, #[serde(default)] pub publishers: Vec<String>,
      pub max_fwhm_arcsec: Option<f64>, pub min_stars: Option<i64>, pub byte_budget: Option<i64> }  // empty lists = all
  #[derive(Serialize, TS)] #[serde(rename_all = "camelCase")]
  pub struct PolicyPreview { pub frames: usize, pub bytes: i64, pub already_held: usize, pub to_fetch: usize, pub to_fetch_bytes: i64 }
  #[derive(Serialize, TS)] #[serde(rename_all = "camelCase")]
  pub struct CollabReplicationPaused { pub project_id: String, pub missing: usize, pub missing_bytes: i64 }
  pub struct DiskTruth { pub present: Vec<(String, i32)>, pub missing_replicas: Vec<String>, pub missing_own: Vec<String>, pub missing_bytes: i64, pub rehashed: usize }
  pub(crate) async fn disk_truth(ctx: &ServiceContext, project_id: &str) -> Result<DiskTruth, ApiError>;
  pub(crate) fn frame_need(rows: &[LocalFrameRow], policy: &ReplicationPolicy, role_allows: bool, auto_on: bool, paused: bool) -> Vec<LocalFrameRow>; // pure; ordered rarest-first then oldest
  pub(crate) async fn report_holders(ctx: &ServiceContext, project_id: &str, present: &[(String, i32)]) -> Result<usize, ApiError>; // P8 chunking; 403 → Ok(0)
  pub(crate) async fn fetch_frames(ctx: &ServiceContext, sync: &SyncRuntime, project_id: &str, need: Vec<LocalFrameRow>, emitter: …) -> Result<FetchOutcome, ApiError>;
  pub struct FetchOutcome { pub landed: usize, pub failed: usize, pub awaiting_gc: usize }
  pub async fn get_collab_policy(ctx: &ServiceContext, project_id: &str) -> Result<ReplicationPolicy, ApiError>;
  pub async fn set_collab_policy(ctx: &ServiceContext, project_id: &str, policy: ReplicationPolicy) -> Result<PolicyPreview, ApiError>;
  pub async fn preview_collab_policy(ctx: &ServiceContext, project_id: &str, policy: ReplicationPolicy) -> Result<PolicyPreview, ApiError>;
  pub async fn resolve_collab_loss(ctx: Arc<ServiceContext>, sync: Arc<SyncRuntime>, project_id: &str, action: LossAction, emitter: …) -> Result<(), ApiError>;
  #[derive(Deserialize, TS)] #[serde(rename_all = "camelCase")] pub enum LossAction { Restore, StopHolding }
  ```
  New commands, each with a Tauri wrapper and an Axum mirror:
  `get_collab_policy`, `set_collab_policy`, `preview_collab_policy` and
  `resolve_collab_loss`.
- Consumes: Tasks 2–5 and 8.

**Pass per project** (the existing per-project loop and error isolation of
`run_auto_sync_pass` are kept):
1. `require_collaboration_root`. Missing → `warn!` once per pass, skip every
   project.
2. `disk_truth`. For each row with `landed_path`:
   - `stat` the path;
   - missing → `set_missing`, then `unseed_project_frame`, then collect it
     into `missing_own` or `missing_replicas`;
   - a `size:mtime` different from `size_mtime_seen` → re-hash with xxh3
     (`package::xxh3_full_file`). If the hash differs, treat the file as
     missing (spec §5.5). If it matches, update `size_mtime_seen`.
   - For each missing replica, `collab_blob_health(blake3)`: `Missing` →
     `awaiting_gc = 0`, otherwise `awaiting_gc = 1` (P20).
3. **Loss guard (P14).** Skip it when `replication_paused` is already set.
   - Held replicas = rows with `origin = replica` and `on_disk` before this
     pass.
   - Trip when `missing_replicas / held > fraction` OR
     `missing_bytes > bytes`, and at least 2 frames are missing, so a
     single deletion never trips it.
   - On trip: `set_replication_paused(true)`, emit
     `collab-replication-paused`, and log
     `warn!(project_id, count, bytes, "replication paused by the loss guard")`.
   - The holder `remove` for the missing frames is still sent, because the
     truth goes to the hub either way.
4. `report_holders(present)`: a full report (P8). A lost own file is
   removed here too, so the hub stops sending peers to this device.
5. `frame_need(...)`:
   - `state == "published"` ∧ `accepted` ∧ `origin == replica` ∧
     `!on_disk` ∧ `!locally_declined` ∧ `!awaiting_gc` ∧ policy match;
   - `role_allows` is `role_allows_replication(data_role, is_coordinator)`;
   - `auto_on` is `force || auto_replicate`;
   - no frames when `paused`;
   - byte budget: walk in order, and stop adding when
     `held_bytes + next.byte_size > budget`;
   - order: `holder_count` ascending, then `manifest_json.createdAt`
     ascending.
6. `fetch_frames(need)` (P11, P21, P24):
   1. Take a `FramePullClaim` for the project; if one is already held,
      return.
   2. Take one `ReceiveGate` permit (`sync.inbound_control().await.map(|c| c.receive_gate.acquire())`,
      as today).
   3. Work in batches of 200 frames, taken ONE AFTER ANOTHER inside the same
      pass until the need set is empty, the pass is cancelled, or a batch lands
      nothing (no providers/all failed) — never "the rest on the next pass",
      which would stretch a 10 000-frame first replication over ~50 passes.
      For each frame of a batch
      call `frame_holders` and map them to `EndpointId`s minus self. Add
      a relay-only dial hint per provider, reusing CE:2132-2149.
   4. Frames with no provider are skipped with
      `debug!(frame_uuid, "no fresh holder")`.
   5. P24 check: if `status(hash)` is `Complete` and the entry is readable
      under another frame's tag, `link_or_copy` from that frame's
      `landed_path`, seed and record, without a fetch.
   6. Otherwise `fetch_blobs_assigned` into the collab store. The
      in-flight tag is `in-flight/project/<pid>/<uuid>/<ver>`.
   7. For each successful key, run `land_frame`. For each failed key,
      `set_error`.
7. `land_frame(row)`:
   1. Pick the destination:
      `dir = publisher_dir(pid, publisher_account_id)` or
      `<Collab>/<sanitize_slug(project.slug)>/<unique publisher slug, P10>`,
      then `dest = unique_path(dir/validate_rel_path(file_name))`.
   2. `blobs::export_child(store, hash, &dest)`.
   3. On `export_source_vanished`, drop the tags, set `awaiting_gc`, and
      stop. That is not a failure.
   4. Set the tag `project/<pid>/<uuid>/<ver>` and delete the in-flight
      tag.
   5. In one DB tx: `set_landed` plus a `sync_history` row, the way
      `process_project_frame` writes it today. When the new version of a
      frame replaces an older landed file, delete the old file after the
      commit.
   6. On tx error, remove `dest`, unseed, and log `error!`.
   7. After the batch, send `put_holders(full = false, add = landed)`.

**`resolve_collab_loss`:**
- **`Restore`:**
  1. Call the existing scan entry for the Collaboration root (search
     `scan_root` in `api/scan_roots.rs` for the function the "Rescan" button
     uses) and await it. The reconcile repairs moved files (Task 12 points
     it at the new table; until then the old table is used, so the test for
     Restore lands in Task 12).
  2. Clear `replication_paused`.
  3. Kick the pass.
- **`StopHolding`:** `set_declined(true)` for the rows that are still
  missing, then clear `replication_paused`.

- [ ] **Step 1: Write the failing tests.**
  - **Pure (`frame_need`):**
    - `need_excludes_own_declined_unaccepted_pending_and_awaiting_gc`;
    - `need_is_rarest_first_then_oldest`;
    - `byte_budget_counts_already_held_bytes`;
    - `policy_filters_by_canonical_filter_publisher_fwhm_stars`;
    - `paused_needs_nothing`.
  - **Disk truth (temp dir, no iroh):**
    - `deleted_replica_is_missing_and_unseeded`;
    - `edited_file_counts_as_missing` (same size, new mtime, different
      bytes);
    - `touched_but_identical_file_is_kept_and_mtime_updated`;
    - `deleted_own_file_is_missing_own_and_never_needed`.
  - **Loss guard:**
    - `one_deleted_replica_does_not_trip_the_guard`;
    - `eleven_percent_missing_trips_and_pauses`;
    - `bytes_threshold_trips_below_the_fraction`;
    - `stop_holding_declines_the_missing_and_unpauses`.
  - **Holders:**
    - `full_report_carries_only_present_frames` (fake hub: a deleted file
      is no longer a holder after the pass, which is the F5 regression
      test);
    - `over_ten_thousand_frames_chunks_full_then_add`.
  - **Real iroh (two nodes, fake hub):**
    - `frame_lands_by_rename_and_becomes_the_seed`:
      1. The publisher node seeds a 4 MiB file (Task 5) and the fake hub
         lists it as a holder.
      2. The receiver runs one pass.
      3. The file exists at
         `<Collab>/m31/<publisher-slug>/c_x.fits` with identical bytes.
      4. The receiver's collab store dir holds < 1 % of 4 MiB, which
         shows the store-owned data was renamed out.
      5. The tag `project/p1/<uuid>/1` is present on the receiver.
      6. A THIRD node can fetch the blob from the receiver alone, with the
         publisher shut down.
    - `second_pass_after_delete_waits_for_gc_then_refetches`:
      1. Land a frame.
      2. Delete the landed file.
      3. Pass 1: the row is `awaiting_gc` and no fetch is attempted (the
         P20 panic guard). Assert no request reached the provider through
         the provider-event counter.
      4. Force GC through the test hook. If iroh-blobs 0.103 exposes no GC
         trigger, set a test-only `GcConfig { interval: 1s }` via a
         `#[cfg(test)]` override in `open_fs_store`.
      5. Pass 2 re-fetches and the file is back.
    - `fetch_failure_of_one_frame_lands_the_others`.
    - `identical_content_second_frame_is_linked_not_fetched` (P24).
  - **Commands:** `set_policy_returns_a_preview_and_persists`.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib api::collab_exchange`. Expect FAIL.
- [ ] **Step 3: Implement** the pass above. `sync_project_now` = scoped
  `poll_versions_once` + a forced pass, as today. The old package branch of
  the pass is removed here. Package functions that become unreferenced stay
  until Task 12, with `#[allow(dead_code)]`, so this task's diff stays
  reviewable.
- [ ] **Step 4: Run** `cargo test -p athenaeum-core --lib api::collab_exchange sharing::iroh settings::`, `cargo test -p athenaeum-core --test ts_contract` (regenerate), `cargo check --workspace --all-targets`, and `npx tsc --noEmit`.
- [ ] **Step 5: Commit** `feat(collab): per-frame replication — disk truth, holders from disk, rarest-first fetch, land by rename, loss guard, policy`.

---

### Task 10: Auto-publish — one coalesced run per project (R16, §5.2 triggers; P13)

**Files:**
- Create: `crates/athenaeum-core/src/api/collab_autopublish.rs`, registered
  in `api/mod.rs` under the same `render` gate as `publish_collab_frames`
  (`api/mod.rs:63-65`).
- Modify: `crates/athenaeum-core/src/api/collab_exchange.rs`
  - `spawn_collab_auto_sync` also spawns the auto-publish worker, with the
    same arming guard;
  - Task 8's `VersionPollHooks` default calls `request_auto_publish`.
- Modify the trigger sites:
  - the scan-completion path in `api/scan_roots.rs`: find the function that
    emits `scan-complete` through `scanner/mod.rs:1153` and call
    `request_auto_publish_for_scan` after the scan result is known;
  - `api/analysis.rs:201` and `:405`, after `analysis-complete`;
  - a new core fn `api::plate_solve::on_batch_finished(ctx)`, called by
    BOTH hosts where `plate-solve-complete` is emitted
    (`crates/athenaeum-tauri/src/commands/plate_solve.rs` `plate_solve_batch`
    ~134 and its web mirror);
  - `api/collab.rs::link_frame_set` C:411;
  - the Task 8 hook.
- Modify: `api/collab.rs` — `set_project_auto_publish` plus its commands on
  both hosts.
- Test: the module's own tests.

**Interfaces:**
- Produces:
  ```rust
  pub fn request_auto_publish(project_id: Option<&str>);          // None = every project with auto_publish=1 and ≥1 linked set; non-blocking
  pub fn request_auto_publish_for_sets(set_ids: &[i64]);          // maps sets → linked projects via project_links, then request
  pub(crate) fn spawn_auto_publish_worker(ctx: Arc<ServiceContext>, emitter: Option<Arc<dyn ProgressEmitter>>) -> Option<JoinHandle<()>>;
  pub async fn set_project_auto_publish(ctx: &ServiceContext, project_id: &str, on: bool) -> Result<(), ApiError>;
  // statics (same pattern as AUTO_SYNC_KICK): DIRTY: Mutex<HashSet<String>>, ALL_DIRTY: AtomicBool, KICK: Notify
  ```
- Consumes: Task 7 `publish_collab_frames`.

**Worker:**
1. Wait on `KICK`.
2. Sleep a 30 s debounce, so a scan and its analysis coalesce.
3. Drain `DIRTY` and `ALL_DIRTY` into a project list, filtered to
   `auto_publish = 1` and at least one linked set.
4. Run `publish_collab_frames` for each project SEQUENTIALLY. The compute
   queue already serializes the heavy part.
5. A request arriving during a run marks its project dirty again, and the
   loop picks it up after the run ("re-armed").
6. Errors are logged `warn!(project_id, error, "auto-publish failed")` and
   never stop the worker.
7. Signed out, no Collaboration root, or `CollabApiOutdated` → one `warn!`
   per run, skip.

- [ ] **Step 1: Write the failing tests** (inject the publish fn as a seam,
  like `run_auto_sync_pass`'s `download` closure):
  - `requests_during_a_run_rearm_once`: three requests during one run give
    exactly one extra run.
  - `burst_is_coalesced_by_the_debounce`: five requests within 1 s give one
    run. Use a test-configurable debounce of 50 ms.
  - `auto_publish_off_is_skipped`.
  - `sets_map_to_their_linked_projects`.
  - `failure_does_not_stop_the_worker`.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib api::collab_autopublish`. Expect FAIL.
- [ ] **Step 3: Implement.** Wire the triggers. Check both hosts compile
  with `cargo check --workspace --all-targets`.
- [ ] **Step 4: Run** the tests and the workspace check.
- [ ] **Step 5: Commit** `feat(collab): coalesced auto-publish per project on scan, analysis, solve, link and threshold changes`.

---

### Task 11: Command surface and frontend on the per-frame model; "Update required" (P17)

**Owner of the frontend part: frontend-dev subagent. The Rust part: rust-engineer.**

**Files:**
- Modify: `crates/athenaeum-core/src/api/collab_exchange.rs` — new
  `list_project_frames`. Replaces `list_project_packages`, which is deleted
  in Task 12.
- Modify: `crates/athenaeum-core/src/api/collab.rs`
  - `list_moderation_queue` C:1658 returns pending frames;
  - `decide_announcement` C:1733 is replaced by `approve_frame` and
    `reject_frame`.
- Modify the Tauri collab commands, the Axum routes, `invoke_handler`
  (`crates/athenaeum-tauri/src/lib.rs:497-513`) and `build_router`
  (`crates/athenaeum-web/src/routes/mod.rs:329-345`).
- Modify: `ts_export.rs` and the regenerated `src/types/models.ts`.
- Modify the frontend:
  - `src/pages/ProjectDetail.tsx`
  - `src/pages/Projects.tsx`
  - `src/hooks/useProjects.ts`
  - `src/components/collab/ReceiveTab.tsx`
  - `src/components/collab/ModerationQueue.tsx`
  - `src/components/collab/AutoReplicateBar.tsx`
- Create: `src/components/collab/UpdateRequired.tsx` and its Vitest test.

**Command surface after this task** (both hosts, same names):

| Command | Core fn | Status |
| ---- | ---- | ---- |
| `list_collab_projects`, `refresh_collab_projects`, `get_collab_project_detail`, `evaluate_collab_gate`, `list_collab_link_suggestions`, `set_collab_link`, `create_collab_link_intent`, `set_project_auto_replicate`, `sync_project_now`, `export_collab_project` | unchanged | keep |
| `publish_collab_frames` | `publish_collab_frames` | renamed from `publish_collab_package` |
| `refresh_collab_frames` | `poll_versions_once` over all projects, returns `Vec<CollabFramesChange>` | replaces `refresh_collab_packages` |
| `list_collab_frames { projectId }` | `list_project_frames` → `Vec<ProjectFrameView>` | replaces `list_collab_packages` |
| `list_collab_moderation { projectId }` | pending frames → `Vec<ModerationFrameView>` | same name, new shape |
| `approve_collab_frame { projectId, frameUuid, trust }`, `reject_collab_frame { projectId, frameUuid, reason }` | Task 1 client calls + a `sync_manifest` | replace `decide_collab_announcement` |
| `get_collab_policy`, `set_collab_policy`, `preview_collab_policy`, `resolve_collab_loss`, `set_project_auto_publish` | Tasks 9 and 10 | new |
| `download_collab_package`, `list_collab_contributions` | — | removed (the frontend switches to `sync_project_now`; contributions had no caller) |

```rust
#[derive(Serialize, TS)] #[serde(rename_all = "camelCase")]
pub struct ProjectFrameView { pub frame_uuid: String, pub file_name: String, pub publisher: String, pub own: bool,
    pub filter: String, pub exptime_sec: f64, pub date_obs: Option<String>, pub state: String, pub accepted: bool,
    pub accepted_reason: Option<String>, pub holder_count: i64, pub on_disk: bool, pub awaiting_gc: bool,
    pub locally_declined: bool, pub byte_size: i64, pub content_version: i32, pub last_error: Option<String>,
    pub fwhm_arcsec: Option<f64>, pub eccentricity: Option<f64>, pub stars_detected: Option<i64> } // metrics from manifest meta
#[derive(Serialize, TS)] #[serde(rename_all = "camelCase")]
pub struct ModerationFrameView { pub frame_uuid: String, pub file_name: String, pub publisher: String, pub publisher_account_id: String,
    pub filter: String, pub exptime_sec: f64, pub fwhm_arcsec: Option<f64>, pub created_at: String }
```

**Frontend.** This is a minimal adaptation. The six-tab project page is
wave 4.
- **`ProjectDetail.tsx`:**
  - publish calls `publish_collab_frames`;
  - the notification reads "Published N frames · M held back" with a
    `dedupeKey` of `publish-<projectId>-<timestamp>`;
  - `PublicationHistory` and `GateTable` read `list_collab_frames` (own
    rows) instead of packages;
  - `publishedBytes` becomes the sum of own `byteSize`.
- **`ReceiveTab.tsx`:**
  - a table of `list_collab_frames` grouped by publisher, with columns
    file, filter, exposure, holders, and on-disk state
    ("On disk" / "Not on disk" / "Waiting for cleanup" when `awaitingGc` /
    "Not kept" when `locallyDeclined`);
  - the "Download" button becomes "Sync now" (`sync_project_now`);
  - a paused state: listen to `collab-replication-paused` with the
    StrictMode-safe listener pattern from CLAUDE.md, and show "Replication
    paused: N frames missing (M GB)" with two buttons, Restore and Stop
    keeping them (`resolve_collab_loss`);
  - use tokens only (`bg-surface`, `text-content-muted`, `text-error`, …).
- **`ModerationQueue.tsx`:** it lists `ModerationFrameView`s, has
  approve/reject per frame, and a "Trust this publisher" checkbox that is on
  by default (spec §9).
- **`useProjects.ts`:**
  - `refresh_collab_packages` becomes `refresh_collab_frames`;
  - the five package notification kinds are replaced by the
    `FramesChangeKind` kinds, notified on discrete outcomes only;
  - also listen to `collab-published` (Task 7) and
    `collab-frames-changed` (Task 8);
  - when a refresh error message starts with `collab_api_outdated`, set an
    `updateRequired` flag instead of notifying, the way "sign in" is
    detected today (`useProjects.ts:37`).
- **`UpdateRequired.tsx`:** a notice with the text "This project hub needs
  a newer Athenaeum. Update to keep collaborating." and a button that calls
  `openAvailable()` from `UpdatesContext` (`src/contexts/UpdatesContext.tsx:125`).
  It is rendered by `Projects.tsx` and `ProjectDetail.tsx` when
  `updateRequired` is set.
- **`AutoReplicateBar.tsx`:** unchanged except for the new
  `sync_project_now` flow. It also gets an auto-publish switch next to the
  auto-replicate switch (`set_project_auto_publish`).

- [ ] **Step 1: Write the failing tests.**
  - Rust: `list_project_frames_reads_metrics_from_meta`,
    `approve_then_sync_marks_published`, `reject_carries_the_reason`.
  - Vitest:
    - `UpdateRequired.test.tsx`: it renders and the button calls
      `openAvailable`;
    - `useProjects.test.ts`: a `collab_api_outdated` error sets
      `updateRequired` and does not notify;
    - `ReceiveTab.test.tsx`: it renders each on-disk state label and the
      paused banner buttons call `resolve_collab_loss` with `restore` and
      `stopHolding`.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib api::collab` and `npx vitest run src/components/collab src/hooks/useProjects`. Expect FAIL.
- [ ] **Step 3: Implement** the backend commands (both hosts,
  `#[tracing::instrument(skip_all, err)]`, registered), regenerate TS, then
  the frontend.
- [ ] **Step 4: Run** `cargo check --workspace --all-targets`, `cargo test -p athenaeum-core --test ts_contract`, `npx tsc --noEmit` and `npx vitest run`. Then grep: `rg "publish_collab_package|refresh_collab_packages|list_collab_packages|download_collab_package|decide_collab_announcement|pendingAnnouncements" src crates` must print nothing, except the deprecated client methods that Task 12 removes.
- [ ] **Step 5: Commit** `feat(collab): per-frame command surface on both hosts; frontend on frames; update-required state`.

---

### Task 12: Retire the package layer; scanner and export read the frame table (audit REMOVE list; P12, P26)

**Files:**
- Delete:
  - `crates/athenaeum-core/src/sync/project_ingest.rs` (and its `mod`
    line);
  - `crates/athenaeum-core/src/api/collab_e2e_tests.rs`. Its reusable
    helpers moved to `collab/fake_hub.rs` in Task 8.
- Modify: `crates/athenaeum-core/src/api/collab_exchange.rs` — delete
  everything the audit marks REMOVE:
  - `reconstruct_serve_dir`, `reconstruct_seed_dir`,
    `materialize_package_dir`, `CollabCleanupSink`;
  - `handle_project_request`, `authorize_and_reconstruct_serve`,
    `ensure_collab_sender_engine`;
  - `refresh_project_packages`, `refresh_all_project_packages`,
    `poll_project_announcements`, `apply_announcements`,
    `list_project_packages`, `list_contributions`;
  - `report_have_after_ingest`, `seed_ingested_package`,
    `seed_approved_announcement`, `unseed_package_local_data`,
    `remove_seed_dir`;
  - `download_project_package`, `try_swarm_download`, `swarm_fetch_plan`,
    `rearm_for_fallback`, `set_download_failed`;
  - `SWARM_UNFIT` and its helpers, `PENDING_PACKAGE_CHANGES`,
    `drain_pending_package_changes`, `wait_for_local_complete`,
    `probe_holder`, `held_package_ids_for_project`,
    `package_fully_held`, `manifest_fully_local`, `HeldState`;
  - `SEED_DIR`, `SWARM_STAGING_DIR`, `PackageStateChange`,
    `ProjectPackageView`, `ProjectDownloadProgress`, `ContributionView`;
  - the tests that exercise only these.
- Modify: `crates/athenaeum-core/src/db/collab_exchange.rs` — delete the
  package and contribution functions. Delete the file if it ends up empty,
  and remove its `mod` line.
- Modify: `crates/athenaeum-core/src/collab/hub_client.rs` — delete the
  deprecated package methods and wire structs (`AnnounceRequest`,
  `AnnounceResponse`, `AnnouncementWire`).
- Modify: `crates/athenaeum-core/src/sync/receiver.rs`
  - `ProjectAnnounceReceived` :1278 and `ProjectRequestReceived` :1341
    arms → `warn!(from = %…, "retired collab message ignored")` and drop;
  - delete `handle_project_announce` :3377 and the hook types
    `ProjectAnnounceGate` :91, `ProjectAnnouncementsRefresher` :99,
    `ProjectIngestedHook` :104, `ProjectRequestHandler` :115 and
    `ProjectReceiveHooks` :122.
- Modify: `crates/athenaeum-core/src/api/sync.rs`
  - remove `announcements_refresher` :588, `on_project_ingested_hook` :611,
    `project_request_handler` :643, `project_announce_gate` :524 and the
    `receiver_hooks` fields :536;
  - remove the `collab_sender: &SyncSenderRuntime` parameter from the ~15
    functions that thread it (audit: :892, :974, :1085, :1323, :1636,
    :1690, :2864, :3721 …) and their host callers;
  - remove `AppState.collab_sender` (`crates/athenaeum-tauri/src/commands/mod.rs:39`,
    `lib.rs:98`, `crates/athenaeum-web/src/routes/mod.rs:524`);
  - `connect_gate` :487 keeps admitting project members through
    `collab::authz::node_in_any_project`, because the collab ALPN uses the
    same gate;
  - `cleanup_orphan_blob_stores` :1270 also deletes the dirs
    `<working>/collab_pub`, `collab_serve`, `collab_seed` and
    `collab_swarm`, and the tag prefixes `project/` and `collab/` in the
    PERSONAL store, once per start. Log `info!(count, "legacy collab data removed")`.
- Modify: `crates/athenaeum-core/src/db/schema.rs` — `DROP TABLE IF EXISTS project_contributions;`
  then `DROP TABLE IF EXISTS project_packages;` (child first). Remove
  `idx_project_contributions_package` from the
  `every_foreign_key_child_column_is_indexed` list. Delete the old CREATE
  statements, because `init_db` must not recreate them.
- Modify: `crates/athenaeum-core/src/scanner/mod.rs` —
  `reconcile_project_contribution` ~:628 becomes `reconcile_project_file`
  (P26):
  - It is called for EVERY file under the Collaboration root: both walkers
    check the root kind before the `ATH_PRJ` divert. Outside the root the
    `ATH_PRJ` divert still calls it.
  - Its branches are:
    - `find_by_landed_path` → known, no-op;
    - full xxh3, then `find_by_project_and_xxh3` over every project (or
      the header's project when `ATH_PRJ` is present). The old path is
      gone → moved, `update_landed_path`. The old path still exists →
      duplicate, `warn!`;
    - else unknown → `warn!` plus an insert into a new small table
      `collab_foreign_files(path PRIMARY KEY, project_id NULL, seen_at)`
      for R18's list (UI in wave 4).
  - No branch creates `files`/`frames` rows.
- Modify: `crates/athenaeum-core/src/export/project_collector.rs` —
  `collect_project_export_data` :61 reads `project_frames_local` rows with
  `on_disk = 1` and `accepted = 1`, in both `own` and `replica` origin,
  using `landed_path` (P26). The WBPP hierarchy is unchanged in this wave
  (R1's new hierarchy is wave 5).
- Modify: `docs/superpowers/open-items.md` — P15: delete "Collab publish
  rework — its own cycle" (~1235-1243) and update the Windows-coverage note
  (~147-148).
- Test:
  - scanner tests (~3230+) rewritten over the new table:
    `collab_root_file_without_stamp_is_reconciled_not_catalogued`,
    `moved_replica_is_repaired`, `duplicate_warns`,
    `unknown_file_is_listed_inert`,
    `own_frame_outside_the_root_is_untouched_by_the_root_walk`;
  - `project_collector` tests;
  - `legacy_collab_dirs_and_tags_are_removed_once`;
  - `retired_project_messages_are_dropped` (receiver);
  - Task 9's `Restore` test:
    `restore_after_a_folder_move_repairs_paths_and_unpauses`.
- [ ] **Step 1: Write the new tests above** (they fail against the old reconcile and collector).
- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib scanner:: export::project_collector api::sync api::collab_exchange`. Expect FAIL.
- [ ] **Step 3: Delete and adapt** as listed. Grep until clean:

```bash
rg -n "project_packages|project_contributions|collab_pub|collab_serve|collab_seed|collab_swarm|report_have|list_announcements|seed_project_collection|collab_sender|ProjectIngest|project_ingest" crates src
```

  Allowed survivors:
  - the `DROP TABLE` lines;
  - the legacy-cleanup function;
  - the frozen `Msg` variants and their golden tests;
  - `seed_project_collection` only if a personal-sync caller exists.
    Otherwise delete it too, and keep `unseed_project`.
- [ ] **Step 4: Run.**
  - `cargo test -p athenaeum-core` (ALL targets, no filter).
  - `cargo test --workspace`.
  - `cargo check -p athenaeum-core --no-default-features`.
  - `cargo check --workspace --all-targets`.
  - `npx tsc --noEmit`.
  - `npx vitest run`.
- [ ] **Step 5: Commit** `refactor(collab): retire the package layer — tables, dirs, push-seed, collab sender, project ingest; scanner and export read the frame table`.

---

### Task 13: Three-instance end-to-end with a disk ledger; docs; final gates (P16, spec §15 subset)

**Files:**
- Create: `crates/athenaeum-core/src/api/collab_v3_e2e_tests.rs`
  (`#[cfg(all(test, unix))]`, a multi-thread runtime, the way the old e2e
  file was gated).
- Modify:
  - `docs/transfers/README.md`, the collaboration section: the folder model,
    the collab store and ALPN, one copy per machine with the ledger table,
    the version poll, disk truth, the loss guard and policy;
  - the logging spec's "Unified event schema": add `project_id`,
    `frame_uuid`, `content_version`, `bytes` and `holders` if absent. Check
    first, and add only the missing ones;
  - `CLAUDE.md`: the Transfers bullet list gets one line, "Collab v3:
    per-frame exchange, collab store under the Collaboration root on its
    own ALPN; a project frame's path lives only in `project_frames_local`
    (P26)". Update the Tauri command count line, recounted with `rg -c "#\[tauri::command\]" crates/athenaeum-tauri/src/commands`;
  - `docs/superpowers/open-items.md`: a new "Collab v3 wave 2" entry with
    the owed three-machine acceptance below.

**The test** (one fake hub, three contexts A = contributor, B = processor,
C = coordinator, three real iroh nodes, relay disabled, each with its own
Collaboration root in a temp dir):
1. The project has `requireApproval = true`, a default dictionary, and no
   thresholds (`gateVersion 0`). Seed A with a set of 6 publishable frames:
   `FILTER = "Red"`, masters linked (reuse Task 6's fixture), and a plate
   solve on 3 of them.
2. A publishes. All 6 announce as `pending`. The ledger check on A:
   `<A collab>/m31/<a>/` holds exactly 6 files, and the bytes under A's
   whole working dir plus the collab store, outside the 6 files, are less
   than 1 % of their size.
3. C, as coordinator, lists moderation (6 frames) and approves one with
   `trust = true`. The fake publishes all 6.
4. B runs `poll_versions_once` and then a pass. It lands 6 files under
   `<B collab>/m31/<a>/` with byte-identical content (xxh3 equal to A's).
   Ledger on B:
   - total bytes under B's collab root = the 6 files plus less than 1 %;
   - no file anywhere under B's working dir larger than 64 KiB;
   - B is now a fresh holder of all 6 in the fake hub.
5. A's node shuts down. C sets policy `filters = ["R"]` and runs a pass. C
   fetches all 6 from B alone. That proves received frames are seeds.
6. B deletes 1 landed file, and the pass drops B as its holder in the fake
   hub (the F5 regression). After forced GC the next pass re-fetches it
   from C.
7. B deletes 4 of 6 at once. The loss guard pauses and
   `collab-replication-paused` is emitted. `StopHolding` marks them
   declined, and the next pass fetches nothing.
8. A (restarted) re-publishes after a master changes. B's next pass lands
   version 2 over version 1: one file, same path, new bytes, and the old
   tag is gone.
9. The WBPP project export on B includes exactly the on-disk accepted
   frames.

- [ ] **Step 1: Write the test** as above, one `#[tokio::test(flavor = "multi_thread")]`.
  Put each numbered block behind a helper, so a failure names the step.
- [ ] **Step 2: Run it.**
  `cargo test -p athenaeum-core --lib api::collab_v3_e2e_tests -- --nocapture`.
  Fix what it finds in the owning task's code, with the fix commits named
  after that task.
- [ ] **Step 3: Docs** as listed. The open-items entry for the owed
  acceptance run holds the exact steps:
  1. Deploy nothing new to the hub; the test hub runs wave 1 after its
     owed deploy.
  2. Three machines: this Mac, a second account's device, and the Linux
     runner or a VM. Test relay. One project with `requireApproval`.
  3. Walk the §15 subset:
     - publish 20 frames;
     - approve with trust;
     - replication with `filters = [R]`;
     - 10 more frames after a scan (auto-publish announces within 15 s of
       the scan's analysis and solve);
     - delete 3 replicas, then delete 15 at once;
     - exclusion shows as `Excluded` on B.
  4. Measure `du -sh` of each Collaboration root and of each working dir's
     `blobs/` before and after. The expected ledger: the collab root is the
     payload plus under 1 %, and personal `blobs/` is unchanged.
  5. Also record the relay-byte fraction (spec §12).
- [ ] **Step 4: Final gates** (ALL must be green; paste the summary lines
  into the ledger):
  - `cargo test -p athenaeum-core` (all targets);
  - `cargo test --workspace`;
  - `cargo check -p athenaeum-core --no-default-features`;
  - `npx tsc --noEmit`;
  - `npx vitest run`;
  - `rg -n "println!|eprintln!" crates/athenaeum-core/src` (no new hits
    outside tests).
- [ ] **Step 5: Commit** `test(collab): three-instance per-frame e2e with a one-copy disk ledger; docs`.

---

## Self-review

**Spec coverage:**

| Spec item (wave 2) | Covered by |
| ---- | ---- |
| §5.1 folder model | Task 3 (store and root) and P25 (no fallback) |
| §5.2 publish | Tasks 6 and 7 (Task 10 for triggers) |
| §5.3 receive | Task 9 |
| §5.4 local storage | Task 2 |
| §5.5 disk truth | Task 9 |
| §5.6 scanner | Task 12 (P26 / A1) |
| R7 policy | Task 9 (commands) and Task 11 (minimal UI; the full editor is wave 4) |
| R16 auto-publish | Task 10 |
| R17 deletion and loss guard | Tasks 9 and 11 |
| R18 foreign files | Task 12 (listing table; UI wave 4) |
| R19 versioned poll | Task 8 |
| §11 compatibility and 409 | Tasks 1 and 11 (P17); tables and dirs dropped in Task 12 |
| §14 E2E on three instances with the disk ledger | Task 13 (in-process) plus the owed three-machine run |

Left to later waves, by design:
- attestation and the filter-mapping modal, ZP and the plate-solve
  precondition (wave 3; P3–P5 are the wave-2 stand-ins);
- the six-tab UI, History and the Settings UI for the guard keys (wave 4);
- canonical-grid stacking and the R1 export hierarchy (wave 5).

**Type consistency:**
- `FrameViewWire` (Task 1) is consumed by `upsert_from_manifest` (Task 2),
  `sync_manifest` (Task 8) and `FakeHub` (Task 8).
- `LocalFrameRow` (Task 2) is used by `frame_need`/`land_frame` (Task 9)
  and `list_project_frames` (Task 11).
- `project_frame_tag` (Task 5) is used in Tasks 7 and 9.
- `fetch_blobs_assigned` and `FrameFetch` (Task 4) are used in Task 9.
- `require_collaboration_root` (Task 3) is used in Tasks 7, 9 and 10.
- `COLLAB_API_OUTDATED_MSG` (Task 1) is matched by the prefix
  `collab_api_outdated` in Task 11.
- `gov_caps_json` / `synced_caps_json` (Task 2) are written by
  `refresh_projects` / `set_sync_state` (Task 8).

**Risk notes for the executor:**
- **Task 3 touches the node's router.** The personal-sync iroh suite is the
  guard: run it whole.
- **Task 4 changes a hot loop.** The collection path must stay
  byte-for-byte equivalent in behaviour. The existing `sharing/iroh/tests.rs`
  assign and hedge tests are the pin.
- **Task 12 is a large deletion.** Commit it only with the full
  `cargo test -p athenaeum-core` green, as the owner rule after 2026-09-18
  and 2026-09-20 requires.
