# Collab v3 — Wave 3: app live exchange — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The app stops polling and starts listening. One event stream per
device carries every project change, holder change and presence change from
the hub, in cursor order. The device reports its holdings as durable claims
through a local outbox with a digest check. A pure scheduler core picks
frames rarest-first from live providers and cancels work the moment a
version, an exclusion or a membership change makes it wrong. The disk
decides what is servable: a storage marker, a file watcher, a stat sweep and
a per-request serve check drive an explicit per-frame state machine. Nothing
lands over a changed file, nothing is deleted by automation, and a personal
transfer never waits behind collab for more than one frame.

**Architecture.** Four pure, ungated layers carry all the decisions, so they
compile headless and are tested without a network or a clock:
- `collab/live/` — wire types of the event channel, the SSE parser, the
  full-jitter back-off, the claim digest, the cursor rules, the holder map
  and presence, the outbox coalescer;
- `collab/storage/` — the storage marker, the watcher aggregator, the stat
  sweep, the per-frame state machine, the deletion rules (L4);
- `collab/serve.rs` — the serve decision;
- `collab/scheduler/` — the deterministic scheduler core and its seeded
  simulation.

One gated orchestrator, `api/collab_live/`, owns the side effects: the event
session (stream, beat, catch-up), the holdings flusher, the storage task and
the scheduler's effect executor. It replaces the wave-2 worker
(`spawn_collab_auto_sync` and its three loops) at the same two call sites.
The transport keeps its wave-2 pieces and adapts them in place:
- the assignment engine gains a live item queue, live provider sets, refusal
  codes and a stream cap;
- a new collab connection pool dials with `connect_with_opts`;
- the collab provider gets a get-request intercept (the serve check and the
  upload stream limit) and a registry of accepted connections so a
  membership change can close them;
- the upload pacer becomes class-aware and `ReceiveGate` gets two classes
  with yield-on-demand;
- collab landing exports to `<target>.athtmp`, renames over the target and
  re-imports the target by reference (ruling P12).

**Tech Stack:** Rust 2021 (toolchain 1.96). iroh `=1.2.0`, iroh-blobs
`=0.103.0` (iroh-util 0.6.0 connection pool, noq 1.3.0), reqwest 0.13.4
(feature `stream` added), tokio 1.53, notify 8.2.0 (new core dependency, the
version `crates/perseus` already uses), trash 5.2.9 (new), rusqlite, ts-rs
(`src/types/models.ts` regenerated, never hand-edited). Tests: wiremock 0.6
behind an axum 0.8 front for the fake hub (axum as a new dev-dependency of
core, already in `Cargo.lock` through `athenaeum-web`). React/TS with Vitest.

**Spec:** `docs/superpowers/specs/2026-09-25-collab-v3-live-exchange-design.md`
(owner-approved 2026-09-25): §4 (client side), §6, §7, §8, §9, §11, the app
part of §12 and §14. Parent spec
`docs/superpowers/specs/2026-09-23-collab-v3-per-frame-model-design.md`
(R1–R21, A1–A4). **Wire contract:** the hub plan
`docs/superpowers/plans/2026-09-25-collab-v3-wave3-hub-live-exchange-plan.md`,
section "Wire contract (what the app codes against)". Where that section and
the spec differ on a wire shape, the hub plan wins. The summary this plan
codes against is § Hub contract below.

## Global Constraints

- **Branch.** App repo `/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum`,
  branch `collab-v3-wave3` from `main` HEAD at execution time (main contains
  wave 2, `cd2ef3e3`, and this plan's commit). Task 1 creates it:
  `git checkout -b collab-v3-wave3`. The hub repo is NOT touched.
- **Two backends in sync.** Every new, changed or removed command changes its
  Tauri wrapper (`crates/athenaeum-tauri/src/commands/collab.rs`, registered
  in `crates/athenaeum-tauri/src/lib.rs` `invoke_handler`) and its Axum mirror
  (`crates/athenaeum-web/src/routes/collab.rs`, registered in
  `crates/athenaeum-web/src/routes/mod.rs`) in the same task. Logic lives in
  `athenaeum-core` (`api/collab_live/`). Tauri wrappers wear
  `#[tracing::instrument(skip_all, err)]`; web mirrors
  `#[tracing::instrument(skip_all, err(Debug))]`. `get_collab_live_status`
  is polled by the UI and wears `level = "debug"` on both hosts.
- **Frontend access.** No `@tauri-apps/*` outside `src/api/`. The frontend
  calls only `api.invoke` / `api.listen`, with the StrictMode-safe listener
  pattern from `CLAUDE.md`. Design tokens only, never raw colours.
  Notifications only through `notify()`, only on discrete outcomes, only
  from the app-root hook `useCollabNotifications` (wave-2 ruling R29).
- **Settings page.** `docs/settings/README.md` is the contract: registry
  entry, `SettingNumber` inside `SettingsSection`, defaults from
  `settings::defaults::all()`, no Save button, no banner.
- **TS mirror.** `src/types/models.ts` is generated. After any change to a
  ts-rs type run `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`
  and commit the regenerated file.
- **Errors and logging.** Never swallow an error: every `Err` is logged
  (`error!`/`warn!`) before it is returned or folded. `tracing` only; no
  `println!`/`eprintln!` outside tests. The message is a short stable
  phrase, data goes in snake_case fields from the dictionary
  (`docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`, "Unified
  event schema"). This wave adds, in Task 18, exactly these field names:
  `epoch`, `version`, `prev`, `holder_seq`, `report_seq`, `session_id`,
  `device`, `from_state`, `to_state`, `streams`, `digest_match`, `refused`,
  `retry_in_ms`, `store_id`, `window_count`, `class`. It also records the
  wave-2 drift `retry_secs` as retired. Reused: `project_id`, `frame_uuid`,
  `content_version`, `path`, `count`, `error`, `outcome`, `reason`,
  `duration_ms`, `bytes`, `provider`, `attempt`, `state`, `kind`.
- **No third-party project names** in code, comments, docs or commit
  messages. Our own dependencies (iroh, notify, tokio, reqwest, axum,
  trash) may be named.
- **Frozen wire between apps.** The `Msg` postcard enum and its golden pins
  (`sharing/wire_golden_tests.rs`) are untouched. The collab ALPN
  `athenaeum/collab-blobs/1` is unchanged.
- **Personal sync must not change behaviour.** The personal store
  `<working_dir>/blobs`, its `GatedBlobs`, its provider-event mask, its
  `export_child`, its `ControlPool` and its receive lane semantics stay as
  they are. The two-class `ReceiveGate` admits a personal transfer exactly
  as before when no collab unit is waiting. `cargo test -p athenaeum-core`
  covers them.
- **Feature gates.** `collab/live/*`, `collab/storage/*`,
  `collab/scheduler/*`, `collab/serve.rs`, `db/collab_live.rs`,
  `sharing/iroh/collab_pool.rs` are ungated. `api/collab_live/` is
  `#[cfg(all(feature = "render", feature = "solver"))]`, like `api::collab`,
  because it calls `api::collab::refresh_projects_reporting` and the
  render-gated publish path. `cargo check -p athenaeum-core --no-default-features`
  compiles the pure layers and must stay green.
- **Timing and threshold constants** — copied verbatim from the spec, each a
  named `pub const` in the module named, never an inline literal:

  | Constant | Value | Spec | Module |
  | ---- | ---- | ---- | ---- |
  | presence beat interval | 15 s | §4.2 | `collab::live::presence` (`BEAT_INTERVAL`) |
  | hub beat-silence rule (hub-owned; the app shows it only) | 40 s | §4.2 | `collab::live::presence` (`HUB_SILENCE`) |
  | hub grace after stream close (hub-owned) | 10 s | §4.2 | `collab::live::presence` (`HUB_GRACE`) |
  | hub keepalive (hub-owned) | 20 s | §4.1 | `collab::live::stream` (`HUB_KEEPALIVE`) |
  | stream read timeout (2.5 × keepalive) | 50 s | §4.1 | `collab::live::stream` (`READ_TIMEOUT`) |
  | reconnect / request / dial back-off | full jitter, 1 s base, 60 s cap | §4.1, §4.6, §7.3 | `collab::live::backoff` (`BACKOFF_BASE`, `BACKOFF_CAP`) |
  | outbox flush default (hub may change it via `nextFlushMs`) | 1 s | §6.2 | `collab::live::outbox` (`DEFAULT_FLUSH`) |
  | outbox immediate flush | 500 entries | §6.2 | `collab::live::outbox` (`FLUSH_AT_ENTRIES`) |
  | digest check off the critical path | hourly | §6.3 | `collab::live::outbox` (`DIGEST_CHECK_EVERY`) |
  | deletion settle | 60 s | L4, §9.2 | `collab::storage::watch` (`SETTLE`) |
  | deletion window | rolling 5 min | L4 | `collab::storage::deletions` (`WINDOW`) |
  | mass-deletion threshold | > 10 frames in the window | L4 | `collab::storage::deletions` (`MASS_THRESHOLD = 10`) |
  | second-deletion rule | deleted again within 24 h | L4 | `collab::storage::deletions` (`SECOND_DELETION`) |
  | last-copy warning | fewer than 2 other holders of the current version, offline included | L4, I7 | `collab::storage::deletions` (`LAST_COPY_MIN_OTHERS = 2`) |
  | work-unit cap | 256 MiB | §7.2 | `collab::scheduler::core` (`WORK_UNIT_MAX_BYTES`) |
  | starvation guard | 1 h waiting jumps the queue | §7.2 | `collab::scheduler::core` (`STARVATION`) |
  | watcher aggregation | 10 s | §9.2 | `collab::storage::watch` (`AGGREGATE`) |
  | stat sweep, watcher healthy | hourly, jittered ±25 % | §9.2 | `collab::storage::sweep` (`SWEEP_HEALTHY`, `SWEEP_JITTER = 0.25`) |
  | stat sweep, watcher dead or network volume | every 5 min | §9.2 | `collab::storage::sweep` (`SWEEP_DEGRADED`) |
  | mtime tolerance (FAT, SMB) | 2 s | §9.2 | `collab::storage::sweep` (`MTIME_TOLERANCE_SECS`) |
  | device-replace prompt | marker device offline > 7 days | §9.5 | `collab::storage::marker` (`REPLACE_PROMPT_AFTER`) |
  | retirement proposal | offline > 30 days, never automatic | §9.5 | `collab::storage::marker` (`RETIRE_PROPOSAL_AFTER`) |
  | collab dial connect timeout | 10 s | §7.3 | `sharing::iroh::collab_pool` (`CONNECT_TIMEOUT`) |
  | collab connection keep-open after last request | 60 s | §7.3 | `sharing::iroh::collab_pool` (`KEEP_OPEN`) |
  | collab QUIC idle timeout / keep-alive | 30 s / 5 s (the iroh defaults, set explicitly) | §7.3 | `sharing::iroh::collab_pool` (`IDLE_TIMEOUT`, `KEEP_ALIVE`) |

- **Test gates.**
  - Every task ends green on `cargo test -p athenaeum-core --all-targets <filter>`
    for its own tests and on `cargo check --workspace --all-targets`.
  - Every task that touches a pure layer also runs
    `cargo check -p athenaeum-core --no-default-features`.
  - Every task that touches the frontend runs `npx tsc --noEmit` and
    `npx vitest run`.
  - Task 18 runs the full gate: `cargo test -p athenaeum-core` (all
    targets, no filter), `cargo test --workspace`,
    `cargo check -p athenaeum-core --no-default-features`, `npx tsc --noEmit`,
    `npx vitest run`.
- **Commits.** Commit as the configured user `eg013ra1n <vilen.sharifov@gmail.com>`.
  Every message ends with exactly:
  ```
  Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01RYd9tanU4ehWLDXkPv159r
  ```
  The `git commit` lines below omit the trailer for brevity; always add it
  as a second `-m`. No push, no deploy.
- **Formatting.** `rustfmt <files you touched>`, never `cargo fmt -p`.
  Clippy is not a gate.
- **Dependencies (Task 1 adds them all).** In `crates/athenaeum-core/Cargo.toml`:
  - reqwest features gain `"stream"` (today it is on only through iroh's
    feature unification);
  - `notify = "8"` (resolves to 8.2.0, licence CC0-1.0, the crate and version
    `crates/perseus/Cargo.toml:70` uses);
  - `trash = "5"` (resolves to 5.2.9, licence MIT, rust-version 1.85;
    macOS through `NSFileManager`, Windows through the shell API with its
    default COM apartment mode, Linux through the freedesktop trash;
    `trash::delete(path) -> Result<(), trash::Error>` is all we call; the
    default `chrono` feature is kept — chrono is already in the tree);
  - dev-dependency `axum = "0.8"` (0.8.9, already in `Cargo.lock`) for the
    fake hub's event-stream front.

---
## Hub contract (from the hub plan's § Wire contract — what the app consumes)

All paths are under `<hub>/api/v1`. `Authorization: Bearer <deviceToken>`.
Error bodies: `{"error":"<message>"}`, empty for 401/403, or the typed bodies
below. `device` is always the **standard padded base64 of the device's
32-byte public key** (hub ruling P3) — the same string as `GET /devices` →
`pubkey` and the membership snapshot's `nodes[]`. `epoch` is opaque: compare
for equality only. `sessionId` and `digest` are 32 lowercase hex characters.

### Event stream — `GET /me/events`

- `200 text/event-stream`, `X-Accel-Buffering: no`. Framing
  `event: <name>\ndata: <one-line JSON>\n\n`; keepalive comment `:\n\n`
  every 20 s; the first event carries `retry: 3000`.
- 401 (empty) → the re-authentication path; 403 (empty) → a perseus device
  (never for the app); `409 {"error":"collab_api_outdated"}` → "update
  required".
- A second stream from the same device ends the older one. The hub closes the
  stream on revoke/retire, account block, `DELETE /me/presence` and shutdown.

| Event | `data` | Notes |
| ---- | ---- | ---- |
| `hello` | `{"sessionId","epoch","accountId","projects":{"<pid>":{"version","holderSeq","claimCount","claimDigest","reportSeq","presence":[{"device","serving","relayUrl"}]}}}` | first event. `projects` = exactly the account's projects now. `presence` includes this device; `[]` during the hub's 30 s warm-up. `reportSeq` = highest stored for this device in that project. |
| `project` | `{"projectId","prev","version","kinds":[…],"frames":[FrameEvent…]?,"more":bool}` | `kinds` ⊆ `frames, meta, members, thresholds, dictionary, grid`, in that order. `frames` present only if every changed row is inlined (≤ 50, all `published`); `more:true` → pull the manifest delta. |
| `holders` | `{"projectId","prev","seq","deltas":[{"device","add":[[frameSeq,contentVersion]…],"rm":[frameSeq…]}]}` | a claim on an unknown `frameSeq` is kept until the manifest delivers it. |
| `presence` | `{"projectId","replace":bool,"changes":[{"device","connected","serving","relayUrl"}]}` | `connected:false` carries `serving:false` and the last `relayUrl`. `replace:true` once at the end of the hub's warm-up. |
| `account` | `{"kind":"joined"\|"left","projectId"}` | own account's membership. |
| `resync` | `{"projectId","what":"project"\|"holders"}` | catch up that side over REST. |
| `versions` | `{"<pid>":[version, holderSeq],…}` | every 60 s. |

`FrameEvent` = the manifest `FrameView` minus `own`:
`{frameUuid, frameSeq, publisherAccountId, publisherDisplayName, fileName,
contentVersion, blake3, byteSize, xxh3, filterRaw, filterCanonical, channel,
exptimeSec, dateObs|null, meta, gateVersion, accepted, acceptedReason|null,
state, rejectReason|null, manifestVersion, createdAt}`. The app derives
`own = publisherAccountId == hello.accountId`.

**Cursor rules (I3), per project.** Keep `(epoch, version)` and
`(epoch, holderSeq)`. Apply a `project`/`holders` event iff `prev == cursor`,
then set the cursor to `version`/`seq`. Ignore it iff `version`/`seq` ≤
cursor. Otherwise catch up over REST (`GET …/manifest?since=<manifest
cursor>` + the small documents of the listed kinds, or `GET …/holders?since=`)
and continue. `resync` → catch up the named side. `versions`: a head above the
cursor → catch up; a head below → epoch change. **Epoch change** =
`hello.epoch ≠ stored epoch` or any head below the stored cursor → reload every
snapshot, send a `full:true` report per project, re-announce own frames the
hub no longer lists under their existing uuids.

### Presence — `POST /me/presence`, `DELETE /me/presence`

| Route | Body | Success | Errors |
| ---- | ---- | ---- | ---- |
| `POST` | `{"sessionId","serving":{"<pid>":bool},"relayUrl":"https://…"\|null}` | 204 | `409 {"error":"session_gone"}` → reopen the stream at once; 400 bad session id / relay URL |
| `DELETE` | `{"sessionId"}` | 204 (also unknown session) | 400 |

Beat right after `hello`, every 15 s, and at once when the serving map or the
home relay changes. A project missing from `serving` counts as `false`.

### Holders

- `GET /projects/{id}/holders/snapshot` → `{"epoch","holderSeq","version","frames":[{"seq","uuid","contentVersion"}],"devices":[{"device","displayName","relayUrl"|null,"claims":[[startSeq,runLength,contentVersion]…]}]}`.
  One REPEATABLE READ read; `holderSeq`/`version` are its cursors. `devices`
  = every non-revoked athenaeum device of a current member, own included.
  Runs are sorted, never overlap, and may name `frameSeq` values not in
  `frames`.
- `GET /projects/{id}/holders?since=S[&after=A][&limit=N][&epoch=E]` →
  `{"epoch","holderSeq","floor","deltas":[{"device","add":[[seq,cv]],"rm":[seq]}],"hasMore","next":{"since","after"}|null}`.
  Each changed claim in its current state. After a multi-page catch-up the
  cursor is the **first** page's `holderSeq`. `410` bodies:
  `{"error":"holders_below_floor",…}`, `{"error":"holders_cursor_ahead",…}`
  → reload the snapshot; `{"error":"epoch_changed","epoch"}` → epoch change.
- `PUT /projects/{id}/holders/self` body
  `{"reportSeq","full","add":[{"uuid","contentVersion"}],"remove":["<uuid>"],"digest","count"}`
  → `200 {"holderSeq","digestMatch","nextFlushMs","refused":["<uuid>"]}`.
  - `reportSeq` ≥ 1, from ONE per-device counter that survives restarts;
    every entry is stamped with it; the hub keeps the highest per
    `(device, frame)`. After a data loss, raise it above
    `hello.projects[*].reportSeq`.
  - Coalesce the outbox per frame (last op wins) before sending.
  - `full:true`: `add` is the whole claim set as of `reportSeq` R; `remove`
    must be empty.
  - `digest`/`count` describe the whole claim set **after** this report.
  - `refused` frames are not stored (and any live hub claim is tombstoned):
    drop them from the local claim set.
  - An empty report (`full:false`, no `add`, no `remove`) is a pure digest
    check.
  - `409 {"error":"collab_api_outdated"}` for a body without `reportSeq`.
- **Implicit claims.** A successful announce claims `(uuid, 1)` for the
  announcing device; a successful version (single or an `ok` batch entry)
  claims `(uuid, newContentVersion)`. Add them to the local claim set and
  digest **without** reporting. Flush any pending outbox entry for such a
  frame **before** calling version.

### The claim digest — pinned vectors (the app pins the same)

`key(uuid, cv)` = first 16 bytes of `blake3(uuid_bytes[16] ‖ cv as u32 big-endian)`.
`digest(set)` = byte-wise XOR of `key` over the device's own non-removed
claims in the project; `count` = number of them; empty set →
`00000000000000000000000000000000`, count 0.

| Input | Value |
| ---- | ---- |
| `a = 00000000-0000-4000-8000-000000000001`, cv 1: input bytes | `0000000000004000800000000000000100000001` |
| `blake3(input)` | `129cef69895583884285d1404f99e181afc3b63d273a5314902d2ec7bf24183f` |
| `key(a, 1)` | `129cef69895583884285d1404f99e181` |
| `key(b = …000000000002, 1)` | `c3ef44a7f50605a0dee0abca26ecf959` |
| `key(c = …000000000003, 2)` | `40fc3b9d29d73f9101f6293d9c3b5cc4` |
| `key(c, 1)` | `b8c7cf6630052129879bab6949f47d5b` |
| `{(a,1),(b,1)}` count 2 | `d173abce7c5386289c657a8a697518d8` |
| `{(a,1),(b,1),(c,2)}` count 3 | `918f90535584b9b99d9353b7f54e441c` |
| `{(a,1),(b,1),(c,1)}` count 3 | `69b464a84c56a7011bfed1e320816583` |

### Frames and devices

- Manifest `FrameView` (`GET /projects/{id}/manifest`) gains `frameSeq` and
  loses `holderCount`; everything else as in wave 2.
- `POST /projects/{id}/frames/{uuid}/version` body
  `{"expectedVersion","blake3","byteSize","xxh3"}` → `{"contentVersion","projectVersion"}`;
  `409 {"error":"version_conflict","contentVersion":N}`.
- `POST /projects/{id}/frames/versions` body
  `{"versions":[{"uuid","expectedVersion","blake3","byteSize","xxh3"}]}` (1..=500)
  → `{"projectVersion","results":[{"uuid","status":"ok"|"conflict"|"not_found"|"forbidden","contentVersion"}]}`
  in request order.
- `POST /devices/{id}/revoke` optional body `{"retire":true}` → 204. `{id}`
  is the hub device id from `GET /devices` (not the pubkey). Tombstones the
  device's claims, bumps `members` everywhere, closes its stream.
- Retired: `GET /me/project-versions` and `GET /projects/{id}/frames/{uuid}/holders`
  answer `409 collab_api_outdated`.

---

## Reuse audit (2026-09-25, verified in code at `d16adced`; cite as "audit")

Paths are relative to `crates/athenaeum-core/src/`. `CE` = `api/collab_exchange.rs`,
`C` = `api/collab.rs`, `AP` = `api/collab_autopublish.rs`, `CF` =
`db/collab_frames.rs`, `DC` = `db/collab.rs`, `M` = `sharing/iroh/mod.rs`,
`N` = `sharing/iroh/node.rs`, `A` = `sharing/iroh/assign.rs`, `B` =
`sharing/iroh/blobs.rs`, `R` = `sync/receiver.rs`, `HC` =
`collab/hub_client.rs`. `TC` = `crates/athenaeum-tauri/src/commands/collab.rs`,
`WC` = `crates/athenaeum-web/src/routes/collab.rs`.

### Worker, poll, pass, maintenance, loss guard

| Component | Verdict | Wave-3 change (task) |
| ---- | ---- | ---- |
| `spawn_collab_auto_sync` CE:3781-3803, `AUTO_SYNC_ARMED` CE:3466, callers `api/sync.rs:1379` and `:1717` | ADAPT | Becomes `api::collab_live::spawn_collab_live` at the same two call sites, same once-per-process guard; still arms `spawn_auto_publish_worker` (15) |
| `run_collab_auto_sync_loop` CE:3673-3733, `auto_sync_loop_inner` CE:3750-3775, `tick_loop` CE:658-673, `AUTO_SYNC_KICK` CE:64-68 | REMOVE | Replaced by the event session and the scheduler task (15) |
| `COLLAB_VERSION_POLL_INTERVAL` CE:81, `…_STARTUP_DELAY` CE:84, `COLLAB_AUTO_SYNC_INTERVAL` CE:3455, `…_STARTUP_DELAY` CE:3460 | REMOVE | (15) |
| `poll_versions_once` CE:399-486, `version_poll_tick` CE:626-652, `kick_if_versions_moved` CE:605-615, `POLL_DOWN` CE:620, `refresh_collab_frames` CE:519-533, `ChangeCollector` CE:493-512 | REMOVE | The feed applier replaces the poll (5); `refresh_collab_frames` command removed (16) |
| `POLL_BACKOFF` CE:540-551, `backoff_active`/`back_off`/`clear_backoff` CE:555-592, `clear_poll_backoff_for` CE:596-599 | REMOVE | Per-request retry with full-jitter back-off (2) |
| `sync_manifest` CE:210-225, `sync_manifest_serialized` CE:246-377, `manifest_sync_lock` CE:228-237, `classify_frame_change` CE:151-182, `COLLAB_FRAMES_CHANGED_EVENT` CE:90 | KEEP | The REST catch-up path of the feed applier; `vouched_version` = the event's `version` (5). `upsert_from_manifest` grows the state edges (1, 9) |
| `run_auto_sync_pass` CE:3508-3617, `replication_pass` CE:3620-3638, `auto_sync_pass` CE:3642-3650, `AutoSyncPassOutcome` CE:3470-3483, `PassKind` CE:958-970 | REMOVE | The scheduler core + executor (14, 15) |
| `frame_need` CE:1158-1203, `replicable` CE:1143-1148, `policy_matches` CE:1120-1140, `role_allows_replication` CE:3489-3491, `ReplicationPolicy` CE:977-998, `read_policy`/`policy_preview`/`validate_policy` CE:3224-3266, `get/preview/set_collab_policy` CE:3269-3319 | ADAPT | `frame_need` loses `paused` and the holder-count sort; the need set is `local_state = wanted` ∧ published ∧ accepted ∧ policy ∧ storage ok; ordering moves to the core (14). Policy commands KEEP; `set_collab_policy` moves rows to/from `idle` (9) |
| `fetch_frames` CE:2211-2229, `fetch_frames_gated` CE:2246-2385, `between_batches` CE:2392-2457, `prepare_batch` CE:2521-2674, `run_batch` CE:2688-2796, `FETCH_BATCH` CE:945, `Batch` CE:2497-2506, `holder_lookup_is_frame_level` CE:2511-2514 | REMOVE | Per-frame holder lookups and 200-frame batches go; the executor feeds one-frame units into the live assignment run (15). The dial-hint idiom `peer_dial_addr(.., true)` CE:2651 moves into the collab pool (12) |
| `FramePullClaim`/`IN_FLIGHT_FRAME_PULLS` CE:2074-2101, `FETCH_CANCELS`/`CancelRegistration`/`cancel_project_fetch` CE:2110-2143 | REMOVE | The core's cancel commands (14); `set_project_auto_replicate` CE:3808-3827 feeds the core instead (15) |
| `land_frame` CE:2951-3056, `landing_target` CE:2900-2932, `new_landing_path` CE:2863-2882, `fresh_row` CE:2840-2859, `record_landing` CE:3100-3140, `forget_stale_landing` CE:3061-3088, `link_identical` CE:3145-3218, `identical_landed` CE:2479-2492, `publisher_folder` CE:877-928 | ADAPT | Landing moves to `api/collab_live/landing.rs`: the fence (`set_landed_if`, CF:316) KEEP; the R24 rename-aside (CE:2914-2926) REMOVE — a changed file is `quarantined` and never landed over (11); export through `export_child_replacing` (11) |
| `disk_truth` CE:1421-1597, `readmit` CE:1651-1717, `reseed_if_untagged` CE:1606-1643, `recheck_awaiting_gc` CE:1722-1757, `sweep_in_flight` CE:1775-1834, `run_maintenance` CE:1860-1955, `MaintenanceOutcome` CE:1838-1847, `DiskTruth` CE:1050-1072 | REMOVE / ADAPT | The 20-minute walk and the maintenance loop go (15). Their per-row logic moves: stat + rehash → `collab::storage::sweep` (8); readmit and reseed → the state machine's re-adoption edge (9); `recheck_awaiting_gc` → the executor's GC probe (15); `sweep_in_flight` → run once at store mount (15) |
| `loss_guard` CE:1963-2018, `resolve_collab_loss` CE:3343-3427, `LossAction` CE:1040-1046, `CollabReplicationPaused` CE:1020-1025, `COLLAB_REPLICATION_PAUSED_EVENT` CE:932, `replica_file_lost` CE:3431-3450, `OptEmitter` CE:3322-3330 | REMOVE | L4 deletion rules and the reversible choice (9, 16) |
| `report_holders` CE:2028-2068, `HOLDERS_CHUNK` CE:939, landing delta CE:2340-2344, publish delta C:3310 | REMOVE | Outbox + digest + implicit claims (6) |
| `sync_project_now` CE:3839-3872 | REMOVE | Global `collab_sync_now` (L10) (15, 16) |
| `live_project` CE:2465-2476, `require_collaboration_root` CE:852-869, `mounted_collaboration_root` CE:1298-1310, `ensure_collab_store` CE:1330-1393, `project_disk_lock` CE:1260-1273 | KEEP | `ensure_collab_store` also checks the storage marker first (7); `project_disk_lock` serializes landing, state transitions and publish per project |
| `list_project_frames` CE:801-812, `ProjectFrameView` CE:709-796 | ADAPT | `onDisk`/`awaitingGc`/`locallyDeclined`/`holderCount` → `localState`, `holdersOnline`, `holdersTotal`, `waitingForPublisher`, `newVersionWaiting` (16) |
| `export_project_for_wbpp` CE:3911-4090 | KEEP | Reads `on_disk`, which the state setter keeps equal to `local_state IN ('held','own_held')` (1) |
| `unseed_project_local_data` CE:818-824 | KEEP | Lost projects (R14) |
| `COLLAB_FRAMES_LANDED_EVENT` CE:936, `CollabFramesLanded` CE:1030-1035 | KEEP | Emitted by the executor per landing burst (15) |
| test modules CE:4092-7735 | ADAPT | `mod need` and the landing/policy tests that still apply move to the new modules; `mod poll`, the loss-guard, holder-report, batch and pass tests are deleted with their code (15) |
| `AP` whole file (796 lines) | KEEP | `spawn_auto_publish_worker` AP:340 armed by the new spawner (15) |

### Publish

| Component | Verdict | Wave-3 change (task) |
| ---- | ---- | ---- |
| `run_publish` C:2421-…, `publish_collab_frames` C:2332, `auto_publish_collab_frames` C:2352, `republish_collab_frames` C:2402, `publish_lock` C:2382, recipe hash C:1509-1589, R8 adoption C:2937-2958 | KEEP | — |
| step 6 single `new_frame_version` C:3070-3110 | ADAPT | Batched `frame_versions` (≤ 500, compare-and-set on `expectedVersion`), outbox flushed for those frames first, implicit claims recorded (6) |
| step 7 `put_holders` delta C:3296-3313 | REMOVE | Implicit claims for announced/versioned frames; outbox `add` for R8-adopted frames (6) |

### Local store (DB)

| Component | Verdict | Wave-3 change (task) |
| ---- | ---- | ---- |
| `project_frames_local` schema `db/schema.rs:2400-2445` | ADAPT | + `local_state TEXT`, `frame_seq INTEGER`, `state_changed_at TEXT`; backfill once (1) |
| `locally_declined` (column; written only by CE:3417 through `set_declined` CF:410) | ADAPT → `local_state = 'not_kept'`; the column stays in the table, no code reads or writes it (1) |
| `on_disk`, `awaiting_gc` | KEEP as derived/scheduler flags | `on_disk` is written only by `set_local_state` (= servable); `awaiting_gc` keeps the P20 dead-entry wait (1, 15) |
| `holder_count` (column; wire field `FrameViewWire.holder_count` HC:137-167) | REMOVE from the wire and from `LocalFrameRow`; the column stays, never written; counts are derived from the holder map (1, 6) |
| `rejected_size_mtime` CF:343-376 (R21) | KEEP | The quarantine stamp: an unchanged quarantined file is never rehashed (9) |
| `CF` functions `set_missing` CF:378, `set_declined` CF:410, `set_landed` CF:294 | REMOVE / ADAPT | Replaced by `set_local_state` + `set_landed_if` (1, 11) |
| `collab_projects` `db/schema.rs:2272-2388`; `hub_version` (2342), `replication_paused` (2372) | ADAPT | `hub_version` IS the feed version cursor; + `feed_epoch TEXT`, `holder_seq INTEGER NOT NULL DEFAULT -1`; `replication_paused` stays in the table, unread (1) |
| `collab_foreign_files` `db/schema.rs:2494` + CF:698-764 | KEEP | "Other files" list; + a NEW `list_foreign_files` (1, 16) |
| NEW tables | NEW | `collab_live_meta`, `collab_my_claims`, `collab_outbox`, `collab_holder_devices`, `collab_holder_claims`, `collab_deletions`, `collab_quarantine` (1) |
| settings `collab.loss_guard_fraction`/`_bytes` (`settings/mod.rs` defaults :129-130, keys :371-374, `all()` :218-223, getters :768/:782, test list :812-813, test :833) | REMOVE | Rows deleted idempotently in `init_db` (15). NEW `collab.max_upload_streams`, `collab.max_receive_streams` (10) |

### Transport

| Component | Verdict | Wave-3 change (task) |
| ---- | ---- | ---- |
| collab store under `<Collab>/.athenaeum/blobs`, `open_fs_store` N:909-926, slot `SharedCollabSlot` M:2066, `CollabMount` M:2055, `set_collab_root` N:1593-1674, `COLLAB_BLOBS_ALPN` M:105 | KEEP | `set_collab_root` never creates the root (7) |
| `provider_event_channel` M:580-613 (shared by both stores), `spawn_provider_events` M:648-853 | KEEP for the personal store | The collab store gets its own `collab_provider_event_channel` (`get: InterceptLog`) and consumer `spawn_collab_provider_events` (10). iroh-blobs 0.103 routes EVERY request kind through `mask.get` (`provider/events.rs:462`), so the collab consumer must answer `PushRequestReceived`, `GetManyRequestReceived` and `ObserveRequestReceived` explicitly (ruling P15) |
| `CollabSlotBlobs::accept` M:2106-2128 | ADAPT | Registers a weak handle per accepted connection; `close_collab_connections_not_admitted` (10) |
| `SharedConnectGate` M:152, `connect_gate_admits` M:494-500, host closure `api/sync.rs:501-526` (reads the DB live), `collab::authz::node_in_any_project` (`collab/authz.rs:27`) | KEEP | The predicate already re-reads `members_json`; a `members` event refreshes the membership snapshot, then closes collab connections the predicate no longer admits (5, 10) |
| `UploadPacer` `sharing/iroh/pacer.rs:29-70` | ADAPT | Class-aware: `reserve_class(size, UploadClass)` (10) |
| `fetch_items_assigned` A:650, `run_items` A:660-860, `run_child` A:873-1114, `transfer_once` A:1507-1570, `FetchItem` A:431-441, `MAX_IN_FLIGHT` A:115, `open_pool` A:477-481, `TransferFault` A:489-495, `pick_provider` A:1615-1640 | ADAPT | Live item queue, live provider sets, refusal codes, per-run stream cap, per-item cancel, a `Dialer` over the stock pool or the collab pool (12). The personal collection path (`fetch_children_assigned` A:603) keeps its behaviour |
| `ControlPool` N:634-739 | KEEP (template) | The collab pool copies its idiom (entry map, `close_reason` reuse, idle reaper) with `connect_with_opts` (12) |
| `fetch_blobs_assigned` B:1497-1576, `FrameFetch` B:1474-1480 | REMOVE | Replaced by the live run (12, 15) |
| `export_child` B:696-727 | KEEP for personal sync | Collab landing uses a new `export_child_replacing` (11) |
| `in_flight_tag` B:630-632, `export_source_vanished` B:653-669, `add_path_child` B:226-269, `ensure_child_readable` B:293-358, `probe_first_byte` B:126-134 | KEEP | used by the collab landing (11) |
| `seed_project_frame` N:2897-3004, `unseed_project_frame` N:3015, `project_frame_tag` N:251, `collab_blob_health` N:3099 | KEEP | — |
| `ReceiveGate` R:258-340, call sites R:2113 and CE:2330 | ADAPT | Two classes, yield-on-demand; CE:2330 is removed with the pass (13) |
| home relay URL (no public getter; `relay_health` N:861, `spawn_home_relay_watcher` N:3630-3701) | NEW accessor | `SharedIrohNode::home_relay_url()` + `watch_home_relay()` for the beat (4) |

### Hub client, authz, fake hub

| Component | Verdict | Wave-3 change (task) |
| ---- | ---- | ---- |
| `CollabClient` HC:259-338, `HTTP_TIMEOUT_SECS` HC:13 (a total deadline, cannot carry a stream), `classify` HC:275-295 | ADAPT | New typed errors, retry wrapper; the stream uses its own client (2, 4) |
| `project_versions` HC:412, `put_holders` HC:591, `frame_holders` HC:616, `HolderWire` HC:106, `ProjectVersionWire` HC:126, `HolderRefWire` HC:229 | REMOVE | (2) |
| `new_frame_version` HC:487 | ADAPT | `expectedVersion` + typed `version_conflict` (2) |
| NEW methods | NEW | `holders_snapshot`, `holders_since`, `report_holders`, `frame_versions`, `presence_beat`, `presence_leave` (2) |
| `AccountClientError` `account/client.rs:26-57` | ADAPT | + `Http{status,message}`, `Gone(String)`, `SessionGone`, `VersionConflict{content_version}` (2) |
| `revoke_device` `account/client.rs:233`, `api/account.rs:352` | ADAPT | `retire` flag (7) |
| `sign_out` `api/account.rs:333` | ADAPT | Calls `collab_live::on_sign_out` (DELETE presence, stop the session) (15) |
| `collab/fake_hub.rs` (wiremock catch-all over `Arc<Mutex<FakeHubState>>`, 1380 lines) | ADAPT | v3 routes, claims model, digest, feed counters; an axum front serves `/me/events` and `/me/presence` and proxies the rest to wiremock (3) |

### Commands (both hosts) and frontend

| Component | Verdict | Wave-3 change (task) |
| ---- | ---- | ---- |
| `resolve_collab_loss` TC:236 / WC:300 (+ `LossArgs` WC:17-87) | REMOVE | → `resolve_collab_deletions` (16) |
| `sync_project_now` TC:179 / WC:245 | REMOVE | → `collab_sync_now` (16) |
| `refresh_collab_frames` TC:125 / WC:192 | REMOVE | Events deliver changes; the UI calls `collab_sync_now` (16) |
| every other collab command TC:22-321 / WC:90-397 | KEEP | — |
| NEW commands | NEW | `collab_sync_now`, `get_collab_live_status`, `list_collab_attention`, `resolve_collab_deletions`, `preview_collab_stop_keeping`, `keep_collab_frames_again`, `resolve_collab_changed_file`, `get_collab_storage_status`, `collab_replace_device`, `set_collab_max_upload_streams`, `set_collab_max_receive_streams` (16) |
| `src/components/collab/ReceiveTab.tsx` (paused banner :154-177, `resolve_collab_loss` :95, `sync_project_now` :79, listener :60, `OnDiskBadge` :253) | ADAPT | Attention lists, state badges, no pause (17) |
| `src/components/collab/AutoReplicateBar.tsx` (`sync_project_now` :84) | ADAPT | Sync now → `collab_sync_now` (17) |
| `src/hooks/useCollabNotifications.ts` (paused listener :141, landed :228, changed :169, published :190; mounted `src/components/Layout.tsx:48-49,184`) | ADAPT | − paused; + deletion choice, frame lost, file changed (17) |
| `src/hooks/useProjects.ts` (`refresh_collab_frames` :95) | ADAPT | `collab_sync_now` (17) |
| `src/pages/ProjectDetail.tsx`, `src/pages/Projects.tsx` | ADAPT | Live status line (17) |
| NEW `CollabLiveStatus.tsx`, `CollabAttention.tsx`, `DeviceReplaceDialog.tsx`, Settings section `transfers.collabStreams` | NEW | (17) |

### Disk-copy ledger (one frame, steady state 1×N on every device)

| Case | Copies | Why |
| ---- | ---- | ---- |
| Landing | 1 | The store-owned data file is renamed to `<target>.athtmp` (same volume — the collab store lives on the root's volume), then renamed over `<target>`; the re-import by reference adds the path, it writes no data. `.athtmp` IS the file that becomes the target, never a second copy. Peak: one in-flight frame per active stream above steady. |
| Version bump vN → vN+1 | 1 (+ the in-flight vN+1 until its rename) | vN stays at `<target>` until the rename replaces it atomically (I7, L7). |
| Re-adoption (moved inside the root, device replace, file put back) | 0 new | Kept in place or recorded under its new path; the re-import by reference writes no data. |
| Quarantine | 0 new | The edited file stays where it is until the user acts. |
| "Re-fetch original" | 1 | The edited file goes to the OS trash (the user can undo) before the fetch lands; without a trash, it is deleted only after the user confirms. |
| Own frame outside the root (A1) | 1 | Seeded in place by reference; unchanged. |
| Identical bytes in two frames (P24) | 1 per path | `link_or_copy` from the existing landed path — unchanged from wave 2. |

---
## Plan rulings (decided here; cite as P1…)

Each ruling names the decision, why, and the cost if it is wrong. Wave-2
rulings that stay in force: R8 (adoption on "already announced"), R10
(column-targeted own-row write-backs), R14 (lost projects), R20 (conditional
landing fence), R21 (negative rehash cache — now the quarantine stamp), R23
(in-flight tags), R30, R31 (export = published ∧ accepted ∧ on_disk ∧
¬awaiting_gc), R34 (no parallel publish). Overridden by the spec (§2.1): R2,
R13, R15–R19, R24, R25, R33.

- **P1 — Layering.** Pure, ungated layers (`collab/live`, `collab/storage`,
  `collab/serve.rs`, `collab/scheduler`, `db/collab_live.rs`,
  `sharing/iroh/collab_pool.rs`) hold every decision; one gated orchestrator
  (`api/collab_live/`, `render`+`solver`) performs the effects.
  - *Why:* the decisions compile and test headless and without a clock;
    `api::collab` (which the orchestrator calls) is gated already.
  - *If wrong:* a headless build has no live exchange — as in wave 2, where
    the poll was gated too.
- **P2 — Streaming client.** A second `reqwest::Client` with no total
  timeout, `read_timeout(50 s)` (reqwest 0.13.4 `ClientBuilder::read_timeout`,
  reset after each read) and `connect_timeout(10 s)`; `Response::bytes_stream`
  (feature `stream`, now explicit); a hand-rolled SSE parser; one `warn!`
  when a stream delivers no byte for 25 s after `200` (a buffering proxy).
  - *Why:* the hub client's 30 s total deadline (HC:13, HC:325) kills any
    stream; no SSE crate is in the lock.
  - *If wrong:* a parser defect — pinned by tests that split every sample at
    every byte offset.
- **P3 — One back-off.** `Backoff` = full jitter, uniform in
  `[0, min(60 s, 1 s · 2^attempt)]`, floored at 100 ms, over
  `geometry::ransac::SplitMix64` seeded from `Uuid::new_v4()` (seedable in
  tests). A process-wide reset signal (`live::backoff::reset_all()`, a
  `watch` counter) wakes every sleeping retry and zeroes its attempt count —
  Sync now's "clears every back-off".
  - *Why:* §4.1, §4.6, §7.3, L10; core has no `rand`.
  - *If wrong:* none material.
- **P4 — Retry classification.** `CollabClient`'s classifier returns
  `AccountClientError::Http { status, message }` for statuses it does not map
  (its `Display` keeps the wave-2 text `hub returned {status} ({what}): {msg}`).
  Retryable = `Network` (transport) ∪ `Http` 5xx ∪ `RateLimited` (429).
  Background calls retry without bound and are cancelled by dropping the
  future (the session owns them); user-initiated calls retry at most 3
  attempts. 401 → `SignedOut`; 403 → the project is marked refused
  (`error!`), no retry.
  - *Why:* §4.6; a UI command must not hang for minutes.
  - *If wrong:* a UI action fails after ~3–7 s of hub outage instead of
    waiting.
- **P5 — Feed cursors.** `collab_projects.hub_version` IS the version cursor
  (it already means "project version vouched by the hub", written by
  `set_sync_state`). New columns `feed_epoch TEXT` and
  `holder_seq INTEGER NOT NULL DEFAULT -1`; `-1` means "no local holder map"
  → snapshot. The hub-wide epoch is also kept in `collab_live_meta['epoch']`.
  - *Why:* reuse; no second version counter to keep in step.
  - *If wrong:* none.
- **P6 — Catch-up.** A `project` catch-up is `sync_manifest(vouched =
  Some(version))` plus `refresh_projects_reporting(Some({pid}))` iff the
  kinds include `meta`, `members`, `thresholds` or `dictionary`. Inline
  `frames` are applied by `upsert_from_manifest` in one transaction that
  also advances `hub_version` and `manifest_cursor`. `grid` is logged at
  `debug` (its consumer is wave 6). `resync project` = every kind.
  - *Why:* spec §5.3's last paragraph; the hub needs no change-kind log.
  - *If wrong:* an extra small refresh on some events.
- **P7 — Holder map.** Persisted in `collab_holder_devices` and
  `collab_holder_claims`, mirrored in memory; every delta is applied in the
  same transaction as the `holder_seq` advance. A snapshot is loaded only
  when there is no map, the epoch changed, or the hub answers `410`.
  Providers are derived at query time and never stored.
  - *Why:* §6.1; a hub restart costs one delta read per device.
  - *If wrong:* DB writes per holders event (the hub coalesces them per
    second).
- **P8 — Journal, claims, outbox.** `collab_my_claims(project_id,
  frame_uuid, content_version)` is the device's claim set as queued;
  `collab_outbox(seq, project_id, frame_uuid, op, content_version)` holds
  unsent changes; `seq` comes from ONE device-wide counter
  (`collab_live_meta['report_seq']`) that never goes back. `set_local_state`
  writes the state, `on_disk`, the claim and the outbox row in ONE
  transaction (collision C24). The flush reads the outbox and the claim set
  in one read transaction and computes the digest from that claim set.
  `hello.projects[*].reportSeq` above the counter raises the counter.
  - *Why:* §6.2, §6.3, hub rules on `reportSeq`.
  - *If wrong:* a 5,000-claim digest per flush costs ~2 ms.
- **P9 — Refused claims.** Dropped from `collab_my_claims` (no outbox row),
  `warn!(project_id, frame_uuid, "claim refused by the hub")`, and the row's
  `last_error` is set. A refused frame re-enters the claim set only through a
  later state transition.
  - *Why:* hub P6 — both sides must reach the same set or the digest loops.
  - *If wrong:* a legitimately held frame stays unclaimed until its next
    transition, visible in the UI through `last_error`.
- **P10 — Per-frame state machine.** `project_frames_local.local_state` ∈
  `wanted`, `held`, `missing`, `awaiting_choice`, `quarantined`, `not_kept`,
  `idle` (replicas) and `own_held`, `own_missing`, `own_changed` (own
  frames). `Fetching` is scheduler memory only: a crash leaves `wanted`.
  `on_disk` = servable (`held`, `own_held`) and is written only by
  `set_local_state`. Edges beyond the spec's diagram, closing its gaps:
  - `held(vN)` + `vM` current with the SAME `blake3` → `held(vM)`, outbox
    `add` (an epoch re-announce resets content versions; identical bytes);
  - auto-replicate off: `held` keeps serving, `wanted` is simply not
    dispatched (spec §7.4 cancels fetches only);
  - a role that may not receive: replicas → `idle`;
  - a lost project keeps R14 (replica rows dropped, files inert as Other
    files, its claims, outbox and holder map cleared).
  - *Why:* one explicit, table-tested transition function.
  - *If wrong:* an edge case lands in the wrong list; the seeded simulation
    checks the invariants on every step.
- **P11 — Deletion rules (L4).** Evaluated per settle batch, device-wide (one
  rolling window per Collaboration store, across projects).
  - batch + the settled deletions of the last 5 min > 10 → every frame of
    the window still `missing`, or `wanted` because of a deletion, becomes
    `awaiting_choice` (its fetch is cancelled); one `collab-deletion-choice`
    event;
  - a frame whose previous settled deletion is < 24 h old joins the choice
    whatever the count;
  - otherwise `wanted` (automatic re-fetch);
  - "lost everywhere" is decided at settle from the holder map (no other
    claim of the current version, online or offline) and raises
    `collab-frame-lost` at once.
  - *Why:* L4.
  - *If wrong:* ten deletions every six minutes re-fetch each time; the
    24 h rule then catches the repeats.
- **P12 — Landing by temp file, rename, re-import.** `export_child_replacing`
  exports (TryReference) to `<target>.athtmp`, renames it over `<target>`,
  then re-imports `<target>` by reference (`add_path_child` TryReference +
  hash equality + `ensure_child_readable(CopyRepair::Refuse)`), all under the
  in-flight tag; the seed tag is set afterwards.
  - *Why:* spec §7.5/I7 — and verified against iroh-blobs 0.103:
    `export_path_impl` records the EXPORT target as the entry's external path
    (`store/fs.rs:1316-1320`), so a bare rename would leave the store
    pointing at a dead `.athtmp`. An import merges external path lists sorted
    (`store/fs/entry_state.rs:40-58`, `store/fs/meta.rs:403-406`), and
    `<name>` sorts before `<name>.athtmp`, so the target becomes the first —
    the served — path.
  - The dead second path stays in the entry until GC. `export_child`
    (personal sync) is untouched.
  - *If wrong:* one extra full read of every landed frame. The alternative,
    renaming the old file aside, was rejected: the target path is absent
    during the export.
- **P13 — Nothing lands over a quarantined file.** A `quarantined` frame is
  never dispatched, and the landing fence gains `AND local_state = 'wanted'`
  (`set_landed_if`).
  - *Why:* L5, C38.
  - *If wrong:* none.
- **P14 — The serve check reads the DB per get request.** Index
  `project_frames_local(blake3)`. Served iff the hash is the current version
  of a frame in `held`/`own_held`, the landed file's `(size, mtime)` is
  within 2 s of the record, the store is `Available` or `ReadOnly`, and the
  upload streams in use are below `collab.max_upload_streams`.
  - mismatch → `AbortReason::Permission` (ERR_PERMISSION) and a local check
    request to the storage task;
  - limit → `AbortReason::RateLimited` (ERR_LIMIT);
  - superseded or unknown hash → Permission, logged at `debug`.
  - *Why:* §9.3; one point query per frame request is cheap and never stale.
  - *If wrong:* one SQLite read per request.
- **P15 — iroh-blobs mask quirk.** 0.103 matches `mask.get` for every request
  kind (`provider/events.rs:462`) and never reads `mask.push`,
  `mask.get_many` or `mask.observe`. With `get: InterceptLog` the collab
  consumer receives and answers:
  - `PushRequestReceived` → `Permission`;
  - `GetManyRequestReceived` → `Permission` (no collab caller uses it);
  - `ObserveRequestReceived` → the serve check (a hedge observes a servable
    hash legitimately).

  The personal store keeps `NotifyLog` unchanged (Global Constraint); that
  it accepts inbound pushes today is reported to the owner, not fixed here.
- **P16 — Accepted-connection registry.** `CollabSlotBlobs::accept` records
  `connection.weak_handle()` per remote node id before delegating. On a
  `members` event, after the membership snapshot refresh,
  `SharedIrohNode::close_collab_connections_not_admitted()` upgrades each
  handle and closes (`0`, `b"membership revoked"`) those the connect-gate
  predicate now refuses; the collab pool closes its outbound connections to
  the same node ids.
  - *Why:* I11; today nothing closes an accepted connection (M:2092-2093).
  - *If wrong:* one registry entry per live connection.
- **P17 — Class-aware pacer.** `UploadPacer::reserve_class(size, class)`.
  Personal reservations keep today's bucket. While a personal upload is
  active, collab reservations pace at `COLLAB_SHARE_WHILE_PERSONAL = 10 %` of
  the configured cap — or, with no cap, of the personal class's observed
  rate over the last 2 s, floored at 64 KiB/s. With no personal activity,
  collab uses the normal bucket.
  - *Why:* L1, "a fixed low share of the link whether or not a byte-rate cap
    is set"; the app cannot know the link rate, so the personal throughput
    is the proxy.
  - *If wrong:* collab crawls while personal uploads run — which is the
    ruling's intent.
- **P18 — One collab lane.** The executor takes ONE collab-class
  `ReceiveGate` permit and runs one live assignment run that takes one-frame
  work units from the scheduler, at most `collab.max_receive_streams` in
  flight (replacing `MAX_IN_FLIGHT = 32` for collab).
  - A waiting personal transfer signals yield: the lane takes no new unit,
    lets the frames in flight finish, releases the permit and re-queues. A
    frame above 256 MiB is cut at its next 256 MiB of progress and resumes
    from its verified ranges.
  - *Why:* §8's "a personal transfer waits at most one frame", with swarm
    concurrency independent of `sync.max_concurrent_receives` (default 2).
    "Receive admission is per unit" reads as "each unit is admitted into the
    lane, and the lane yields at unit boundaries".
  - *If wrong:* the gate's collab class changes to one permit per unit — a
    local change; flagged to the owner.
- **P19 — Collab pool.** NEW `sharing/iroh/collab_pool.rs` in the
  `ControlPool` idiom (N:634-739): one connection per provider, dialed with
  `Endpoint::connect_with_opts` and an explicit `QuicTransportConfig`
  (keep-alive 5 s, idle 30 s), wrapped in `tokio::time::timeout(10 s)` (iroh
  has no connect-timeout parameter; `endpoint.rs:1100-1145`), kept open 60 s
  after the last request, one `closed()` watcher per connection reporting
  `PoolEvent::Closed`. It dials the `EndpointAddr` from
  `pairing::peer_dial_addr(node, Some(&report), relay_urls, true)` with the
  live presence relay (S1, C40).
  - *Why:* iroh-util's pool dials plain `connect` with a 1 s timeout and no
    options (`connection_pool.rs:57-66, 186-201`).
  - *If wrong:* one more pool type to maintain.
- **P20 — Refusal and failure codes.** On `GetProgressItem::Error(GetError)`:
  - `iroh_error_code() == ERR_PERMISSION` (1) → that provider is excluded for
    that hash, no strike;
  - `== ERR_LIMIT` (2) → that provider is retried after `LIMIT_RETRY = 2 s`,
    no strike;
  - `GetError::Decode { DecodeError::ParentHashMismatch | LeafHashMismatch }`
    → excluded for that hash, `warn!(device, frame_uuid)`;
  - a dial failure or a lost connection → the scheduler's per-provider dial
    back-off (1 → 60 s), never a hub write.

  A connection-level refusal does not surface through `iroh_error_code`
  (`api/remote.rs:677-682` maps it to `LocalFailure`); dial and close
  results therefore come from the pool (`close_reason()`).
- **P21 — Stream limits.** `collab.max_upload_streams` (default 8, 1..=64)
  and `collab.max_receive_streams` (default 8, 1..=32) are PROVISIONAL.
  Task 18 runs a relay measurement (an `#[ignore]`d test beside
  `api/relay_live_tests.rs`, `ATHENAEUM_TEST_RELAY`) and sets the defaults
  from it; if the relay is unreachable, the provisional values stay and the
  measurement is written into open-items as owed. Both apply live through
  `set_collab_max_upload_streams` / `set_collab_max_receive_streams`.
  - *Why:* §8.
  - *If wrong:* a re-tune commit.
- **P22 — Storage marker.** `<root>/.athenaeum/store-id` holds
  `{"storeId":"<uuid>","deviceId":"<b64 pubkey>"}`, recorded in
  `collab_live_meta` (`store_id`, `store_device`).
  - Written at designation. A root without a marker is adopted once, iff no
    `store_id` is recorded yet (the wave-2 upgrade); a marker naming this
    device with nothing recorded (a reset app DB) is recorded as found; a
    marker naming another device is `Unavailable(OtherDevice)` → P29.
  - Checked before every write, delete, landing, missing classification and
    root scan, and on a watcher root event.
  - The root is never recreated: `set_collab_root` and landing refuse a
    missing root. The read-only probe creates and removes
    `.athenaeum/.write-probe`.
- **P23 — Watcher.** notify 8 `RecommendedWatcher`, recursive on the
  canonicalized root, events over an UNBOUNDED channel (a bounded one can
  deadlock the fs-event thread — the perseus lesson, `crates/perseus/src/watcher.rs`).
  - Ignored: `*.athtmp` and `.athenaeum/**`, except `.athenaeum/canary`.
  - Canary: rewritten every 5 min; no event within 30 s → watcher dead →
    degraded sweep and a UI flag.
  - Network volume: unix `statfs` (macOS `f_fstypename` ∈ `smbfs`, `nfs`,
    `afpfs`, `webdav`, `cifs`; Linux `f_type` SMB/SMB2/CIFS/NFS magic);
    Windows: a `\\` UNC prefix, otherwise the canary decides.
  - *If wrong:* an exotic network file system reads healthy until the canary
    catches it within 5 min.
- **P24 — Sweep.** Covers every `landed_path` of every row, own frames
  outside the root included (A1). Stat with the 2 s tolerance; on drift,
  xxh3 decides "same bytes" (the row's recorded xxh3); re-adoption of a file
  found by hash uses the store import, which computes BLAKE3.
  `rejected_size_mtime` skips unchanged quarantined files.
- **P25 — Epoch change (§4.4).** For every project:
  1. load the holder snapshot;
  2. `manifest_cursor = 0` and `sync_manifest` with the caps-rule prune;
  3. send a `full: true` report;
  4. `reannounce_lost_own_frames`: own rows missing from the refetched
     manifest are announced from their stored `manifest_json`, with the
     project's CURRENT thresholds version as `gateVersion` (thresholds are
     prospective, v3 §10); R8 adoption applies.
  - *If wrong:* a frame that would fail newer thresholds is re-announced —
    it passed when it was published.
- **P26 — Sync now (L10).** Global `collab_sync_now`: drop the stream,
  `reset_all()` back-offs (requests, reconnect, provider dials), reconnect at
  once, then per project catch up both sides, send an empty report (digest
  check) and run a stat sweep. Replaces the per-project `sync_project_now`.
- **P27 — Live status.** `CollabLiveStatus { state, retryInSecs, since,
  storage, watcherDegraded, networkVolume }` with `state` ∈ `off`,
  `connecting`, `live`, `reconnecting`, `unreachable`, `signedOut`,
  `outdated`. `unreachable` after 3 consecutive failed connects (still
  retrying at the cap). Emitted as `collab-live-status` on every change;
  read by `get_collab_live_status`.
- **P28 — Clean exit, sign-out, sleep.** Tauri `RunEvent::ExitRequested |
  Exit` (`crates/athenaeum-tauri/src/lib.rs:521-540`) and the web graceful
  shutdown (`crates/athenaeum-web/src/main.rs:360-390`) call
  `collab_live::shutdown(ctx)` (bounded 2 s: `DELETE /me/presence`, stop the
  session) before the iroh node shutdown. `api::account::sign_out` calls
  `collab_live::on_sign_out`.
  - System sleep: verified — tauri 2.11.5 has no desktop sleep event
    (`RunEvent::Resumed` is an event-loop wake; `WindowEvent::Suspended` is
    mobile-only). No pre-sleep DELETE; the hub's 40 s rule covers it.
  - On wake, the beat task sees a wall-clock jump of more than 30 s beyond
    its 15 s interval and reconnects at once.
  - *If wrong:* a sleeping device shows online for ≤ 40 s.
- **P29 — Device replace (§9.5).** A marker naming another device of the
  same account (from `list_devices`, not retired) makes the store
  `Unavailable(OtherDevice)`. `get_collab_storage_status` offers the replace
  prompt when that device's `lastSeenAt` is older than 7 days, and always
  allows an explicit pick; `proposeRetire` when older than 30 days.
  `collab_replace_device` = `revoke_device(id, retire = true)` → rewrite the
  marker → mount → re-adopt every file under the root whose size + xxh3
  match a manifest row (seeded by reference; the import computes BLAKE3) →
  `held` + outbox `add`. Never automatic.
- **P30 — No new switches.** Spec §4.2 lists "collab serving is off" and
  "the device is paused by the user" among the reasons for
  `serving = false`. Neither switch exists, and minimal scope forbids adding
  them. `serving[pid]` = collab store mounted ∧ storage `Available|ReadOnly`
  ∧ project live. Flagged to the owner.
- **P31 — Single-delete re-fetch latency.** iroh-blobs 0.103 has no public
  blob delete (`api/blobs.rs:159-166` are `pub(crate)`), so a deleted
  replica's entry stays `Complete` over a dead path until the collab GC
  (900 s) drops it after the seed tag is removed at settle. The e2e uses the
  `test_gc` hook. Production latency = settle (60 s) + at most one GC
  interval. Not tuned here (wave-2 P20: a GC change needs its own
  measurement).
- **P32 — Fake hub.** The wiremock catch-all stays for REST and learns every
  v3 route with a claims model (reportSeq rule, refused, digest,
  tombstones), feed counters and a `tokio::sync::broadcast` of events. An
  axum front on `127.0.0.1:0` serves `GET /api/v1/me/events` (SSE from the
  broadcast, `hello` computed from the state) and `POST`/`DELETE
  /api/v1/me/presence`, and proxies every other request to wiremock.
  `FakeHub::uri()` returns the front.
  - *Why:* wiremock cannot stream (`ResponseTemplate` holds a whole body).
  - *If wrong:* ~200 lines of test infrastructure.
- **P33 — Removed settings.** `init_db` deletes the rows idempotently:
  `DELETE FROM settings WHERE key IN ('collab.loss_guard_fraction','collab.loss_guard_bytes')`.
- **P34 — Implicit claims in publish.** After an announce (implicit
  `(uuid, 1)`) and after `ok` batch versions (implicit `(uuid, cv)`),
  `db::collab_live::add_implicit_claim` writes `collab_my_claims` without an
  outbox row; frames adopted by R8 get an outbox `add` (the hub wrote no
  claim for this device). Versions go through `frame_versions` batches of
  ≤ 500 with `expectedVersion` = the local row's current `content_version`,
  and the outbox is flushed for those frames first. `conflict` → `warn!`,
  keep the staged file, re-dirty auto-publish.

---

## File structure

**Core, new (ungated):**
- `collab/live/mod.rs` — `pub mod backoff; pub mod cursor; pub mod digest; pub mod holders; pub mod outbox; pub mod presence; pub mod sse; pub mod stream; pub mod wire;` (Tasks 2, 4, 5, 6)
- `collab/live/backoff.rs`, `collab/live/digest.rs` (Task 2)
- `collab/live/wire.rs`, `collab/live/sse.rs`, `collab/live/stream.rs`, `collab/live/presence.rs` (Task 4)
- `collab/live/cursor.rs` (Task 5)
- `collab/live/holders.rs`, `collab/live/outbox.rs` (Task 6)
- `collab/storage/mod.rs`, `collab/storage/marker.rs` (Task 7); `watch.rs`, `sweep.rs` (Task 8); `states.rs`, `deletions.rs` (Task 9)
- `collab/serve.rs` (Task 10)
- `collab/scheduler/mod.rs`, `collab/scheduler/core.rs`, `collab/scheduler/sim_tests.rs` (Task 14)
- `db/collab_live.rs` (Task 1)
- `sharing/iroh/collab_pool.rs` (Task 12)

**Core, new (gated `render`+`solver`):**
- `api/collab_live/mod.rs` — spawner, handle, status, commands' logic (Tasks 15, 16)
- `api/collab_live/session.rs` — stream, hello, beat, reconnect (Task 15; the pure parts are Task 4)
- `api/collab_live/feed.rs` — cursor application, catch-up, epoch (Task 5)
- `api/collab_live/holdings.rs` — flush, full report, digest check (Task 6)
- `api/collab_live/storage_task.rs` — marker, watcher, sweep, state transitions (Tasks 7–9)
- `api/collab_live/landing.rs` — landing moved out of `collab_exchange.rs` (Task 11)
- `api/collab_live/executor.rs` — scheduler effects (Task 15)
- `api/collab_live/replace.rs` — device replace (Task 7 core, Task 16 command)
- `api/collab_live/serve_oracle.rs` — the DB-backed serve oracle (Task 10)
- `api/collab_live/surface.rs` — command-facing functions and DTOs (Task 16)
- `api/collab_live/test_support.rs` — `#[cfg(test)]` rigs shared by Tasks 7–18 (created in Task 7, grown by each task that names a helper)
- `api/collab_v3_live_e2e_tests.rs` (Task 18; replaces `api/collab_v3_e2e_tests.rs`)

**Core, modified:** `Cargo.toml`, `lib.rs` (nothing new at top level), `collab/mod.rs`, `api/mod.rs`, `db/mod.rs`, `db/schema.rs`, `db/collab_frames.rs`, `db/collab.rs`, `collab/hub_client.rs`, `account/client.rs`, `api/account.rs`, `api/collab.rs`, `api/collab_exchange.rs`, `api/scan_roots.rs`, `api/sync.rs`, `collab/fake_hub.rs`, `sharing/iroh/{mod,node,assign,blobs,pacer}.rs`, `sync/receiver.rs`, `settings/mod.rs`, `scanner/mod.rs`, `ts_export.rs`, `api/relay_live_tests.rs`.

**Hosts:** `crates/athenaeum-tauri/src/commands/collab.rs`, `crates/athenaeum-tauri/src/lib.rs`; `crates/athenaeum-web/src/routes/collab.rs`, `crates/athenaeum-web/src/routes/mod.rs`, `crates/athenaeum-web/src/main.rs`.

**Frontend:** new `src/components/collab/{CollabLiveStatus,CollabAttention,DeviceReplaceDialog}.tsx` (+ tests); modified `src/components/collab/{ReceiveTab,AutoReplicateBar}.tsx`, `src/hooks/{useCollabNotifications,useProjects}.ts`, `src/pages/{ProjectDetail,Projects}.tsx`, `src/components/Layout.tsx`, `src/settings/registry.ts`, `src/components/settings/TransfersSection.tsx`, `src/components/settings/tabs/TransfersTab.tsx`, `src/types/models.ts` (generated).

**Docs:** `docs/transfers/README.md`, `docs/superpowers/open-items.md`, the logging spec dictionary, `docs/frontend/notifications.md` (the stale kind list), `CLAUDE.md` (the collab bullet and command count).

**Task order and implementer:**

| # | Task | Implementer |
| ---- | ---- | ---- |
| 1 | Schema, local-state migration, dependencies | rust-engineer |
| 2 | Hub client v3, typed errors, retry and back-off, the digest | rust-engineer |
| 3 | Fake hub v3 with an event-stream front | rust-engineer |
| 4 | Event-stream client: wire types, SSE parser, stream reader, presence beat body | rust-engineer |
| 5 | Feed applier: cursors, catch-up, resync, versions, epoch change, account and members events | rust-engineer |
| 6 | Holder map, providers, outbox, flush, digest reconciliation, implicit claims in publish | rust-engineer |
| 7 | Storage marker, availability, device-replace core | rust-engineer |
| 8 | File watcher, settle, canary, stat sweep | rust-engineer |
| 9 | Per-frame state machine, deletion rules, quarantine, re-adoption | rust-engineer |
| 10 | Provider intercept: serve check, upload stream limit, class-aware pacer, connection registry | rust-engineer |
| 11 | Landing: temp file, rename, re-import; never over a quarantined file | rust-engineer |
| 12 | Collab pool and the live assignment run | rust-engineer |
| 13 | Two-class ReceiveGate with yield-on-demand | rust-engineer |
| 14 | Scheduler deterministic core and seeded simulation | rust-engineer |
| 15 | Live orchestrator: session, executor, spawner, exit/sign-out; retire the wave-2 worker and loss guard | rust-engineer |
| 16 | Command surface on both hosts, DTOs, ts-rs | rust-engineer |
| 17 | Frontend: live status, Sync now, attention lists, choices, device replace, Settings, notifications | frontend-dev |
| 18 | Three-instance e2e, relay stream measurement, docs, final gates | rust-engineer |

---
### Task 1: Schema, local-state migration, dependencies (P5, P8, P10, P22)

**Implementer:** rust-engineer.

**Files:**
- Modify: `crates/athenaeum-core/Cargo.toml` (reqwest features L50-59, new deps `notify`, `trash`, dev-dep `axum`).
- Modify: `crates/athenaeum-core/src/db/schema.rs` — collab section after the `rejected_size_mtime` ALTER (~:2445) and after `collab_projects` ALTERs (~:2388).
- Create: `crates/athenaeum-core/src/db/collab_live.rs`; register `pub mod collab_live;` in `crates/athenaeum-core/src/db/mod.rs`.
- Modify: `crates/athenaeum-core/src/db/collab_frames.rs` — `LocalState`, `SELECT_COLS` (:35-38), `LocalFrameRow` (:69-107), `row_from_sql` (:108-137), `upsert_from_manifest` (:148-205), `record_own` (:251), `set_landed` (:294), `set_landed_if` (:316), `set_missing` (:378), `set_declined` (:410); new `set_local_state`.
- Modify: `crates/athenaeum-core/src/db/collab.rs` — `SELECT_COLS` (:20-25), `CollabProjectRow` (:28-88), row mapper, `mark_lost` (:219); new `set_feed_version`, `set_holder_seq`.
- Test: unit tests in `db/collab_live.rs`, `db/collab_frames.rs`, `db/collab.rs`; schema idempotency in `db/schema.rs`.

**Interfaces:**
- Consumes: nothing new.
- Produces:
  ```rust
  // db/collab_frames.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  pub enum LocalState { Wanted, Held, Missing, AwaitingChoice, Quarantined, NotKept, Idle, OwnHeld, OwnMissing, OwnChanged }
  impl LocalState {
      pub fn as_db_str(self) -> &'static str;          // "wanted" | "held" | "missing" | "awaiting_choice" | "quarantined" | "not_kept" | "idle" | "own_held" | "own_missing" | "own_changed"
      pub fn from_db_str(s: &str) -> LocalState;        // unknown → Wanted for a replica, logged at warn by the caller
      pub fn servable(self) -> bool;                    // Held | OwnHeld
  }
  // LocalFrameRow gains: pub local_state: LocalState, pub frame_seq: Option<i32>
  pub struct StateWrite { pub from: LocalState, pub to: LocalState, pub claim: Option<crate::db::collab_live::ClaimOp> }
  pub fn set_local_state(conn: &Connection, project_id: &str, frame_uuid: &str, to: LocalState) -> Result<Option<StateWrite>>; // None = no such row
  pub fn set_frame_seq(conn: &Connection, project_id: &str, frame_uuid: &str, frame_seq: i32) -> Result<usize>;
  // db/collab_live.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum ClaimOp { Add { content_version: i32 }, Remove }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct OutboxRow { pub seq: i64, pub frame_uuid: String, pub op: ClaimOp }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct HolderDeviceRow { pub device: String, pub display_name: String, pub relay_url: Option<String> }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct DeletionRecord { pub project_id: String, pub frame_uuid: String, pub settled_at_ms: i64 }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct QuarantineRow { pub project_id: String, pub frame_uuid: String, pub path: String, pub detected_at: String, pub quarantined_version: i32, pub observed_size_mtime: Option<String> }
  pub const META_REPORT_SEQ: &str = "report_seq";
  pub const META_EPOCH: &str = "epoch";
  pub const META_ACCOUNT_ID: &str = "account_id";
  pub const META_STORE_ID: &str = "store_id";
  pub const META_STORE_DEVICE: &str = "store_device";
  pub fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>>;
  pub fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<()>;
  pub fn next_report_seq(conn: &Connection) -> Result<i64>;
  pub fn current_report_seq(conn: &Connection) -> Result<i64>;
  pub fn raise_report_seq(conn: &Connection, at_least: i64) -> Result<i64>;
  pub fn record_claim_change(conn: &Connection, project_id: &str, frame_uuid: &str, op: ClaimOp) -> Result<i64>;
  pub fn add_implicit_claim(conn: &Connection, project_id: &str, frame_uuid: &str, content_version: i32) -> Result<()>;
  pub fn drop_claims(conn: &Connection, project_id: &str, frame_uuids: &[String]) -> Result<usize>;
  pub fn my_claims(conn: &Connection, project_id: &str) -> Result<Vec<(String, i32)>>;
  pub fn outbox(conn: &Connection, project_id: &str) -> Result<Vec<OutboxRow>>;
  pub fn outbox_len(conn: &Connection, project_id: &str) -> Result<usize>;
  pub fn outbox_projects(conn: &Connection) -> Result<Vec<String>>;
  pub fn ack_outbox(conn: &Connection, project_id: &str, up_to_seq: i64) -> Result<usize>;
  pub fn clear_project_live_state(conn: &Connection, project_id: &str) -> Result<()>;
  pub fn replace_holders(conn: &Connection, project_id: &str, devices: &[HolderDeviceRow], claims: &[(String, i32, i32)]) -> Result<()>;
  pub fn upsert_holder_device(conn: &Connection, project_id: &str, dev: &HolderDeviceRow) -> Result<()>;
  pub fn apply_holder_delta(conn: &Connection, project_id: &str, device: &str, add: &[(i32, i32)], rm: &[i32]) -> Result<()>;
  pub fn load_holders(conn: &Connection, project_id: &str) -> Result<(Vec<HolderDeviceRow>, Vec<(String, i32, i32)>)>;
  pub fn record_deletion(conn: &Connection, project_id: &str, frame_uuid: &str, settled_at_ms: i64) -> Result<()>;
  pub fn deletions_since(conn: &Connection, since_ms: i64) -> Result<Vec<DeletionRecord>>;
  pub fn prune_deletions(conn: &Connection, older_than_ms: i64) -> Result<usize>;
  pub fn quarantine(conn: &Connection, row: &QuarantineRow) -> Result<()>;
  pub fn unquarantine(conn: &Connection, project_id: &str, frame_uuid: &str) -> Result<usize>;
  pub fn list_quarantine(conn: &Connection, project_id: &str) -> Result<Vec<QuarantineRow>>;
  // db/collab.rs — CollabProjectRow gains: pub feed_epoch: Option<String>, pub holder_seq: i64
  pub fn set_feed_version(conn: &Connection, project_id: &str, epoch: &str, version: i64) -> Result<usize>;
  pub fn set_holder_seq(conn: &Connection, project_id: &str, epoch: &str, holder_seq: i64) -> Result<usize>;
  ```

- [ ] **Step 1: Branch and dependencies**

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum
git checkout main && git checkout -b collab-v3-wave3
```

In `crates/athenaeum-core/Cargo.toml`:
- reqwest's feature list (L50-59) gains `"stream"`.
- `[dependencies]` gains `notify = "8"` and `trash = "5"`; the `uuid`
  dependency (L20) gains the `"v5"` feature (`collab::live::digest::uuid_for_digest`, Task 2).
- `[dev-dependencies]` gains `axum = "0.8"`.

Run: `cargo tree -p athenaeum-core -i notify --depth 0 && cargo tree -p athenaeum-core -i trash --depth 0`
Expected: `notify v8.2.0` and `trash v5.2.9`.

- [ ] **Step 2: Write the failing tests** (`db/collab_live.rs`, `#[cfg(test)] mod tests`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    fn conn_with_project() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO collab_projects (project_id, slug, title, data_role, target_name,
               target_ra_deg, target_dec_deg, target_radius_deg, membership_version,
               snapshot_payload_b64, snapshot_signature_b64, members_json)
             VALUES ('p1','m31','M31','send_receive','M31',10.0,41.0,1.0,1,'','','[]')",
            [],
        )
        .unwrap();
        conn
    }

    #[test]
    fn report_seq_is_monotonic_and_can_be_raised() {
        let conn = conn_with_project();
        assert_eq!(current_report_seq(&conn).unwrap(), 0);
        assert_eq!(next_report_seq(&conn).unwrap(), 1);
        assert_eq!(next_report_seq(&conn).unwrap(), 2);
        assert_eq!(raise_report_seq(&conn, 120).unwrap(), 120);
        assert_eq!(next_report_seq(&conn).unwrap(), 121);
        // raising below the current value never lowers it
        assert_eq!(raise_report_seq(&conn, 5).unwrap(), 121);
    }

    #[test]
    fn claim_changes_update_the_claim_set_and_append_to_the_outbox() {
        let conn = conn_with_project();
        let s1 = record_claim_change(&conn, "p1", "u1", ClaimOp::Add { content_version: 1 }).unwrap();
        let s2 = record_claim_change(&conn, "p1", "u2", ClaimOp::Add { content_version: 2 }).unwrap();
        let s3 = record_claim_change(&conn, "p1", "u1", ClaimOp::Remove).unwrap();
        assert!(s1 < s2 && s2 < s3);
        assert_eq!(my_claims(&conn, "p1").unwrap(), vec![("u2".to_string(), 2)]);
        let rows = outbox(&conn, "p1").unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2], OutboxRow { seq: s3, frame_uuid: "u1".into(), op: ClaimOp::Remove });
        assert_eq!(ack_outbox(&conn, "p1", s2).unwrap(), 2);
        assert_eq!(outbox_len(&conn, "p1").unwrap(), 1);
        assert_eq!(outbox_projects(&conn).unwrap(), vec!["p1".to_string()]);
    }

    #[test]
    fn implicit_claims_never_touch_the_outbox() {
        let conn = conn_with_project();
        add_implicit_claim(&conn, "p1", "u9", 1).unwrap();
        assert_eq!(my_claims(&conn, "p1").unwrap(), vec![("u9".to_string(), 1)]);
        assert_eq!(outbox_len(&conn, "p1").unwrap(), 0);
        assert_eq!(drop_claims(&conn, "p1", &["u9".to_string()]).unwrap(), 1);
        assert!(my_claims(&conn, "p1").unwrap().is_empty());
    }

    #[test]
    fn holder_map_round_trips_and_deltas_apply() {
        let conn = conn_with_project();
        let dev = HolderDeviceRow { device: "AAA=".into(), display_name: "Anna".into(), relay_url: None };
        replace_holders(&conn, "p1", &[dev.clone()], &[("AAA=".into(), 1, 1), ("AAA=".into(), 2, 1)]).unwrap();
        apply_holder_delta(&conn, "p1", "AAA=", &[(3, 2)], &[1]).unwrap();
        let (devices, claims) = load_holders(&conn, "p1").unwrap();
        assert_eq!(devices, vec![dev]);
        assert_eq!(claims, vec![("AAA=".to_string(), 2, 1), ("AAA=".to_string(), 3, 2)]);
        // a delta for an unknown device creates a placeholder device row
        apply_holder_delta(&conn, "p1", "BBB=", &[(2, 1)], &[]).unwrap();
        assert_eq!(load_holders(&conn, "p1").unwrap().0.len(), 2);
    }

    #[test]
    fn clearing_a_project_drops_claims_outbox_and_holders() {
        let conn = conn_with_project();
        record_claim_change(&conn, "p1", "u1", ClaimOp::Add { content_version: 1 }).unwrap();
        apply_holder_delta(&conn, "p1", "AAA=", &[(1, 1)], &[]).unwrap();
        clear_project_live_state(&conn, "p1").unwrap();
        assert!(my_claims(&conn, "p1").unwrap().is_empty());
        assert_eq!(outbox_len(&conn, "p1").unwrap(), 0);
        assert!(load_holders(&conn, "p1").unwrap().1.is_empty());
    }

    #[test]
    fn deletions_and_quarantine_book_keeping() {
        let conn = conn_with_project();
        record_deletion(&conn, "p1", "u1", 1_000).unwrap();
        record_deletion(&conn, "p1", "u2", 400_000).unwrap();
        assert_eq!(deletions_since(&conn, 300_000).unwrap().len(), 1);
        assert_eq!(prune_deletions(&conn, 300_000).unwrap(), 1);
        let q = QuarantineRow {
            project_id: "p1".into(), frame_uuid: "u1".into(), path: "/c/m31/a/x.fits".into(),
            detected_at: String::new(), quarantined_version: 2, observed_size_mtime: Some("10:20".into()),
        };
        // the FK needs the frame row
        conn.execute(
            "INSERT INTO project_frames_local (project_id, frame_uuid, content_version, origin,
               publisher_account_id, publisher_display, file_name, filter_canonical, state,
               byte_size, xxh3, blake3) VALUES ('p1','u1',2,'replica','a','A','x.fits','R','published',10,'x','b')",
            [],
        )
        .unwrap();
        quarantine(&conn, &q).unwrap();
        let listed = list_quarantine(&conn, "p1").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].quarantined_version, 2);
        assert_eq!(unquarantine(&conn, "p1", "u1").unwrap(), 1);
    }
}
```

And in `db/collab_frames.rs` tests (extend the module at :774):

```rust
#[test]
fn set_local_state_writes_on_disk_and_the_claim_in_one_transaction() {
    let conn = conn();
    upsert_from_manifest(&conn, "p1", &view("u1", 1)).unwrap();
    assert_eq!(get(&conn, "p1", "u1").unwrap().unwrap().local_state, LocalState::Wanted);

    let tx = conn.unchecked_transaction().unwrap();
    let w = set_local_state(&tx, "p1", "u1", LocalState::Held).unwrap().unwrap();
    assert_eq!((w.from, w.to), (LocalState::Wanted, LocalState::Held));
    assert_eq!(w.claim, Some(crate::db::collab_live::ClaimOp::Add { content_version: 1 }));
    tx.rollback().unwrap();
    // rolled back together: no state change, no outbox row
    assert_eq!(get(&conn, "p1", "u1").unwrap().unwrap().local_state, LocalState::Wanted);
    assert_eq!(crate::db::collab_live::outbox_len(&conn, "p1").unwrap(), 0);

    set_local_state(&conn, "p1", "u1", LocalState::Held).unwrap();
    let row = get(&conn, "p1", "u1").unwrap().unwrap();
    assert!(row.on_disk);
    // non-servable → non-servable writes no claim
    set_local_state(&conn, "p1", "u1", LocalState::Missing).unwrap();
    let w = set_local_state(&conn, "p1", "u1", LocalState::AwaitingChoice).unwrap().unwrap();
    assert_eq!(w.claim, None);
    assert!(!get(&conn, "p1", "u1").unwrap().unwrap().on_disk);
    assert_eq!(crate::db::collab_live::outbox_len(&conn, "p1").unwrap(), 2); // add, remove
}

#[test]
fn unpublished_or_excluded_replicas_start_idle() {
    let conn = conn();
    let mut v = view("u2", 1);
    v.accepted = false;
    upsert_from_manifest(&conn, "p1", &v).unwrap();
    assert_eq!(get(&conn, "p1", "u2").unwrap().unwrap().local_state, LocalState::Idle);
    let mut p = view("u3", 1);
    p.state = "pending".into();
    upsert_from_manifest(&conn, "p1", &p).unwrap();
    assert_eq!(get(&conn, "p1", "u3").unwrap().unwrap().local_state, LocalState::Idle);
}
```

And in `db/schema.rs` tests:

```rust
#[test]
fn wave3_backfill_maps_wave2_rows_once() {
    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();
    conn.execute_batch(
        "INSERT INTO collab_projects (project_id, slug, title, data_role, target_name,
           target_ra_deg, target_dec_deg, target_radius_deg, membership_version,
           snapshot_payload_b64, snapshot_signature_b64, members_json)
         VALUES ('p1','m31','M31','send_receive','M31',10.0,41.0,1.0,1,'','','[]');
         INSERT INTO project_frames_local (project_id, frame_uuid, content_version, origin,
           publisher_account_id, publisher_display, file_name, filter_canonical, state,
           accepted, byte_size, xxh3, blake3, on_disk, locally_declined)
         VALUES
           ('p1','own-on',1,'own','me','Me','a.fits','R','published',1,1,'x','b',1,0),
           ('p1','own-off',1,'own','me','Me','b.fits','R','published',1,1,'x','b',0,0),
           ('p1','rep-held',1,'replica','o','O','c.fits','R','published',1,1,'x','b',1,0),
           ('p1','rep-declined',1,'replica','o','O','d.fits','R','published',1,1,'x','b',0,1),
           ('p1','rep-want',1,'replica','o','O','e.fits','R','published',1,1,'x','b',0,0),
           ('p1','rep-excl',1,'replica','o','O','f.fits','R','published',0,1,'x','b',1,0);
         UPDATE project_frames_local SET local_state = NULL;",
    )
    .unwrap();
    init_db(&conn).unwrap();
    let state = |u: &str| -> String {
        conn.query_row(
            "SELECT local_state FROM project_frames_local WHERE frame_uuid = ?1",
            [u],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(state("own-on"), "own_held");
    assert_eq!(state("own-off"), "own_missing");
    assert_eq!(state("rep-held"), "held");
    assert_eq!(state("rep-declined"), "not_kept");
    assert_eq!(state("rep-want"), "wanted");
    assert_eq!(state("rep-excl"), "idle");
    // a second init changes nothing
    conn.execute("UPDATE project_frames_local SET local_state = 'missing' WHERE frame_uuid = 'rep-want'", []).unwrap();
    init_db(&conn).unwrap();
    assert_eq!(state("rep-want"), "missing");
}
```

- [ ] **Step 3: Run the tests to watch them fail**

Run: `cargo test -p athenaeum-core --lib db::collab_live db::collab_frames db::schema::tests::wave3_backfill_maps_wave2_rows_once`
Expected: compile errors — `collab_live`, `LocalState`, `set_local_state` do not exist.

- [ ] **Step 4: Schema** (`db/schema.rs`, inside `init_db`, in the collab section; every ALTER guarded by the existing `column_exists` helper at :302):

```rust
// ── collab v3 wave 3 (live exchange) ─────────────────────────────────────
for (col, ddl) in [
    ("feed_epoch", "ALTER TABLE collab_projects ADD COLUMN feed_epoch TEXT"),
    ("holder_seq", "ALTER TABLE collab_projects ADD COLUMN holder_seq INTEGER NOT NULL DEFAULT -1"),
] {
    if !column_exists(conn, "collab_projects", col)? {
        conn.execute(ddl, [])?;
    }
}
for (col, ddl) in [
    ("local_state", "ALTER TABLE project_frames_local ADD COLUMN local_state TEXT"),
    ("frame_seq", "ALTER TABLE project_frames_local ADD COLUMN frame_seq INTEGER"),
    ("state_changed_at", "ALTER TABLE project_frames_local ADD COLUMN state_changed_at TEXT"),
] {
    if !column_exists(conn, "project_frames_local", col)? {
        conn.execute(ddl, [])?;
    }
}
// One-time backfill (idempotent: only rows that never had a state).
conn.execute(
    "UPDATE project_frames_local SET local_state = CASE
        WHEN origin = 'own' AND on_disk = 1 THEN 'own_held'
        WHEN origin = 'own' THEN 'own_missing'
        WHEN locally_declined = 1 THEN 'not_kept'
        WHEN state <> 'published' OR accepted = 0 THEN 'idle'
        WHEN on_disk = 1 THEN 'held'
        ELSE 'wanted' END,
      state_changed_at = datetime('now')
     WHERE local_state IS NULL",
    [],
)?;
conn.execute_batch(
    "CREATE INDEX IF NOT EXISTS idx_project_frames_local_blake3 ON project_frames_local(blake3);
     CREATE INDEX IF NOT EXISTS idx_project_frames_local_state ON project_frames_local(project_id, local_state);
     CREATE TABLE IF NOT EXISTS collab_live_meta (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
     );
     CREATE TABLE IF NOT EXISTS collab_my_claims (
        project_id TEXT NOT NULL,
        frame_uuid TEXT NOT NULL,
        content_version INTEGER NOT NULL,
        PRIMARY KEY (project_id, frame_uuid),
        FOREIGN KEY (project_id) REFERENCES collab_projects(project_id) ON DELETE CASCADE
     );
     CREATE TABLE IF NOT EXISTS collab_outbox (
        seq INTEGER PRIMARY KEY,
        project_id TEXT NOT NULL,
        frame_uuid TEXT NOT NULL,
        op TEXT NOT NULL CHECK (op IN ('add','rm')),
        content_version INTEGER NOT NULL DEFAULT 0,
        FOREIGN KEY (project_id) REFERENCES collab_projects(project_id) ON DELETE CASCADE
     );
     CREATE INDEX IF NOT EXISTS idx_collab_outbox_project ON collab_outbox(project_id);
     CREATE TABLE IF NOT EXISTS collab_holder_devices (
        project_id TEXT NOT NULL,
        device TEXT NOT NULL,
        display_name TEXT NOT NULL DEFAULT '',
        relay_url TEXT,
        PRIMARY KEY (project_id, device),
        FOREIGN KEY (project_id) REFERENCES collab_projects(project_id) ON DELETE CASCADE
     );
     CREATE TABLE IF NOT EXISTS collab_holder_claims (
        project_id TEXT NOT NULL,
        device TEXT NOT NULL,
        frame_seq INTEGER NOT NULL,
        content_version INTEGER NOT NULL,
        PRIMARY KEY (project_id, device, frame_seq),
        FOREIGN KEY (project_id) REFERENCES collab_projects(project_id) ON DELETE CASCADE
     );
     CREATE TABLE IF NOT EXISTS collab_deletions (
        id INTEGER PRIMARY KEY,
        project_id TEXT NOT NULL,
        frame_uuid TEXT NOT NULL,
        settled_at_ms INTEGER NOT NULL,
        FOREIGN KEY (project_id) REFERENCES collab_projects(project_id) ON DELETE CASCADE
     );
     CREATE INDEX IF NOT EXISTS idx_collab_deletions_project ON collab_deletions(project_id);
     CREATE INDEX IF NOT EXISTS idx_collab_deletions_settled ON collab_deletions(settled_at_ms);
     CREATE TABLE IF NOT EXISTS collab_quarantine (
        project_id TEXT NOT NULL,
        frame_uuid TEXT NOT NULL,
        path TEXT NOT NULL,
        detected_at TEXT NOT NULL DEFAULT (datetime('now')),
        quarantined_version INTEGER NOT NULL,
        observed_size_mtime TEXT,
        PRIMARY KEY (project_id, frame_uuid),
        FOREIGN KEY (project_id, frame_uuid) REFERENCES project_frames_local(project_id, frame_uuid) ON DELETE CASCADE
     );",
)?;
```

`every_foreign_key_child_column_is_indexed` (schema.rs:3013) must stay green:
every new FK child column set is the leading prefix of a primary key or has
the index created above.

- [ ] **Step 5: `db/collab_live.rs`**

```rust
//! Local state of the collab v3 live exchange (wave 3, plan P5/P7/P8/P11).
//!
//! * `collab_live_meta` — hub-wide values: the device's `report_seq`
//!   counter (one per device, never goes back), the hub epoch, the account
//!   id seen in `hello`, the storage marker's store id and device.
//! * `collab_my_claims` / `collab_outbox` — the device's claim set and the
//!   unsent changes to it. Written ONLY through [`record_claim_change`]
//!   (inside the same transaction as the local state change that causes it,
//!   collision C24) or [`add_implicit_claim`] (announce/version, no report).
//! * `collab_holder_devices` / `collab_holder_claims` — the persisted holder
//!   map of every project (spec §6.1).
//! * `collab_deletions` — settled deletions, for the L4 window and the 24 h
//!   second-deletion rule.
//! * `collab_quarantine` — the Changed files list (L5).

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

pub const META_REPORT_SEQ: &str = "report_seq";
pub const META_EPOCH: &str = "epoch";
pub const META_ACCOUNT_ID: &str = "account_id";
pub const META_STORE_ID: &str = "store_id";
pub const META_STORE_DEVICE: &str = "store_device";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOp {
    Add { content_version: i32 },
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRow {
    pub seq: i64,
    pub frame_uuid: String,
    pub op: ClaimOp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolderDeviceRow {
    pub device: String,
    pub display_name: String,
    pub relay_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletionRecord {
    pub project_id: String,
    pub frame_uuid: String,
    pub settled_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineRow {
    pub project_id: String,
    pub frame_uuid: String,
    pub path: String,
    pub detected_at: String,
    pub quarantined_version: i32,
    pub observed_size_mtime: Option<String>,
}

pub fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM collab_live_meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()?)
}

pub fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO collab_live_meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn current_report_seq(conn: &Connection) -> Result<i64> {
    Ok(meta_get(conn, META_REPORT_SEQ)?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0))
}

/// The next journal sequence (hub rule: one monotonically increasing counter
/// per device, never reused, surviving restarts).
pub fn next_report_seq(conn: &Connection) -> Result<i64> {
    let next = current_report_seq(conn)? + 1;
    meta_set(conn, META_REPORT_SEQ, &next.to_string())?;
    Ok(next)
}

/// Raise the counter to at least `at_least` (a `hello.reportSeq` above ours
/// means the local journal was lost). Never lowers it.
pub fn raise_report_seq(conn: &Connection, at_least: i64) -> Result<i64> {
    let cur = current_report_seq(conn)?;
    if at_least > cur {
        meta_set(conn, META_REPORT_SEQ, &at_least.to_string())?;
        return Ok(at_least);
    }
    Ok(cur)
}

/// Change the device's claim set and append the change to the outbox, under
/// one new journal sequence. Callers pass the SAME connection/transaction
/// that writes the state change (C24).
pub fn record_claim_change(conn: &Connection, project_id: &str, frame_uuid: &str, op: ClaimOp) -> Result<i64> {
    let seq = next_report_seq(conn)?;
    match op {
        ClaimOp::Add { content_version } => {
            conn.execute(
                "INSERT INTO collab_my_claims (project_id, frame_uuid, content_version) VALUES (?1, ?2, ?3)
                 ON CONFLICT(project_id, frame_uuid) DO UPDATE SET content_version = excluded.content_version",
                params![project_id, frame_uuid, content_version],
            )?;
            conn.execute(
                "INSERT INTO collab_outbox (seq, project_id, frame_uuid, op, content_version) VALUES (?1, ?2, ?3, 'add', ?4)",
                params![seq, project_id, frame_uuid, content_version],
            )?;
        }
        ClaimOp::Remove => {
            conn.execute(
                "DELETE FROM collab_my_claims WHERE project_id = ?1 AND frame_uuid = ?2",
                params![project_id, frame_uuid],
            )?;
            conn.execute(
                "INSERT INTO collab_outbox (seq, project_id, frame_uuid, op, content_version) VALUES (?1, ?2, ?3, 'rm', 0)",
                params![seq, project_id, frame_uuid],
            )?;
        }
    }
    Ok(seq)
}

/// A claim the hub wrote itself (announce → `(uuid, 1)`, version → the new
/// content version). Enters the claim set and the digest; never the outbox.
pub fn add_implicit_claim(conn: &Connection, project_id: &str, frame_uuid: &str, content_version: i32) -> Result<()> {
    conn.execute(
        "INSERT INTO collab_my_claims (project_id, frame_uuid, content_version) VALUES (?1, ?2, ?3)
         ON CONFLICT(project_id, frame_uuid) DO UPDATE SET content_version = excluded.content_version",
        params![project_id, frame_uuid, content_version],
    )?;
    Ok(())
}

/// Drop claims the hub refused (P9). No outbox row: the hub already
/// tombstoned them.
pub fn drop_claims(conn: &Connection, project_id: &str, frame_uuids: &[String]) -> Result<usize> {
    let mut n = 0;
    for u in frame_uuids {
        n += conn.execute(
            "DELETE FROM collab_my_claims WHERE project_id = ?1 AND frame_uuid = ?2",
            params![project_id, u],
        )?;
    }
    Ok(n)
}

pub fn my_claims(conn: &Connection, project_id: &str) -> Result<Vec<(String, i32)>> {
    let mut stmt = conn.prepare(
        "SELECT frame_uuid, content_version FROM collab_my_claims WHERE project_id = ?1 ORDER BY frame_uuid",
    )?;
    let rows = stmt.query_map([project_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn outbox(conn: &Connection, project_id: &str) -> Result<Vec<OutboxRow>> {
    let mut stmt = conn.prepare(
        "SELECT seq, frame_uuid, op, content_version FROM collab_outbox WHERE project_id = ?1 ORDER BY seq",
    )?;
    let rows = stmt.query_map([project_id], |r| {
        let op: String = r.get(2)?;
        let cv: i32 = r.get(3)?;
        Ok(OutboxRow {
            seq: r.get(0)?,
            frame_uuid: r.get(1)?,
            op: if op == "add" { ClaimOp::Add { content_version: cv } } else { ClaimOp::Remove },
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn outbox_len(conn: &Connection, project_id: &str) -> Result<usize> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM collab_outbox WHERE project_id = ?1",
        [project_id],
        |r| r.get::<_, i64>(0),
    )? as usize)
}

pub fn outbox_projects(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT project_id FROM collab_outbox ORDER BY project_id")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Delete the outbox rows a successful report carried (`seq <= up_to_seq`).
/// Rows appended during the flush have higher sequences and stay.
pub fn ack_outbox(conn: &Connection, project_id: &str, up_to_seq: i64) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM collab_outbox WHERE project_id = ?1 AND seq <= ?2",
        params![project_id, up_to_seq],
    )?)
}

/// A lost project (R14) or an epoch reload: forget claims, outbox and holders.
pub fn clear_project_live_state(conn: &Connection, project_id: &str) -> Result<()> {
    for table in ["collab_my_claims", "collab_outbox", "collab_holder_claims", "collab_holder_devices"] {
        conn.execute(&format!("DELETE FROM {table} WHERE project_id = ?1"), [project_id])?;
    }
    Ok(())
}

/// Replace a project's holder map with a snapshot (one transaction is the
/// caller's).
pub fn replace_holders(
    conn: &Connection,
    project_id: &str,
    devices: &[HolderDeviceRow],
    claims: &[(String, i32, i32)],
) -> Result<()> {
    conn.execute("DELETE FROM collab_holder_claims WHERE project_id = ?1", [project_id])?;
    conn.execute("DELETE FROM collab_holder_devices WHERE project_id = ?1", [project_id])?;
    for d in devices {
        upsert_holder_device(conn, project_id, d)?;
    }
    let mut stmt = conn.prepare(
        "INSERT INTO collab_holder_claims (project_id, device, frame_seq, content_version) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (device, seq, cv) in claims {
        stmt.execute(params![project_id, device, seq, cv])?;
    }
    Ok(())
}

pub fn upsert_holder_device(conn: &Connection, project_id: &str, dev: &HolderDeviceRow) -> Result<()> {
    conn.execute(
        "INSERT INTO collab_holder_devices (project_id, device, display_name, relay_url) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(project_id, device) DO UPDATE SET display_name = excluded.display_name, relay_url = excluded.relay_url",
        params![project_id, dev.device, dev.display_name, dev.relay_url],
    )?;
    Ok(())
}

/// Apply one device's delta (`add` = `[frameSeq, contentVersion]`, `rm` =
/// `frameSeq`). An unknown device gets a placeholder row (its name arrives
/// with the next snapshot).
pub fn apply_holder_delta(conn: &Connection, project_id: &str, device: &str, add: &[(i32, i32)], rm: &[i32]) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO collab_holder_devices (project_id, device) VALUES (?1, ?2)",
        params![project_id, device],
    )?;
    for (seq, cv) in add {
        conn.execute(
            "INSERT INTO collab_holder_claims (project_id, device, frame_seq, content_version) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project_id, device, frame_seq) DO UPDATE SET content_version = excluded.content_version",
            params![project_id, device, seq, cv],
        )?;
    }
    for seq in rm {
        conn.execute(
            "DELETE FROM collab_holder_claims WHERE project_id = ?1 AND device = ?2 AND frame_seq = ?3",
            params![project_id, device, seq],
        )?;
    }
    Ok(())
}

pub fn load_holders(conn: &Connection, project_id: &str) -> Result<(Vec<HolderDeviceRow>, Vec<(String, i32, i32)>)> {
    let mut stmt = conn.prepare(
        "SELECT device, display_name, relay_url FROM collab_holder_devices WHERE project_id = ?1 ORDER BY device",
    )?;
    let devices = stmt
        .query_map([project_id], |r| {
            Ok(HolderDeviceRow { device: r.get(0)?, display_name: r.get(1)?, relay_url: r.get(2)? })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut stmt = conn.prepare(
        "SELECT device, frame_seq, content_version FROM collab_holder_claims WHERE project_id = ?1 ORDER BY device, frame_seq",
    )?;
    let claims = stmt
        .query_map([project_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((devices, claims))
}

pub fn record_deletion(conn: &Connection, project_id: &str, frame_uuid: &str, settled_at_ms: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO collab_deletions (project_id, frame_uuid, settled_at_ms) VALUES (?1, ?2, ?3)",
        params![project_id, frame_uuid, settled_at_ms],
    )?;
    Ok(())
}

pub fn deletions_since(conn: &Connection, since_ms: i64) -> Result<Vec<DeletionRecord>> {
    let mut stmt = conn.prepare(
        "SELECT project_id, frame_uuid, settled_at_ms FROM collab_deletions WHERE settled_at_ms >= ?1 ORDER BY settled_at_ms",
    )?;
    let rows = stmt.query_map([since_ms], |r| {
        Ok(DeletionRecord { project_id: r.get(0)?, frame_uuid: r.get(1)?, settled_at_ms: r.get(2)? })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn prune_deletions(conn: &Connection, older_than_ms: i64) -> Result<usize> {
    Ok(conn.execute("DELETE FROM collab_deletions WHERE settled_at_ms < ?1", [older_than_ms])?)
}

pub fn quarantine(conn: &Connection, row: &QuarantineRow) -> Result<()> {
    conn.execute(
        "INSERT INTO collab_quarantine (project_id, frame_uuid, path, quarantined_version, observed_size_mtime)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(project_id, frame_uuid) DO UPDATE SET path = excluded.path,
            observed_size_mtime = excluded.observed_size_mtime",
        params![row.project_id, row.frame_uuid, row.path, row.quarantined_version, row.observed_size_mtime],
    )?;
    Ok(())
}

pub fn unquarantine(conn: &Connection, project_id: &str, frame_uuid: &str) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM collab_quarantine WHERE project_id = ?1 AND frame_uuid = ?2",
        params![project_id, frame_uuid],
    )?)
}

pub fn list_quarantine(conn: &Connection, project_id: &str) -> Result<Vec<QuarantineRow>> {
    let mut stmt = conn.prepare(
        "SELECT project_id, frame_uuid, path, detected_at, quarantined_version, observed_size_mtime
         FROM collab_quarantine WHERE project_id = ?1 ORDER BY detected_at, frame_uuid",
    )?;
    let rows = stmt.query_map([project_id], |r| {
        Ok(QuarantineRow {
            project_id: r.get(0)?,
            frame_uuid: r.get(1)?,
            path: r.get(2)?,
            detected_at: r.get(3)?,
            quarantined_version: r.get(4)?,
            observed_size_mtime: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
```

(Paste the Step 2 test module at the bottom.)

- [ ] **Step 6: `db/collab_frames.rs`**
  - Add `LocalState` (Interfaces) with `as_db_str`/`from_db_str`/`servable`.
  - `SELECT_COLS` appends `, local_state, frame_seq`; `LocalFrameRow` appends
    `pub local_state: LocalState, pub frame_seq: Option<i32>`; `row_from_sql`
    reads index 25 (`LocalState::from_db_str(&row.get::<_, Option<String>>(25)?.unwrap_or_default())`)
    and 26. `holder_count` and `locally_declined` stay in the struct until
    Task 15 (callers still compile).
  - `upsert_from_manifest`: the INSERT sets
    `local_state = CASE WHEN ?4 = 'own' THEN 'own_held' WHEN ?9 = 'published' AND ?10 = 1 THEN 'wanted' ELSE 'idle' END`
    for a new row. On conflict, keep today's `content_version > content_version`
    rule and extend it: `local_state = CASE WHEN origin = 'replica' AND
    excluded.content_version > content_version AND local_state = 'held' THEN
    'wanted' ELSE local_state END` and `on_disk` follows the same rule (it
    already does). Task 9 replaces this interim rule with the full edge set.
  - New `set_local_state` — the ONLY writer of `on_disk` from here on:

```rust
/// Move a frame to `to`, keeping `on_disk` equal to "servable" and appending
/// the claim change to the outbox when servability flips (plan P8, C24).
/// Returns the transition, or `None` when the row does not exist. The caller
/// supplies the transaction; nothing is committed here.
pub fn set_local_state(conn: &Connection, project_id: &str, frame_uuid: &str, to: LocalState) -> Result<Option<StateWrite>> {
    let Some((from_raw, cv)): Option<(Option<String>, i32)> = conn
        .query_row(
            "SELECT local_state, content_version FROM project_frames_local WHERE project_id = ?1 AND frame_uuid = ?2",
            params![project_id, frame_uuid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let from = LocalState::from_db_str(from_raw.as_deref().unwrap_or("wanted"));
    conn.execute(
        "UPDATE project_frames_local SET local_state = ?3, on_disk = ?4,
            state_changed_at = datetime('now'), updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2",
        params![project_id, frame_uuid, to.as_db_str(), to.servable()],
    )?;
    let claim = match (from.servable(), to.servable()) {
        (false, true) => Some(crate::db::collab_live::ClaimOp::Add { content_version: cv }),
        (true, false) => Some(crate::db::collab_live::ClaimOp::Remove),
        _ => None,
    };
    if let Some(op) = claim {
        crate::db::collab_live::record_claim_change(conn, project_id, frame_uuid, op)?;
    }
    Ok(Some(StateWrite { from, to, claim }))
}
```

  - Interim consistency until Tasks 9/11/15 replace the wave-2 writers:
    `set_landed` and `set_landed_if` also write `local_state = CASE WHEN
    origin = 'own' THEN 'own_held' ELSE 'held' END`; `set_missing` writes
    `local_state = CASE WHEN origin = 'own' THEN 'own_missing' ELSE 'missing' END`;
    `set_declined(.., true)` writes `'not_kept'` and `(.., false)` writes
    `'wanted'`; `record_own` writes `'own_held'` when its `on_disk` is set,
    else `'own_missing'`. These interim writers do NOT append outbox rows
    (the wave-2 full report still runs until Task 15).
  - `set_frame_seq` = one column-targeted UPDATE.

- [ ] **Step 7: `db/collab.rs`** — `SELECT_COLS` appends `, feed_epoch, holder_seq`;
  the row struct and mapper gain the two fields; `upsert_project` does NOT
  write them (add them to the "never writes" doc list); `mark_lost` also sets
  `feed_epoch = NULL, holder_seq = -1`. Add:

```rust
/// Advance the feed's version cursor (plan P5: `hub_version` is that cursor).
pub fn set_feed_version(conn: &Connection, project_id: &str, epoch: &str, version: i64) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE collab_projects SET feed_epoch = ?2, hub_version = ?3 WHERE project_id = ?1",
        params![project_id, epoch, version],
    )?)
}

/// Advance the holder cursor; `-1` means "no local holder map".
pub fn set_holder_seq(conn: &Connection, project_id: &str, epoch: &str, holder_seq: i64) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE collab_projects SET feed_epoch = ?2, holder_seq = ?3 WHERE project_id = ?1",
        params![project_id, epoch, holder_seq],
    )?)
}
```

  Extend `upsert_project_preserves_local_columns` (db/collab.rs:589) to set
  both new columns first and assert an upsert keeps them.

- [ ] **Step 8: Run the tests**

Run: `cargo test -p athenaeum-core --lib db:: && cargo check --workspace --all-targets && cargo check -p athenaeum-core --no-default-features`
Expected: PASS, including `every_foreign_key_child_column_is_indexed`,
`rename_pending_announcements_is_idempotent` and the new tests.

- [ ] **Step 9: Commit**

```bash
rustfmt crates/athenaeum-core/src/db/collab_live.rs crates/athenaeum-core/src/db/collab_frames.rs crates/athenaeum-core/src/db/collab.rs crates/athenaeum-core/src/db/schema.rs crates/athenaeum-core/src/db/mod.rs
git add crates/athenaeum-core/Cargo.toml Cargo.lock crates/athenaeum-core/src/db/
git commit -m "feat(collab): wave-3 local state — per-frame local_state, claims/outbox/holder-map/deletions/quarantine tables, feed cursors"
```

---
### Task 2: Hub client v3 — typed errors, retry with full-jitter back-off, the claim digest (P3, P4, § Hub contract)

**Implementer:** rust-engineer.

**Files:**
- Create: `crates/athenaeum-core/src/collab/live/mod.rs`, `collab/live/backoff.rs`, `collab/live/digest.rs`, `collab/live/wire.rs` (REST shapes now; Task 4 appends the event types). Register `pub mod live;` in `collab/mod.rs`.
- Modify: `crates/athenaeum-core/src/account/client.rs` (enum :26-57, `Display` :82-102).
- Modify: `crates/athenaeum-core/src/collab/hub_client.rs` — `classify` :275-292, `network_status` :296-305, `FrameViewWire` :137-167 (`frameSeq` in, `holderCount` out, `own` defaulted), `new_frame_version` :487; new methods; deprecate `project_versions` :412, `put_holders` :591, `frame_holders` :616.
- Modify (match arms / text matchers only): `api/collab.rs:874-893` (`client_err`), `:2151-2171` (`is_stale_gate_refusal`, `already_announced_in`), `:3423-3435` (`decide_err`), `:3091` (the `new_frame_version` call); `api/collab_exchange.rs:33-56` (`client_err`), `:2511-2514`; `api/account.rs:158-181` (`map_client_err`); `db/collab_frames.rs` (`upsert_from_manifest` stops writing `holder_count`); `collab/fake_hub.rs` (drop `holder_count` from the views it builds).
- Test: `#[cfg(test)]` modules of `collab/live/backoff.rs`, `collab/live/digest.rs`, `collab/hub_client.rs` (wiremock).

**Interfaces:**
- Consumes: nothing new.
- Produces:
  ```rust
  // account/client.rs — new variants
  pub enum AccountClientError { /* … */ Http { status: u16, message: String }, Gone(String), SessionGone, VersionConflict { content_version: i32 } }
  impl AccountClientError { pub fn hub_text(&self) -> Option<&str>; }      // Network(m) | Http{message} → Some
  // collab/live/backoff.rs
  pub const BACKOFF_BASE: Duration = Duration::from_secs(1);
  pub const BACKOFF_CAP: Duration = Duration::from_secs(60);
  pub const BACKOFF_FLOOR: Duration = Duration::from_millis(100);
  pub struct Backoff { /* attempt: u32, rng: SplitMix64 */ }
  impl Backoff { pub fn new() -> Self; pub fn with_seed(seed: u64) -> Self; pub fn attempt(&self) -> u32; pub fn reset(&mut self); pub fn next_delay(&mut self) -> Duration; }
  pub fn reset_all();                                   // Sync now (L10, P26)
  pub fn reset_signal() -> tokio::sync::watch::Receiver<u64>;
  pub async fn sleep_or_reset(delay: Duration, reset: &mut tokio::sync::watch::Receiver<u64>) -> bool; // true = a reset cut the sleep short
  // collab/live/digest.rs
  pub const ZERO_HEX: &str = "00000000000000000000000000000000";
  pub fn claim_key(frame_uuid: &uuid::Uuid, content_version: u32) -> [u8; 16];
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
  pub struct ClaimDigest { pub count: i64, pub xor: [u8; 16] }
  impl ClaimDigest { pub fn add(&mut self, u: &uuid::Uuid, cv: u32); pub fn remove(&mut self, u: &uuid::Uuid, cv: u32); pub fn hex(&self) -> String; pub fn of_claims<'a>(claims: impl IntoIterator<Item = (&'a str, i32)>) -> anyhow::Result<ClaimDigest>; }
  pub fn uuid_for_digest(s: &str) -> uuid::Uuid;       // parse, else a stable v5 UUID (test ids)
  // collab/live/wire.rs (all camelCase)
  pub struct SnapshotFrameWire { pub seq: i32, pub uuid: String, pub content_version: i32 }
  pub struct SnapshotDeviceWire { pub device: String, pub display_name: String, pub relay_url: Option<String>, pub claims: Vec<[i32; 3]> }
  pub struct HoldersSnapshotWire { pub epoch: String, pub holder_seq: i64, pub version: i64, pub frames: Vec<SnapshotFrameWire>, pub devices: Vec<SnapshotDeviceWire> }
  pub struct HolderDeltaWire { pub device: String, #[serde(default)] pub add: Vec<(i32, i32)>, #[serde(default)] pub rm: Vec<i32> }
  pub struct DeltaCursorWire { pub since: i64, pub after: String }
  pub struct HolderDeltaPageWire { pub epoch: String, pub holder_seq: i64, pub floor: i64, pub deltas: Vec<HolderDeltaWire>, pub has_more: bool, pub next: Option<DeltaCursorWire> }
  pub struct ClaimWire { pub uuid: String, pub content_version: i32 }                              // Serialize
  pub struct HoldersReportWire { pub report_seq: i64, pub full: bool, pub add: Vec<ClaimWire>, pub remove: Vec<String>, pub digest: String, pub count: i64 } // Serialize
  pub struct HoldersReportReplyWire { pub holder_seq: i64, pub digest_match: bool, pub next_flush_ms: u64, #[serde(default)] pub refused: Vec<String> }
  pub struct VersionInWire { pub uuid: String, pub expected_version: i32, pub blake3: String, pub byte_size: i64, pub xxh3: String } // Serialize
  #[serde(rename_all = "snake_case")] pub enum VersionStatus { Ok, Conflict, NotFound, Forbidden }
  pub struct VersionResultWire { pub uuid: String, pub status: VersionStatus, pub content_version: i32 }
  pub struct VersionsReplyWire { pub project_version: i64, pub results: Vec<VersionResultWire> }
  pub struct BeatWire { pub session_id: String, pub serving: std::collections::BTreeMap<String, bool>, pub relay_url: Option<String> } // Serialize
  pub fn expand_runs(runs: &[[i32; 3]]) -> impl Iterator<Item = (i32, i32)> + '_;                   // (frameSeq, contentVersion)
  // collab/hub_client.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum RetryPolicy { Background, Interactive }
  pub const INTERACTIVE_ATTEMPTS: u32 = 3;
  pub fn is_retryable(e: &AccountClientError) -> bool;
  pub async fn with_retry<T, F, Fut>(what: &'static str, policy: RetryPolicy, op: F) -> Result<T, AccountClientError>
      where F: FnMut() -> Fut, Fut: std::future::Future<Output = Result<T, AccountClientError>>;
  impl CollabClient {
      pub async fn holders_snapshot(&self, token: &str, project_id: &str) -> Result<HoldersSnapshotWire, AccountClientError>;
      pub async fn holders_since(&self, token: &str, project_id: &str, since: i64, after: Option<&str>, epoch: Option<&str>) -> Result<HolderDeltaPageWire, AccountClientError>; // 410 → Gone(error)
      pub async fn report_holders(&self, token: &str, project_id: &str, body: &HoldersReportWire) -> Result<HoldersReportReplyWire, AccountClientError>;
      pub async fn frame_versions(&self, token: &str, project_id: &str, versions: &[VersionInWire]) -> Result<VersionsReplyWire, AccountClientError>;
      pub async fn new_frame_version(&self, token: &str, project_id: &str, frame_uuid: &str, expected_version: i32, blake3: &str, byte_size: i64, xxh3: &str) -> Result<NewVersionWire, AccountClientError>; // 409 version_conflict → VersionConflict
      pub async fn presence_beat(&self, body: &BeatWire) -> Result<(), AccountClientError>;          // 409 session_gone → SessionGone; no bearer
      pub async fn presence_leave(&self, session_id: &str) -> Result<(), AccountClientError>;        // no bearer
      pub fn base_url(&self) -> &str;
  }
  // FrameViewWire: + #[serde(default)] pub frame_seq: i32; #[serde(default)] pub own: bool; holder_count removed
  ```

- [ ] **Step 1: Write the failing tests**

`collab/live/digest.rs` — the hub plan's vectors, verbatim:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn u(s: &str) -> Uuid { Uuid::parse_str(s).unwrap() }
    const A: &str = "00000000-0000-4000-8000-000000000001";
    const B: &str = "00000000-0000-4000-8000-000000000002";
    const C: &str = "00000000-0000-4000-8000-000000000003";

    fn hex16(b: &[u8; 16]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }

    #[test]
    fn input_bytes_and_blake3_match_the_hub_vector() {
        let mut input = Vec::new();
        input.extend_from_slice(u(A).as_bytes());
        input.extend_from_slice(&1u32.to_be_bytes());
        let hex: String = input.iter().map(|x| format!("{x:02x}")).collect();
        assert_eq!(hex, "0000000000004000800000000000000100000001");
        assert_eq!(blake3::hash(&input).to_hex().as_str(),
            "129cef69895583884285d1404f99e181afc3b63d273a5314902d2ec7bf24183f");
    }

    #[test]
    fn keys_match_the_hub_vectors() {
        assert_eq!(hex16(&claim_key(&u(A), 1)), "129cef69895583884285d1404f99e181");
        assert_eq!(hex16(&claim_key(&u(B), 1)), "c3ef44a7f50605a0dee0abca26ecf959");
        assert_eq!(hex16(&claim_key(&u(C), 2)), "40fc3b9d29d73f9101f6293d9c3b5cc4");
        assert_eq!(hex16(&claim_key(&u(C), 1)), "b8c7cf6630052129879bab6949f47d5b");
    }

    #[test]
    fn set_digests_match_the_hub_vectors() {
        let d = ClaimDigest::of_claims([(A, 1), (B, 1)]).unwrap();
        assert_eq!((d.count, d.hex().as_str()), (2, "d173abce7c5386289c657a8a697518d8"));
        let d = ClaimDigest::of_claims([(A, 1), (B, 1), (C, 2)]).unwrap();
        assert_eq!((d.count, d.hex().as_str()), (3, "918f90535584b9b99d9353b7f54e441c"));
        let d = ClaimDigest::of_claims([(A, 1), (B, 1), (C, 1)]).unwrap();
        assert_eq!((d.count, d.hex().as_str()), (3, "69b464a84c56a7011bfed1e320816583"));
        assert_eq!(ClaimDigest::default().hex(), ZERO_HEX);
    }

    #[test]
    fn remove_undoes_add_and_order_does_not_matter() {
        let mut d = ClaimDigest::default();
        d.add(&u(C), 2);
        d.add(&u(A), 1);
        d.add(&u(B), 1);
        d.remove(&u(C), 2);
        assert_eq!(d, ClaimDigest::of_claims([(B, 1), (A, 1)]).unwrap());
    }
}
```

`collab/live/backoff.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_stay_inside_the_full_jitter_envelope_and_cap_at_sixty_seconds() {
        let mut b = Backoff::with_seed(7);
        for attempt in 0..20u32 {
            let upper = BACKOFF_CAP.min(BACKOFF_BASE.saturating_mul(1u32 << attempt.min(16)));
            let d = b.next_delay();
            assert!(d >= BACKOFF_FLOOR && d <= upper.max(BACKOFF_FLOOR), "attempt {attempt}: {d:?} > {upper:?}");
        }
        assert!(b.attempt() >= 6);
        b.reset();
        assert_eq!(b.attempt(), 0);
        assert!(b.next_delay() <= BACKOFF_BASE);
    }

    #[test]
    fn same_seed_same_sequence() {
        let a: Vec<_> = { let mut b = Backoff::with_seed(42); (0..8).map(|_| b.next_delay()).collect() };
        let c: Vec<_> = { let mut b = Backoff::with_seed(42); (0..8).map(|_| b.next_delay()).collect() };
        assert_eq!(a, c);
    }

    #[tokio::test]
    async fn reset_all_cuts_a_sleep_short() {
        let mut rx = reset_signal();
        let sleeper = tokio::spawn(async move { sleep_or_reset(Duration::from_secs(30), &mut rx).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        reset_all();
        let cut = tokio::time::timeout(Duration::from_secs(2), sleeper).await.unwrap().unwrap();
        assert!(cut);
    }
}
```

`collab/hub_client.rs` tests (wiremock, extend the module at :656):

```rust
#[tokio::test]
async fn statuses_are_typed() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/v1/me/presence"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({"error":"session_gone"})))
        .mount(&server).await;
    Mock::given(method("POST")).and(path("/api/v1/projects/p1/frames/u1/version"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({"error":"version_conflict","contentVersion":3})))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/api/v1/projects/p1/holders"))
        .respond_with(ResponseTemplate::new(410).set_body_json(json!({"error":"holders_below_floor","floor":9,"holderSeq":20})))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/api/v1/projects/p1/holders/snapshot"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({"error":"db down"})))
        .mount(&server).await;
    let c = CollabClient::new(server.uri()).unwrap();
    let beat = BeatWire { session_id: "0".repeat(32), serving: Default::default(), relay_url: None };
    assert!(matches!(c.presence_beat(&beat).await, Err(AccountClientError::SessionGone)));
    assert!(matches!(c.new_frame_version("t", "p1", "u1", 1, &"b".repeat(64), 10, &"0".repeat(16)).await,
        Err(AccountClientError::VersionConflict { content_version: 3 })));
    assert!(matches!(c.holders_since("t", "p1", 3, None, None).await,
        Err(AccountClientError::Gone(ref e)) if e == "holders_below_floor"));
    let err = c.holders_snapshot("t", "p1").await.unwrap_err();
    assert!(matches!(err, AccountClientError::Http { status: 503, .. }));
    assert!(is_retryable(&err));
    assert!(err.to_string().contains("db down"), "{err}");
}

#[tokio::test]
async fn report_holders_sends_the_v3_body_and_decodes_the_reply() {
    let server = MockServer::start().await;
    Mock::given(method("PUT")).and(path("/api/v1/projects/p1/holders/self"))
        .and(body_json(json!({"reportSeq":120,"full":false,
            "add":[{"uuid":"u1","contentVersion":1}],"remove":["u2"],
            "digest":"d173abce7c5386289c657a8a697518d8","count":2})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "holderSeq":18,"digestMatch":true,"nextFlushMs":1000,"refused":["u9"]})))
        .expect(1).mount(&server).await;
    let c = CollabClient::new(server.uri()).unwrap();
    let body = HoldersReportWire {
        report_seq: 120, full: false,
        add: vec![ClaimWire { uuid: "u1".into(), content_version: 1 }],
        remove: vec!["u2".into()],
        digest: "d173abce7c5386289c657a8a697518d8".into(), count: 2,
    };
    let r = c.report_holders("t", "p1", &body).await.unwrap();
    assert_eq!((r.holder_seq, r.digest_match, r.next_flush_ms), (18, true, 1000));
    assert_eq!(r.refused, vec!["u9".to_string()]);
}

#[tokio::test]
async fn snapshot_runs_expand_and_versions_batch_decodes() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/v1/projects/p1/holders/snapshot"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "epoch":"e1","holderSeq":17,"version":42,
            "frames":[{"seq":1,"uuid":"u1","contentVersion":2}],
            "devices":[{"device":"AAA=","displayName":"Anna","relayUrl":null,
                        "claims":[[1,3,1],[5,1,2],[6,1,1]]}]})))
        .mount(&server).await;
    Mock::given(method("POST")).and(path("/api/v1/projects/p1/frames/versions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "projectVersion":43,"results":[{"uuid":"u1","status":"ok","contentVersion":2},
                                           {"uuid":"u2","status":"conflict","contentVersion":5}]})))
        .mount(&server).await;
    let c = CollabClient::new(server.uri()).unwrap();
    let s = c.holders_snapshot("t", "p1").await.unwrap();
    let claims: Vec<_> = expand_runs(&s.devices[0].claims).collect();
    assert_eq!(claims, vec![(1, 1), (2, 1), (3, 1), (5, 2), (6, 1)]);
    let v = c.frame_versions("t", "p1", &[]).await.unwrap();
    assert_eq!(v.results[1].status, VersionStatus::Conflict);
    assert_eq!(v.results[1].content_version, 5);
}

#[tokio::test]
async fn interactive_retry_gives_up_after_three_attempts_and_background_recovers() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/v1/projects/p1/holders/snapshot"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(4).mount(&server).await;
    Mock::given(method("GET")).and(path("/api/v1/projects/p1/holders/snapshot"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "epoch":"e","holderSeq":0,"version":1,"frames":[],"devices":[]})))
        .mount(&server).await;
    let c = CollabClient::new(server.uri()).unwrap();
    let e = with_retry("snapshot", RetryPolicy::Interactive, || c.holders_snapshot("t", "p1")).await;
    assert!(matches!(e, Err(AccountClientError::Http { status: 503, .. })));
    // one 503 left, then 200: the background policy retries through it
    let ok = tokio::time::timeout(std::time::Duration::from_secs(10),
        with_retry("snapshot", RetryPolicy::Background, || c.holders_snapshot("t", "p1"))).await.unwrap();
    assert!(ok.is_ok());
}

#[tokio::test]
async fn frame_view_reads_frame_seq_without_holder_count_or_own() {
    let v: FrameViewWire = serde_json::from_value(json!({
        "frameUuid":"u1","frameSeq":12,"publisherAccountId":"a","publisherDisplayName":"Ann",
        "fileName":"c_x.fits","contentVersion":1,"blake3":"b","byteSize":10,"xxh3":"x",
        "filterRaw":"Red","filterCanonical":"R","channel":"mono","exptimeSec":300.0,
        "dateObs":null,"meta":{},"gateVersion":0,"accepted":true,"acceptedReason":null,
        "state":"published","rejectReason":null,"manifestVersion":9,"createdAt":"2026-09-25T00:00:00Z"})).unwrap();
    assert_eq!((v.frame_seq, v.own), (12, false));
}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib collab::live collab::hub_client`. Expected: compile errors (modules and methods missing).

- [ ] **Step 3: `collab/live/backoff.rs`**

```rust
//! One full-jitter exponential back-off for the whole live exchange (spec
//! §4.1 reconnect, §4.6 per-request retry, §7.3 provider dials; plan P3).
//! `reset_all` is Sync now's "clears every back-off" (L10).

use std::sync::OnceLock;
use std::time::Duration;

use crate::geometry::ransac::SplitMix64;

pub const BACKOFF_BASE: Duration = Duration::from_secs(1);
pub const BACKOFF_CAP: Duration = Duration::from_secs(60);
/// Full jitter can draw ~0; the floor keeps a failing loop from spinning.
pub const BACKOFF_FLOOR: Duration = Duration::from_millis(100);

pub struct Backoff {
    attempt: u32,
    rng: SplitMix64,
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

impl Backoff {
    pub fn new() -> Self {
        let seed = uuid::Uuid::new_v4().as_u128() as u64;
        Self::with_seed(seed)
    }

    pub fn with_seed(seed: u64) -> Self {
        Self { attempt: 0, rng: SplitMix64(seed) }
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// Uniform in `[0, min(cap, base · 2^attempt)]`, floored; advances the attempt.
    pub fn next_delay(&mut self) -> Duration {
        let exp = 1u32 << self.attempt.min(16);
        let upper = BACKOFF_CAP.min(BACKOFF_BASE.saturating_mul(exp));
        self.attempt = self.attempt.saturating_add(1);
        let drawn = upper.mul_f64(self.rng.next_f64());
        drawn.max(BACKOFF_FLOOR)
    }
}

fn reset_tx() -> &'static tokio::sync::watch::Sender<u64> {
    static TX: OnceLock<tokio::sync::watch::Sender<u64>> = OnceLock::new();
    TX.get_or_init(|| tokio::sync::watch::channel(0u64).0)
}

/// Wake every sleeping retry and make it start over (Sync now, P26).
pub fn reset_all() {
    reset_tx().send_modify(|n| *n = n.wrapping_add(1));
    tracing::info!("collab back-offs cleared");
}

pub fn reset_signal() -> tokio::sync::watch::Receiver<u64> {
    let mut rx = reset_tx().subscribe();
    rx.mark_unchanged();
    rx
}

/// Sleep `delay`, or less if [`reset_all`] fires. `true` = cut short.
pub async fn sleep_or_reset(delay: Duration, reset: &mut tokio::sync::watch::Receiver<u64>) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(delay) => false,
        changed = reset.changed() => changed.is_ok(),
    }
}
```

- [ ] **Step 4: `collab/live/digest.rs`**

```rust
//! The order-independent claim digest (spec §6.3; hub plan § Wire contract
//! "The claim digest"). Both sides compute it from the device's report
//! stream only, so a version bump never changes it.

use uuid::Uuid;

pub const ZERO_HEX: &str = "00000000000000000000000000000000";

/// First 16 bytes of `blake3(uuid_bytes ‖ content_version as u32 BE)`.
pub fn claim_key(frame_uuid: &Uuid, content_version: u32) -> [u8; 16] {
    let mut input = [0u8; 20];
    input[..16].copy_from_slice(frame_uuid.as_bytes());
    input[16..].copy_from_slice(&content_version.to_be_bytes());
    let h = blake3::hash(&input);
    let mut out = [0u8; 16];
    out.copy_from_slice(&h.as_bytes()[..16]);
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClaimDigest {
    pub count: i64,
    pub xor: [u8; 16],
}

impl ClaimDigest {
    fn fold(&mut self, u: &Uuid, cv: u32) {
        for (a, b) in self.xor.iter_mut().zip(claim_key(u, cv)) {
            *a ^= b;
        }
    }

    pub fn add(&mut self, u: &Uuid, cv: u32) {
        self.fold(u, cv);
        self.count += 1;
    }

    pub fn remove(&mut self, u: &Uuid, cv: u32) {
        self.fold(u, cv);
        self.count -= 1;
    }

    pub fn hex(&self) -> String {
        self.xor.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The digest of a whole claim set (`(frame_uuid, content_version)`).
    pub fn of_claims<'a>(claims: impl IntoIterator<Item = (&'a str, i32)>) -> anyhow::Result<ClaimDigest> {
        let mut d = ClaimDigest::default();
        for (uuid, cv) in claims {
            let cv = u32::try_from(cv).map_err(|_| anyhow::anyhow!("claim {uuid}: content version {cv} < 0"))?;
            d.add(&uuid_for_digest(uuid), cv);
        }
        Ok(d)
    }
}

/// The UUID a frame id contributes to the digest. Hub frame ids always parse;
/// a non-UUID id (only test fixtures such as `"u1"`) maps to a stable v5 UUID
/// so the app and the fake hub agree.
pub fn uuid_for_digest(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap_or_else(|_| Uuid::new_v5(&Uuid::NAMESPACE_OID, s.as_bytes()))
}
```

- [ ] **Step 5: `collab/live/wire.rs`** — the REST shapes of the Interfaces block, each
  `#[derive(Debug, Clone, PartialEq, serde::Deserialize)]` (replies) or
  `serde::Serialize` (requests) with `#[serde(rename_all = "camelCase")]`,
  plus:

```rust
/// Expand snapshot runs `[startSeq, runLength, contentVersion]` into
/// `(frameSeq, contentVersion)` pairs (hub § Wire contract "Run-length claims").
pub fn expand_runs(runs: &[[i32; 3]]) -> impl Iterator<Item = (i32, i32)> + '_ {
    runs.iter()
        .flat_map(|[start, len, cv]| (0..(*len).max(0)).map(move |i| (start + i, *cv)))
}
```

  `collab/live/mod.rs` declares `pub mod backoff; pub mod digest; pub mod wire;`
  (later tasks append their modules).

- [ ] **Step 6: Typed errors.** In `account/client.rs` add the four variants.
  `Display`: `Http { message, .. }` → `message` (the full wave-2 text
  `hub returned {status} ({what}): {msg}`, so log lines keep their shape);
  `Gone(e)` → `"hub answered 410: {e}"`; `SessionGone` → `"session_gone"`;
  `VersionConflict { content_version }` → `"version_conflict (hub has content version {content_version})"`.
  Add:

```rust
impl AccountClientError {
    /// The hub's status text for the text matchers that recognise hub
    /// refusals (stale gate, already announced, already decided).
    pub fn hub_text(&self) -> Option<&str> {
        match self {
            AccountClientError::Network(m) => Some(m),
            AccountClientError::Http { message, .. } => Some(message),
            _ => None,
        }
    }
}
```

  Rewrite `classify` (HC:275):

```rust
async fn classify(status: StatusCode, resp: reqwest::Response, what: &str) -> AccountClientError {
    match status {
        StatusCode::UNAUTHORIZED => AccountClientError::Unauthorized,
        StatusCode::FORBIDDEN => AccountClientError::Forbidden,
        StatusCode::TOO_MANY_REQUESTS => AccountClientError::RateLimited,
        StatusCode::CONFLICT | StatusCode::GONE => {
            let text = resp.text().await.unwrap_or_default();
            let json: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
            let error = json.get("error").and_then(|v| v.as_str()).unwrap_or("").to_string();
            match (status, error.as_str()) {
                (_, "collab_api_outdated") => AccountClientError::CollabApiOutdated,
                (_, "session_gone") => AccountClientError::SessionGone,
                (_, "version_conflict") => AccountClientError::VersionConflict {
                    content_version: json.get("contentVersion").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                },
                (StatusCode::GONE, _) => AccountClientError::Gone(error),
                _ => http_status(status, what, if error.is_empty() { text.trim() } else { &error }),
            }
        }
        s => {
            let msg = body_message(resp).await;
            http_status(s, what, &msg)
        }
    }
}

/// `Http` with the wave-2 message text (was `network_status`).
fn http_status(status: StatusCode, what: &str, msg: &str) -> AccountClientError {
    let message = if msg.is_empty() {
        format!("hub returned {status} ({what})")
    } else {
        format!("hub returned {status} ({what}): {msg}")
    };
    AccountClientError::Http { status: status.as_u16(), message }
}
```

  Replace every `network_status(` call with `http_status(`. Transport errors
  (`net`, HC:264) stay `Network`.

- [ ] **Step 7: Retry.**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryPolicy {
    /// Owned by the live session: retries until it succeeds or the future is dropped.
    Background,
    /// A user action: at most [`INTERACTIVE_ATTEMPTS`] attempts.
    Interactive,
}

pub const INTERACTIVE_ATTEMPTS: u32 = 3;

/// Transport errors, 5xx and 429 (spec §4.6). 401/403/409/410 never retry.
pub fn is_retryable(e: &AccountClientError) -> bool {
    match e {
        AccountClientError::Network(_) | AccountClientError::RateLimited => true,
        AccountClientError::Http { status, .. } => *status >= 500,
        _ => false,
    }
}

pub async fn with_retry<T, F, Fut>(what: &'static str, policy: RetryPolicy, mut op: F) -> Result<T, AccountClientError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, AccountClientError>>,
{
    use crate::collab::live::backoff::{reset_signal, sleep_or_reset, Backoff};
    let mut backoff = Backoff::new();
    let mut reset = reset_signal();
    loop {
        match op().await {
            Ok(v) => {
                if backoff.attempt() > 0 {
                    tracing::info!(command = what, attempt = backoff.attempt(), "hub request recovered");
                }
                return Ok(v);
            }
            Err(e) if is_retryable(&e) => {
                if policy == RetryPolicy::Interactive && backoff.attempt() + 1 >= INTERACTIVE_ATTEMPTS {
                    tracing::warn!(command = what, attempt = backoff.attempt() + 1, error = %e, "hub request failed; giving up");
                    return Err(e);
                }
                let delay = backoff.next_delay();
                if backoff.attempt() == 1 {
                    tracing::warn!(command = what, error = %e, retry_in_ms = delay.as_millis() as u64, "hub request failed; retrying");
                } else {
                    tracing::debug!(command = what, attempt = backoff.attempt(), error = %e, retry_in_ms = delay.as_millis() as u64, "hub request failed again; retrying");
                }
                if sleep_or_reset(delay, &mut reset).await {
                    backoff.reset();
                }
            }
            Err(e) => return Err(e),
        }
    }
}
```

- [ ] **Step 8: New methods and `FrameViewWire`.** Implement the methods of the
  Interfaces block on the existing `self.http`/`self.url()`/`bearer_auth`
  idiom: `holders_snapshot` → `get_json("/projects/{id}/holders/snapshot")`;
  `holders_since` builds `.query(&[("since", since.to_string())])` plus
  `after`/`epoch` when `Some`; `report_holders` is a `PUT` with the JSON body
  and a 200 JSON reply; `frame_versions` a `POST` of
  `{"versions": versions}`; `new_frame_version` sends
  `{"expectedVersion", "blake3", "byteSize", "xxh3"}`; `presence_beat` /
  `presence_leave` are `POST`/`DELETE` on `/me/presence` **without** a bearer
  (session-authenticated), success = 204. Every non-success goes through
  `classify`. `FrameViewWire`: add `#[serde(default)] pub frame_seq: i32`,
  make `own` `#[serde(default)]`, delete `holder_count` (and its
  `#[serde(default)]`). Mark `project_versions`, `put_holders`,
  `frame_holders` `#[deprecated(note = "collab v3 wave 3: removed in Task 15")]`
  and add `#[allow(deprecated)]` at their three call sites in
  `api/collab_exchange.rs` (411, 2052, 2342, 2593) and the test at 6728.

- [ ] **Step 9: Callers.**
  - `upsert_from_manifest` drops the `holder_count` column and parameter from
    its INSERT/UPDATE (the column keeps its value; Task 15 drops the struct
    field). Fix the `view()` helper in `db/collab_frames.rs` tests and every
    `FrameViewWire { … }` literal the compiler lists (`rg -n "holder_count:" crates/athenaeum-core/src`).
  - `collab/fake_hub.rs`: stop setting `holder_count` in the views it builds.
  - `is_stale_gate_refusal`, `already_announced_in`, `decide_err`
    (api/collab.rs:2151-2171, 3423-3435) and `holder_lookup_is_frame_level`
    (api/collab_exchange.rs:2511-2514) match on `e.hub_text()` instead of the
    `Network(m)` pattern — same substrings.
  - The three `client_err`/`map_client_err` helpers gain arms:
    `Http { message, .. }` and `Gone(message)` → `ApiError::Internal(format!("Hub request failed: {message}"))`
    (the `Network` arm's text); `SessionGone` → `ApiError::Internal("hub session expired".into())`;
    `VersionConflict { content_version }` →
    `ApiError::Conflict(format!("version_conflict: the hub has content version {content_version}"))`.
  - `api/collab.rs:3091`: pass `f.content_version - 1` as `expected_version`
    (the version this update supersedes; Task 6 moves the call to the batch
    route).

- [ ] **Step 10: Run** `cargo test -p athenaeum-core --lib collab:: db::collab_frames api::collab && cargo check --workspace --all-targets && cargo check -p athenaeum-core --no-default-features`. Expected: PASS (the wave-2 hub-client tests still pass: `other_409_keeps_the_hub_message` reads the message through `Display`).

- [ ] **Step 11: Commit**

```bash
rustfmt crates/athenaeum-core/src/collab/live/*.rs crates/athenaeum-core/src/collab/hub_client.rs crates/athenaeum-core/src/account/client.rs
git add -A crates/athenaeum-core/src
git commit -m "feat(collab): hub client for live exchange — typed errors, full-jitter retry, holder snapshot/delta/report, CAS versions, presence; the claim digest with the hub's vectors"
```

---
### Task 3: Fake hub v3 with an event-stream front (P32, § Hub contract)

**Implementer:** rust-engineer.

**Files:**
- Modify: `crates/athenaeum-core/src/collab/fake_hub.rs` (state :92-144, `start` :216-234, `uri` :236, router `route` :587-641, `new_version` :1083-1137, `put_holders` :1275-1351, `frame_holders` :1353-1380, `holders_of` :508-522, announce :954-1081).
- Modify: `crates/athenaeum-core/Cargo.toml` `[dev-dependencies]`: `tokio = { version = "1", features = ["net"] }` (the axum front binds a `TcpListener`; core's own tokio has no `net`).
- Test: a new `#[cfg(test)] mod tests` at the bottom of `collab/fake_hub.rs` (the file has none today).

**Interfaces:**
- Consumes: `collab::live::wire::*` and `collab::live::digest::ClaimDigest` (Task 2).
- Produces (test-only API; every later task's integration tests use it):
  ```rust
  pub struct FakeTimings { pub keepalive: Duration, pub grace: Duration, pub silence: Duration }   // default 20 s / 10 s / 40 s; tests shorten
  pub struct FakeClaim { pub content_version: i32, pub report_seq: i64, pub removed: bool, pub changed_seq: i64 }
  // FakeProject gains: pub holder_seq: i64, pub next_frame_seq: i32, pub claims: BTreeMap<(String /*device*/, String /*uuid*/), FakeClaim>, pub holder_floor: i64
  // FakeProject loses: holders (the 75-minute freshness map)
  // FakeHubState gains: pub epoch: String, pub timings: FakeTimings, pub sessions: HashMap<String, FakeSession>, pub feed: tokio::sync::broadcast::Sender<FeedMsg>, pub holder_writes: u64, pub dropped_events: HashMap<String, usize>
  pub struct FakeSession { pub device: String, pub account_id: String, pub serving: BTreeMap<String, bool>, pub relay_url: Option<String>, pub last_beat: Instant, pub detached_at: Option<Instant>, pub kill: tokio::sync::watch::Sender<bool> }
  #[derive(Clone, Debug)] pub struct FeedMsg { pub project_id: Option<String>, pub account_id: Option<String>, pub name: &'static str, pub data: String }
  impl FakeHub {
      pub fn uri(&self) -> String;                                  // the axum FRONT (events + presence local, rest proxied)
      pub fn set_timings(&self, t: FakeTimings);
      pub fn holders_of(&self, project_id: &str, uuid: &str) -> Vec<String>;      // devices with a live claim on the CURRENT content version
      pub fn claim_of(&self, project_id: &str, device: &str, uuid: &str) -> Option<FakeClaim>;
      pub fn holder_writes(&self) -> u64;                             // claim rows whose (cv, removed) changed
      pub fn connected(&self, project_id: &str) -> Vec<String>;       // devices currently connected in that project
      pub fn serving(&self, project_id: &str, device: &str) -> bool;
      pub fn drop_next_events(&self, project_id: &str, n: usize);     // a gap: the next n events of that project are not delivered
      pub fn send_resync(&self, project_id: &str, what: &str);        // "project" | "holders"
      pub fn send_versions(&self);                                    // the 60 s state vector, on demand
      pub fn kill_streams(&self);                                     // hub restart: every stream closes, sessions and presence cleared
      pub fn rotate_epoch(&self) -> String;                           // a restore: new epoch
      pub fn forget_frames(&self, project_id: &str, uuids: &[&str]);  // a restore that lost these rows (and their claims)
      pub fn revoke_device(&self, device_pubkey_b64: &str, retire: bool);  // I11: tombstone claims, bump members everywhere, close its stream
      pub fn set_api_outdated(&self, on: bool);                       // every v3 route answers 409 collab_api_outdated
  }
  ```

The fake implements the § Hub contract exactly, with these simplifications
(each documented in the file's module doc): no coalescing windows (one event
per commit, contiguous `prev`), no flap damping, no warm-up, the 60 s
`versions` vector only on `send_versions()`.

- [ ] **Step 1: Write the failing tests** (bottom of `collab/fake_hub.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::hub_client::CollabClient;
    use crate::collab::live::wire::{ClaimWire, HoldersReportWire};
    use crate::collab::live::digest::ClaimDigest;

    async fn hub_with_member() -> FakeHub {
        let hub = FakeHub::start().await;
        hub.add_account("tok", "acc-me", "Me", "AAA=", None);
        hub.add_account("tok-o", "acc-o", "Other", "BBB=", None);
        hub.add_project("p1", "m31", &[("acc-me", "send_receive", false), ("acc-o", "send", false)], false);
        hub.seed_frames("p1", "acc-o", &["u1", "u2"], "published");
        hub
    }

    /// Reads SSE frames from the front with reqwest.
    async fn open(hub: &FakeHub, token: &str) -> reqwest::Response {
        reqwest::Client::new()
            .get(format!("{}/api/v1/me/events", hub.uri()))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
    }

    async fn next_event(resp: &mut reqwest::Response, buf: &mut String) -> (String, serde_json::Value) {
        loop {
            if let Some(end) = buf.find("\n\n") {
                let block: String = buf.drain(..end + 2).collect();
                let name = block.lines().find_map(|l| l.strip_prefix("event: ")).map(str::to_string);
                let data: String = block.lines().filter_map(|l| l.strip_prefix("data: ")).collect();
                if let Some(n) = name {
                    return (n, serde_json::from_str(&data).unwrap());
                }
                continue;
            }
            let chunk = tokio::time::timeout(Duration::from_secs(5), resp.chunk()).await.unwrap().unwrap().unwrap();
            buf.push_str(&String::from_utf8_lossy(&chunk));
        }
    }

    #[tokio::test]
    async fn hello_carries_cursors_digest_and_presence() {
        let hub = hub_with_member().await;
        let mut resp = open(&hub, "tok").await;
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers()["content-type"], "text/event-stream");
        let mut buf = String::new();
        let (name, hello) = next_event(&mut resp, &mut buf).await;
        assert_eq!(name, "hello");
        assert_eq!(hello["accountId"], "acc-me");
        let p = &hello["projects"]["p1"];
        assert_eq!(p["claimDigest"], crate::collab::live::digest::ZERO_HEX);
        assert_eq!(p["holderSeq"], 1); // the seeding publisher's implicit claims
        assert!(p["presence"].as_array().unwrap().iter().any(|e| e["device"] == "AAA="));
    }

    #[tokio::test]
    async fn a_report_advances_the_holder_cursor_and_streams_a_contiguous_delta() {
        let hub = hub_with_member().await;
        let mut resp = open(&hub, "tok").await;
        let mut buf = String::new();
        let (_, hello) = next_event(&mut resp, &mut buf).await;
        let seq0 = hello["projects"]["p1"]["holderSeq"].as_i64().unwrap();
        let c = CollabClient::new(hub.uri()).unwrap();
        let digest = ClaimDigest::of_claims([("u1", 1)]).unwrap();
        let reply = c.report_holders("tok", "p1", &HoldersReportWire {
            report_seq: 1, full: false, add: vec![ClaimWire { uuid: "u1".into(), content_version: 1 }],
            remove: vec![], digest: digest.hex(), count: 1,
        }).await.unwrap();
        assert!(reply.digest_match);
        assert_eq!(reply.holder_seq, seq0 + 1);
        let (name, ev) = loop {
            let e = next_event(&mut resp, &mut buf).await;
            if e.0 == "holders" { break e; }
        };
        assert_eq!(name, "holders");
        assert_eq!((ev["prev"].as_i64().unwrap(), ev["seq"].as_i64().unwrap()), (seq0, seq0 + 1));
        assert_eq!(ev["deltas"][0]["device"], "AAA=");
        assert_eq!(hub.holders_of("p1", "u1").len(), 2); // publisher + me
    }

    #[tokio::test]
    async fn an_older_report_seq_never_overrides_a_newer_one() {
        let hub = hub_with_member().await;
        let c = CollabClient::new(hub.uri()).unwrap();
        let body = |seq: i64, add: bool| HoldersReportWire {
            report_seq: seq, full: false,
            add: if add { vec![ClaimWire { uuid: "u1".into(), content_version: 1 }] } else { vec![] },
            remove: if add { vec![] } else { vec!["u1".into()] },
            digest: crate::collab::live::digest::ZERO_HEX.into(), count: 0,
        };
        c.report_holders("tok", "p1", &body(5, false)).await.unwrap();
        c.report_holders("tok", "p1", &body(4, true)).await.unwrap(); // delayed older add
        assert!(hub.claim_of("p1", "AAA=", "u1").map_or(true, |c| c.removed));
    }

    #[tokio::test]
    async fn refused_claims_and_retired_routes() {
        let hub = hub_with_member().await;
        let c = CollabClient::new(hub.uri()).unwrap();
        let reply = c.report_holders("tok", "p1", &HoldersReportWire {
            report_seq: 1, full: false,
            add: vec![ClaimWire { uuid: "nope".into(), content_version: 1 },
                      ClaimWire { uuid: "u2".into(), content_version: 9 }],
            remove: vec![], digest: crate::collab::live::digest::ZERO_HEX.into(), count: 0,
        }).await.unwrap();
        assert_eq!(reply.refused.len(), 2);
        assert!(reply.digest_match); // refused keys are removed from our digest before comparing
        let r = reqwest::Client::new().get(format!("{}/api/v1/me/project-versions", hub.uri()))
            .bearer_auth("tok").send().await.unwrap();
        assert_eq!(r.status(), 409);
    }

    #[tokio::test]
    async fn versions_compare_and_set_and_revocation_tombstones() {
        let hub = hub_with_member().await;
        let c = CollabClient::new(hub.uri()).unwrap();
        let conflict = c.new_frame_version("tok-o", "p1", "u1", 7, &"c".repeat(64), 10, &"0".repeat(16)).await;
        assert!(matches!(conflict, Err(crate::account::AccountClientError::VersionConflict { content_version: 1 })));
        let ok = c.new_frame_version("tok-o", "p1", "u1", 1, &"c".repeat(64), 10, &"0".repeat(16)).await.unwrap();
        assert_eq!(ok.content_version, 2);
        assert_eq!(hub.holders_of("p1", "u1"), vec!["BBB=".to_string()]); // implicit claim at v2
        hub.revoke_device("BBB=", true);
        assert!(hub.holders_of("p1", "u1").is_empty());
    }

    #[tokio::test]
    async fn snapshot_and_since_page_current_rows() {
        let hub = hub_with_member().await;
        let c = CollabClient::new(hub.uri()).unwrap();
        let snap = c.holders_snapshot("tok", "p1").await.unwrap();
        assert_eq!(snap.frames.len(), 2);
        let publisher = snap.devices.iter().find(|d| d.device == "BBB=").unwrap();
        assert_eq!(publisher.claims, vec![[1, 2, 1]]);
        let page = c.holders_since("tok", "p1", 0, None, None).await.unwrap();
        assert_eq!(page.holder_seq, snap.holder_seq);
        let gone = c.holders_since("tok", "p1", 99, None, None).await;
        assert!(matches!(gone, Err(crate::account::AccountClientError::Gone(ref e)) if e == "holders_cursor_ahead"));
    }
}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib collab::fake_hub`. Expected: compile errors.

- [ ] **Step 3: State and claims model.** Replace `FakeProject.holders` with
  `claims`, `holder_seq`, `holder_floor`, `next_frame_seq`, add the
  `FakeHubState` fields of the Interfaces block (the broadcast channel with
  capacity 1024; `epoch` starts as `"epoch-1"`), and the helpers:

```rust
impl FakeProject {
    /// Write one claim under the report_seq rule; returns true when the
    /// claim's visible state (content_version, removed) changed.
    fn write_claim(&mut self, device: &str, uuid: &str, cv: i32, removed: bool, report_seq: i64) -> bool {
        let key = (device.to_string(), uuid.to_string());
        if let Some(c) = self.claims.get(&key) {
            if c.report_seq > report_seq || (c.report_seq == report_seq && report_seq != 0) {
                return false;
            }
        }
        let changed = self
            .claims
            .get(&key)
            .map_or(!removed, |c| c.content_version != cv || c.removed != removed);
        let changed_seq = if changed { self.holder_seq + 1 } else { self.claims.get(&key).map_or(0, |c| c.changed_seq) };
        self.claims.insert(key, FakeClaim { content_version: cv, report_seq, removed, changed_seq });
        changed
    }

    fn digest_of(&self, device: &str) -> ClaimDigest {
        let mut d = ClaimDigest::default();
        for ((dev, uuid), c) in &self.claims {
            if dev == device && !c.removed {
                d.add(&crate::collab::live::digest::uuid_for_digest(uuid), c.content_version as u32);
            }
        }
        d
    }
}
```

  `seed_frames` and `announce` assign `frame_seq = next_frame_seq++`, set
  `FrameViewWire.frame_seq`, write the publisher device's implicit claim
  `(uuid, 1)` with `report_seq = 0`, bump `holder_seq` once per commit, and
  publish one `holders` event and one `project` event.

- [ ] **Step 4: Publishing events.** One helper used by every mutating route:

```rust
impl FakeHubState {
    fn publish(&mut self, project_id: &str, name: &'static str, data: Value) {
        if let Some(n) = self.dropped_events.get_mut(project_id) {
            if *n > 0 {
                *n -= 1;
                return; // a gap the client must detect by prev != cursor
            }
        }
        let _ = self.feed.send(FeedMsg {
            project_id: Some(project_id.to_string()),
            account_id: None,
            name,
            data: data.to_string(),
        });
    }

    /// `project` event for one bump; frames inlined iff ≤ 50 and all published.
    fn publish_bump(&mut self, pid: &str, prev: i64, kinds: &[&str], frames: &[String]) {
        let p = &self.projects[pid];
        let rows: Vec<&FrameViewWire> = frames.iter().filter_map(|u| p.frames.get(u)).collect();
        let inline = !rows.is_empty() && rows.len() <= 50 && rows.iter().all(|f| f.state == "published");
        let mut ev = json!({ "projectId": pid, "prev": prev, "version": p.version, "kinds": kinds, "more": !frames.is_empty() && !inline });
        if inline {
            ev["frames"] = Value::Array(rows.iter().map(|f| { let mut v = serde_json::to_value(f).unwrap(); v.as_object_mut().unwrap().remove("own"); v }).collect());
        }
        self.publish(pid, "project", ev);
    }

    fn publish_holders(&mut self, pid: &str, prev: i64, deltas: Value) {
        let seq = self.projects[pid].holder_seq;
        self.publish(pid, "holders", json!({ "projectId": pid, "prev": prev, "seq": seq, "deltas": deltas }));
    }
}
```

  Every existing mutating route (`announce`, `approve`, `reject`,
  `add_member`, `remove_member`, `set_caps`, `set_accepted`,
  `set_thresholds_version`, `set_dictionary`, `update_frame`, `bump`) calls
  `publish_bump` with the right kinds (`frames` / `members` / `thresholds` /
  `dictionary` / `meta`). `add_member`/`remove_member` also send an
  `account` event (`account_id: Some(..)`, `project_id: None`) to the
  affected account.

- [ ] **Step 5: v3 REST routes** in `route()` (replacing the three retired ones):
  - `("GET", ["me","project-versions"])` and
    `("GET", ["projects",_,"frames",_,"holders"])` → `error(409, "collab_api_outdated")`.
  - `("GET", ["projects",pid,"holders","snapshot"])` → the contract shape;
    `frames` visibility-filtered with the existing `visible()`; `devices` =
    every device of every current member, claims run-length encoded
    (consecutive `frame_seq` with equal `content_version`, removed claims
    skipped).
  - `("GET", ["projects",pid,"holders"])` → `since` < `holder_floor` → 410
    `holders_below_floor`; `since` > `holder_seq` → 410
    `holders_cursor_ahead`; `epoch` query ≠ state epoch → 410
    `{"error":"epoch_changed","epoch":…}`; else every claim with
    `changed_seq > since`, grouped by device into `add`/`rm`, one page
    (`hasMore: false`) unless `page_size` is lower.
  - `("PUT", ["projects",pid,"holders","self"])` → body without `reportSeq`
    → 409 outdated; the contract's validations; per entry: refused when the
    frame is unknown, `cv < 1` or `cv > current`, or the member may not hold
    it (publisher any state; `send_receive`/`data.moderate` published;
    `data.moderate` pending) — refused entries are listed and any live claim
    tombstoned; `full` tombstones unlisted live claims with a lower
    report_seq; `holder_seq` bumps once when any visible change happened
    (and `holder_writes += changed rows`); `digestMatch` compares
    `digest_of(device)` with the body's digest minus the refused keys;
    reply `{holderSeq, digestMatch, nextFlushMs: 1000, refused}`. An empty
    report writes nothing.
  - `("POST", ["projects",pid,"frames",uuid,"version"])` → no
    `expectedVersion` → 409 outdated; mismatch → 409
    `{"error":"version_conflict","contentVersion":cur}`; success bumps,
    writes the implicit claim `(uuid, new)` with `report_seq` kept, and
    NEVER deletes other claims.
  - `("POST", ["projects",pid,"frames","versions"])` → the batch shape,
    one bump if any `ok`.

- [ ] **Step 6: The axum front.** In `FakeHub::start`, after the wiremock mount:

```rust
let front = axum::Router::new()
    .route("/api/v1/me/events", axum::routing::get(front_events))
    .route("/api/v1/me/presence", axum::routing::post(front_beat).delete(front_leave))
    .fallback(front_proxy)
    .with_state(FrontState { state: Arc::clone(&state), upstream: server.uri(), http: reqwest::Client::new() });
let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("fake hub front binds");
let front_url = format!("http://{}", listener.local_addr().expect("front addr"));
let front_task = tokio::spawn(async move {
    if let Err(e) = axum::serve(listener, front).await {
        tracing::error!(error = %e, "fake hub front stopped");
    }
});
```

  `FakeHub` gains `front_url: String` and `front_task: tokio::task::JoinHandle<()>`
  (aborted in `Drop`); `uri()` returns `front_url`. A background presence
  ticker (`tokio::time::interval(50 ms)`) expires detached sessions after
  `timings.grace` and silent sessions after `timings.silence`, publishing
  `presence` `connected:false`.

```rust
#[derive(Clone)]
struct FrontState {
    state: Arc<Mutex<FakeHubState>>,
    upstream: String,
    http: reqwest::Client,
}

async fn front_proxy(axum::extract::State(fx): axum::extract::State<FrontState>, req: axum::extract::Request) -> axum::response::Response {
    let (parts, body) = req.into_parts();
    let path_q = parts.uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_default();
    let bytes = axum::body::to_bytes(body, 16 * 1024 * 1024).await.unwrap_or_default();
    let mut up = fx.http.request(parts.method.clone(), format!("{}{}", fx.upstream, path_q)).body(bytes.to_vec());
    for h in ["authorization", "content-type"] {
        if let Some(v) = parts.headers.get(h) {
            up = up.header(h, v.clone());
        }
    }
    match up.send().await {
        Ok(r) => {
            let status = r.status();
            let ct = r.headers().get("content-type").cloned();
            let body = r.bytes().await.unwrap_or_default();
            let mut resp = axum::response::Response::new(axum::body::Body::from(body));
            *resp.status_mut() = status;
            if let Some(ct) = ct {
                resp.headers_mut().insert("content-type", ct);
            }
            resp
        }
        Err(e) => {
            tracing::error!(error = %e, "fake hub proxy failed");
            axum::response::Response::builder().status(502).body(axum::body::Body::empty()).expect("static response")
        }
    }
}

async fn front_events(axum::extract::State(fx): axum::extract::State<FrontState>, headers: axum::http::HeaderMap) -> axum::response::Response {
    use axum::response::IntoResponse;
    let token = headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).map(str::to_string);
    let opened = {
        let mut st = fx.state.lock().expect("fake hub state poisoned");
        if st.api_outdated {
            return (axum::http::StatusCode::CONFLICT, axum::Json(json!({"error": "collab_api_outdated"}))).into_response();
        }
        let Some(acct) = token.and_then(|t| st.tokens.get(&t).cloned()) else {
            return axum::http::StatusCode::UNAUTHORIZED.into_response();
        };
        st.open_session(&acct) // replaces an older session of the same device (its kill fires), marks connected, returns (hello, feed rx, kill rx, keepalive)
    };
    let (hello, mut feed, mut kill, keepalive, account_id) = opened;
    let state = Arc::clone(&fx.state);
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::convert::Infallible>>(64);
    tokio::spawn(async move {
        let first = format!("retry: 3000\nevent: hello\ndata: {hello}\n\n");
        if tx.send(Ok(first.into())).await.is_err() { return; }
        let mut tick = tokio::time::interval(keepalive);
        tick.tick().await;
        loop {
            let frame = tokio::select! {
                _ = kill.changed() => break,
                _ = tick.tick() => ":\n\n".to_string(),
                msg = feed.recv() => match msg {
                    Ok(m) => {
                        let deliver = {
                            let st = state.lock().expect("fake hub state poisoned");
                            match (&m.project_id, &m.account_id) {
                                (Some(pid), _) => st.projects.get(pid).is_some_and(|p| p.member(&account_id).is_some()),
                                (None, Some(a)) => a == &account_id,
                                (None, None) => true,
                            }
                        };
                        if !deliver { continue; }
                        format!("event: {}\ndata: {}\n\n", m.name, m.data)
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                },
            };
            if tx.send(Ok(frame.into())).await.is_err() { break; }
        }
        // stream closed → the session detaches; the ticker applies the grace
        state.lock().expect("fake hub state poisoned").detach_sessions_without_streams();
    });
    let stream = n0_future::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|item| (item, rx)) });
    axum::response::Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("x-accel-buffering", "no")
        .body(axum::body::Body::from_stream(stream))
        .expect("static response")
}
```

  `front_beat` validates `sessionId` (32 lowercase hex) → unknown →
  `409 {"error":"session_gone"}`; else updates `serving`/`relay_url`/`last_beat`
  and publishes a `presence` change per project whose serving flag changed;
  204. `front_leave` removes the session, publishes `connected:false`, fires
  its kill; 204 (also unknown). `open_session` builds `hello` exactly as the
  contract: `accountId`, per project `version`, `holderSeq`, `claimCount` and
  `claimDigest` from `digest_of(device)`, `reportSeq` = max stored for the
  device, `presence` = connected devices of members with their serving flag.
  `kill_streams()` fires every session's kill and clears `sessions`.

- [ ] **Step 7: Run** `cargo test -p athenaeum-core --lib collab::fake_hub` → PASS. Then
  `cargo test -p athenaeum-core --lib api::collab_exchange collab::` — the
  wave-2 tests that used `holders_of`, the retired routes or the 75-minute
  holders are expected to FAIL now; mark each failing wave-2 test that
  exercises retired behaviour (`mod poll`, `full_report_*`,
  `over_ten_thousand_frames_*`, `a_failing_hub_costs_one_holder_lookup_per_pass`,
  `a_frame_the_hub_no_longer_shows_is_skipped_alone`, loss-guard tests)
  `#[ignore = "collab v3 wave 3: retired in Task 15"]` in this commit; Task 15
  deletes them. List them in the commit body.

- [ ] **Step 8: Commit**

```bash
rustfmt crates/athenaeum-core/src/collab/fake_hub.rs crates/athenaeum-core/src/collab/live/digest.rs
git add -A crates/athenaeum-core
git commit -m "test(collab): fake hub speaks the live-exchange contract — claims with report_seq, digest, snapshot/since, CAS versions, SSE front with presence"
```

---
### Task 4: Event-stream client — event types, SSE parser, stream reader, presence book, home-relay watch (P2, P28)

**Implementer:** rust-engineer.

**Files:**
- Modify: `crates/athenaeum-core/src/collab/live/wire.rs` (append the event types), `collab/live/mod.rs` (`pub mod presence; pub mod sse; pub mod stream;`).
- Create: `crates/athenaeum-core/src/collab/live/sse.rs`, `collab/live/stream.rs`, `collab/live/presence.rs`.
- Modify: `crates/athenaeum-core/src/sharing/iroh/node.rs` — new `home_relay_url()` and `home_relay_watch()` beside `endpoint_addr` (:1340).
- Test: unit tests in the three new files; stream tests against `FakeHub` (Task 3).

**Interfaces:**
- Consumes: `FakeHub` (Task 3, tests only), `BeatWire` (Task 2).
- Produces:
  ```rust
  // collab/live/wire.rs — events (Deserialize, camelCase)
  pub struct PresenceEntry { pub device: String, pub serving: bool, pub relay_url: Option<String> }
  pub struct HelloProject { pub version: i64, pub holder_seq: i64, pub claim_count: i64, pub claim_digest: String, pub report_seq: i64, #[serde(default)] pub presence: Vec<PresenceEntry> }
  pub struct HelloEvent { pub session_id: String, pub epoch: String, pub account_id: String, pub projects: std::collections::BTreeMap<String, HelloProject> }
  #[serde(rename_all = "lowercase")] pub enum ChangeKind { Frames, Meta, Members, Thresholds, Dictionary, Grid, #[serde(other)] Unknown }
  pub struct ProjectEvent { pub project_id: String, pub prev: i64, pub version: i64, pub kinds: Vec<ChangeKind>, #[serde(default)] pub frames: Option<Vec<crate::collab::hub_client::FrameViewWire>>, #[serde(default)] pub more: bool }
  pub struct HoldersEvent { pub project_id: String, pub prev: i64, pub seq: i64, pub deltas: Vec<HolderDeltaWire> }
  pub struct PresenceChange { pub device: String, pub connected: bool, pub serving: bool, pub relay_url: Option<String> }
  pub struct PresenceEvent { pub project_id: String, #[serde(default)] pub replace: bool, pub changes: Vec<PresenceChange> }
  #[serde(rename_all = "lowercase")] pub enum AccountKind { Joined, Left }
  pub struct AccountEvent { pub kind: AccountKind, pub project_id: String }
  #[serde(rename_all = "lowercase")] pub enum ResyncWhat { Project, Holders }
  pub struct ResyncEvent { pub project_id: String, pub what: ResyncWhat }
  pub type VersionsEvent = std::collections::BTreeMap<String, (i64, i64)>;
  #[derive(Debug, Clone, PartialEq)]
  pub enum LiveEvent { Hello(HelloEvent), Project(ProjectEvent), Holders(HoldersEvent), Presence(PresenceEvent), Account(AccountEvent), Resync(ResyncEvent), Versions(VersionsEvent), Unknown(String) }
  pub fn decode_event(name: &str, data: &str) -> Result<LiveEvent, serde_json::Error>;
  // collab/live/sse.rs
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum SseFrame { Event { name: String, data: String }, Comment, Retry(u64) }
  #[derive(Default)] pub struct SseParser { /* buf, name, data */ }
  impl SseParser { pub fn push(&mut self, chunk: &[u8]) -> Vec<SseFrame>; }
  // collab/live/stream.rs
  pub const HUB_KEEPALIVE: Duration = Duration::from_secs(20);
  pub const READ_TIMEOUT: Duration = Duration::from_secs(50);
  pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
  pub const SILENT_STREAM_WARN: Duration = Duration::from_secs(25);
  pub fn stream_http_client() -> reqwest::Client;
  #[derive(Debug)] pub enum OpenError { Unauthorized, Forbidden, Outdated, Status(u16), Transport(String) }
  pub async fn open(client: &reqwest::Client, hub_url: &str, token: &str) -> Result<reqwest::Response, OpenError>;
  #[derive(Debug, PartialEq, Eq)] pub enum StreamEnd { Closed, ReadError(String), Cancelled, ReceiverGone }
  pub async fn pump(resp: reqwest::Response, tx: &tokio::sync::mpsc::Sender<LiveEvent>, cancel: &mut tokio::sync::watch::Receiver<bool>) -> StreamEnd;
  // collab/live/presence.rs
  pub const BEAT_INTERVAL: Duration = Duration::from_secs(15);
  pub const HUB_SILENCE: Duration = Duration::from_secs(40);
  pub const HUB_GRACE: Duration = Duration::from_secs(10);
  pub const WAKE_JUMP: Duration = Duration::from_secs(30);
  #[derive(Debug, Clone, PartialEq, Eq)] pub struct PeerPresence { pub connected: bool, pub serving: bool, pub relay_url: Option<String> }
  #[derive(Debug, Default, Clone)] pub struct PresenceBook { /* project → device → PeerPresence */ }
  impl PresenceBook {
      pub fn apply_hello(&mut self, project_id: &str, entries: &[PresenceEntry]);   // replaces that project's map
      pub fn apply_event(&mut self, ev: &PresenceEvent) -> Vec<String>;            // devices whose candidacy changed
      pub fn forget_project(&mut self, project_id: &str);
      pub fn is_online_serving(&self, project_id: &str, device: &str) -> bool;
      pub fn relay_url(&self, project_id: &str, device: &str) -> Option<&str>;
      pub fn is_connected(&self, project_id: &str, device: &str) -> bool;
  }
  pub fn woke_from_sleep(wall_elapsed: Duration, mono_elapsed: Duration) -> bool;  // wall − mono > WAKE_JUMP
  // sharing/iroh/node.rs
  impl SharedIrohNode {
      pub fn home_relay_url(&self) -> Option<String>;                               // endpoint.addr().relay_urls().next()
      pub fn home_relay_watch(&self) -> tokio::sync::watch::Receiver<Option<String>>;  // changes only; one watcher task per node, spawned on first call
  }
  ```

- [ ] **Step 1: Write the failing tests**

`collab/live/sse.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "retry: 3000\nevent: hello\ndata: {\"a\":1}\n\n:\n\nevent: holders\ndata: {\"b\":\ndata: 2}\n\n";

    fn expected() -> Vec<SseFrame> {
        vec![
            SseFrame::Retry(3000),
            SseFrame::Event { name: "hello".into(), data: "{\"a\":1}".into() },
            SseFrame::Comment,
            SseFrame::Event { name: "holders".into(), data: "{\"b\":\n2}".into() },
        ]
    }

    #[test]
    fn whole_sample_parses() {
        let mut p = SseParser::default();
        assert_eq!(p.push(SAMPLE.as_bytes()), expected());
    }

    #[test]
    fn every_split_point_gives_the_same_frames() {
        let bytes = SAMPLE.as_bytes();
        for cut in 0..=bytes.len() {
            let mut p = SseParser::default();
            let mut got = p.push(&bytes[..cut]);
            got.extend(p.push(&bytes[cut..]));
            assert_eq!(got, expected(), "split at {cut}");
        }
    }

    #[test]
    fn crlf_and_utf8_split_inside_a_character() {
        let s = "event: project\r\ndata: {\"t\":\"Ω\"}\r\n\r\n".as_bytes();
        let omega = s.iter().position(|b| *b == 0xCE).unwrap();
        let mut p = SseParser::default();
        let mut got = p.push(&s[..omega + 1]);
        got.extend(p.push(&s[omega + 1..]));
        assert_eq!(got, vec![SseFrame::Event { name: "project".into(), data: "{\"t\":\"Ω\"}".into() }]);
    }
}
```

`collab/live/wire.rs` (append to its tests):

```rust
#[test]
fn every_event_of_the_contract_decodes() {
    let hello = r#"{"sessionId":"0123456789abcdef0123456789abcdef","epoch":"e1","accountId":"acc","projects":{"p1":{"version":42,"holderSeq":17,"claimCount":3,"claimDigest":"00000000000000000000000000000000","reportSeq":120,"presence":[{"device":"AAA=","serving":true,"relayUrl":"https://r"}]}}}"#;
    let LiveEvent::Hello(h) = decode_event("hello", hello).unwrap() else { panic!() };
    assert_eq!(h.projects["p1"].report_seq, 120);
    let project = r#"{"projectId":"p1","prev":41,"version":42,"kinds":["frames","members","grid"],"more":true}"#;
    let LiveEvent::Project(p) = decode_event("project", project).unwrap() else { panic!() };
    assert_eq!(p.kinds, vec![ChangeKind::Frames, ChangeKind::Members, ChangeKind::Grid]);
    assert!(p.frames.is_none() && p.more);
    let holders = r#"{"projectId":"p1","prev":16,"seq":17,"deltas":[{"device":"AAA=","add":[[12,1],[13,1]],"rm":[7]}]}"#;
    let LiveEvent::Holders(hd) = decode_event("holders", holders).unwrap() else { panic!() };
    assert_eq!(hd.deltas[0].add, vec![(12, 1), (13, 1)]);
    let presence = r#"{"projectId":"p1","replace":false,"changes":[{"device":"AAA=","connected":false,"serving":false,"relayUrl":null}]}"#;
    assert!(matches!(decode_event("presence", presence).unwrap(), LiveEvent::Presence(_)));
    assert!(matches!(decode_event("account", r#"{"kind":"left","projectId":"p1"}"#).unwrap(),
        LiveEvent::Account(AccountEvent { kind: AccountKind::Left, .. })));
    assert!(matches!(decode_event("resync", r#"{"projectId":"p1","what":"holders"}"#).unwrap(),
        LiveEvent::Resync(ResyncEvent { what: ResyncWhat::Holders, .. })));
    let LiveEvent::Versions(v) = decode_event("versions", r#"{"p1":[42,17]}"#).unwrap() else { panic!() };
    assert_eq!(v["p1"], (42, 17));
    assert!(matches!(decode_event("future", "{}").unwrap(), LiveEvent::Unknown(n) if n == "future"));
}
```

`collab/live/presence.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::live::wire::{PresenceChange, PresenceEntry, PresenceEvent};

    #[test]
    fn hello_replaces_and_events_patch_or_replace() {
        let mut b = PresenceBook::default();
        b.apply_hello("p1", &[PresenceEntry { device: "A".into(), serving: true, relay_url: Some("https://r1".into()) }]);
        assert!(b.is_online_serving("p1", "A"));
        let changed = b.apply_event(&PresenceEvent { project_id: "p1".into(), replace: false, changes: vec![
            PresenceChange { device: "B".into(), connected: true, serving: false, relay_url: None },
            PresenceChange { device: "A".into(), connected: true, serving: true, relay_url: Some("https://r2".into()) },
        ]});
        // A stays a candidate (only its relay moved); B is connected but not serving
        assert!(changed.is_empty());
        assert_eq!(b.relay_url("p1", "A"), Some("https://r2"));
        assert!(b.is_connected("p1", "B") && !b.is_online_serving("p1", "B"));
        let changed = b.apply_event(&PresenceEvent { project_id: "p1".into(), replace: true, changes: vec![
            PresenceChange { device: "B".into(), connected: true, serving: true, relay_url: None },
        ]});
        assert_eq!(changed, vec!["A".to_string(), "B".to_string()]); // A dropped by replace, B now serving
        assert!(!b.is_connected("p1", "A"));
        assert!(b.is_online_serving("p1", "B"));
    }

    #[test]
    fn a_wall_clock_jump_beyond_the_beat_means_the_machine_slept() {
        assert!(!woke_from_sleep(Duration::from_secs(16), Duration::from_secs(15)));
        assert!(woke_from_sleep(Duration::from_secs(120), Duration::from_secs(15)));
    }
}
```

`collab/live/stream.rs` (tests against the fake hub):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::fake_hub::{FakeHub, FakeTimings};
    use crate::collab::live::wire::LiveEvent;

    #[tokio::test]
    async fn opens_reads_hello_and_ends_when_the_hub_closes() {
        let hub = FakeHub::start().await;
        hub.add_account("tok", "acc", "Me", "AAA=", None);
        hub.add_project("p1", "m31", &[("acc", "send_receive", false)], false);
        hub.set_timings(FakeTimings { keepalive: Duration::from_millis(200), grace: Duration::from_millis(200), silence: Duration::from_secs(40) });
        let client = stream_http_client();
        let resp = open(&client, &hub.uri(), "tok").await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let (_cancel_tx, mut cancel) = tokio::sync::watch::channel(false);
        let pumping = tokio::spawn(async move { pump(resp, &tx, &mut cancel).await });
        let first = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        assert!(matches!(first, LiveEvent::Hello(ref h) if h.projects.contains_key("p1")));
        hub.kill_streams();
        let end = tokio::time::timeout(Duration::from_secs(5), pumping).await.unwrap().unwrap();
        assert_eq!(end, StreamEnd::Closed);
    }

    #[tokio::test]
    async fn open_types_401_and_409() {
        let hub = FakeHub::start().await;
        let client = stream_http_client();
        assert!(matches!(open(&client, &hub.uri(), "nobody").await, Err(OpenError::Unauthorized)));
        hub.add_account("tok", "acc", "Me", "AAA=", None);
        hub.set_api_outdated(true);
        assert!(matches!(open(&client, &hub.uri(), "tok").await, Err(OpenError::Outdated)));
    }

    #[tokio::test]
    async fn cancel_ends_the_pump() {
        let hub = FakeHub::start().await;
        hub.add_account("tok", "acc", "Me", "AAA=", None);
        let client = stream_http_client();
        let resp = open(&client, &hub.uri(), "tok").await.unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let (cancel_tx, mut cancel) = tokio::sync::watch::channel(false);
        let pumping = tokio::spawn(async move { pump(resp, &tx, &mut cancel).await });
        cancel_tx.send(true).unwrap();
        assert_eq!(tokio::time::timeout(Duration::from_secs(5), pumping).await.unwrap().unwrap(), StreamEnd::Cancelled);
    }
}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib collab::live`. Expected: compile errors.

- [ ] **Step 3: `collab/live/sse.rs`** — spec-compliant subset (`event`, `data`,
  `retry`, comments; `id` ignored; LF, CR or CRLF line ends; bytes buffered
  until a full line so a UTF-8 character split across chunks is safe):

```rust
//! A minimal Server-Sent Events parser for the hub's event channel
//! (spec §4.1; hub § Wire contract framing). Pure: bytes in, frames out.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseFrame {
    Event { name: String, data: String },
    Comment,
    Retry(u64),
}

#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    name: Option<String>,
    data: Option<String>,
    saw_cr: bool,
}

impl SseParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseFrame> {
        let mut out = Vec::new();
        for &b in chunk {
            if self.saw_cr {
                self.saw_cr = false;
                if b == b'\n' {
                    continue; // CRLF: the CR already ended the line
                }
            }
            match b {
                b'\n' | b'\r' => {
                    self.saw_cr = b == b'\r';
                    let line = std::mem::take(&mut self.buf);
                    self.line(&String::from_utf8_lossy(&line), &mut out);
                }
                _ => self.buf.push(b),
            }
        }
        out
    }

    fn line(&mut self, line: &str, out: &mut Vec<SseFrame>) {
        if line.is_empty() {
            // dispatch
            if let Some(data) = self.data.take() {
                let name = self.name.take().unwrap_or_else(|| "message".to_string());
                out.push(SseFrame::Event { name, data });
            }
            self.name = None;
            return;
        }
        if line.starts_with(':') {
            out.push(SseFrame::Comment);
            return;
        }
        let (field, value) = match line.find(':') {
            Some(i) => (&line[..i], line[i + 1..].strip_prefix(' ').unwrap_or(&line[i + 1..])),
            None => (line, ""),
        };
        match field {
            "event" => self.name = Some(value.to_string()),
            "data" => match &mut self.data {
                Some(d) => {
                    d.push('\n');
                    d.push_str(value);
                }
                None => self.data = Some(value.to_string()),
            },
            "retry" => {
                if let Ok(ms) = value.parse() {
                    out.push(SseFrame::Retry(ms));
                }
            }
            _ => {} // "id" and unknown fields are ignored
        }
    }
}
```

- [ ] **Step 4: `collab/live/wire.rs` events** — the Interfaces types with
  `#[derive(Debug, Clone, PartialEq, serde::Deserialize)]` +
  `#[serde(rename_all = "camelCase")]`, and:

```rust
pub fn decode_event(name: &str, data: &str) -> Result<LiveEvent, serde_json::Error> {
    Ok(match name {
        "hello" => LiveEvent::Hello(serde_json::from_str(data)?),
        "project" => LiveEvent::Project(serde_json::from_str(data)?),
        "holders" => LiveEvent::Holders(serde_json::from_str(data)?),
        "presence" => LiveEvent::Presence(serde_json::from_str(data)?),
        "account" => LiveEvent::Account(serde_json::from_str(data)?),
        "resync" => LiveEvent::Resync(serde_json::from_str(data)?),
        "versions" => LiveEvent::Versions(serde_json::from_str(data)?),
        other => LiveEvent::Unknown(other.to_string()),
    })
}
```

- [ ] **Step 5: `collab/live/stream.rs`**

```rust
//! The dedicated streaming client for `GET /me/events` (spec §4.1, plan P2).
//! The hub client's 30 s total deadline cannot carry a stream, so this one
//! has none: a per-read timeout of 2.5 × the hub's keepalive detects a dead
//! stream, and the session (api::collab_live::session) reconnects with the
//! full-jitter back-off.

use std::time::Duration;

use n0_future::StreamExt as _;

use super::sse::{SseFrame, SseParser};
use super::wire::{decode_event, LiveEvent};

pub const HUB_KEEPALIVE: Duration = Duration::from_secs(20);
pub const READ_TIMEOUT: Duration = Duration::from_secs(50);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const SILENT_STREAM_WARN: Duration = Duration::from_secs(25);

pub fn stream_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "event stream client build failed; using defaults");
            reqwest::Client::new()
        })
}

#[derive(Debug)]
pub enum OpenError {
    Unauthorized,
    Forbidden,
    Outdated,
    Status(u16),
    Transport(String),
}

pub async fn open(client: &reqwest::Client, hub_url: &str, token: &str) -> Result<reqwest::Response, OpenError> {
    let url = format!("{}/api/v1/me/events", hub_url.trim_end_matches('/'));
    let resp = client
        .get(url)
        .bearer_auth(token)
        .header("accept", "text/event-stream")
        .send()
        .await
        .map_err(|e| OpenError::Transport(e.to_string()))?;
    match resp.status().as_u16() {
        200 => Ok(resp),
        401 => Err(OpenError::Unauthorized),
        403 => Err(OpenError::Forbidden),
        409 => {
            let body = resp.text().await.unwrap_or_default();
            if body.contains("collab_api_outdated") {
                Err(OpenError::Outdated)
            } else {
                Err(OpenError::Status(409))
            }
        }
        s => Err(OpenError::Status(s)),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum StreamEnd {
    Closed,
    ReadError(String),
    Cancelled,
    ReceiverGone,
}

/// Read the stream until it ends, sending every decoded event. Keepalive
/// comments reset the read timeout inside reqwest and are not forwarded.
pub async fn pump(
    resp: reqwest::Response,
    tx: &tokio::sync::mpsc::Sender<LiveEvent>,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
) -> StreamEnd {
    let mut body = resp.bytes_stream();
    let mut parser = SseParser::default();
    let mut got_byte = false;
    let silent = tokio::time::sleep(SILENT_STREAM_WARN);
    tokio::pin!(silent);
    loop {
        let chunk = tokio::select! {
            _ = cancel.changed() => return StreamEnd::Cancelled,
            _ = &mut silent, if !got_byte => {
                tracing::warn!(duration_ms = SILENT_STREAM_WARN.as_millis() as u64, "event stream connected but silent; a buffering proxy may sit in between");
                got_byte = true; // warn once
                continue;
            }
            c = body.next() => c,
        };
        match chunk {
            None => return StreamEnd::Closed,
            Some(Err(e)) => {
                tracing::warn!(error = %e, "event stream read failed");
                return StreamEnd::ReadError(e.to_string());
            }
            Some(Ok(bytes)) => {
                got_byte = true;
                for frame in parser.push(&bytes) {
                    let SseFrame::Event { name, data } = frame else { continue };
                    match decode_event(&name, &data) {
                        Ok(LiveEvent::Unknown(n)) => tracing::debug!(kind = %n, "unknown event ignored"),
                        Ok(ev) => {
                            if tx.send(ev).await.is_err() {
                                return StreamEnd::ReceiverGone;
                            }
                        }
                        Err(e) => tracing::error!(kind = %name, error = %e, "event failed to decode; dropped"),
                    }
                }
            }
        }
    }
}
```

  A dropped malformed event is safe: the next event's `prev` mismatches and
  the applier catches up over REST (I3).

- [ ] **Step 6: `collab/live/presence.rs`** — the book of the Interfaces block
  (a `HashMap<String, HashMap<String, PeerPresence>>`; `apply_event` with
  `replace: true` rebuilds the project's map from `changes`; a device is a
  "candidate" when `connected && serving`; the returned vector lists devices
  whose candidacy flipped, sorted) and:

```rust
/// A wall-clock step larger than the monotonic step by more than
/// [`WAKE_JUMP`] means the machine slept (plan P28: tauri has no desktop
/// sleep event, so the beat loop detects the wake and reconnects at once).
pub fn woke_from_sleep(wall_elapsed: Duration, mono_elapsed: Duration) -> bool {
    wall_elapsed.saturating_sub(mono_elapsed) > WAKE_JUMP
}
```

- [ ] **Step 7: Home relay accessors** in `sharing/iroh/node.rs`:

```rust
/// The home relay URL the endpoint currently advertises (`None` with the
/// relay disabled or before the first home relay). Sent in the presence
/// beat so a device that re-homes stays dialable (spec §4.2, C40).
pub fn home_relay_url(&self) -> Option<String> {
    self.endpoint().addr().relay_urls().next().map(|u| u.to_string())
}

/// Changes of [`Self::home_relay_url`], deduplicated. One watcher task per
/// node, spawned on first call and ended with the node's endpoint.
pub fn home_relay_watch(&self) -> tokio::sync::watch::Receiver<Option<String>> {
    let mut slot = self.home_relay_tx.lock().expect("home relay watch poisoned");
    if let Some(tx) = slot.as_ref() {
        return tx.subscribe();
    }
    let (tx, rx) = tokio::sync::watch::channel(self.home_relay_url());
    let endpoint = self.endpoint();
    let tx2 = tx.clone();
    tokio::spawn(async move {
        use iroh::Watcher as _;
        let mut addrs = endpoint.watch_addr().stream();
        while let Some(addr) = n0_future::StreamExt::next(&mut addrs).await {
            let url = addr.relay_urls().next().map(|u| u.to_string());
            tx2.send_if_modified(|cur| {
                if *cur != url { *cur = url; true } else { false }
            });
        }
        tracing::debug!("home relay watch ended");
    });
    *slot = Some(tx);
    rx
}
```

  Add the field `home_relay_tx: std::sync::Mutex<Option<tokio::sync::watch::Sender<Option<String>>>>`
  to `SharedIrohNode` (initialised `Mutex::new(None)` in `bind_with`).
  Test in `sharing/iroh/tests.rs`: a node bound with `RelayMode::Disabled`
  returns `None` from `home_relay_url()` and its watch starts at `None`.

- [ ] **Step 8: Run** `cargo test -p athenaeum-core --lib collab::live sharing::iroh::tests::home_relay && cargo check -p athenaeum-core --no-default-features`. Expected: PASS.

- [ ] **Step 9: Commit**

```bash
rustfmt crates/athenaeum-core/src/collab/live/*.rs crates/athenaeum-core/src/sharing/iroh/node.rs
git add -A crates/athenaeum-core/src
git commit -m "feat(collab): event-stream client — SSE parser, event types, stream pump with read timeout, presence book, home-relay watch"
```

---
### Task 5: Feed applier — cursors, catch-up, resync, versions, epoch change, account and members events (I2, I3, §4.3–§4.4, P5, P6, P25)

**Implementer:** rust-engineer.

**Files:**
- Create: `crates/athenaeum-core/src/collab/live/cursor.rs` (pure); register in `collab/live/mod.rs`.
- Create: `crates/athenaeum-core/src/api/collab_live/mod.rs` (module skeleton: `pub mod feed;` — later tasks add their modules) and `api/collab_live/feed.rs`. Register `#[cfg(all(feature = "render", feature = "solver"))] pub mod collab_live;` in `api/mod.rs` next to `pub mod collab;` (:68-69).
- Modify: `crates/athenaeum-core/src/api/collab_exchange.rs` — `sync_manifest_serialized` (:246-377) returns the seen set; new `pub(crate) async fn sync_manifest_full`.
- Modify: `crates/athenaeum-core/src/db/collab_frames.rs` — `upsert_from_manifest` writes `frame_seq` when the wire carries one (> 0).
- Modify: `crates/athenaeum-core/src/api/collab.rs` — make `announce_batches` (:2174) `pub(crate)`.
- Test: unit tests in `collab/live/cursor.rs`; integration tests in `api/collab_live/feed.rs` over `FakeHub`.

**Interfaces:**
- Consumes: `LiveEvent` and the event types (Task 4), `CollabClient` + `with_retry` (Task 2), `FakeHub` (Task 3), `db::collab::{set_feed_version, set_holder_seq}`, `db::collab_live::{meta_get, meta_set, clear_project_live_state}` (Task 1).
- Produces:
  ```rust
  // collab/live/cursor.rs
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct FeedCursor { pub epoch: Option<String>, pub version: i64, pub holder_seq: i64 }   // holder_seq −1 = no local holder map
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Step { Apply, Ignore, CatchUp }
  pub fn step(cursor: i64, prev: i64, head: i64) -> Step;
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum HolderPlan { InSync, Delta, Snapshot }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub struct HelloPlan { pub epoch_changed: bool, pub catch_up_project: bool, pub holders: HolderPlan }
  pub fn plan_hello(stored: &FeedCursor, hub_epoch: &str, head_version: i64, head_holder_seq: i64) -> HelloPlan;
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum VersionsPlan { InSync, CatchUp { project: bool, holders: bool }, EpochChange }
  pub fn plan_versions(stored: &FeedCursor, head_version: i64, head_holder_seq: i64) -> VersionsPlan;
  // api/collab_exchange.rs
  pub(crate) async fn sync_manifest_full(ctx: &ServiceContext, project_id: &str, emitter: Option<&dyn ProgressEmitter>, vouched_version: Option<i64>) -> Result<std::collections::HashSet<String>, ApiError>; // since=0, prunes, returns every uuid the hub listed
  // api/collab_live/feed.rs
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum FeedEffect { NeedSetChanged(String), ProvidersChanged(String), MembersChanged(String), ProjectJoined(String), ProjectGone(String), EpochChanged }
  /// The holder side of the feed (Task 6 implements it for `Holdings`).
  #[async_trait::async_trait]
  pub trait HolderSide: Send {
      async fn on_hello_project(&mut self, project_id: &str, hello: &HelloProject, plan: HolderPlan, epoch: &str) -> Result<Vec<FeedEffect>, ApiError>;
      async fn on_holders_event(&mut self, ev: &HoldersEvent, epoch: &str) -> Result<Vec<FeedEffect>, ApiError>;
      async fn catch_up(&mut self, project_id: &str, epoch: &str) -> Result<Vec<FeedEffect>, ApiError>;
      async fn reload(&mut self, project_id: &str, epoch: &str) -> Result<Vec<FeedEffect>, ApiError>;        // snapshot + full report
      fn forget(&mut self, project_id: &str);
  }
  pub struct FeedApplier { /* ctx: Arc<ServiceContext>, client: CollabClient, token: String, emitter: Option<Arc<dyn ProgressEmitter>>, pub presence: PresenceBook, pub account_id: Option<String>, pub epoch: Option<String> */ }
  impl FeedApplier {
      pub fn new(ctx: Arc<ServiceContext>, client: CollabClient, token: String, emitter: Option<Arc<dyn ProgressEmitter>>) -> Self;
      pub async fn apply(&mut self, ev: LiveEvent, holders: &mut dyn HolderSide) -> Result<Vec<FeedEffect>, ApiError>;
      pub async fn catch_up_project(&mut self, project_id: &str, version: Option<i64>, kinds: &[ChangeKind]) -> Result<Vec<FeedEffect>, ApiError>;
      pub async fn epoch_change(&mut self, new_epoch: &str, heads: &std::collections::BTreeMap<String, i64>, holders: &mut dyn HolderSide) -> Result<Vec<FeedEffect>, ApiError>; // heads = per-project version from the triggering hello/versions
  }
  pub(crate) async fn reannounce_lost_own_frames(ctx: &ServiceContext, client: &CollabClient, token: &str, project_id: &str, seen: &std::collections::HashSet<String>) -> Result<usize, ApiError>;
  ```
  (`async-trait` is already a core dependency, Cargo.toml:68.)

- [ ] **Step 1: Write the failing pure tests** (`collab/live/cursor.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn cur(epoch: Option<&str>, version: i64, holder_seq: i64) -> FeedCursor {
        FeedCursor { epoch: epoch.map(str::to_string), version, holder_seq }
    }

    #[test]
    fn step_applies_contiguous_ignores_old_and_catches_up_on_a_gap() {
        assert_eq!(step(41, 41, 42), Step::Apply);
        assert_eq!(step(42, 41, 42), Step::Ignore); // duplicate
        assert_eq!(step(45, 41, 42), Step::Ignore); // older than the cursor
        assert_eq!(step(40, 41, 42), Step::CatchUp); // gap
    }

    #[test]
    fn hello_plans() {
        // first connection after the upgrade: no epoch, no holder map
        assert_eq!(plan_hello(&cur(None, 40, -1), "e1", 42, 5),
            HelloPlan { epoch_changed: false, catch_up_project: true, holders: HolderPlan::Snapshot });
        // resume: same epoch, behind on holders only
        assert_eq!(plan_hello(&cur(Some("e1"), 42, 3), "e1", 42, 5),
            HelloPlan { epoch_changed: false, catch_up_project: false, holders: HolderPlan::Delta });
        // in sync
        assert_eq!(plan_hello(&cur(Some("e1"), 42, 5), "e1", 42, 5),
            HelloPlan { epoch_changed: false, catch_up_project: false, holders: HolderPlan::InSync });
        // a new epoch
        assert!(plan_hello(&cur(Some("e1"), 42, 5), "e2", 50, 9).epoch_changed);
        // cursor ahead of the hub's head = a restore under the same epoch
        assert!(plan_hello(&cur(Some("e1"), 42, 5), "e1", 30, 5).epoch_changed);
        assert!(plan_hello(&cur(Some("e1"), 42, 5), "e1", 42, 2).epoch_changed);
    }

    #[test]
    fn versions_plans() {
        let c = cur(Some("e1"), 42, 5);
        assert_eq!(plan_versions(&c, 42, 5), VersionsPlan::InSync);
        assert_eq!(plan_versions(&c, 43, 5), VersionsPlan::CatchUp { project: true, holders: false });
        assert_eq!(plan_versions(&c, 42, 7), VersionsPlan::CatchUp { project: false, holders: true });
        assert_eq!(plan_versions(&c, 41, 5), VersionsPlan::EpochChange);
        // no local holder map yet: a holder head is a catch-up, never an epoch change
        assert_eq!(plan_versions(&cur(Some("e1"), 42, -1), 42, 0), VersionsPlan::CatchUp { project: false, holders: true });
    }
}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib collab::live::cursor`. Expected: compile error.

- [ ] **Step 3: `collab/live/cursor.rs`**

```rust
//! The cursor rules of the event channel (spec I3, §4.4; hub § Wire contract
//! "Cursor rules"). Pure decisions; the applier performs them.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedCursor {
    pub epoch: Option<String>,
    pub version: i64,
    /// −1 = no local holder map (load the snapshot).
    pub holder_seq: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Apply,
    Ignore,
    CatchUp,
}

/// For a `project` (`head` = version) or `holders` (`head` = seq) event.
pub fn step(cursor: i64, prev: i64, head: i64) -> Step {
    if head <= cursor {
        Step::Ignore
    } else if prev == cursor {
        Step::Apply
    } else {
        Step::CatchUp
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HolderPlan {
    InSync,
    Delta,
    Snapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelloPlan {
    pub epoch_changed: bool,
    pub catch_up_project: bool,
    pub holders: HolderPlan,
}

pub fn plan_hello(stored: &FeedCursor, hub_epoch: &str, head_version: i64, head_holder_seq: i64) -> HelloPlan {
    let epoch_differs = stored.epoch.as_deref().is_some_and(|e| e != hub_epoch);
    let ahead = head_version < stored.version || (stored.holder_seq >= 0 && head_holder_seq < stored.holder_seq);
    if epoch_differs || ahead {
        return HelloPlan { epoch_changed: true, catch_up_project: true, holders: HolderPlan::Snapshot };
    }
    let holders = if stored.holder_seq < 0 {
        HolderPlan::Snapshot
    } else if head_holder_seq > stored.holder_seq {
        HolderPlan::Delta
    } else {
        HolderPlan::InSync
    };
    HelloPlan { epoch_changed: false, catch_up_project: head_version > stored.version, holders }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionsPlan {
    InSync,
    CatchUp { project: bool, holders: bool },
    EpochChange,
}

pub fn plan_versions(stored: &FeedCursor, head_version: i64, head_holder_seq: i64) -> VersionsPlan {
    if head_version < stored.version || (stored.holder_seq >= 0 && head_holder_seq < stored.holder_seq) {
        return VersionsPlan::EpochChange;
    }
    let project = head_version > stored.version;
    let holders = head_holder_seq > stored.holder_seq;
    if project || holders {
        VersionsPlan::CatchUp { project, holders }
    } else {
        VersionsPlan::InSync
    }
}
```

- [ ] **Step 4: Manifest sync returns what it saw.** Refactor
  `sync_manifest_serialized` into `sync_manifest_inner(ctx, pid, emitter,
  vouched, force_full: bool) -> Result<(Vec<CollabFramesChange>, HashSet<String>), ApiError>`:
  `start = 0` and the caps-rule prune when `caps_changed || force_full`.
  `sync_manifest` keeps its signature (`force_full = false`, drops the set);
  add:

```rust
/// Refetch the whole manifest from 0 and prune rows the hub no longer lists
/// (own rows are never pruned, `delete_not_in`). Returns every uuid the hub
/// listed — the epoch path compares it with the own rows (plan P25).
pub(crate) async fn sync_manifest_full(
    ctx: &ServiceContext,
    project_id: &str,
    emitter: Option<&dyn ProgressEmitter>,
    vouched_version: Option<i64>,
) -> Result<HashSet<String>, ApiError> {
    let (_, seen) = sync_manifest_inner(ctx, project_id, emitter, vouched_version, true).await?;
    Ok(seen)
}
```

  `upsert_from_manifest` adds `frame_seq = CASE WHEN ?17 > 0 THEN ?17 ELSE frame_seq END`
  (and the value on insert) from `v.frame_seq`.

- [ ] **Step 5: Write the failing integration tests** (`api/collab_live/feed.rs`,
  `#[cfg(test)] mod tests`). They reuse the wave-2 fixture style
  (`crate::api::collab_exchange` test helpers `test_ctx` + `wire_hub` —
  make `test_ctx` and `wire_hub` `pub(crate)` inside a `#[cfg(test)] pub(crate) mod test_support`
  in `api/collab_exchange.rs`, moved verbatim from CE:4299 and CE:4342):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::fake_hub::FakeHub;
    use crate::collab::live::wire::*;

    struct NoHolders(Vec<String>);
    #[async_trait::async_trait]
    impl HolderSide for NoHolders {
        async fn on_hello_project(&mut self, pid: &str, _: &HelloProject, plan: HolderPlan, _: &str) -> Result<Vec<FeedEffect>, ApiError> { self.0.push(format!("hello {pid} {plan:?}")); Ok(vec![]) }
        async fn on_holders_event(&mut self, ev: &HoldersEvent, _: &str) -> Result<Vec<FeedEffect>, ApiError> { self.0.push(format!("holders {}", ev.seq)); Ok(vec![]) }
        async fn catch_up(&mut self, pid: &str, _: &str) -> Result<Vec<FeedEffect>, ApiError> { self.0.push(format!("catch_up {pid}")); Ok(vec![]) }
        async fn reload(&mut self, pid: &str, _: &str) -> Result<Vec<FeedEffect>, ApiError> { self.0.push(format!("reload {pid}")); Ok(vec![]) }
        fn forget(&mut self, pid: &str) { self.0.push(format!("forget {pid}")); }
    }

    async fn rig() -> (tempfile::TempDir, Arc<ServiceContext>, FakeHub, FeedApplier) {
        let (tmp, ctx) = crate::api::collab_exchange::test_support::test_ctx();
        let ctx = Arc::new(ctx);
        let hub = FakeHub::start().await;
        hub.add_account("tok", "acc-me", "Me", "AAA=", None);
        hub.add_account("tok-o", "acc-o", "Other", "BBB=", None);
        hub.add_project("p1", "m31", &[("acc-me", "send_receive", false), ("acc-o", "send", false)], false);
        crate::api::collab_exchange::test_support::wire_hub(&ctx, &hub, "tok");
        crate::api::collab::refresh_projects(&ctx).await.unwrap();
        let client = CollabClient::new(hub.uri()).unwrap();
        let f = FeedApplier::new(Arc::clone(&ctx), client, "tok".into(), None);
        (tmp, ctx, hub, f)
    }

    async fn manifest_requests(hub: &FakeHub) -> usize {
        hub.server.received_requests().await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.path().ends_with("/manifest"))
            .count()
    }

    fn cursor(ctx: &ServiceContext) -> (Option<String>, i64) {
        let conn = crate::api::db(ctx).unwrap().conn();
        let p = crate::db::collab::get_project(&conn, "p1").unwrap().unwrap();
        (p.feed_epoch, p.hub_version)
    }

    #[tokio::test]
    async fn inline_frames_apply_without_a_manifest_read() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h).await.unwrap();
        let before = manifest_requests(&hub).await;
        hub.seed_frames("p1", "acc-o", &["u1"], "published");
        let ev = project_event_from_hub(&hub, "p1"); // the event the fake just published
        let effects = f.apply(LiveEvent::Project(ev.clone()), &mut h).await.unwrap();
        assert_eq!(manifest_requests(&hub).await, before);
        assert!(effects.contains(&FeedEffect::NeedSetChanged("p1".into())));
        let conn = crate::api::db(&ctx).unwrap().conn();
        let row = crate::db::collab_frames::get(&conn, "p1", "u1").unwrap().unwrap();
        assert_eq!(row.frame_seq, Some(1));
        drop(conn);
        assert_eq!(cursor(&ctx).1, ev.version);
    }

    #[tokio::test]
    async fn a_gap_catches_up_over_rest_and_an_old_event_is_ignored() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h).await.unwrap();
        hub.seed_frames("p1", "acc-o", &["u1"], "published");
        hub.seed_frames("p1", "acc-o", &["u2"], "published");
        let last = project_event_from_hub(&hub, "p1"); // prev = cursor + 1 → a gap
        let before = manifest_requests(&hub).await;
        f.apply(LiveEvent::Project(last.clone()), &mut h).await.unwrap();
        assert_eq!(manifest_requests(&hub).await, before + 1);
        assert_eq!(cursor(&ctx).1, last.version);
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert!(crate::db::collab_frames::get(&conn, "p1", "u1").unwrap().is_some());
        drop(conn);
        // replaying it is a no-op
        f.apply(LiveEvent::Project(last), &mut h).await.unwrap();
        assert_eq!(manifest_requests(&hub).await, before + 1);
    }

    #[tokio::test]
    async fn members_kind_refreshes_the_snapshot_and_reports_it() {
        let (_t, _ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h).await.unwrap();
        hub.add_member("p1", "acc-x", "send_receive", false);
        let ev = project_event_from_hub(&hub, "p1");
        assert!(ev.kinds.contains(&ChangeKind::Members));
        let effects = f.apply(LiveEvent::Project(ev), &mut h).await.unwrap();
        assert!(effects.contains(&FeedEffect::MembersChanged("p1".into())));
    }

    #[tokio::test]
    async fn account_left_marks_the_project_lost_and_forgets_its_live_state() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h).await.unwrap();
        hub.remove_member("p1", "acc-me");
        let effects = f.apply(LiveEvent::Account(AccountEvent { kind: AccountKind::Left, project_id: "p1".into() }), &mut h).await.unwrap();
        assert!(effects.contains(&FeedEffect::ProjectGone("p1".into())));
        assert!(h.0.contains(&"forget p1".to_string()));
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert!(crate::db::collab::lost_at(&conn, "p1").unwrap().is_some());
    }

    #[tokio::test]
    async fn an_epoch_change_refetches_everything_and_reannounces_lost_own_frames() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        // an own frame the hub knows
        hub.seed_frames("p1", "acc-me", &["own1"], "published");
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h).await.unwrap();
        f.catch_up_project("p1", None, &[ChangeKind::Frames]).await.unwrap();
        // the hub is restored without it
        hub.forget_frames("p1", &["own1"]);
        let e2 = hub.rotate_epoch();
        let effects = f.apply(LiveEvent::Hello(hello(&hub, &e2)), &mut h).await.unwrap();
        assert!(effects.contains(&FeedEffect::EpochChanged));
        assert!(h.0.contains(&"reload p1".to_string()));
        assert!(hub.frame("p1", "own1").is_some(), "re-announced under the same uuid");
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert_eq!(crate::db::collab_live::meta_get(&conn, crate::db::collab_live::META_EPOCH).unwrap().as_deref(), Some(e2.as_str()));
    }

    #[tokio::test]
    async fn versions_vector_catches_up_or_changes_epoch() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h).await.unwrap();
        let (_, v) = cursor(&ctx);
        let mut vv = VersionsEvent::new();
        vv.insert("p1".into(), (v + 3, 0));
        hub.seed_frames("p1", "acc-o", &["u5"], "published");
        f.apply(LiveEvent::Versions(vv), &mut h).await.unwrap();
        assert!(cursor(&ctx).1 > v);
    }
}
```

  Test helpers in the same module: `hello(&FakeHub, epoch) -> HelloEvent`
  builds the hello from the fake's state (`FakeHub` exposes
  `pub fn hello_for(&self, token: &str) -> serde_json::Value` — add it in this
  task, it is the same builder `open_session` uses) and overrides `epoch`;
  `project_event_from_hub(&FakeHub, pid) -> ProjectEvent` reads the last
  `project` event the fake published (add `pub fn last_event(&self, name: &str, project_id: &str) -> Option<serde_json::Value>`
  to `FakeHub`, fed by a copy of every `publish` into a bounded
  `VecDeque<FeedMsg>` of 256).

- [ ] **Step 6: Run** `cargo test -p athenaeum-core --lib api::collab_live::feed`. Expected: compile errors.

- [ ] **Step 7: `api/collab_live/feed.rs`** — the applier. Key bodies:

```rust
impl FeedApplier {
    fn stored_cursor(&self, project_id: &str) -> Result<Option<FeedCursor>, ApiError> {
        let db = crate::api::db(&self.ctx)?;
        let conn = db.conn();
        Ok(crate::db::collab::get_live_project(&conn, project_id)
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .map(|p| FeedCursor { epoch: p.feed_epoch, version: p.hub_version, holder_seq: p.holder_seq }))
    }

    pub async fn apply(&mut self, ev: LiveEvent, holders: &mut dyn HolderSide) -> Result<Vec<FeedEffect>, ApiError> {
        match ev {
            LiveEvent::Hello(h) => self.on_hello(h, holders).await,
            LiveEvent::Project(p) => self.on_project(p).await,
            LiveEvent::Holders(h) => {
                let epoch = self.epoch.clone().unwrap_or_default();
                holders.on_holders_event(&h, &epoch).await
            }
            LiveEvent::Presence(p) => {
                let changed = self.presence.apply_event(&p);
                Ok(if changed.is_empty() { vec![] } else { vec![FeedEffect::ProvidersChanged(p.project_id)] })
            }
            LiveEvent::Account(a) => self.on_account(a, holders).await,
            LiveEvent::Resync(r) => {
                let epoch = self.epoch.clone().unwrap_or_default();
                match r.what {
                    ResyncWhat::Project => self.catch_up_project(&r.project_id, None, &ALL_KINDS).await,
                    ResyncWhat::Holders => holders.catch_up(&r.project_id, &epoch).await,
                }
            }
            LiveEvent::Versions(v) => self.on_versions(v, holders).await,
            LiveEvent::Unknown(_) => Ok(vec![]),
        }
    }
}

const ALL_KINDS: [ChangeKind; 5] = [ChangeKind::Frames, ChangeKind::Meta, ChangeKind::Members, ChangeKind::Thresholds, ChangeKind::Dictionary];
```

  - **`on_hello`**: store `account_id` (`meta_set(META_ACCOUNT_ID)`),
    `raise_report_seq(max(hello.projects[*].report_seq))`. A cached live
    project absent from `hello.projects` → `on_account(Left)`. A project in
    `hello.projects` without a cache row → `refresh_projects_reporting(Some({pid}))`
    then treat as joined. For each project: `presence.apply_hello`; plan with
    `plan_hello(stored, &hello.epoch, version, holder_seq)`. If any plan says
    `epoch_changed`, run `epoch_change(&hello.epoch)` ONCE for all projects and
    return (the heads map = `hello.projects[*].version`). Else per project: `catch_up_project(pid, Some(version), &ALL_KINDS)`
    when `catch_up_project`; `holders.on_hello_project(pid, hp, plan.holders, epoch)`
    (the holder side also compares `hp.claimCount`/`hp.claimDigest` with the
    local claim set and sends the full report on a mismatch — Task 6).
    `self.epoch = Some(hello.epoch)`; `meta_set(META_EPOCH)`.
  - **`on_project`**: `step(stored.version, ev.prev, ev.version)`:
    `Ignore` → `Ok(vec![])`; `CatchUp` → `catch_up_project(pid, Some(ev.version), &ev.kinds)`
    (every kind of a gap is unknown, so pass `&ALL_KINDS`); `Apply` →
    `apply_contiguous(ev)`:

```rust
async fn apply_contiguous(&mut self, ev: ProjectEvent) -> Result<Vec<FeedEffect>, ApiError> {
    let pid = ev.project_id.clone();
    let epoch = self.epoch.clone().unwrap_or_default();
    let mut effects = Vec::new();
    let small_docs = ev.kinds.iter().any(|k| matches!(k, ChangeKind::Meta | ChangeKind::Members | ChangeKind::Thresholds | ChangeKind::Dictionary));
    if small_docs {
        let only: std::collections::HashSet<String> = [pid.clone()].into();
        let report = crate::api::collab::refresh_projects_reporting(&self.ctx, Some(&only)).await?;
        for moved in &report.gate_moved {
            crate::api::collab::on_thresholds_or_dictionary_moved(&self.ctx, moved);
        }
        if ev.kinds.contains(&ChangeKind::Members) {
            effects.push(FeedEffect::MembersChanged(pid.clone()));
        }
    }
    if ev.kinds.contains(&ChangeKind::Grid) {
        tracing::debug!(project_id = %pid, version = ev.version, "grid change seen; no consumer in this wave");
    }
    match (ev.kinds.contains(&ChangeKind::Frames), ev.frames) {
        (true, Some(rows)) if !ev.more => {
            self.apply_inline(&pid, ev.version, &epoch, rows)?;
            effects.push(FeedEffect::NeedSetChanged(pid.clone()));
        }
        (true, _) => {
            crate::api::collab_exchange::sync_manifest(&self.ctx, &pid, self.emitter.as_deref(), Some(ev.version)).await?;
            effects.push(FeedEffect::NeedSetChanged(pid.clone()));
        }
        (false, _) => {}
    }
    let db = crate::api::db(&self.ctx)?;
    crate::db::collab::set_feed_version(&db.conn(), &pid, &epoch, ev.version).map_err(|e| ApiError::Internal(e.to_string()))?;
    tracing::debug!(project_id = %pid, prev = ev.prev, version = ev.version, "project event applied");
    Ok(effects)
}

fn apply_inline(&self, pid: &str, version: i64, epoch: &str, mut rows: Vec<FrameViewWire>) -> Result<(), ApiError> {
    use crate::db::collab_frames as frames_db;
    let account = self.account_id.clone().unwrap_or_default();
    let db = crate::api::db(&self.ctx)?;
    let conn = db.conn();
    let project = crate::api::collab_exchange::live_project(&conn, pid)?;
    let tx = conn.unchecked_transaction()?;
    let mut max_mv = project.manifest_cursor;
    let mut counts: std::collections::BTreeMap<FramesChangeKind, usize> = Default::default();
    for v in rows.iter_mut() {
        v.own = v.publisher_account_id == account;
        let prev = frames_db::get(&tx, pid, &v.frame_uuid)?;
        for kind in crate::api::collab_exchange::classify_frame_change(prev.as_ref(), v) {
            *counts.entry(kind).or_default() += 1;
        }
        frames_db::upsert_from_manifest(&tx, pid, v)?;
        max_mv = max_mv.max(v.manifest_version);
    }
    crate::db::collab::set_sync_state(&tx, pid, Some(version), max_mv, &project.gov_caps_json)?;
    crate::db::collab::set_feed_version(&tx, pid, epoch, version)?;
    tx.commit()?;
    for (kind, count) in counts {
        let change = CollabFramesChange { project_id: pid.to_string(), kind, count };
        tracing::info!(project_id = pid, count, kind = kind.as_str(), "manifest delta applied");
        if let Some(em) = self.emitter.as_deref() {
            crate::events::emit_event(em, COLLAB_FRAMES_CHANGED_EVENT, &change);
        }
    }
    Ok(())
}
```

  (`FramesChangeKind::as_str` and `classify_frame_change` become
  `pub(crate)`; the `?` on rusqlite/anyhow errors maps through the
  existing `From` impls on `ApiError` — use `.map_err(|e| ApiError::Internal(e.to_string()))`
  where none exists.)
  - **`catch_up_project(pid, version, kinds)`**: small docs as above when
    `kinds` names any; then `sync_manifest(ctx, pid, emitter, version)`;
    then `set_feed_version(pid, epoch, version.unwrap_or(stored.version))`.
    Returns `NeedSetChanged` (+ `MembersChanged` when `Members` was listed).
  - **`on_versions`**: per project in the vector with a live cache row,
    `plan_versions`: `EpochChange` → `epoch_change(current epoch, heads)` once
    (heads = the vector's versions);
    `CatchUp { project, holders }` → the matching catch-ups.
  - **`on_account`**: `Joined` → `refresh_projects_reporting(Some({pid}))`,
    `catch_up_project(pid, None, &ALL_KINDS)`, `holders.reload(pid)`,
    `ProjectJoined(pid)`. `Left` → `refresh_projects_reporting(Some({pid}))`
    (marks it lost, R14), `clear_project_live_state`, `presence.forget_project`,
    `holders.forget(pid)`, `ProjectGone(pid)`.
  - **`epoch_change(new_epoch)`** (P25): `warn!(epoch = new_epoch, "hub epoch changed; reloading every project")`;
    `meta_set(META_EPOCH, new_epoch)`; for every live project:
    `set_holder_seq(pid, new_epoch, -1)`; `seen = sync_manifest_full(ctx, pid, emitter, None)`;
    `holders.reload(pid, new_epoch)` (snapshot + full report, Task 6);
    `reannounce_lost_own_frames(ctx, client, token, pid, &seen)`;
    `set_feed_version(pid, new_epoch, heads[pid])` (the head of the
    triggering hello or versions vector; a project missing from `heads` keeps
    its manifest-derived `hub_version`). Returns `EpochChanged` +
    `NeedSetChanged` per project.
  - **`reannounce_lost_own_frames`**:

```rust
pub(crate) async fn reannounce_lost_own_frames(
    ctx: &ServiceContext,
    client: &CollabClient,
    token: &str,
    project_id: &str,
    seen: &std::collections::HashSet<String>,
) -> Result<usize, ApiError> {
    use crate::db::collab_frames::{self as frames_db, FrameOrigin};
    let (lost, gate) = {
        let db = crate::api::db(ctx)?;
        let conn = db.conn();
        let project = crate::api::collab_exchange::live_project(&conn, project_id)?;
        let lost: Vec<FrameInWire> = frames_db::list_for_project(&conn, project_id)
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .into_iter()
            .filter(|r| r.origin == FrameOrigin::Own && !seen.contains(&r.frame_uuid))
            .filter_map(|r| crate::api::collab_exchange::parse_manifest_wire(project_id, &r.frame_uuid, &r.manifest_json, "reannounce"))
            .map(|v| FrameInWire {
                frame_uuid: v.frame_uuid, file_name: v.file_name, blake3: v.blake3, byte_size: v.byte_size,
                xxh3: v.xxh3, filter_raw: v.filter_raw, filter_canonical: v.filter_canonical,
                channel: v.channel, exptime_sec: v.exptime_sec, date_obs: v.date_obs,
                gate_version: 0, meta: v.meta,
            })
            .collect();
        (lost, project.thresholds_version.unwrap_or(0))
    };
    let mut announced = 0usize;
    for mut batch in crate::api::collab::announce_batches(lost) {
        for f in batch.iter_mut() {
            f.gate_version = gate; // thresholds are prospective (v3 §10, plan P25)
        }
        match with_retry("reannounce", RetryPolicy::Background, || client.announce_frames(token, project_id, &batch)).await {
            Ok(resp) => {
                let db = crate::api::db(ctx)?;
                let conn = db.conn();
                for f in &batch {
                    crate::db::collab_live::add_implicit_claim(&conn, project_id, &f.frame_uuid, 1)
                        .map_err(|e| ApiError::Internal(e.to_string()))?;
                }
                announced += resp.announced;
            }
            Err(e) if e.hub_text().is_some_and(|m| m.contains("already announced")) => {
                tracing::info!(project_id, count = batch.len(), "own frames already back on the hub");
            }
            Err(e) => {
                tracing::error!(project_id, count = batch.len(), error = %e, "re-announce of own frames failed");
                return Err(crate::api::collab_exchange::client_err_pub(e));
            }
        }
    }
    if announced > 0 {
        tracing::warn!(project_id, count = announced, "own frames re-announced after an epoch change");
    }
    Ok(announced)
}
```

  (`client_err_pub` = the existing private `client_err` (CE:33) made
  `pub(crate)` under that name.)

- [ ] **Step 8: Run** `cargo test -p athenaeum-core --lib collab::live::cursor api::collab_live::feed api::collab_exchange && cargo check --workspace --all-targets && cargo check -p athenaeum-core --no-default-features`. Expected: PASS.

- [ ] **Step 9: Commit**

```bash
rustfmt crates/athenaeum-core/src/collab/live/cursor.rs crates/athenaeum-core/src/api/collab_live/*.rs crates/athenaeum-core/src/api/collab_exchange.rs crates/athenaeum-core/src/api/collab.rs crates/athenaeum-core/src/db/collab_frames.rs crates/athenaeum-core/src/collab/fake_hub.rs
git add -A crates/athenaeum-core/src
git commit -m "feat(collab): feed applier — prev-checked project events, inline frames, REST catch-up, resync, versions vector, epoch change with own-frame re-announce"
```

---
### Task 6: Holder map, providers, outbox flush, digest reconciliation, implicit claims in publish (§6, I4, P7–P9, P34)

**Implementer:** rust-engineer.

**Files:**
- Create: `crates/athenaeum-core/src/collab/live/holders.rs`, `collab/live/outbox.rs` (pure); register in `collab/live/mod.rs`.
- Create: `crates/athenaeum-core/src/api/collab_live/holdings.rs`; `pub mod holdings;` in `api/collab_live/mod.rs`.
- Modify: `crates/athenaeum-core/src/api/collab.rs` — `run_publish` step 5 (announce success, :2909-2918; R8 adoption, :2937-2958), step 6 (versions, :3016-3130), step 7 (holder delta, :3296-3313).
- Test: unit tests in `holders.rs`, `outbox.rs`; integration tests in `holdings.rs` over `FakeHub`; publish tests in `api/collab.rs` `tests::publish` (extend).

**Interfaces:**
- Consumes: Task 1 (`collab_live` DB functions, `ClaimOp`, `OutboxRow`, `HolderDeviceRow`), Task 2 (wire, digest, client), Task 4 (`PresenceBook`), Task 5 (`HolderSide`, `FeedEffect`, `cursor::{step, HolderPlan}`).
- Produces:
  ```rust
  // collab/live/holders.rs
  #[derive(Debug, Clone, PartialEq, Eq)] pub struct DeviceInfo { pub display_name: String, pub relay_url: Option<String> }
  #[derive(Debug, Default, Clone, PartialEq)]
  pub struct ProjectHolders { pub devices: HashMap<String, DeviceInfo>, pub claims: HashMap<String, HashMap<i32, i32>> } // device → frameSeq → contentVersion
  impl ProjectHolders {
      pub fn from_snapshot(s: &HoldersSnapshotWire) -> Self;
      pub fn from_rows(devices: &[HolderDeviceRow], claims: &[(String, i32, i32)]) -> Self;
      pub fn apply_delta(&mut self, d: &HolderDeltaWire);
      pub fn claimants(&self, frame_seq: i32, content_version: i32) -> Vec<&str>;         // sorted
      pub fn device_rows(&self) -> Vec<HolderDeviceRow>;
      pub fn claim_rows(&self) -> Vec<(String, i32, i32)>;
  }
  #[derive(Debug, Clone, PartialEq, Eq)] pub struct Provider { pub device: String, pub relay_url: Option<String> }
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)] pub struct Redundancy { pub online: usize, pub total: usize }
  pub struct FrameRef<'a> { pub project_id: &'a str, pub frame_seq: i32, pub content_version: i32, pub publisher_devices: &'a HashSet<String> }
  pub fn providers(h: &ProjectHolders, presence: &PresenceBook, members: &HashSet<String>, me: &str, f: &FrameRef<'_>) -> Vec<Provider>;   // I4 + I5
  pub fn redundancy(h: &ProjectHolders, presence: &PresenceBook, members: &HashSet<String>, me: &str, f: &FrameRef<'_>) -> Redundancy;   // other holders of the current version
  pub fn waiting_for_publisher(h: &ProjectHolders, presence: &PresenceBook, members: &HashSet<String>, me: &str, f: &FrameRef<'_>) -> bool; // L7 "vN waiting for the publisher"
  // collab/live/outbox.rs
  pub const DEFAULT_FLUSH: Duration = Duration::from_millis(1000);
  pub const FLUSH_AT_ENTRIES: usize = 500;
  pub const DIGEST_CHECK_EVERY: Duration = Duration::from_secs(3600);
  #[derive(Debug, Clone, PartialEq)] pub struct Coalesced { pub add: Vec<ClaimWire>, pub remove: Vec<String>, pub max_seq: i64 }
  pub fn coalesce(rows: &[OutboxRow]) -> Coalesced;                                         // last op per frame wins
  pub fn delta_report(rows: &[OutboxRow], claims: &[(String, i32)]) -> anyhow::Result<HoldersReportWire>;
  pub fn full_report(claims: &[(String, i32)], report_seq: i64) -> anyhow::Result<HoldersReportWire>;
  pub fn digest_check(claims: &[(String, i32)], report_seq: i64) -> anyhow::Result<HoldersReportWire>;
  pub struct FlushClock { /* next_flush: Duration, first_pending: Option<Instant> */ }
  impl FlushClock { pub fn new() -> Self; pub fn on_append(&mut self, now: Instant); pub fn due(&self, now: Instant, pending: usize) -> bool; pub fn flushed(&mut self, next_flush_ms: u64); pub fn deadline(&self) -> Option<Instant>; }
  // api/collab_live/holdings.rs
  pub fn member_devices(project: &crate::db::collab::CollabProjectRow) -> HashSet<String>;   // base64 node ids of the signed snapshot's members
  pub struct Holdings { /* ctx, client, token, me: String, maps: HashMap<String, ProjectHolders>, clocks: HashMap<String, FlushClock>, backoffs: HashMap<String, Backoff>, last_digest_check: Instant */ }
  #[derive(Debug, Clone, PartialEq, Eq)] pub struct FlushOutcome { pub sent: usize, pub digest_match: bool, pub refused: usize }
  impl Holdings {
      pub fn load(ctx: Arc<ServiceContext>, client: CollabClient, token: String, me: String) -> Result<Self, ApiError>;
      pub async fn snapshot(&mut self, project_id: &str, epoch: &str) -> Result<Vec<FeedEffect>, ApiError>;
      pub async fn delta_resume(&mut self, project_id: &str, epoch: &str) -> Result<Vec<FeedEffect>, ApiError>;
      pub async fn flush(&mut self, project_id: &str) -> Result<FlushOutcome, ApiError>;
      pub async fn flush_due(&mut self, now: Instant) -> Vec<(String, Result<FlushOutcome, ApiError>)>;
      pub async fn full_report(&mut self, project_id: &str) -> Result<(), ApiError>;
      pub async fn digest_check(&mut self, project_id: &str) -> Result<bool, ApiError>;          // empty report; mismatch → full_report
      pub async fn hourly_digest_checks(&mut self, now: Instant);
      pub fn note_append(&mut self, project_id: &str, now: Instant);                              // the executor calls it after every state transition that wrote an outbox row
      pub fn next_deadline(&self) -> Option<Instant>;
      pub fn map(&self, project_id: &str) -> Option<&ProjectHolders>;
  }
  #[async_trait::async_trait] impl HolderSide for Holdings { /* Task 5 trait */ }
  pub(crate) async fn flush_project_now(ctx: &ServiceContext, project_id: &str) -> Result<(), ApiError>; // publish uses it before versions
  ```

- [ ] **Step 1: Write the failing pure tests.**

`collab/live/holders.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::live::presence::PresenceBook;
    use crate::collab::live::wire::*;

    fn snap() -> HoldersSnapshotWire {
        HoldersSnapshotWire {
            epoch: "e".into(), holder_seq: 5, version: 9,
            frames: vec![SnapshotFrameWire { seq: 1, uuid: "u1".into(), content_version: 2 }],
            devices: vec![
                SnapshotDeviceWire { device: "PUB".into(), display_name: "Pub".into(), relay_url: Some("https://r0".into()), claims: vec![[1, 1, 2]] },
                SnapshotDeviceWire { device: "OLD".into(), display_name: "Old".into(), relay_url: None, claims: vec![[1, 1, 1]] },
                SnapshotDeviceWire { device: "ME".into(), display_name: "Me".into(), relay_url: None, claims: vec![[1, 1, 2]] },
                SnapshotDeviceWire { device: "OFF".into(), display_name: "Off".into(), relay_url: None, claims: vec![[1, 1, 2]] },
            ],
        }
    }

    fn members() -> HashSet<String> {
        ["PUB", "OLD", "ME", "OFF"].iter().map(|s| s.to_string()).collect()
    }

    fn presence() -> PresenceBook {
        let mut p = PresenceBook::default();
        p.apply_hello("p1", &[
            PresenceEntry { device: "PUB".into(), serving: true, relay_url: Some("https://r1".into()) },
            PresenceEntry { device: "OLD".into(), serving: true, relay_url: None },
            PresenceEntry { device: "ME".into(), serving: true, relay_url: None },
        ]);
        p
    }

    #[test]
    fn providers_are_derived_from_version_presence_membership_and_not_me() {
        let h = ProjectHolders::from_snapshot(&snap());
        let pubs: HashSet<String> = ["PUB".to_string()].into();
        let f = FrameRef { project_id: "p1", frame_seq: 1, content_version: 2, publisher_devices: &pubs };
        let got = providers(&h, &presence(), &members(), "ME", &f);
        // OLD claims v1 (superseded), OFF is offline, ME is me
        assert_eq!(got, vec![Provider { device: "PUB".into(), relay_url: Some("https://r1".into()) }]);
        assert_eq!(redundancy(&h, &presence(), &members(), "ME", &f), Redundancy { online: 1, total: 2 });
        // not a member any more → never a provider
        let only_me: HashSet<String> = ["ME".to_string()].into();
        assert!(providers(&h, &presence(), &only_me, "ME", &f).is_empty());
    }

    #[test]
    fn deltas_apply_in_any_order_relative_to_versions() {
        let mut h = ProjectHolders::from_snapshot(&snap());
        h.apply_delta(&HolderDeltaWire { device: "OLD".into(), add: vec![(1, 3)], rm: vec![] });
        h.apply_delta(&HolderDeltaWire { device: "PUB".into(), add: vec![], rm: vec![1] });
        assert_eq!(h.claimants(1, 3), vec!["OLD"]);
        assert_eq!(h.claimants(1, 2), vec!["ME", "OFF"]);
        // a claim on a frameSeq the manifest does not know yet is kept
        h.apply_delta(&HolderDeltaWire { device: "NEW".into(), add: vec![(99, 1)], rm: vec![] });
        assert_eq!(h.claimants(99, 1), vec!["NEW"]);
        let rows = h.claim_rows();
        assert_eq!(ProjectHolders::from_rows(&h.device_rows(), &rows).claimants(99, 1), vec!["NEW"]);
    }

    #[test]
    fn a_new_version_held_only_by_the_offline_publisher_is_waiting_for_it() {
        let h = ProjectHolders::from_snapshot(&snap());
        let offs: HashSet<String> = ["OFF".to_string()].into();
        let f = FrameRef { project_id: "p1", frame_seq: 1, content_version: 2, publisher_devices: &offs };
        let mut p = PresenceBook::default();
        p.apply_hello("p1", &[]);
        assert!(waiting_for_publisher(&h, &p, &members(), "ME", &f) == false); // ME and PUB also hold v2
        let mut h2 = h.clone();
        h2.apply_delta(&HolderDeltaWire { device: "PUB".into(), add: vec![], rm: vec![1] });
        h2.apply_delta(&HolderDeltaWire { device: "ME".into(), add: vec![], rm: vec![1] });
        assert!(waiting_for_publisher(&h2, &p, &members(), "ME", &f));
    }
}
```

`collab/live/outbox.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::collab_live::{ClaimOp, OutboxRow};

    fn row(seq: i64, u: &str, op: ClaimOp) -> OutboxRow { OutboxRow { seq, frame_uuid: u.into(), op } }

    #[test]
    fn last_op_per_frame_wins_and_lists_are_disjoint() {
        let rows = vec![
            row(1, "u1", ClaimOp::Add { content_version: 1 }),
            row(2, "u2", ClaimOp::Add { content_version: 1 }),
            row(3, "u1", ClaimOp::Remove),
            row(4, "u2", ClaimOp::Add { content_version: 2 }),
        ];
        let c = coalesce(&rows);
        assert_eq!(c.max_seq, 4);
        assert_eq!(c.remove, vec!["u1".to_string()]);
        assert_eq!(c.add, vec![ClaimWire { uuid: "u2".into(), content_version: 2 }]);
    }

    #[test]
    fn reports_carry_the_digest_of_the_whole_claim_set_after_the_report() {
        let claims = vec![("00000000-0000-4000-8000-000000000001".to_string(), 1), ("00000000-0000-4000-8000-000000000002".to_string(), 1)];
        let r = delta_report(&[row(7, "00000000-0000-4000-8000-000000000002", ClaimOp::Add { content_version: 1 })], &claims).unwrap();
        assert_eq!((r.report_seq, r.full, r.count), (7, false, 2));
        assert_eq!(r.digest, "d173abce7c5386289c657a8a697518d8");
        let f = full_report(&claims, 9).unwrap();
        assert!(f.full && f.remove.is_empty() && f.add.len() == 2 && f.report_seq == 9);
        let e = digest_check(&claims, 0).unwrap();
        assert!(e.add.is_empty() && e.remove.is_empty() && !e.full && e.report_seq == 1);
    }

    #[test]
    fn flush_clock_waits_next_flush_or_five_hundred_entries() {
        let t0 = Instant::now();
        let mut c = FlushClock::new();
        assert!(!c.due(t0, 0));
        c.on_append(t0);
        assert!(!c.due(t0 + Duration::from_millis(500), 3));
        assert!(c.due(t0 + Duration::from_millis(1000), 3));
        assert!(c.due(t0, FLUSH_AT_ENTRIES));
        c.flushed(2000);
        c.on_append(t0);
        assert!(!c.due(t0 + Duration::from_millis(1500), 1));
        assert!(c.due(t0 + Duration::from_millis(2000), 1));
    }
}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib collab::live::holders collab::live::outbox`. Expected: compile errors.

- [ ] **Step 3: Pure implementations.**

```rust
// collab/live/holders.rs (core of it)
impl ProjectHolders {
    pub fn from_snapshot(s: &HoldersSnapshotWire) -> Self {
        let mut h = ProjectHolders::default();
        for d in &s.devices {
            h.devices.insert(d.device.clone(), DeviceInfo { display_name: d.display_name.clone(), relay_url: d.relay_url.clone() });
            let claims = h.claims.entry(d.device.clone()).or_default();
            for (seq, cv) in crate::collab::live::wire::expand_runs(&d.claims) {
                claims.insert(seq, cv);
            }
        }
        h
    }

    pub fn apply_delta(&mut self, d: &HolderDeltaWire) {
        self.devices.entry(d.device.clone()).or_insert_with(|| DeviceInfo { display_name: String::new(), relay_url: None });
        let claims = self.claims.entry(d.device.clone()).or_default();
        for (seq, cv) in &d.add {
            claims.insert(*seq, *cv);
        }
        for seq in &d.rm {
            claims.remove(seq);
        }
    }

    pub fn claimants(&self, frame_seq: i32, content_version: i32) -> Vec<&str> {
        let mut out: Vec<&str> = self
            .claims
            .iter()
            .filter(|(_, c)| c.get(&frame_seq) == Some(&content_version))
            .map(|(d, _)| d.as_str())
            .collect();
        out.sort_unstable();
        out
    }
}

/// Candidates for a frame (spec I4 + I5): claims the CURRENT version, is
/// connected AND serving in this project, is a member, and is not me.
/// Never cleared by a version event — availability is derived (§6.1).
pub fn providers(h: &ProjectHolders, presence: &PresenceBook, members: &HashSet<String>, me: &str, f: &FrameRef<'_>) -> Vec<Provider> {
    h.claimants(f.frame_seq, f.content_version)
        .into_iter()
        .filter(|d| *d != me && members.contains(*d) && presence.is_online_serving(f.project_id, d))
        .map(|d| Provider {
            device: d.to_string(),
            relay_url: presence
                .relay_url(f.project_id, d)
                .map(str::to_string)
                .or_else(|| h.devices.get(d).and_then(|i| i.relay_url.clone())),
        })
        .collect()
}

pub fn redundancy(h: &ProjectHolders, presence: &PresenceBook, members: &HashSet<String>, me: &str, f: &FrameRef<'_>) -> Redundancy {
    let others: Vec<&str> = h
        .claimants(f.frame_seq, f.content_version)
        .into_iter()
        .filter(|d| *d != me && members.contains(*d))
        .collect();
    Redundancy {
        online: others.iter().filter(|d| presence.is_online_serving(f.project_id, d)).count(),
        total: others.len(),
    }
}

pub fn waiting_for_publisher(h: &ProjectHolders, presence: &PresenceBook, members: &HashSet<String>, me: &str, f: &FrameRef<'_>) -> bool {
    let holders: Vec<&str> = h
        .claimants(f.frame_seq, f.content_version)
        .into_iter()
        .filter(|d| *d != me && members.contains(*d))
        .collect();
    !holders.is_empty()
        && holders.iter().all(|d| f.publisher_devices.contains(*d))
        && holders.iter().all(|d| !presence.is_online_serving(f.project_id, d))
}
```

```rust
// collab/live/outbox.rs (core of it)
pub fn coalesce(rows: &[OutboxRow]) -> Coalesced {
    let mut last: std::collections::BTreeMap<&str, ClaimOp> = Default::default();
    let mut max_seq = 0;
    for r in rows {
        last.insert(r.frame_uuid.as_str(), r.op);
        max_seq = max_seq.max(r.seq);
    }
    let mut add = Vec::new();
    let mut remove = Vec::new();
    for (uuid, op) in last {
        match op {
            ClaimOp::Add { content_version } => add.push(ClaimWire { uuid: uuid.to_string(), content_version }),
            ClaimOp::Remove => remove.push(uuid.to_string()),
        }
    }
    Coalesced { add, remove, max_seq }
}

pub fn delta_report(rows: &[OutboxRow], claims: &[(String, i32)]) -> anyhow::Result<HoldersReportWire> {
    let c = coalesce(rows);
    let d = ClaimDigest::of_claims(claims.iter().map(|(u, v)| (u.as_str(), *v)))?;
    Ok(HoldersReportWire { report_seq: c.max_seq.max(1), full: false, add: c.add, remove: c.remove, digest: d.hex(), count: d.count })
}

pub fn full_report(claims: &[(String, i32)], report_seq: i64) -> anyhow::Result<HoldersReportWire> {
    let d = ClaimDigest::of_claims(claims.iter().map(|(u, v)| (u.as_str(), *v)))?;
    Ok(HoldersReportWire {
        report_seq: report_seq.max(1),
        full: true,
        add: claims.iter().map(|(u, v)| ClaimWire { uuid: u.clone(), content_version: *v }).collect(),
        remove: Vec::new(),
        digest: d.hex(),
        count: d.count,
    })
}

pub fn digest_check(claims: &[(String, i32)], report_seq: i64) -> anyhow::Result<HoldersReportWire> {
    let d = ClaimDigest::of_claims(claims.iter().map(|(u, v)| (u.as_str(), *v)))?;
    Ok(HoldersReportWire { report_seq: report_seq.max(1), full: false, add: vec![], remove: vec![], digest: d.hex(), count: d.count })
}

pub struct FlushClock {
    next_flush: Duration,
    first_pending: Option<Instant>,
}

impl FlushClock {
    pub fn new() -> Self { Self { next_flush: DEFAULT_FLUSH, first_pending: None } }
    pub fn on_append(&mut self, now: Instant) { self.first_pending.get_or_insert(now); }
    pub fn due(&self, now: Instant, pending: usize) -> bool {
        pending >= FLUSH_AT_ENTRIES || self.first_pending.is_some_and(|t| now >= t + self.next_flush)
    }
    pub fn flushed(&mut self, next_flush_ms: u64) {
        self.next_flush = Duration::from_millis(next_flush_ms.max(1));
        self.first_pending = None;
    }
    pub fn deadline(&self) -> Option<Instant> { self.first_pending.map(|t| t + self.next_flush) }
}
```

- [ ] **Step 4: Write the failing integration tests** (`api/collab_live/holdings.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::fake_hub::FakeHub;
    use crate::db::collab_live as live_db;

    async fn rig() -> (tempfile::TempDir, Arc<ServiceContext>, FakeHub, Holdings) {
        let (tmp, ctx) = crate::api::collab_exchange::test_support::test_ctx();
        let ctx = Arc::new(ctx);
        let hub = FakeHub::start().await;
        hub.add_account("tok", "acc-me", "Me", "AAA=", None);
        hub.add_account("tok-o", "acc-o", "Other", "BBB=", None);
        hub.add_project("p1", "m31", &[("acc-me", "send_receive", false), ("acc-o", "send", false)], false);
        hub.seed_frames("p1", "acc-o", &["u1", "u2"], "published");
        crate::api::collab_exchange::test_support::wire_hub(&ctx, &hub, "tok");
        crate::api::collab::refresh_projects(&ctx).await.unwrap();
        crate::api::collab_exchange::sync_manifest(&ctx, "p1", None, None).await.unwrap();
        let h = Holdings::load(Arc::clone(&ctx), CollabClient::new(hub.uri()).unwrap(), "tok".into(), "AAA=".into()).unwrap();
        (tmp, ctx, hub, h)
    }

    #[tokio::test]
    async fn snapshot_persists_and_reloads_without_a_second_snapshot() {
        let (_t, ctx, hub, mut h) = rig().await;
        h.snapshot("p1", "epoch-1").await.unwrap();
        assert_eq!(h.map("p1").unwrap().claimants(1, 1), vec!["BBB="]);
        let h2 = Holdings::load(Arc::clone(&ctx), CollabClient::new(hub.uri()).unwrap(), "tok".into(), "AAA=".into()).unwrap();
        assert_eq!(h2.map("p1"), h.map("p1"));
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert!(crate::db::collab::get_project(&conn, "p1").unwrap().unwrap().holder_seq >= 1);
        // the snapshot also filled frame_seq on the manifest rows
        assert_eq!(crate::db::collab_frames::get(&conn, "p1", "u2").unwrap().unwrap().frame_seq, Some(2));
    }

    #[tokio::test]
    async fn a_state_change_flushes_with_a_matching_digest_and_acks_the_outbox() {
        let (_t, ctx, hub, mut h) = rig().await;
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            crate::db::collab_frames::set_local_state(&conn, "p1", "u1", crate::db::collab_frames::LocalState::Held).unwrap();
        }
        h.note_append("p1", Instant::now());
        let out = h.flush("p1").await.unwrap();
        assert_eq!(out, FlushOutcome { sent: 1, digest_match: true, refused: 0 });
        assert_eq!(hub.holders_of("p1", "u1"), vec!["AAA=".to_string(), "BBB=".to_string()]);
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert_eq!(live_db::outbox_len(&conn, "p1").unwrap(), 0);
    }

    #[tokio::test]
    async fn a_digest_mismatch_sends_one_full_report_that_repairs_the_hub() {
        let (_t, ctx, hub, mut h) = rig().await;
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            // a claim the hub never heard about (the local outbox was lost)
            live_db::add_implicit_claim(&conn, "p1", "u2", 1).unwrap();
        }
        assert!(!h.digest_check("p1").await.unwrap()); // mismatch → full report inside
        assert!(hub.holders_of("p1", "u2").contains(&"AAA=".to_string()));
        assert!(h.digest_check("p1").await.unwrap());
    }

    #[tokio::test]
    async fn refused_claims_leave_the_local_claim_set() {
        let (_t, ctx, _hub, mut h) = rig().await;
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            live_db::record_claim_change(&conn, "p1", "ghost", crate::db::collab_live::ClaimOp::Add { content_version: 1 }).unwrap();
        }
        let out = h.flush("p1").await.unwrap();
        assert_eq!(out.refused, 1);
        assert!(out.digest_match);
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert!(live_db::my_claims(&conn, "p1").unwrap().is_empty());
    }

    #[tokio::test]
    async fn delta_resume_and_410_falls_back_to_the_snapshot() {
        let (_t, ctx, hub, mut h) = rig().await;
        h.snapshot("p1", "epoch-1").await.unwrap();
        hub.seed_frames("p1", "acc-o", &["u3"], "published");
        h.delta_resume("p1", "epoch-1").await.unwrap();
        assert_eq!(h.map("p1").unwrap().claimants(3, 1), vec!["BBB="]);
        // a cursor ahead of the hub → 410 → snapshot
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            crate::db::collab::set_holder_seq(&conn, "p1", "epoch-1", 999).unwrap();
        }
        h.delta_resume("p1", "epoch-1").await.unwrap();
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert!(crate::db::collab::get_project(&conn, "p1").unwrap().unwrap().holder_seq < 999);
    }

    #[tokio::test]
    async fn a_hub_outage_keeps_the_outbox_for_later() {
        let (_t, ctx, hub, mut h) = rig().await;
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            crate::db::collab_frames::set_local_state(&conn, "p1", "u1", crate::db::collab_frames::LocalState::Held).unwrap();
        }
        hub.set_failing("/holders/self", true);
        assert!(h.flush("p1").await.is_err());
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert_eq!(live_db::outbox_len(&conn, "p1").unwrap(), 1);
        drop(conn);
        hub.set_failing("/holders/self", false);
        assert_eq!(h.flush("p1").await.unwrap().sent, 1);
    }
}
```

- [ ] **Step 5: `api/collab_live/holdings.rs`.**
  - `load`: for every live project, `load_holders` → `ProjectHolders::from_rows`;
    a `FlushClock` per project with pending outbox rows started "now" (so a
    restart flushes what was left).
  - `snapshot(pid, epoch)`: `with_retry("holders snapshot", Background, …)`;
    in ONE transaction: `replace_holders`, `set_frame_seq` for every
    snapshot frame, `set_holder_seq(pid, epoch, snap.holder_seq)`; then
    replace the in-memory map. `info!(project_id, holder_seq, count = devices, "holder snapshot loaded")`.
    Returns `[ProvidersChanged(pid)]`.
  - `delta_resume(pid, epoch)`: from the stored `holder_seq` (`< 0` →
    `snapshot`), page `holders_since(since, after, Some(epoch))`; per page, one
    transaction applies `apply_holder_delta` for every device delta; the
    cursor is set to the FIRST page's `holderSeq` after the last page
    (contract). `Gone("epoch_changed")` → return
    `Err(ApiError::Conflict("epoch_changed".into()))` (the session turns it
    into an epoch change); any other `Gone` → `snapshot`.
  - `on_holders_event` (the `HolderSide` impl): `step(stored, ev.prev, ev.seq)`:
    `Apply` → one transaction (every delta + `set_holder_seq(ev.seq)`), then
    memory; `Ignore` → nothing; `CatchUp` → `delta_resume`. Returns
    `[ProvidersChanged(pid)]` when anything applied.
  - `on_hello_project(pid, hp, plan, epoch)`: `Snapshot` → `snapshot`;
    `Delta` → `delta_resume`; `InSync` → nothing. Then compare
    `(hp.claim_count, hp.claim_digest)` with the local claim set's digest;
    mismatch → `full_report(pid)` (`info!(project_id, count, "claim digest mismatch after hello; sending the full claim set")`).
  - `flush(pid)`: read `outbox` + `my_claims` in one read transaction;
    nothing pending → `Ok(sent: 0, digest_match: true)`; build
    `delta_report`; `report_holders` WITHOUT retry (the session re-schedules
    with the per-project `Backoff`); on success: `ack_outbox(max_seq)`,
    `drop_claims(refused)` + `set_error(.., Some("claim refused by the hub"))`
    per refused frame (`warn!`), `clock.flushed(next_flush_ms)`; if
    `!digest_match` → `full_report`. `debug!(project_id, count, holder_seq, digest_match, "holdings flushed")`.
    On error: `warn!` once per outage (latched per project), keep the rows,
    back off.
  - `full_report(pid)`: `R = current_report_seq`; `full_report(claims, R)`;
    send; on success `ack_outbox(R)` and handle `refused` as above.
  - `digest_check(pid)`: `digest_check(claims, current_report_seq)`; mismatch
    → `full_report` and return `false`.
  - `hourly_digest_checks(now)`: once `DIGEST_CHECK_EVERY` has passed, a
    `digest_check` per live project; failures are `warn!`ed and retried next
    hour.
  - `flush_project_now(ctx, pid)`: builds a one-off `Holdings` for that
    project (`load`) and calls `flush` under `with_retry(Interactive)`.
  - `member_devices(project)`: parse `members_json` as
    `Vec<collab::snapshot::SnapshotMember>`, collect every `nodes[]` string
    (already standard base64); a parse error is `warn!`ed and yields the
    empty set (fail closed: no providers).

- [ ] **Step 6: Implicit claims in publish** (`api/collab.rs` `run_publish`):
  - After a successful announce batch (:2909-2918): for every frame in the
    batch, `db::collab_live::add_implicit_claim(conn, pid, uuid, 1)` in the
    same transaction that records the own row (the row write happens in the
    existing step-7 block; add the claim there, per frame).
  - R8 adopted frames (`hub_adopted`): `record_claim_change(Add { content_version: row.content_version })`
    — the hub wrote no claim for this device.
  - Step 6 (versions): first `crate::api::collab_live::holdings::flush_project_now(ctx, pid).await`
    (a failure is `warn!`ed and the versions still go out — the hub keeps
    the highest `reportSeq` per frame, so a late flush is harmless). Then
    replace the per-frame `new_frame_version` loop with batches of ≤ 500
    through `client.frame_versions` (`VersionInWire { uuid,
    expected_version: row.content_version /* re-read under C1c */, blake3,
    byte_size, xxh3 }`); per result: `ok` → the existing success path
    (`set_own_version`, re-tag when the hub's number differs) +
    `add_implicit_claim(uuid, result.content_version)`; `conflict` →
    `warn!(project_id, frame_uuid, content_version, "publish: version conflict; the hub has a newer version")`,
    `unstage_updates` for that frame, `held_back` reason
    `"version conflict: the hub has content version {n}"`, and
    `collab_autopublish::request_auto_publish(Some(pid))` after the run;
    `not_found`/`forbidden` → `record_failed(f, "versioned", …)`.
  - Delete the step-7 `put_holders` delta (:3296-3313).
  - Publish tests (extend `api/collab.rs` `tests::publish`): after a publish,
    `my_claims` holds `(uuid, 1)` for each announced frame, `outbox_len == 0`,
    and `hub.holder_writes()` did not grow beyond the announce's implicit
    claims; a republish with changed bytes records `(uuid, 2)` implicitly; a
    fake-hub version conflict (`hub.update_frame(pid, uuid, |f| f.content_version = 5)`)
    holds the frame back with the conflict reason.

- [ ] **Step 7: Run** `cargo test -p athenaeum-core --lib collab::live api::collab_live api::collab::tests::publish && cargo check --workspace --all-targets && cargo check -p athenaeum-core --no-default-features`. Expected: PASS.

- [ ] **Step 8: Commit**

```bash
rustfmt crates/athenaeum-core/src/collab/live/*.rs crates/athenaeum-core/src/api/collab_live/*.rs crates/athenaeum-core/src/api/collab.rs
git add -A crates/athenaeum-core/src
git commit -m "feat(collab): holder map with delta resume, derived providers, outbox flush with digest reconciliation, implicit claims and CAS batch versions in publish"
```

---
### Task 7: Storage marker, availability, device-replace core (§9.1, §9.5, L9, P22, P29)

**Implementer:** rust-engineer.

**Files:**
- Create: `crates/athenaeum-core/src/collab/storage/mod.rs` (`pub mod marker;` — Tasks 8/9 add theirs), `collab/storage/marker.rs`. Register `pub mod storage;` in `collab/mod.rs`.
- Create: `crates/athenaeum-core/src/api/collab_live/replace.rs`; `pub mod replace;` in `api/collab_live/mod.rs`.
- Modify: `crates/athenaeum-core/src/api/scan_roots.rs` — `set_collaboration_dir` (:856-888) writes the marker after a successful designation + mount; `clear_collaboration_dir` (:1090) forgets the recorded store.
- Modify: `crates/athenaeum-core/src/api/collab_exchange.rs` — `ensure_collab_store` (:1330-1393) checks the store before mounting.
- Modify: `crates/athenaeum-core/src/account/client.rs` `revoke_device` (:233-251) gains `retire: bool`; `crates/athenaeum-core/src/api/account.rs` gains `revoke_device_retire` beside `revoke_device` (:352).
- Test: unit tests in `marker.rs`; integration tests in `replace.rs` over `FakeHub` (+ a `GET /devices` + `POST /devices/{id}/revoke` route added to the fake in this task).

**Interfaces:**
- Consumes: `db::collab_live::{meta_get, meta_set, META_STORE_ID, META_STORE_DEVICE}` (Task 1), `FakeHub` (Task 3).
- Produces:
  ```rust
  // collab/storage/marker.rs
  pub const MARKER_REL: &str = ".athenaeum/store-id";
  pub const WRITE_PROBE_REL: &str = ".athenaeum/.write-probe";
  pub const REPLACE_PROMPT_AFTER: chrono::Duration = chrono::Duration::days(7);
  pub const RETIRE_PROPOSAL_AFTER: chrono::Duration = chrono::Duration::days(30);
  #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)] #[serde(rename_all = "camelCase")]
  pub struct StoreMarker { pub store_id: String, pub device_id: String }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum UnavailableReason { PathMissing, NotADirectory, MarkerMissing, MarkerMismatch, OtherDevice { device_id: String } }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum StoreState { Available, ReadOnly, Unavailable(UnavailableReason) }
  impl StoreState { pub fn serving(&self) -> bool; pub fn fetching(&self) -> bool; }  // serving: Available|ReadOnly; fetching: Available
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum CheckOutcome { State(StoreState), Adopt(StoreMarker) }   // Adopt = no store recorded yet: record this marker (writing it when absent)
  pub fn read_marker(root: &Path) -> std::io::Result<Option<StoreMarker>>;
  pub fn write_marker(root: &Path, m: &StoreMarker) -> std::io::Result<()>;             // .athenaeum must exist or is created INSIDE an existing root; tmp + rename
  pub fn check_store(root: &Path, recorded: Option<&StoreMarker>, me: &str) -> CheckOutcome;
  pub fn offer_flags(last_seen: Option<chrono::DateTime<chrono::Utc>>, now: chrono::DateTime<chrono::Utc>) -> (Option<i64>, bool, bool); // (offline_days, prompt, propose_retire)
  pub struct StoreGuard { /* root, me, recorded: Mutex<Option<StoreMarker>>, state: RwLock<StoreState> */ }
  impl StoreGuard {
      pub fn new(root: PathBuf, me: String, recorded: Option<StoreMarker>) -> Self;
      pub fn check_now(&self) -> StoreState;                // re-evaluates; logs every state change at warn/info
      pub fn state(&self) -> StoreState;                    // last result, never blocks on I/O
      pub fn root(&self) -> &Path;
      pub fn take_adoption(&self) -> Option<StoreMarker>;   // the marker to record after an Adopt outcome
  }
  // api/collab_live/replace.rs
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct ReplaceOffer { pub device_id: String, pub device_pubkey: String, pub device_name: String, pub last_seen_at: Option<String>, pub offline_days: Option<i64>, pub prompt: bool, pub propose_retire: bool }
  pub async fn replace_offer(ctx: &ServiceContext, marker_device: &str) -> Result<Option<ReplaceOffer>, ApiError>; // None = not a device of this account (a foreign store)
  #[derive(Debug, Clone, PartialEq, Eq)] pub struct ReplaceOutcome { pub scanned: usize, pub adopted: usize }
  pub async fn replace_device(ctx: &ServiceContext, device_id: &str) -> Result<ReplaceOutcome, ApiError>;
  pub(crate) async fn adopt_by_hash(ctx: &ServiceContext, node: &SharedIrohNode, path: &Path) -> Result<Option<(String, String)>, ApiError>; // (project_id, frame_uuid) adopted as held/own_held
  // account
  pub async fn revoke_device(&self, token: &str, device_id: &str, retire: bool) -> Result<(), AccountClientError>; // client
  pub async fn revoke_device_retire(ctx: &ServiceContext, device_id: String) -> Result<(), ApiError>;           // api::account
  ```

- [ ] **Step 1: Write the failing tests** (`collab/storage/marker.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn m(store: &str, dev: &str) -> StoreMarker { StoreMarker { store_id: store.into(), device_id: dev.into() } }

    #[test]
    fn missing_path_and_file_root_are_unavailable_and_never_recreated() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("gone");
        assert_eq!(check_store(&gone, Some(&m("s", "ME")), "ME"), CheckOutcome::State(StoreState::Unavailable(UnavailableReason::PathMissing)));
        assert!(!gone.exists());
        let file = tmp.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(check_store(&file, Some(&m("s", "ME")), "ME"), CheckOutcome::State(StoreState::Unavailable(UnavailableReason::NotADirectory)));
    }

    #[test]
    fn marker_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // nothing recorded yet (a wave-2 root): adopt, write a marker naming me
        let CheckOutcome::Adopt(new) = check_store(root, None, "ME") else { panic!("adopt") };
        assert_eq!(new.device_id, "ME");
        write_marker(root, &new).unwrap();
        assert_eq!(read_marker(root).unwrap(), Some(new.clone()));
        assert_eq!(check_store(root, Some(&new), "ME"), CheckOutcome::State(StoreState::Available));
        // another disk mounted at the same path
        assert_eq!(check_store(root, Some(&m("other-store", "ME")), "ME"),
            CheckOutcome::State(StoreState::Unavailable(UnavailableReason::MarkerMismatch)));
        // a recorded store whose marker vanished
        std::fs::remove_file(root.join(MARKER_REL)).unwrap();
        assert_eq!(check_store(root, Some(&new), "ME"),
            CheckOutcome::State(StoreState::Unavailable(UnavailableReason::MarkerMissing)));
        // another device's root (two machines on one NAS folder, or a reinstall)
        write_marker(root, &m(&new.store_id, "OTHER")).unwrap();
        assert_eq!(check_store(root, Some(&new), "ME"),
            CheckOutcome::State(StoreState::Unavailable(UnavailableReason::OtherDevice { device_id: "OTHER".into() })));
        assert_eq!(check_store(root, None, "ME"),
            CheckOutcome::State(StoreState::Unavailable(UnavailableReason::OtherDevice { device_id: "OTHER".into() })));
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_root_still_serves() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mk = m("s", "ME");
        write_marker(root, &mk).unwrap();
        let athenaeum = root.join(".athenaeum");
        std::fs::set_permissions(&athenaeum, std::fs::Permissions::from_mode(0o555)).unwrap();
        let got = check_store(root, Some(&mk), "ME");
        std::fs::set_permissions(&athenaeum, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(got, CheckOutcome::State(StoreState::ReadOnly));
        assert!(StoreState::ReadOnly.serving() && !StoreState::ReadOnly.fetching());
    }

    #[test]
    fn replace_prompt_after_seven_days_and_retire_proposal_after_thirty() {
        let now = chrono::Utc::now();
        assert_eq!(offer_flags(Some(now - chrono::Duration::days(2)), now), (Some(2), false, false));
        assert_eq!(offer_flags(Some(now - chrono::Duration::days(8)), now), (Some(8), true, false));
        assert_eq!(offer_flags(Some(now - chrono::Duration::days(31)), now), (Some(31), true, true));
        assert_eq!(offer_flags(None, now), (None, true, false));
    }

    #[test]
    fn the_guard_records_state_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let mk = m("s", "ME");
        write_marker(tmp.path(), &mk).unwrap();
        let g = StoreGuard::new(tmp.path().to_path_buf(), "ME".into(), Some(mk));
        assert_eq!(g.check_now(), StoreState::Available);
        std::fs::remove_file(tmp.path().join(MARKER_REL)).unwrap();
        assert_eq!(g.check_now(), StoreState::Unavailable(UnavailableReason::MarkerMissing));
        assert_eq!(g.state(), StoreState::Unavailable(UnavailableReason::MarkerMissing));
    }
}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib collab::storage::marker`. Expected: compile errors.

- [ ] **Step 3: `collab/storage/marker.rs`**

```rust
//! The Collaboration root's storage marker (spec §9.1, plan P22): a random
//! store id plus the device that designated it. Unmounted is not deleted —
//! an unavailable store stops serving and fetching and changes no frame.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

pub const MARKER_REL: &str = ".athenaeum/store-id";
pub const WRITE_PROBE_REL: &str = ".athenaeum/.write-probe";
pub const REPLACE_PROMPT_AFTER: chrono::Duration = chrono::Duration::days(7);
pub const RETIRE_PROPOSAL_AFTER: chrono::Duration = chrono::Duration::days(30);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreMarker {
    pub store_id: String,
    pub device_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnavailableReason {
    PathMissing,
    NotADirectory,
    MarkerMissing,
    MarkerMismatch,
    OtherDevice { device_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreState {
    Available,
    ReadOnly,
    Unavailable(UnavailableReason),
}

impl StoreState {
    /// A read-only remount must not take a full replica off the swarm.
    pub fn serving(&self) -> bool {
        matches!(self, StoreState::Available | StoreState::ReadOnly)
    }
    pub fn fetching(&self) -> bool {
        matches!(self, StoreState::Available)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    State(StoreState),
    Adopt(StoreMarker),
}

pub fn read_marker(root: &Path) -> std::io::Result<Option<StoreMarker>> {
    match std::fs::read(root.join(MARKER_REL)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Write the marker atomically (`.tmp` + rename). Creates `.athenaeum` only
/// inside an EXISTING root — never the root itself.
pub fn write_marker(root: &Path, m: &StoreMarker) -> std::io::Result<()> {
    if !root.is_dir() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "collaboration root is not an existing folder"));
    }
    let dir = root.join(".athenaeum");
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join("store-id.tmp");
    std::fs::write(&tmp, serde_json::to_vec(m).map_err(std::io::Error::other)?)?;
    std::fs::rename(&tmp, root.join(MARKER_REL))
}

fn writable(root: &Path) -> bool {
    let probe = root.join(WRITE_PROBE_REL);
    match std::fs::write(&probe, b"probe") {
        Ok(()) => {
            if let Err(e) = std::fs::remove_file(&probe) {
                tracing::warn!(path = %probe.display(), error = %e, "collaboration write probe left behind");
            }
            true
        }
        Err(_) => false,
    }
}

pub fn check_store(root: &Path, recorded: Option<&StoreMarker>, me: &str) -> CheckOutcome {
    use CheckOutcome::*;
    use StoreState::*;
    use UnavailableReason::*;
    if !root.exists() {
        return State(Unavailable(PathMissing));
    }
    if !root.is_dir() {
        return State(Unavailable(NotADirectory));
    }
    let marker = match read_marker(root) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(path = %root.display(), error = %e, "collaboration store marker unreadable");
            return State(Unavailable(MarkerMissing));
        }
    };
    match (marker, recorded) {
        (None, None) => Adopt(StoreMarker { store_id: uuid::Uuid::new_v4().to_string(), device_id: me.to_string() }),
        (None, Some(_)) => State(Unavailable(MarkerMissing)),
        (Some(m), _) if m.device_id != me => State(Unavailable(OtherDevice { device_id: m.device_id })),
        (Some(m), None) => Adopt(m),
        (Some(m), Some(r)) if m.store_id != r.store_id => State(Unavailable(MarkerMismatch)),
        (Some(_), Some(_)) => State(if writable(root) { Available } else { ReadOnly }),
    }
}

pub fn offer_flags(last_seen: Option<chrono::DateTime<chrono::Utc>>, now: chrono::DateTime<chrono::Utc>) -> (Option<i64>, bool, bool) {
    match last_seen {
        None => (None, true, false),
        Some(t) => {
            let off = now - t;
            (Some(off.num_days()), off > REPLACE_PROMPT_AFTER, off > RETIRE_PROPOSAL_AFTER)
        }
    }
}

pub struct StoreGuard {
    root: PathBuf,
    me: String,
    recorded: Mutex<Option<StoreMarker>>,
    adoption: Mutex<Option<StoreMarker>>,
    state: RwLock<StoreState>,
}

impl StoreGuard {
    pub fn new(root: PathBuf, me: String, recorded: Option<StoreMarker>) -> Self {
        Self { root, me, recorded: Mutex::new(recorded), adoption: Mutex::new(None), state: RwLock::new(StoreState::Unavailable(UnavailableReason::MarkerMissing)) }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn state(&self) -> StoreState {
        self.state.read().expect("store guard poisoned").clone()
    }

    pub fn take_adoption(&self) -> Option<StoreMarker> {
        self.adoption.lock().expect("store guard poisoned").take()
    }

    pub fn check_now(&self) -> StoreState {
        let recorded = self.recorded.lock().expect("store guard poisoned").clone();
        let next = match check_store(&self.root, recorded.as_ref(), &self.me) {
            CheckOutcome::State(s) => s,
            CheckOutcome::Adopt(m) => match read_marker(&self.root) {
                Ok(Some(_)) => {
                    *self.recorded.lock().expect("store guard poisoned") = Some(m.clone());
                    *self.adoption.lock().expect("store guard poisoned") = Some(m);
                    StoreState::Available
                }
                _ => match write_marker(&self.root, &m) {
                    Ok(()) => {
                        tracing::info!(store_id = %m.store_id, path = %self.root.display(), "collaboration store marker written");
                        *self.recorded.lock().expect("store guard poisoned") = Some(m.clone());
                        *self.adoption.lock().expect("store guard poisoned") = Some(m);
                        StoreState::Available
                    }
                    Err(e) => {
                        tracing::warn!(path = %self.root.display(), error = %e, "collaboration store marker could not be written");
                        StoreState::ReadOnly
                    }
                },
            },
        };
        let mut cur = self.state.write().expect("store guard poisoned");
        if *cur != next {
            match &next {
                StoreState::Available => tracing::info!(path = %self.root.display(), "collaboration storage available"),
                other => tracing::warn!(path = %self.root.display(), state = ?other, "collaboration storage not available"),
            }
            *cur = next.clone();
        }
        next
    }
}
```

- [ ] **Step 4: Wire the marker in.**
  - `set_collaboration_dir` (scan_roots.rs:856): after the mount succeeded,
    build a `StoreGuard` with the recorded marker (`meta_get(META_STORE_ID)` +
    `meta_get(META_STORE_DEVICE)`), `check_now()`; an `Adopt` result is
    recorded with `meta_set` for both keys; `Unavailable(OtherDevice)` →
    undo the designation exactly as a failed mount does (the existing
    `undo_collaboration_designation` path) and return
    `ApiError::Conflict("collab_other_device: this Collaboration folder belongs to another device of your account")`
    (Task 16/17 turn the prefix into the replace prompt). A NEW root (no
    marker, nothing recorded, or a recorded store for a different path) gets a
    fresh marker: when the designated path differs from the recorded store's
    path, forget the recorded ids first (`clear_collaboration_dir` does the
    same: `DELETE FROM collab_live_meta WHERE key IN ('store_id','store_device')`).
  - `ensure_collab_store` (CE:1330): before `set_collab_root`, run the same
    check; mount only when the state is `serving()`; otherwise `warn!` once
    per state (latched in `COLLAB_MOUNT_ATTEMPTS`) and return `None`.
  - Device id = standard base64 of the node id
    (`base64::engine::general_purpose::STANDARD.encode(node.node_id())`),
    the hub's `device` encoding (P3).

- [ ] **Step 5: Retire flag.** `CollabClient`'s sibling `HubClient::revoke_device(token, id, retire)`
  sends `{"retire": true}` when `retire`, no body otherwise; the existing
  `api::account::revoke_device` passes `false`; new
  `api::account::revoke_device_retire(ctx, device_id)` passes `true` (same
  self-revoke handling, `info!(device_id, retire = true, "device retired")`).

- [ ] **Step 6: `api/collab_live/replace.rs`** — tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_offline_account_device_is_offered_and_a_foreign_device_is_not() {
        let (_t, ctx, hub) = crate::api::collab_live::test_support::signed_in_rig().await;
        hub.add_device("acc-me", "OLD-DEV", "old-id", "Old laptop", Some(chrono::Utc::now() - chrono::Duration::days(9)));
        let offer = replace_offer(&ctx, "OLD-DEV").await.unwrap().unwrap();
        assert_eq!((offer.device_id.as_str(), offer.prompt, offer.propose_retire), ("old-id", true, false));
        assert!(replace_offer(&ctx, "SOMEONE-ELSE").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn replacing_retires_rewrites_the_marker_and_adopts_files_by_hash() {
        let (_t, ctx, hub) = crate::api::collab_live::test_support::signed_in_rig().await;
        let root = crate::api::collab_live::test_support::collab_root(&ctx);
        hub.add_device("acc-me", "OLD-DEV", "old-id", "Old laptop", None);
        // the old device's landed replica is already in the folder
        let (pid, uuid, path) = crate::api::collab_live::test_support::seed_replica_file(&ctx, &hub, &root).await;
        write_marker(&root, &StoreMarker { store_id: "s1".into(), device_id: "OLD-DEV".into() }).unwrap();
        let out = replace_device(&ctx, "old-id").await.unwrap();
        assert_eq!(out.adopted, 1);
        assert!(hub.device_retired("old-id"));
        assert_eq!(read_marker(&root).unwrap().unwrap().device_id, crate::api::collab_live::test_support::my_device(&ctx).await);
        let conn = crate::api::db(&ctx).unwrap().conn();
        let row = crate::db::collab_frames::get(&conn, &pid, &uuid).unwrap().unwrap();
        assert_eq!(row.local_state, crate::db::collab_frames::LocalState::Held);
        assert_eq!(row.landed_path.as_deref(), Some(path.to_string_lossy().as_ref()));
        assert_eq!(crate::db::collab_live::outbox_len(&conn, &pid).unwrap(), 1); // one add, zero transfer
    }
}
```

  Add to the fake hub: `add_device(account, pubkey_b64, id, name, last_seen: Option<DateTime<Utc>>)`,
  `GET /api/v1/devices` (the account's non-retired devices:
  `[{id, name, pubkey, capability:"athenaeum", createdAt, lastSeenAt}]`),
  `POST /api/v1/devices/{id}/revoke` (optional `{"retire":bool}`; runs the
  same effects as `revoke_device`), `device_retired(id) -> bool`. Add the
  `api/collab_live/test_support.rs` module (`#[cfg(test)]`, shared by Tasks
  7–18): `signed_in_rig()` (ctx + fake hub + a bound node with relay
  disabled + a designated `<tmp>/Collab` + project `p1`), `collab_root(ctx)`,
  `my_device(ctx)`, `seed_replica_file(ctx, hub, root) -> (pid, uuid, PathBuf)`
  (seeds a published frame on the hub from a real small file, syncs the
  manifest, copies the file to `<root>/m31/other/<name>`, returns its path).

  Implementation:
  - `replace_offer(ctx, marker_device)`: `api::account::list_devices(ctx)`;
    find the device whose `pubkey == marker_device`; none → `Ok(None)`;
    else `offer_flags(parse(last_seen_at), now)` → `ReplaceOffer`.
  - `replace_device(ctx, device_id)`: `revoke_device_retire` → read the
    marker (keep its `store_id`) → `write_marker(root, {store_id, me})` →
    `meta_set` both keys → `ensure_collab_store` → walk the root with
    `walkdir` (skip `.athenaeum`, `*.athtmp`), `adopt_by_hash` per file →
    `info!(scanned, count = adopted, "collaboration folder re-adopted after a device replace")`.
  - `adopt_by_hash(ctx, node, path)`: size + xxh3 (`package::xxh3_full_file`
    on `spawn_blocking`) → `find_by_project_and_xxh3` over every live project
    (`frames_db::project_ids`) → candidates whose `byte_size` matches and
    whose state is `wanted`, `missing`, `awaiting_choice`, `not_kept` (the
    match is always against the current version's hash — L6 "user puts the
    file back") or `own_missing`; an `idle` row only gets its path recorded
    (an excluded frame is never served — its state stays `idle`); `node.seed_project_frame(pid, uuid, cv, path)` (reference
    import — the returned BLAKE3 must equal `row.blake3`, else skip with a
    `warn!`); then under `project_disk_lock`, one transaction:
    `update_landed_path`, `set_size_mtime_seen`, and the state through
    `collab::storage::states::transition` with `FileBack` (replicas; `PutBack`
    for `not_kept`) or `FileBack` (own) — Task 9 adds the pure transition;
    until then this task writes `set_local_state(Held | OwnHeld)` directly,
    which is the same result (→ outbox `add`). Returns `Some((pid, uuid))`.

- [ ] **Step 7: Run** `cargo test -p athenaeum-core --lib collab::storage::marker api::collab_live::replace api::scan_roots && cargo check --workspace --all-targets && cargo check -p athenaeum-core --no-default-features`. Expected: PASS (the existing `set_collaboration_dir` tests still pass: a fresh folder gets a marker; the R5 promotion tests are unaffected).

- [ ] **Step 8: Commit**

```bash
rustfmt crates/athenaeum-core/src/collab/storage/*.rs crates/athenaeum-core/src/api/collab_live/*.rs crates/athenaeum-core/src/api/scan_roots.rs crates/athenaeum-core/src/api/collab_exchange.rs crates/athenaeum-core/src/account/client.rs crates/athenaeum-core/src/api/account.rs crates/athenaeum-core/src/collab/fake_hub.rs
git add -A crates/athenaeum-core/src
git commit -m "feat(collab): storage marker (available / read-only / unavailable, another device's root refused), device replace with retire and re-adoption by hash"
```

---
### Task 8: File watcher, settle window, canary, stat sweep (§9.2, P23, P24)

**Implementer:** rust-engineer.

**Files:**
- Create: `crates/athenaeum-core/src/collab/storage/watch.rs`, `collab/storage/sweep.rs`; register both in `collab/storage/mod.rs`.
- Test: unit tests in both files (a real-watcher test on a temp dir included).

**Interfaces:**
- Consumes: `geometry::ransac::SplitMix64`.
- Produces:
  ```rust
  // collab/storage/watch.rs
  pub const AGGREGATE: Duration = Duration::from_secs(10);
  pub const SETTLE: Duration = Duration::from_secs(60);
  pub const CANARY_REL: &str = ".athenaeum/canary";
  pub const CANARY_EVERY: Duration = Duration::from_secs(300);
  pub const CANARY_DEADLINE: Duration = Duration::from_secs(30);
  #[derive(Debug, Clone, PartialEq, Eq)] pub enum FsSignal { Touched(PathBuf), Root, Canary, WatchError(String) }
  #[derive(Debug, Default, Clone, PartialEq, Eq)] pub struct Drained { pub changed: Vec<PathBuf>, pub removed: Vec<PathBuf>, pub root_touched: bool }
  #[derive(Debug, Default)] pub struct Aggregator { /* touched, window_started, pending_removals, aggregate, settle */ }
  impl Aggregator {
      pub fn with_timings(aggregate: Duration, settle: Duration) -> Self;   // tests and the e2e shorten both; Default = AGGREGATE / SETTLE
      pub fn observe(&mut self, sig: &FsSignal, now: Instant);
      pub fn drain(&mut self, now: Instant, exists: impl Fn(&Path) -> bool) -> Drained;
      pub fn next_deadline(&self) -> Option<Instant>;
      pub fn pending_removal(&self, path: &Path) -> bool;
  }
  pub fn is_ignored(root: &Path, path: &Path) -> bool;
  #[derive(Debug, Default)] pub struct Canary { /* written_at, seen, dead */ }
  impl Canary { pub fn due(&self, now: Instant) -> bool; pub fn wrote(&mut self, now: Instant); pub fn observed(&mut self); pub fn dead(&mut self, now: Instant) -> bool; }
  pub fn write_canary(root: &Path) -> std::io::Result<()>;
  pub fn spawn_watcher(root: &Path, tx: tokio::sync::mpsc::UnboundedSender<FsSignal>) -> Option<notify::RecommendedWatcher>;
  // collab/storage/sweep.rs
  pub const MTIME_TOLERANCE_SECS: i64 = 2;
  pub const SWEEP_HEALTHY: Duration = Duration::from_secs(3600);
  pub const SWEEP_DEGRADED: Duration = Duration::from_secs(300);
  pub const SWEEP_JITTER: f64 = 0.25;
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub struct Stamp { pub size: u64, pub mtime: i64 }
  impl Stamp { pub fn of(meta: &std::fs::Metadata) -> Stamp; pub fn parse(s: &str) -> Option<Stamp>; pub fn encode(&self) -> String; pub fn matches(&self, other: &Stamp) -> bool; }
  #[derive(Debug, Clone, PartialEq, Eq)] pub enum StatVerdict { Same, Drifted(Stamp), Missing, Unreadable(String) }
  pub fn stat_verdict(path: &Path, recorded: Option<Stamp>) -> StatVerdict;
  pub fn next_sweep_delay(degraded: bool, rng: &mut SplitMix64) -> Duration;
  pub fn is_network_volume(path: &Path) -> bool;
  ```

- [ ] **Step 1: Write the failing tests.**

`collab/storage/watch.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn p(s: &str) -> PathBuf { PathBuf::from(s) }

    #[test]
    fn changes_wait_for_the_aggregation_window() {
        let t0 = Instant::now();
        let mut a = Aggregator::default();
        a.observe(&FsSignal::Touched(p("/c/m31/a/x.fits")), t0);
        assert_eq!(a.drain(t0 + Duration::from_secs(5), |_| true), Drained::default());
        let d = a.drain(t0 + AGGREGATE, |_| true);
        assert_eq!(d.changed, vec![p("/c/m31/a/x.fits")]);
        assert!(d.removed.is_empty());
    }

    #[test]
    fn a_removal_is_concluded_only_after_the_settle_window() {
        let t0 = Instant::now();
        let mut a = Aggregator::default();
        a.observe(&FsSignal::Touched(p("/c/x.fits")), t0);
        let gone = |_: &Path| false;
        assert!(a.drain(t0 + AGGREGATE, gone).removed.is_empty());
        assert!(a.pending_removal(Path::new("/c/x.fits")));
        assert!(a.drain(t0 + AGGREGATE + Duration::from_secs(30), gone).removed.is_empty());
        let d = a.drain(t0 + AGGREGATE + SETTLE, gone);
        assert_eq!(d.removed, vec![p("/c/x.fits")]);
    }

    #[test]
    fn a_move_is_one_event_not_a_deletion() {
        let t0 = Instant::now();
        let mut a = Aggregator::default();
        let present: HashSet<PathBuf> = [p("/c/new/x.fits")].into();
        let exists = |q: &Path| present.contains(q);
        a.observe(&FsSignal::Touched(p("/c/old/x.fits")), t0);
        a.observe(&FsSignal::Touched(p("/c/new/x.fits")), t0);
        let d = a.drain(t0 + AGGREGATE, exists);
        assert_eq!(d.changed, vec![p("/c/new/x.fits")]);
        // the old path settles as removed; the storage task re-adopts the new
        // path by hash BEFORE it rules on the removal (Task 9)
        let d = a.drain(t0 + AGGREGATE + SETTLE, exists);
        assert_eq!(d.removed, vec![p("/c/old/x.fits")]);
    }

    #[test]
    fn a_file_back_before_the_settle_cancels_the_removal() {
        let t0 = Instant::now();
        let mut a = Aggregator::default();
        a.observe(&FsSignal::Touched(p("/c/x.fits")), t0);
        a.drain(t0 + AGGREGATE, |_| false);
        a.observe(&FsSignal::Touched(p("/c/x.fits")), t0 + Duration::from_secs(20));
        let d = a.drain(t0 + Duration::from_secs(30), |_| true);
        assert_eq!(d.changed, vec![p("/c/x.fits")]);
        assert!(a.drain(t0 + SETTLE * 2, |_| true).removed.is_empty());
    }

    #[test]
    fn our_own_temp_files_and_the_store_are_ignored_but_the_canary_is_not() {
        let root = Path::new("/c");
        assert!(is_ignored(root, Path::new("/c/m31/a/x.fits.athtmp")));
        assert!(is_ignored(root, Path::new("/c/.athenaeum/blobs/data/ab.data")));
        assert!(!is_ignored(root, Path::new("/c/.athenaeum/canary")));
        assert!(!is_ignored(root, Path::new("/c/m31/a/x.fits")));
    }

    #[test]
    fn the_canary_declares_a_silent_watcher_dead() {
        let t0 = Instant::now();
        let mut c = Canary::default();
        assert!(c.due(t0));
        c.wrote(t0);
        assert!(!c.dead(t0 + Duration::from_secs(10)));
        assert!(c.dead(t0 + CANARY_DEADLINE));
        c.wrote(t0 + CANARY_EVERY);
        c.observed();
        assert!(!c.dead(t0 + CANARY_EVERY + CANARY_DEADLINE));
    }

    #[tokio::test]
    async fn a_real_watcher_reports_a_new_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let Some(_w) = spawn_watcher(&root, tx) else {
            eprintln!("no filesystem watcher on this platform; skipping");
            return;
        };
        tokio::time::sleep(Duration::from_millis(200)).await;
        std::fs::write(root.join("x.fits"), b"data").unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let sig = tokio::time::timeout_at(deadline, rx.recv()).await.expect("an event within 10 s").unwrap();
            if sig == FsSignal::Touched(root.join("x.fits")) {
                break;
            }
        }
    }
}
```

`collab/storage/sweep.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_round_trip_and_tolerate_two_seconds_of_mtime() {
        let s = Stamp::parse("1048576:1727000000").unwrap();
        assert_eq!(s.encode(), "1048576:1727000000");
        assert!(s.matches(&Stamp { size: 1048576, mtime: 1727000002 }));
        assert!(!s.matches(&Stamp { size: 1048576, mtime: 1727000003 }));
        assert!(!s.matches(&Stamp { size: 1048575, mtime: 1727000000 }));
        assert_eq!(Stamp::parse("junk"), None);
    }

    #[test]
    fn stat_verdicts() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("x.fits");
        std::fs::write(&f, b"0123456789").unwrap();
        let st = Stamp::of(&std::fs::metadata(&f).unwrap());
        assert_eq!(stat_verdict(&f, Some(st)), StatVerdict::Same);
        std::fs::write(&f, b"0123456789-longer").unwrap();
        assert!(matches!(stat_verdict(&f, Some(st)), StatVerdict::Drifted(_)));
        std::fs::remove_file(&f).unwrap();
        assert_eq!(stat_verdict(&f, Some(st)), StatVerdict::Missing);
    }

    #[test]
    fn sweep_cadence_is_hourly_with_jitter_or_five_minutes_when_degraded() {
        let mut rng = crate::geometry::ransac::SplitMix64(9);
        for _ in 0..200 {
            let d = next_sweep_delay(false, &mut rng);
            assert!(d >= SWEEP_HEALTHY.mul_f64(1.0 - SWEEP_JITTER) && d <= SWEEP_HEALTHY.mul_f64(1.0 + SWEEP_JITTER), "{d:?}");
        }
        assert_eq!(next_sweep_delay(true, &mut rng), SWEEP_DEGRADED);
    }

    #[test]
    fn a_local_temp_dir_is_not_a_network_volume() {
        assert!(!is_network_volume(&std::env::temp_dir()));
        #[cfg(windows)]
        assert!(is_network_volume(Path::new(r"\\nas\share\collab")));
    }
}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib collab::storage::watch collab::storage::sweep`. Expected: compile errors.

- [ ] **Step 3: `collab/storage/watch.rs`**

```rust
//! The fast path of change detection (spec §9.2): notify events aggregated
//! for 10 s; a removal concluded only after a 60 s settle, so a move or a
//! rename is one event. The stat sweep (`sweep.rs`) is the authority and the
//! serve check (`collab::serve`) the correctness gate; this only makes the
//! common case fast.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const AGGREGATE: Duration = Duration::from_secs(10);
pub const SETTLE: Duration = Duration::from_secs(60);
pub const CANARY_REL: &str = ".athenaeum/canary";
pub const CANARY_EVERY: Duration = Duration::from_secs(300);
pub const CANARY_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsSignal {
    Touched(PathBuf),
    Root,
    Canary,
    WatchError(String),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Drained {
    pub changed: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    pub root_touched: bool,
}

#[derive(Debug)]
pub struct Aggregator {
    touched: BTreeSet<PathBuf>,
    window_started: Option<Instant>,
    root_touched: bool,
    pending_removals: BTreeMap<PathBuf, Instant>,
    aggregate: Duration,
    settle: Duration,
}

impl Default for Aggregator {
    fn default() -> Self {
        Self::with_timings(AGGREGATE, SETTLE)
    }
}

impl Aggregator {
    pub fn with_timings(aggregate: Duration, settle: Duration) -> Self {
        Self { touched: BTreeSet::new(), window_started: None, root_touched: false, pending_removals: BTreeMap::new(), aggregate, settle }
    }

    pub fn observe(&mut self, sig: &FsSignal, now: Instant) {
        match sig {
            FsSignal::Touched(p) => {
                self.touched.insert(p.clone());
                self.window_started.get_or_insert(now);
            }
            FsSignal::Root => {
                self.root_touched = true;
                self.window_started.get_or_insert(now);
            }
            FsSignal::Canary | FsSignal::WatchError(_) => {}
        }
    }

    pub fn drain(&mut self, now: Instant, exists: impl Fn(&Path) -> bool) -> Drained {
        let mut out = Drained::default();
        if self.window_started.is_some_and(|t| now >= t + self.aggregate) {
            self.window_started = None;
            out.root_touched = std::mem::take(&mut self.root_touched);
            for p in std::mem::take(&mut self.touched) {
                if exists(&p) {
                    self.pending_removals.remove(&p);
                    out.changed.push(p);
                } else {
                    self.pending_removals.entry(p).or_insert(now);
                }
            }
        }
        let settled: Vec<PathBuf> = self
            .pending_removals
            .iter()
            .filter(|(_, since)| now >= **since + self.settle)
            .map(|(p, _)| p.clone())
            .collect();
        for p in settled {
            self.pending_removals.remove(&p);
            if exists(&p) {
                out.changed.push(p);
            } else {
                out.removed.push(p);
            }
        }
        out.changed.sort();
        out.changed.dedup();
        out.removed.sort();
        out
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        let w = self.window_started.map(|t| t + self.aggregate);
        let r = self.pending_removals.values().map(|t| *t + self.settle).min();
        match (w, r) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    pub fn pending_removal(&self, path: &Path) -> bool {
        self.pending_removals.contains_key(path)
    }
}

pub fn is_ignored(root: &Path, path: &Path) -> bool {
    if path.extension().is_some_and(|e| e == "athtmp") {
        return true;
    }
    match path.strip_prefix(root) {
        Ok(rel) => rel.starts_with(".athenaeum") && rel != Path::new(CANARY_REL),
        Err(_) => false,
    }
}

#[derive(Debug, Default)]
pub struct Canary {
    written_at: Option<Instant>,
    seen: bool,
}

impl Canary {
    pub fn due(&self, now: Instant) -> bool {
        self.written_at.is_none_or(|t| now >= t + CANARY_EVERY)
    }
    pub fn wrote(&mut self, now: Instant) {
        self.written_at = Some(now);
        self.seen = false;
    }
    pub fn observed(&mut self) {
        self.seen = true;
    }
    /// True once a write went unobserved for [`CANARY_DEADLINE`].
    pub fn dead(&mut self, now: Instant) -> bool {
        self.written_at.is_some_and(|t| !self.seen && now >= t + CANARY_DEADLINE)
    }
}

pub fn write_canary(root: &Path) -> std::io::Result<()> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_default();
    std::fs::write(root.join(CANARY_REL), stamp)
}

/// A recursive watcher on the root. Events cross an UNBOUNDED channel: a
/// bounded one can block the platform's fs-event thread (P23). `None` when
/// no watcher can be established — the caller then runs the degraded sweep.
pub fn spawn_watcher(root: &Path, tx: tokio::sync::mpsc::UnboundedSender<FsSignal>) -> Option<notify::RecommendedWatcher> {
    use notify::Watcher as _;
    let root_owned = root.to_path_buf();
    let canary = root.join(CANARY_REL);
    let sink = move |res: notify::Result<notify::Event>| match res {
        Ok(event) => {
            for path in event.paths {
                let sig = if path == root_owned {
                    FsSignal::Root
                } else if path == canary {
                    FsSignal::Canary
                } else if is_ignored(&root_owned, &path) {
                    continue;
                } else {
                    FsSignal::Touched(path)
                };
                if tx.send(sig).is_err() {
                    return;
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "collaboration folder watcher error");
            let _ = tx.send(FsSignal::WatchError(e.to_string()));
        }
    };
    let mut watcher = match notify::recommended_watcher(sink) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(path = %root.display(), error = %e, "collaboration folder watcher unavailable; periodic check only");
            return None;
        }
    };
    if let Err(e) = watcher.watch(root, notify::RecursiveMode::Recursive) {
        tracing::warn!(path = %root.display(), error = %e, "collaboration folder watch failed; periodic check only");
        return None;
    }
    Some(watcher)
}
```

  (`Option::is_none_or` is stable since Rust 1.82; the toolchain is 1.96.)

- [ ] **Step 4: `collab/storage/sweep.rs`**

```rust
//! The authority of change detection (spec §9.2): `size:mtime` with a 2 s
//! tolerance for FAT and SMB, hourly (±25 %) while the watcher is healthy,
//! every 5 minutes while it is not or the store is on a network volume.

use std::path::Path;
use std::time::Duration;

use crate::geometry::ransac::SplitMix64;

pub const MTIME_TOLERANCE_SECS: i64 = 2;
pub const SWEEP_HEALTHY: Duration = Duration::from_secs(3600);
pub const SWEEP_DEGRADED: Duration = Duration::from_secs(300);
pub const SWEEP_JITTER: f64 = 0.25;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub size: u64,
    pub mtime: i64,
}

impl Stamp {
    pub fn of(meta: &std::fs::Metadata) -> Stamp {
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Stamp { size: meta.len(), mtime }
    }

    /// The wave-2 `size_mtime_seen` format, `"<len>:<mtime_secs>"`.
    pub fn parse(s: &str) -> Option<Stamp> {
        let (a, b) = s.split_once(':')?;
        Some(Stamp { size: a.parse().ok()?, mtime: b.parse().ok()? })
    }

    pub fn encode(&self) -> String {
        format!("{}:{}", self.size, self.mtime)
    }

    pub fn matches(&self, other: &Stamp) -> bool {
        self.size == other.size && (self.mtime - other.mtime).abs() <= MTIME_TOLERANCE_SECS
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatVerdict {
    Same,
    Drifted(Stamp),
    Missing,
    Unreadable(String),
}

pub fn stat_verdict(path: &Path, recorded: Option<Stamp>) -> StatVerdict {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let now = Stamp::of(&meta);
            match recorded {
                Some(r) if r.matches(&now) => StatVerdict::Same,
                _ => StatVerdict::Drifted(now),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => StatVerdict::Missing,
        Err(e) => StatVerdict::Unreadable(e.to_string()),
    }
}

pub fn next_sweep_delay(degraded: bool, rng: &mut SplitMix64) -> Duration {
    if degraded {
        return SWEEP_DEGRADED;
    }
    let factor = 1.0 - SWEEP_JITTER + 2.0 * SWEEP_JITTER * rng.next_f64();
    SWEEP_HEALTHY.mul_f64(factor)
}

pub fn is_network_volume(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: `c` is a valid NUL-terminated path; `st` is a valid out-pointer.
        if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
            return false;
        }
        let name: Vec<u8> = st.f_fstypename.iter().take_while(|b| **b != 0).map(|b| *b as u8).collect();
        return matches!(name.as_slice(), b"smbfs" | b"nfs" | b"afpfs" | b"webdav" | b"cifs");
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: as above.
        if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
            return false;
        }
        const NFS: i64 = 0x6969;
        const SMB: i64 = 0x517B;
        const SMB2: i64 = 0xFE53_4D42;
        const CIFS: i64 = 0xFF53_4D42;
        return matches!(st.f_type as i64, NFS | SMB | SMB2 | CIFS);
    }
    #[cfg(windows)]
    {
        return path.as_os_str().to_string_lossy().starts_with(r"\\");
    }
    #[allow(unreachable_code)]
    false
}
```

- [ ] **Step 5: Run** `cargo test -p athenaeum-core --lib collab::storage && cargo check -p athenaeum-core --no-default-features && cargo check --workspace --all-targets`. Expected: PASS. (`eprintln!` in the watcher test is test-only, allowed.)

- [ ] **Step 6: Commit**

```bash
rustfmt crates/athenaeum-core/src/collab/storage/*.rs
git add -A crates/athenaeum-core/src/collab/storage
git commit -m "feat(collab): collaboration-folder watcher with 10 s aggregation and 60 s settle, canary, stat sweep with 2 s mtime tolerance and network-volume detection"
```

---
### Task 9: Per-frame state machine, deletion rules, quarantine, re-adoption — the storage engine (§9.1–§9.4, L4–L6, I7, I9, P10, P11, P13, P24)

**Implementer:** rust-engineer.

**Files:**
- Create: `crates/athenaeum-core/src/collab/storage/states.rs`, `collab/storage/deletions.rs` (pure); register in `collab/storage/mod.rs`.
- Create: `crates/athenaeum-core/src/api/collab_live/storage_task.rs`; `pub mod storage_task;` in `api/collab_live/mod.rs`.
- Modify: `crates/athenaeum-core/src/db/collab_frames.rs` — `upsert_from_manifest` (the full edge set replaces Task 1's interim rule); new `rows_under(conn, dir) -> Vec<LocalFrameRow>`, `rows_with_landed_path(conn) -> Vec<LocalFrameRow>`, `list_by_state(conn, project_id, &[LocalState])`.
- Modify: `crates/athenaeum-core/src/api/collab_live/replace.rs` — `adopt_by_hash` also re-adopts a `held` row whose recorded path no longer exists (a move inside the root).
- Modify: `crates/athenaeum-core/src/api/collab_exchange.rs` — `set_collab_policy` (:3296-3319) and `set_project_auto_replicate` (:3808-3827) call `storage_task::apply_policy`.
- Test: table tests in `states.rs`, `deletions.rs`; engine tests in `storage_task.rs` on real temp files.

**Interfaces:**
- Consumes: `LocalState`, `set_local_state`, `collab_live::{record_deletion, deletions_since, prune_deletions, quarantine, unquarantine, list_quarantine}` (Task 1); `Aggregator`, `FsSignal`, `Canary`, `spawn_watcher`, `Stamp`, `stat_verdict`, `next_sweep_delay`, `is_network_volume` (Task 8); `StoreGuard`, `StoreState` (Task 7); `adopt_by_hash` (Task 7); `Redundancy` (Task 6).
- Produces:
  ```rust
  // collab/storage/states.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum StateEvent { Landed, FileGone, FileBack, StampDrift, ContentChanged, Ruled(DeletionRuling), Refetch, StopKeeping, DeleteChanged, KeepAgain, PutBack, Moved, NewVersion { same_bytes: bool }, Excluded, Reincluded }
  pub fn transition(origin: FrameOrigin, from: LocalState, ev: StateEvent) -> Option<LocalState>;   // None = the event does not apply in that state
  // collab/storage/deletions.rs
  pub const WINDOW: Duration = Duration::from_secs(300);
  pub const MASS_THRESHOLD: usize = 10;
  pub const SECOND_DELETION: Duration = Duration::from_secs(24 * 3600);
  pub const LAST_COPY_MIN_OTHERS: usize = 2;
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum DeletionRuling { Refetch, AwaitChoice }
  #[derive(Debug, Clone, PartialEq, Eq)] pub struct RuledBatch { pub rulings: Vec<((String, String), DeletionRuling)>, pub mass: bool, pub window_count: usize, pub pull_into_choice: Vec<(String, String)> }
  pub fn rule_batch(history: &[DeletionRecord], batch: &[(String, String)], now_ms: i64) -> RuledBatch;
  pub fn last_copy_warning(other_holders_total: usize) -> bool;     // < 2
  pub fn lost_everywhere(other_holders_total: usize) -> bool;       // == 0
  // api/collab_live/storage_task.rs
  pub trait HolderView: Send + Sync { fn other_holders(&self, project_id: &str, frame_uuid: &str) -> Redundancy; }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum StorageEvent {
      StateChanged { project_id: String, frame_uuid: String, from: LocalState, to: LocalState },
      DeletionChoice { count: usize, project_ids: Vec<String> },
      FrameLost { project_id: String, frame_uuid: String, file_name: String },
      Quarantined { project_id: String, frame_uuid: String, file_name: String },
      Availability(StoreState),
      WatcherDegraded(bool),
  }
  #[derive(Debug, Clone, Copy)] pub struct StorageTimings { pub aggregate: Duration, pub settle: Duration, pub sweep_healthy: Duration, pub sweep_degraded: Duration }
  impl Default for StorageTimings { /* AGGREGATE, SETTLE, SWEEP_HEALTHY, SWEEP_DEGRADED */ }
  pub struct StorageEngine { /* ctx, node, guard, agg, canary, watcher, fs_rx, degraded, network, next_sweep, rng, timings */ }
  impl StorageEngine {
      pub fn start(ctx: Arc<ServiceContext>, node: Arc<SharedIrohNode>, guard: Arc<StoreGuard>) -> Self;   // = start_with(.., StorageTimings::default())
      pub fn start_with(ctx: Arc<ServiceContext>, node: Arc<SharedIrohNode>, guard: Arc<StoreGuard>, timings: StorageTimings) -> Self;
      pub fn next_deadline(&self) -> Instant;
      pub async fn recv_signal(&mut self) -> Option<FsSignal>;                      // select!-friendly
      pub fn on_signal(&mut self, sig: FsSignal, now: Instant);
      pub async fn tick(&mut self, now: Instant, holders: &dyn HolderView) -> Vec<StorageEvent>;
      pub async fn sweep(&mut self, holders: &dyn HolderView) -> Vec<StorageEvent>;
      pub async fn local_check(&mut self, project_id: &str, frame_uuid: &str) -> Vec<StorageEvent>;
      pub fn degraded(&self) -> bool;
      pub fn network(&self) -> bool;
  }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum DeletionAction { Refetch, StopKeeping }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum ChangedAction { RefetchOriginal, Delete }
  #[derive(Debug, Clone, PartialEq, Eq)] pub struct ChangedOutcome { pub trashed: bool }
  #[derive(Debug, Clone, PartialEq, Eq)] pub struct LastCopyRow { pub frame_uuid: String, pub file_name: String, pub holders_online: usize, pub holders_total: usize, pub at_risk: bool }
  pub fn resolve_deletions(ctx: &ServiceContext, project_id: &str, frame_uuids: Option<&[String]>, action: DeletionAction) -> Result<usize, ApiError>;
  pub fn keep_again(ctx: &ServiceContext, project_id: &str, frame_uuids: Option<&[String]>) -> Result<usize, ApiError>;
  pub async fn resolve_changed_file(ctx: &ServiceContext, node: &SharedIrohNode, project_id: &str, frame_uuid: &str, action: ChangedAction, confirmed_delete: bool) -> Result<ChangedOutcome, ApiError>;
  pub fn last_copy_report(ctx: &ServiceContext, holders: &dyn HolderView, project_id: &str, frame_uuids: &[String]) -> Result<Vec<LastCopyRow>, ApiError>;
  pub fn apply_policy(ctx: &ServiceContext, project_id: &str) -> Result<usize, ApiError>;
  pub const TRASH_UNAVAILABLE: &str = "trash_unavailable";
  ```

- [ ] **Step 1: Write the failing pure tests.**

`collab/storage/states.rs` — one row per edge of spec §9.4 plus P10's additions:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::collab_frames::{FrameOrigin::*, LocalState::*};
    use StateEvent::*;

    #[test]
    fn replica_edges_of_the_spec_diagram() {
        let cases: &[(LocalState, StateEvent, Option<LocalState>)] = &[
            (Wanted, Landed, Some(Held)),
            (Held, FileGone, Some(Missing)),
            (Missing, FileBack, Some(Held)),
            (Wanted, FileBack, Some(Held)),          // the file is back (or found by hash) before the fetch
            (AwaitingChoice, FileBack, Some(Held)),
            (Idle, FileBack, None),                  // an excluded frame is never served
            (Missing, Ruled(DeletionRuling::Refetch), Some(Wanted)),
            (Missing, Ruled(DeletionRuling::AwaitChoice), Some(AwaitingChoice)),
            (Wanted, Ruled(DeletionRuling::AwaitChoice), Some(AwaitingChoice)), // mass pull-in of a re-fetch in progress
            (AwaitingChoice, Refetch, Some(Wanted)),
            (AwaitingChoice, StopKeeping, Some(NotKept)),
            (AwaitingChoice, NewVersion { same_bytes: false }, Some(AwaitingChoice)),
            (Held, StampDrift, Some(Held)),
            (Held, ContentChanged, Some(Quarantined)),
            (Quarantined, Refetch, Some(Wanted)),
            (Quarantined, DeleteChanged, Some(NotKept)),
            (Quarantined, NewVersion { same_bytes: false }, Some(Quarantined)),
            (NotKept, KeepAgain, Some(Wanted)),
            (NotKept, PutBack, Some(Held)),
            (Held, Moved, Some(Held)),
            (Held, NewVersion { same_bytes: false }, Some(Wanted)),
            (Held, NewVersion { same_bytes: true }, Some(Held)),
            (Wanted, Excluded, Some(Idle)),
            (Held, Excluded, Some(Idle)),
            (AwaitingChoice, Excluded, Some(Idle)),
            (Idle, Reincluded, Some(Wanted)),
            // things that must never happen
            (Quarantined, Landed, None),        // nothing lands over a quarantined file (P13)
            (NotKept, NewVersion { same_bytes: false }, Some(NotKept)), // a decline survives versions (L6)
            (Held, KeepAgain, None),
            (Idle, Landed, None),
        ];
        for (from, ev, want) in cases {
            assert_eq!(transition(Replica, *from, *ev), *want, "{from:?} + {ev:?}");
        }
    }

    #[test]
    fn own_frames_are_never_refetched_or_quarantined_as_replicas() {
        assert_eq!(transition(Own, OwnHeld, FileGone), Some(OwnMissing));
        assert_eq!(transition(Own, OwnMissing, FileBack), Some(OwnHeld));
        assert_eq!(transition(Own, OwnHeld, ContentChanged), Some(OwnChanged));
        assert_eq!(transition(Own, OwnChanged, StampDrift), Some(OwnHeld)); // the same bytes came back
        assert_eq!(transition(Own, OwnHeld, StampDrift), Some(OwnHeld));
        assert_eq!(transition(Own, OwnMissing, Ruled(DeletionRuling::Refetch)), None);
        assert_eq!(transition(Own, OwnHeld, NewVersion { same_bytes: true }), Some(OwnHeld));
    }
}
```

`collab/storage/deletions.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn rec(u: &str, at: i64) -> DeletionRecord { DeletionRecord { project_id: "p1".into(), frame_uuid: u.into(), settled_at_ms: at } }
    fn key(u: &str) -> (String, String) { ("p1".into(), u.into()) }
    const MIN: i64 = 60_000;

    #[test]
    fn up_to_ten_in_the_window_are_refetched() {
        let batch: Vec<_> = (0..10).map(|i| key(&format!("u{i}"))).collect();
        let r = rule_batch(&[], &batch, 100 * MIN);
        assert!(!r.mass);
        assert_eq!(r.window_count, 10);
        assert!(r.rulings.iter().all(|(_, d)| *d == DeletionRuling::Refetch));
    }

    #[test]
    fn eleven_in_a_rolling_window_raise_one_choice_and_pull_in_the_earlier_ones() {
        let history: Vec<_> = (0..6).map(|i| rec(&format!("h{i}"), 100 * MIN - 4 * MIN)).collect();
        let batch: Vec<_> = (0..5).map(|i| key(&format!("b{i}"))).collect();
        let r = rule_batch(&history, &batch, 100 * MIN);
        assert!(r.mass);
        assert_eq!(r.window_count, 11);
        assert!(r.rulings.iter().all(|(_, d)| *d == DeletionRuling::AwaitChoice));
        assert_eq!(r.pull_into_choice.len(), 6);
        // history older than 5 minutes does not count
        let old: Vec<_> = (0..6).map(|i| rec(&format!("h{i}"), 100 * MIN - 6 * MIN)).collect();
        assert!(!rule_batch(&old, &batch, 100 * MIN).mass);
    }

    #[test]
    fn a_second_deletion_within_a_day_joins_the_choice_whatever_the_count() {
        let history = vec![rec("u1", 100 * MIN - 23 * 60 * MIN)];
        let r = rule_batch(&history, &[key("u1"), key("u2")], 100 * MIN);
        assert!(!r.mass);
        assert_eq!(r.rulings, vec![(key("u1"), DeletionRuling::AwaitChoice), (key("u2"), DeletionRuling::Refetch)]);
        let old = vec![rec("u1", 100 * MIN - 25 * 60 * MIN)];
        assert_eq!(rule_batch(&old, &[key("u1")], 100 * MIN).rulings, vec![(key("u1"), DeletionRuling::Refetch)]);
    }

    #[test]
    fn last_copy_and_lost_everywhere_thresholds() {
        assert!(last_copy_warning(0) && last_copy_warning(1) && !last_copy_warning(2));
        assert!(lost_everywhere(0) && !lost_everywhere(1));
    }
}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib collab::storage::states collab::storage::deletions`. Expected: compile errors.

- [ ] **Step 3: Pure implementations.**

```rust
// collab/storage/states.rs
//! The per-frame local state machine (spec §9.4 exactly, plus plan P10's
//! gap-closing edges). Pure: `(origin, from, event) → to`. Reporting follows
//! from `set_local_state` (servable ↔ not servable → outbox).

use crate::collab::storage::deletions::DeletionRuling;
use crate::db::collab_frames::{FrameOrigin, LocalState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateEvent {
    Landed,
    FileGone,
    FileBack,
    StampDrift,
    ContentChanged,
    Ruled(DeletionRuling),
    Refetch,
    StopKeeping,
    DeleteChanged,
    KeepAgain,
    PutBack,
    Moved,
    NewVersion { same_bytes: bool },
    Excluded,
    Reincluded,
}

pub fn transition(origin: FrameOrigin, from: LocalState, ev: StateEvent) -> Option<LocalState> {
    use LocalState::*;
    use StateEvent::*;
    if origin == FrameOrigin::Own {
        return match (from, ev) {
            (OwnHeld, FileGone) => Some(OwnMissing),
            (OwnMissing, FileBack) => Some(OwnHeld),
            (OwnHeld, ContentChanged) => Some(OwnChanged),
            (OwnChanged, StampDrift) | (OwnHeld, StampDrift) => Some(OwnHeld),
            (OwnHeld | OwnMissing | OwnChanged, NewVersion { .. }) => Some(from),
            (OwnHeld, Moved) => Some(OwnHeld),
            _ => None,
        };
    }
    match (from, ev) {
        (Wanted, Landed) => Some(Held),
        (Held, FileGone) => Some(Missing),
        (Missing | Wanted | AwaitingChoice, FileBack) => Some(Held),
        (Missing | Wanted, Ruled(DeletionRuling::AwaitChoice)) => Some(AwaitingChoice),
        (Missing, Ruled(DeletionRuling::Refetch)) => Some(Wanted),
        (AwaitingChoice, Refetch) => Some(Wanted),
        (AwaitingChoice, StopKeeping) => Some(NotKept),
        (Held, StampDrift) => Some(Held),
        (Held, ContentChanged) => Some(Quarantined),
        (Quarantined, Refetch) => Some(Wanted),
        (Quarantined, DeleteChanged) => Some(NotKept),
        (NotKept, KeepAgain) => Some(Wanted),
        (NotKept, PutBack) => Some(Held),
        (Held, Moved) => Some(Held),
        (Held, NewVersion { same_bytes: true }) => Some(Held),
        (Held, NewVersion { same_bytes: false }) => Some(Wanted),
        (Wanted | Missing | AwaitingChoice | Quarantined | NotKept | Idle, NewVersion { .. }) => Some(from),
        (Wanted | Held | AwaitingChoice | Missing, Excluded) => Some(Idle),
        (Idle, Reincluded) => Some(Wanted),
        _ => None,
    }
}
```

```rust
// collab/storage/deletions.rs
//! L4 — replica deletions judged over a rolling 5-minute window, each after
//! a 60 s settle. Pure; the storage engine applies the rulings.

use std::collections::BTreeSet;
use std::time::Duration;

use crate::db::collab_live::DeletionRecord;

pub const WINDOW: Duration = Duration::from_secs(300);
pub const MASS_THRESHOLD: usize = 10;
pub const SECOND_DELETION: Duration = Duration::from_secs(24 * 3600);
pub const LAST_COPY_MIN_OTHERS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletionRuling {
    Refetch,
    AwaitChoice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuledBatch {
    pub rulings: Vec<((String, String), DeletionRuling)>,
    pub mass: bool,
    pub window_count: usize,
    pub pull_into_choice: Vec<(String, String)>,
}

pub fn rule_batch(history: &[DeletionRecord], batch: &[(String, String)], now_ms: i64) -> RuledBatch {
    let window_start = now_ms - WINDOW.as_millis() as i64;
    let day_start = now_ms - SECOND_DELETION.as_millis() as i64;
    let batch_set: BTreeSet<&(String, String)> = batch.iter().collect();
    let in_window: BTreeSet<(String, String)> = history
        .iter()
        .filter(|r| r.settled_at_ms >= window_start)
        .map(|r| (r.project_id.clone(), r.frame_uuid.clone()))
        .filter(|k| !batch_set.contains(k))
        .collect();
    let window_count = in_window.len() + batch_set.len();
    let mass = window_count > MASS_THRESHOLD;
    let rulings = batch
        .iter()
        .map(|k| {
            let again = history
                .iter()
                .any(|r| r.project_id == k.0 && r.frame_uuid == k.1 && r.settled_at_ms >= day_start && r.settled_at_ms < now_ms);
            let ruling = if mass || again { DeletionRuling::AwaitChoice } else { DeletionRuling::Refetch };
            (k.clone(), ruling)
        })
        .collect();
    RuledBatch { rulings, mass, window_count, pull_into_choice: if mass { in_window.into_iter().collect() } else { Vec::new() } }
}

pub fn last_copy_warning(other_holders_total: usize) -> bool {
    other_holders_total < LAST_COPY_MIN_OTHERS
}

pub fn lost_everywhere(other_holders_total: usize) -> bool {
    other_holders_total == 0
}
```

- [ ] **Step 4: Manifest edges** — rewrite `upsert_from_manifest`'s local-state
  handling: read the previous row first (inside the same connection /
  transaction the caller passed), run the UPSERT for the manifest columns
  (no `local_state`/`on_disk` in the UPDATE list any more), then:

```rust
// after the UPSERT, in upsert_from_manifest
use crate::collab::storage::states::{transition, StateEvent};
if let Some(prev) = prev {
    let published = v.state == "published" && v.accepted;
    let was_published = prev.state == "published" && prev.accepted;
    let ev = if prev.content_version != v.content_version {
        Some(StateEvent::NewVersion { same_bytes: prev.blake3 == v.blake3 })
    } else if was_published && !published {
        Some(StateEvent::Excluded)
    } else if !was_published && published {
        Some(StateEvent::Reincluded)
    } else {
        None
    };
    if let Some(ev) = ev {
        if let Some(to) = transition(prev.origin, prev.local_state, ev) {
            if to != prev.local_state {
                set_local_state(conn, project_id, &v.frame_uuid, to)?;
            } else if matches!(ev, StateEvent::NewVersion { same_bytes: true }) && to.servable() {
                // P10: same bytes under a new version — re-claim at the new version
                crate::db::collab_live::record_claim_change(conn, project_id, &v.frame_uuid,
                    crate::db::collab_live::ClaimOp::Add { content_version: v.content_version })?;
            }
        }
        if matches!(ev, StateEvent::NewVersion { same_bytes: false }) && prev.origin == FrameOrigin::Replica {
            conn.execute(
                "UPDATE project_frames_local SET size_mtime_seen = NULL WHERE project_id = ?1 AND frame_uuid = ?2 AND local_state <> 'quarantined'",
                params![project_id, v.frame_uuid],
            )?;
        }
    }
}
```

  A brand-new row keeps Task 1's initial state (`own_held` / `wanted` /
  `idle`). Extend `db/collab_frames.rs` tests: a version bump of a `held`
  replica → `wanted` + one outbox `rm`; with the same blake3 → stays `held`
  + one outbox `add` at the new version; a `quarantined` or `not_kept` row
  keeps its state; exclusion → `idle` + `rm`.

- [ ] **Step 5: Write the failing engine tests** (`api/collab_live/storage_task.rs`),
  on the Task 7 `test_support` rig (real temp files, a bound node, the
  collab store mounted, `test_gc` armed):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::collab_live::test_support as ts;
    use crate::collab::live::holders::Redundancy;
    use crate::db::collab_frames::{self as frames_db, LocalState};

    struct Holders(usize);
    impl HolderView for Holders {
        fn other_holders(&self, _: &str, _: &str) -> Redundancy { Redundancy { online: self.0, total: self.0 } }
    }

    fn state(ctx: &ServiceContext, pid: &str, uuid: &str) -> LocalState {
        let conn = crate::api::db(ctx).unwrap().conn();
        frames_db::get(&conn, pid, uuid).unwrap().unwrap().local_state
    }

    #[tokio::test]
    async fn a_touched_file_with_the_same_bytes_keeps_serving() {
        let rig = ts::landed_rig(1).await; // one held replica, landed and seeded
        let (pid, uuid, path) = rig.frames[0].clone();
        ts::set_mtime(&path, 60);
        let mut eng = rig.engine();
        let ev = eng.local_check(&pid, &uuid).await;
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::Held);
        assert!(ev.is_empty());
    }

    #[tokio::test]
    async fn an_edited_replica_is_quarantined_at_once_and_stops_serving() {
        let rig = ts::landed_rig(1).await;
        let (pid, uuid, path) = rig.frames[0].clone();
        ts::overwrite_same_size(&path);
        let mut eng = rig.engine();
        let ev = eng.local_check(&pid, &uuid).await;
        assert!(matches!(ev.as_slice(), [StorageEvent::StateChanged { to: LocalState::Quarantined, .. }, StorageEvent::Quarantined { .. }]));
        assert!(path.exists(), "the edited file is never touched");
        let conn = crate::api::db(&rig.ctx).unwrap().conn();
        assert_eq!(crate::db::collab_live::list_quarantine(&conn, &pid).unwrap().len(), 1);
        assert!(!frames_db::get(&conn, &pid, &uuid).unwrap().unwrap().on_disk);
    }

    #[tokio::test]
    async fn a_single_deletion_settles_then_refetches() {
        let rig = ts::landed_rig(1).await;
        let (pid, uuid, path) = rig.frames[0].clone();
        std::fs::remove_file(&path).unwrap();
        let mut eng = rig.engine();
        let t0 = Instant::now();
        eng.on_signal(FsSignal::Touched(path.clone()), t0);
        assert!(eng.tick(t0 + crate::collab::storage::watch::AGGREGATE, &Holders(2)).await.is_empty());
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::Held); // not concluded yet
        eng.tick(t0 + crate::collab::storage::watch::AGGREGATE + crate::collab::storage::watch::SETTLE, &Holders(2)).await;
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::Wanted);
    }

    #[tokio::test]
    async fn fifteen_deletions_raise_one_choice_and_a_last_copy_raises_lost_everywhere() {
        let rig = ts::landed_rig(15).await;
        let mut eng = rig.engine();
        let t0 = Instant::now();
        for (_, _, path) in &rig.frames {
            std::fs::remove_file(path).unwrap();
            eng.on_signal(FsSignal::Touched(path.clone()), t0);
        }
        eng.tick(t0 + crate::collab::storage::watch::AGGREGATE, &Holders(0)).await;
        let ev = eng.tick(t0 + crate::collab::storage::watch::AGGREGATE + crate::collab::storage::watch::SETTLE, &Holders(0)).await;
        let choices: Vec<_> = ev.iter().filter(|e| matches!(e, StorageEvent::DeletionChoice { .. })).collect();
        assert_eq!(choices.len(), 1);
        assert!(matches!(choices[0], StorageEvent::DeletionChoice { count: 15, .. }));
        assert_eq!(ev.iter().filter(|e| matches!(e, StorageEvent::FrameLost { .. })).count(), 15);
        for (pid, uuid, _) in &rig.frames {
            assert_eq!(state(&rig.ctx, pid, uuid), LocalState::AwaitingChoice);
        }
        // the choice is reversible and non-blocking
        let (pid, _, _) = &rig.frames[0];
        assert_eq!(resolve_deletions(&rig.ctx, pid, None, DeletionAction::StopKeeping).unwrap(), 15);
        assert_eq!(keep_again(&rig.ctx, pid, None).unwrap(), 15);
        assert_eq!(state(&rig.ctx, pid, &rig.frames[3].1), LocalState::Wanted);
    }

    #[tokio::test]
    async fn a_moved_file_is_readopted_by_hash_without_a_deletion() {
        let rig = ts::landed_rig(1).await;
        let (pid, uuid, path) = rig.frames[0].clone();
        let moved = path.parent().unwrap().parent().unwrap().join("renamed.fits");
        std::fs::rename(&path, &moved).unwrap();
        let mut eng = rig.engine();
        let t0 = Instant::now();
        eng.on_signal(FsSignal::Touched(path.clone()), t0);
        eng.on_signal(FsSignal::Touched(moved.clone()), t0);
        eng.tick(t0 + crate::collab::storage::watch::AGGREGATE, &Holders(2)).await;
        eng.tick(t0 + crate::collab::storage::watch::AGGREGATE + crate::collab::storage::watch::SETTLE, &Holders(2)).await;
        let conn = crate::api::db(&rig.ctx).unwrap().conn();
        let row = frames_db::get(&conn, &pid, &uuid).unwrap().unwrap();
        assert_eq!(row.local_state, LocalState::Held);
        assert_eq!(row.landed_path.as_deref(), Some(moved.to_string_lossy().as_ref()));
    }

    #[tokio::test]
    async fn an_unavailable_store_changes_no_frame() {
        let rig = ts::landed_rig(3).await;
        std::fs::remove_file(rig.root.join(crate::collab::storage::marker::MARKER_REL)).unwrap();
        for (_, _, p) in &rig.frames {
            std::fs::remove_file(p).unwrap();
        }
        let mut eng = rig.engine();
        let t0 = Instant::now();
        eng.on_signal(FsSignal::Root, t0);
        let ev = eng.sweep(&Holders(2)).await;
        assert!(ev.iter().all(|e| matches!(e, StorageEvent::Availability(_))));
        for (pid, uuid, _) in &rig.frames {
            assert_eq!(state(&rig.ctx, pid, uuid), LocalState::Held);
        }
    }

    #[tokio::test]
    async fn refetch_original_trashes_or_asks_and_delete_needs_confirmation() {
        let rig = ts::landed_rig(1).await;
        let (pid, uuid, path) = rig.frames[0].clone();
        ts::overwrite_same_size(&path);
        let mut eng = rig.engine();
        eng.local_check(&pid, &uuid).await;
        let err = resolve_changed_file(&rig.ctx, &rig.node, &pid, &uuid, ChangedAction::Delete, false).await.unwrap_err();
        assert!(err.to_string().contains("confirm"), "{err}");
        let out = resolve_changed_file(&rig.ctx, &rig.node, &pid, &uuid, ChangedAction::Delete, true).await.unwrap();
        assert!(!out.trashed && !path.exists());
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::NotKept);
    }
}
```

  `test_support` gains: `landed_rig(n) -> LandedRig { ctx, node, root, frames: Vec<(pid, uuid, PathBuf)> , … }`
  (n real small files published by the fake hub's second account, landed
  through the Task 11 path once it exists — until then, placed directly and
  recorded with `set_landed` + `seed_project_frame` + `set_local_state(Held)`),
  `LandedRig::engine()`, `set_mtime(path, secs_forward)`,
  `overwrite_same_size(path)` (flips one byte, keeps the size, bumps mtime).

- [ ] **Step 6: Run** `cargo test -p athenaeum-core --lib api::collab_live::storage_task`. Expected: compile errors.

- [ ] **Step 7: `api/collab_live/storage_task.rs`.** Behaviour, each step under
  `project_disk_lock(pid)` and in one transaction per frame:
  - **`start`**: `guard.check_now()`; `spawn_watcher(root, fs_tx)` (the root
    canonicalized, P23); `network = is_network_volume(root)`;
    `degraded = watcher.is_none() || network`; `next_sweep = now` (a sweep at
    start confirms the files before anything new is reported, §9.1).
  - **`on_signal`**: `Canary` → `canary.observed()`; `Root` → re-check the
    store at the next tick; `Touched`/`Root` → `agg.observe`.
  - **`tick(now)`**:
    1. `state = guard.check_now()` when the root was touched or on every
       tick where `now ≥ next_check` (every 5 s). A change emits
       `Availability(state)`. Not `serving()` → drain nothing, change no
       frame, return (unmounted is not deleted).
    2. Canary: `due` → `write_canary` + `wrote`; `dead(now)` flips
       `degraded` → `WatcherDegraded(true)` (and `warn!`).
    3. `drained = agg.drain(now, |p| p.exists())`; process `changed` FIRST:
       a path that is the `landed_path` of a `held`/`own_held`/`quarantined`/`own_changed`
       row → `recheck(row)`; the `landed_path` of a row in `missing`,
       `wanted`, `awaiting_choice`, `not_kept` or `own_missing` (the file came
       back) → `adopt_by_hash` (`FileBack` / `PutBack`, seed tag set again,
       stat + hash decide); an unknown
       file → `adopt_by_hash` (re-adoption of a moved or put-back file,
       § "Unknown files" of §9.4) else
       `db::collab_frames::record_foreign_file` (Other files, R18, with the
       R30 stamp). Then `removed`: rows whose `landed_path` equals the path
       or lies under it (a removed folder) → `file_gone(rows)`.
    4. Sweep when `now ≥ next_sweep` → `sweep()`, then
       `next_sweep = now + next_sweep_delay(degraded, rng)`.
  - **`recheck(row)`** (also `local_check`): `stat_verdict(landed_path,
    Stamp::parse(size_mtime_seen))`:
    - `Same` → nothing;
    - `Missing` → `agg.observe(Touched(path))` (the settle decides);
    - `Drifted(now)` → skip when `rejected_size_mtime == now.encode()` (R21);
      xxh3 on `spawn_blocking`: equal to `row.xxh3` → `StampDrift`
      (`set_size_mtime_seen(now)`; for `own_changed`/`quarantined` rows whose
      bytes came back → `own_held` / `held` + `unquarantine`); different →
      `ContentChanged`: `node.unseed_project_frame(pid, uuid)` FIRST (stops
      serving at once), then in one transaction `set_local_state(Quarantined
      | OwnChanged)`, `quarantine(QuarantineRow {…, quarantined_version:
      row.content_version, observed_size_mtime: now})` (replicas only),
      `set_rejected_size_mtime(now)`; events `StateChanged` + `Quarantined`
      (replica) — `warn!(project_id, frame_uuid, path, "replica changed on disk; quarantined")`.
  - **`file_gone(rows)`**: own rows → `OwnHeld → OwnMissing` (no ruling).
    Replica rows `held` → `node.unseed_project_frame` (so the collab GC can
    drop the dead entry, P31) + `set_local_state(Missing)`. Then ONE
    `rule_batch(deletions_since(now − 24 h), batch, now_ms)`:
    `record_deletion` for each; apply each ruling (`Ruled(..)`); when
    `mass`, also move every `pull_into_choice` frame whose state is
    `missing` or `wanted` to `awaiting_choice`, and emit ONE
    `DeletionChoice { count: window_count, project_ids }`. For every frame of
    the batch with `lost_everywhere(holders.other_holders(..).total)` emit
    `FrameLost` (`error!(project_id, frame_uuid, "frame lost everywhere")`).
    `prune_deletions(now − 24 h)`.
  - **`sweep`**: `check_now()` first (not serving → `[Availability]`, stop).
    For every row with a `landed_path` (`rows_with_landed_path`, own rows
    outside the root included, A1) in `held`, `own_held`, `quarantined`,
    `own_changed`: `recheck(row)`.
  - **User actions** (`resolve_deletions`, `keep_again`,
    `resolve_changed_file`): each validates the current state and applies
    `transition`; a row in the wrong state is skipped (counted out).
    `resolve_changed_file`:
    - `RefetchOriginal` → `trash::delete(path)` on `spawn_blocking`; on
      error: `confirmed_delete == false` → return
      `ApiError::Conflict(format!("{TRASH_UNAVAILABLE}: the system trash is not available — confirm to delete the changed file"))`;
      `true` → `std::fs::remove_file`. Then `unquarantine` +
      `Quarantined → Wanted` (`trashed` reports which path ran).
    - `Delete` → requires `confirmed_delete` (else
      `ApiError::Invalid("confirm the deletion of the changed file first")`),
      `remove_file`, `unquarantine`, `Quarantined → NotKept`.
  - **`last_copy_report`**: per frame, `holders.other_holders` →
    `at_risk = last_copy_warning(total)`.
  - **`apply_policy(ctx, pid)`**: `role_allows_replication(row)` and
    `policy_matches` per replica row: published ∧ accepted ∧ allowed ∧
    matching → `Idle → Wanted` (`Reincluded`); otherwise `Wanted | Held |
    AwaitingChoice → Idle` (`Excluded`). Auto-replicate off changes no state
    (P10). Called by `set_collab_policy` and `set_project_auto_replicate`
    after their writes, and by the executor on `MembersChanged` (Task 15).
  - `adopt_by_hash` (replace.rs) gains the candidate class "`held` row whose
    `landed_path` does not exist" → `Moved` (path updated, stays `held`, no
    outbox row because servability did not change).

- [ ] **Step 8: Run** `cargo test -p athenaeum-core --lib collab::storage api::collab_live db::collab_frames && cargo check --workspace --all-targets && cargo check -p athenaeum-core --no-default-features`. Expected: PASS.

- [ ] **Step 9: Commit**

```bash
rustfmt crates/athenaeum-core/src/collab/storage/*.rs crates/athenaeum-core/src/api/collab_live/*.rs crates/athenaeum-core/src/db/collab_frames.rs crates/athenaeum-core/src/api/collab_exchange.rs
git add -A crates/athenaeum-core/src
git commit -m "feat(collab): per-frame state machine and storage engine — settle, L4 deletion window with one reversible choice, lost-everywhere, quarantine of changed replicas, re-adoption by hash"
```

---
### Task 10: Collab provider intercept — serve check, upload stream limit, class-aware pacer, accepted-connection registry (§8 upload side, §9.3, I9, I11, L1, L11, P14–P17, P21)

**Implementer:** rust-engineer.

**Files:**
- Create: `crates/athenaeum-core/src/collab/serve.rs` (pure); `pub mod serve;` in `collab/mod.rs`.
- Create: `crates/athenaeum-core/src/api/collab_live/serve_oracle.rs`; `pub mod serve_oracle;` in `api/collab_live/mod.rs`.
- Modify: `crates/athenaeum-core/src/sharing/iroh/pacer.rs` (whole file, 29-90).
- Modify: `crates/athenaeum-core/src/sharing/iroh/mod.rs` — new `collab_provider_event_channel` beside `provider_event_channel` (:580-613), new `spawn_collab_provider_events` beside `spawn_provider_events` (:648-853), the personal consumer's `GetRequestReceivedNotify` drain task (:666-809) holds a `PersonalUploadGuard`, its `Throttle` arm (:826-845) calls `reserve_class(.., Personal)`; `CollabSlotBlobs` (:2094-2128) gains the connection registry; new `StreamGauge`.
- Modify: `crates/athenaeum-core/src/sharing/iroh/node.rs` — `bind_with` (:1122-1149) uses the collab channel + consumer; new `set_collab_serve_oracle`, `set_collab_upload_limit`, `collab_streams_in_use`, `close_collab_connections_not_admitted`.
- Modify: `crates/athenaeum-core/src/settings/mod.rs` — keys, defaults, `defaults::all()`, getters, the `every_key_with_a_default_is_listed` list (:799-828).
- Test: `collab/serve.rs`, `sharing/iroh/pacer.rs` unit tests; two-node tests in `sharing/iroh/tests.rs`; oracle test in `serve_oracle.rs`.

**Interfaces:**
- Consumes: `Stamp` (Task 8), `StoreGuard` (Task 7), `LocalState` (Task 1), `test_support::landed_rig` (Task 9).
- Produces:
  ```rust
  // collab/serve.rs
  #[derive(Debug, Clone, PartialEq, Eq)] pub struct ServeRecord { pub project_id: String, pub frame_uuid: String, pub path: std::path::PathBuf, pub stamp: Stamp }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum ServeDecision { Serve, RefuseMismatch, RefuseNotHeld, RefuseUnavailable, RefuseLimit }
  pub fn decide(rec: Option<&ServeRecord>, observed: Option<Stamp>, serving: bool, streams_in_use: usize, limit: usize) -> ServeDecision;
  pub trait ServeOracle: Send + Sync {
      fn lookup(&self, blake3_hex: &str) -> Option<ServeRecord>;   // held/own_held row whose CURRENT blake3 is this hash
      fn serving(&self) -> bool;                                   // StoreGuard::state().serving()
      fn on_mismatch(&self, rec: &ServeRecord);                    // queue a local check (never blocks)
  }
  // sharing/iroh/pacer.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum UploadClass { Personal, Collab }
  pub const COLLAB_SHARE_WHILE_PERSONAL: f64 = 0.10;
  pub const COLLAB_FLOOR_BYTES_PER_SEC: u64 = 64 * 1024;
  pub const PERSONAL_RATE_WINDOW: Duration = Duration::from_secs(2);
  impl UploadPacer {
      pub fn reserve_class(&self, size: u64, class: UploadClass) -> Duration;
      pub fn personal_upload(self: &Arc<Self>) -> PersonalUploadGuard;   // RAII: personal transfer active
      pub fn personal_active(&self) -> usize;
  }
  pub struct PersonalUploadGuard { /* Arc<UploadPacer> */ }
  // sharing/iroh/mod.rs
  pub struct StreamGauge { /* in_use: AtomicUsize, limit: AtomicUsize */ }
  impl StreamGauge { pub fn new(limit: usize) -> Arc<Self>; pub fn try_acquire(self: &Arc<Self>) -> Option<StreamPermit>; pub fn in_use(&self) -> usize; pub fn limit(&self) -> usize; pub fn set_limit(&self, n: usize); }
  pub struct StreamPermit { /* Arc<StreamGauge> */ }
  pub type SharedServeOracle = Arc<std::sync::RwLock<Option<Arc<dyn crate::collab::serve::ServeOracle>>>>;
  pub(crate) fn collab_provider_event_channel() -> (EventSender, mpsc::Receiver<ProviderMessage>);
  pub(crate) fn spawn_collab_provider_events(rx: mpsc::Receiver<ProviderMessage>, pacer: Arc<UploadPacer>, oracle: SharedServeOracle, gauge: Arc<StreamGauge>) -> tokio::task::JoinHandle<()>;
  // node.rs
  impl SharedIrohNode {
      pub fn set_collab_serve_oracle(&self, oracle: Option<Arc<dyn crate::collab::serve::ServeOracle>>);
      pub fn set_collab_upload_limit(&self, streams: usize);
      pub fn collab_streams_in_use(&self) -> usize;
      pub fn close_collab_connections_not_admitted(&self) -> usize;       // I11 (P16)
  }
  // settings/mod.rs
  pub mod keys { pub const COLLAB_MAX_UPLOAD_STREAMS: &str = "collab.max_upload_streams"; pub const COLLAB_MAX_RECEIVE_STREAMS: &str = "collab.max_receive_streams"; }
  pub mod defaults { pub const COLLAB_MAX_UPLOAD_STREAMS: &str = "8"; pub const COLLAB_MAX_RECEIVE_STREAMS: &str = "8"; }   // PROVISIONAL (P21), re-set by Task 18's measurement
  pub const COLLAB_UPLOAD_STREAMS_RANGE: std::ops::RangeInclusive<usize> = 1..=64;
  pub const COLLAB_RECEIVE_STREAMS_RANGE: std::ops::RangeInclusive<usize> = 1..=32;
  impl SettingsManager { pub fn get_collab_max_upload_streams(&self, conn: &Connection) -> anyhow::Result<usize>; pub fn get_collab_max_receive_streams(&self, conn: &Connection) -> anyhow::Result<usize>; } // clamped
  // api/collab_live/serve_oracle.rs
  pub struct DbServeOracle { /* ctx, guard: Arc<StoreGuard>, checks: tokio::sync::mpsc::UnboundedSender<(String, String)> */ }
  impl DbServeOracle { pub fn new(ctx: Arc<ServiceContext>, guard: Arc<StoreGuard>, checks: tokio::sync::mpsc::UnboundedSender<(String, String)>) -> Self; }
  impl ServeOracle for DbServeOracle { /* … */ }
  ```

- [ ] **Step 1: Write the failing tests.**

`collab/serve.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::storage::sweep::Stamp;

    fn rec() -> ServeRecord {
        ServeRecord { project_id: "p1".into(), frame_uuid: "u1".into(), path: "/c/x.fits".into(), stamp: Stamp { size: 10, mtime: 100 } }
    }

    #[test]
    fn the_four_conditions_of_the_serve_check() {
        let ok = Some(Stamp { size: 10, mtime: 101 });
        assert_eq!(decide(Some(&rec()), ok, true, 0, 8), ServeDecision::Serve);
        assert_eq!(decide(Some(&rec()), ok, false, 0, 8), ServeDecision::RefuseUnavailable);
        assert_eq!(decide(None, ok, true, 0, 8), ServeDecision::RefuseNotHeld);
        assert_eq!(decide(Some(&rec()), Some(Stamp { size: 10, mtime: 200 }), true, 0, 8), ServeDecision::RefuseMismatch);
        assert_eq!(decide(Some(&rec()), None, true, 0, 8), ServeDecision::RefuseMismatch); // file gone
        assert_eq!(decide(Some(&rec()), ok, true, 8, 8), ServeDecision::RefuseLimit);
    }
}
```

`sharing/iroh/pacer.rs` (extend its tests):

```rust
#[test]
fn collab_uses_the_shared_bucket_when_no_personal_upload_runs() {
    let p = Arc::new(UploadPacer::new(0));
    assert_eq!(p.reserve_class(16 * 1024, UploadClass::Collab), Duration::ZERO);
}

#[test]
fn collab_paces_at_ten_percent_of_the_cap_while_personal_uploads_run() {
    let p = Arc::new(UploadPacer::new(1_000_000)); // 1 MB/s cap
    let _g = p.personal_upload();
    let now = Instant::now();
    // 10 % of 1 MB/s = 100 kB/s: two 50 kB collab chunks → the second waits 0.5 s
    assert_eq!(p.reserve_class_at(now, 50_000, UploadClass::Collab), Duration::ZERO);
    let wait = p.reserve_class_at(now, 50_000, UploadClass::Collab);
    assert!((wait.as_millis() as i64 - 500).abs() <= 5, "{wait:?}");
}

#[test]
fn without_a_cap_collab_follows_the_observed_personal_rate() {
    let p = Arc::new(UploadPacer::new(0));
    let _g = p.personal_upload();
    let now = Instant::now();
    // personal moved 2 MB in the last 2 s → 1 MB/s → collab gets 100 kB/s
    p.record_personal_at(now - Duration::from_millis(1500), 2_000_000);
    assert_eq!(p.reserve_class_at(now, 100_000, UploadClass::Collab), Duration::ZERO);
    let wait = p.reserve_class_at(now, 100_000, UploadClass::Collab);
    assert!((wait.as_millis() as i64 - 1000).abs() <= 10, "{wait:?}");
    drop(_g);
    assert_eq!(p.personal_active(), 0);
    assert_eq!(p.reserve_class_at(now, 100_000, UploadClass::Collab), Duration::ZERO);
}

#[test]
fn personal_is_never_slowed_by_collab() {
    let p = Arc::new(UploadPacer::new(0));
    let _g = p.personal_upload();
    for _ in 0..10 {
        assert_eq!(p.reserve_class(16 * 1024, UploadClass::Personal), Duration::ZERO);
    }
}
```

`sharing/iroh/tests.rs` (two nodes, relay disabled; reuse the file's existing
bind helpers and `test_support::landed_rig` for the provider side):

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_collab_provider_serves_a_held_frame_and_refuses_an_edited_one() {
    let rig = crate::api::collab_live::test_support::landed_rig(1).await; // node B with a held replica + oracle installed
    let (_pid, _uuid, path) = rig.frames[0].clone();
    let fetcher = crate::api::collab_live::test_support::bare_node().await; // node A
    crate::api::collab_live::test_support::pair(&fetcher, &rig.node).await;
    let hash = rig.hash_of(0);
    let conn = fetcher.endpoint().connect(rig.node.endpoint_addr(), crate::sharing::iroh::COLLAB_BLOBS_ALPN).await.unwrap();
    let store = crate::api::collab_live::test_support::scratch_store().await;
    store.remote().execute_get(conn.clone(), iroh_blobs::protocol::GetRequest::blob(hash)).await.expect("served");
    // edit in place, same size: the NEXT request is refused with ERR_PERMISSION
    crate::api::collab_live::test_support::overwrite_same_size(&path);
    let store2 = crate::api::collab_live::test_support::scratch_store().await;
    let err = store2.remote().execute_get(conn.clone(), iroh_blobs::protocol::GetRequest::blob(hash)).await.unwrap_err();
    assert_eq!(err.iroh_error_code(), Some(iroh_blobs::protocol::ERR_PERMISSION));
    assert_eq!(rig.next_local_check().await, Some((rig.frames[0].0.clone(), rig.frames[0].1.clone())));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_upload_stream_limit_refuses_with_err_limit() {
    let rig = crate::api::collab_live::test_support::landed_rig(1).await;
    rig.node.set_collab_upload_limit(1);
    let _held = rig.node.collab_stream_gauge_for_test().try_acquire().unwrap(); // the one stream is busy
    let fetcher = crate::api::collab_live::test_support::bare_node().await;
    crate::api::collab_live::test_support::pair(&fetcher, &rig.node).await;
    let conn = fetcher.endpoint().connect(rig.node.endpoint_addr(), crate::sharing::iroh::COLLAB_BLOBS_ALPN).await.unwrap();
    let store = crate::api::collab_live::test_support::scratch_store().await;
    let err = store.remote().execute_get(conn, iroh_blobs::protocol::GetRequest::blob(rig.hash_of(0))).await.unwrap_err();
    assert_eq!(err.iroh_error_code(), Some(iroh_blobs::protocol::ERR_LIMIT));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_push_into_the_collab_store_is_refused() {
    let rig = crate::api::collab_live::test_support::landed_rig(1).await;
    let fetcher = crate::api::collab_live::test_support::bare_node().await;
    crate::api::collab_live::test_support::pair(&fetcher, &rig.node).await;
    let store = crate::api::collab_live::test_support::scratch_store().await;
    let tt = store.blobs().add_bytes(vec![7u8; 4096]).await.unwrap();
    let conn = fetcher.endpoint().connect(rig.node.endpoint_addr(), crate::sharing::iroh::COLLAB_BLOBS_ALPN).await.unwrap();
    let res = store.remote().execute_push(conn, iroh_blobs::protocol::PushRequest::new(tt.hash, iroh_blobs::protocol::ChunkRangesSeq::root())).await;
    assert!(res.is_err(), "push must be refused (P15)");
    assert!(rig.node.collab_store().unwrap().blobs().status(tt.hash).await.unwrap() == iroh_blobs::api::proto::BlobStatus::NotFound);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_membership_change_closes_accepted_collab_connections() {
    let rig = crate::api::collab_live::test_support::landed_rig(1).await;
    let fetcher = crate::api::collab_live::test_support::bare_node().await;
    crate::api::collab_live::test_support::pair(&fetcher, &rig.node).await;
    let conn = fetcher.endpoint().connect(rig.node.endpoint_addr(), crate::sharing::iroh::COLLAB_BLOBS_ALPN).await.unwrap();
    let store = crate::api::collab_live::test_support::scratch_store().await;
    store.remote().execute_get(conn.clone(), iroh_blobs::protocol::GetRequest::blob(rig.hash_of(0))).await.unwrap();
    rig.node.set_connect_gate(Arc::new(|_| false)); // the fetcher is no longer a member
    assert_eq!(rig.node.close_collab_connections_not_admitted(), 1);
    tokio::time::timeout(std::time::Duration::from_secs(5), conn.closed()).await.expect("closed by the provider");
}
```

  `test_support` gains `bare_node()`, `pair(a, b)` (the e2e file's `pair`
  moved here, `api/collab_v3_e2e_tests.rs:204`), `scratch_store()` (an
  in-memory `iroh_blobs` store), `LandedRig::hash_of(i)`,
  `LandedRig::next_local_check()` (the oracle's check channel), and the
  landed rig installs a `DbServeOracle` on its node.

- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib collab::serve sharing::iroh::pacer sharing::iroh::tests::the_collab_provider`. Expected: compile errors.

- [ ] **Step 3: `collab/serve.rs`**

```rust
//! The per-request serve check (spec §9.3, I9; plan P14). Pure decision;
//! the collab provider consumer asks it before every get.

use crate::collab::storage::sweep::Stamp;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeRecord {
    pub project_id: String,
    pub frame_uuid: String,
    pub path: std::path::PathBuf,
    pub stamp: Stamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeDecision {
    Serve,
    RefuseMismatch,
    RefuseNotHeld,
    RefuseUnavailable,
    RefuseLimit,
}

pub fn decide(rec: Option<&ServeRecord>, observed: Option<Stamp>, serving: bool, streams_in_use: usize, limit: usize) -> ServeDecision {
    if !serving {
        return ServeDecision::RefuseUnavailable;
    }
    let Some(rec) = rec else {
        return ServeDecision::RefuseNotHeld;
    };
    match observed {
        Some(s) if rec.stamp.matches(&s) => {}
        _ => return ServeDecision::RefuseMismatch,
    }
    if streams_in_use >= limit {
        return ServeDecision::RefuseLimit;
    }
    ServeDecision::Serve
}

pub trait ServeOracle: Send + Sync {
    fn lookup(&self, blake3_hex: &str) -> Option<ServeRecord>;
    fn serving(&self) -> bool;
    fn on_mismatch(&self, rec: &ServeRecord);
}
```

- [ ] **Step 4: Class-aware pacer** (`sharing/iroh/pacer.rs`): keep `reserve`/`reserve_at`
  (the personal bucket, unchanged behaviour) and add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadClass {
    Personal,
    Collab,
}

pub const COLLAB_SHARE_WHILE_PERSONAL: f64 = 0.10;
pub const COLLAB_FLOOR_BYTES_PER_SEC: u64 = 64 * 1024;
pub const PERSONAL_RATE_WINDOW: Duration = Duration::from_secs(2);

// new fields on UploadPacer:
//   collab_next_free: Mutex<Option<Instant>>,
//   personal_active: AtomicUsize,
//   personal_window: Mutex<std::collections::VecDeque<(Instant, u64)>>,

impl UploadPacer {
    pub fn reserve_class(&self, size: u64, class: UploadClass) -> Duration {
        self.reserve_class_at(Instant::now(), size, class)
    }

    pub(crate) fn reserve_class_at(&self, now: Instant, size: u64, class: UploadClass) -> Duration {
        match class {
            UploadClass::Personal => {
                self.record_personal_at(now, size);
                self.reserve_at(now, size)
            }
            UploadClass::Collab if self.personal_active.load(Ordering::Relaxed) == 0 => self.reserve_at(now, size),
            UploadClass::Collab => {
                let cap = self.rate();
                let base = if cap > 0 { cap } else { self.personal_rate_at(now) };
                let share = ((base as f64 * COLLAB_SHARE_WHILE_PERSONAL) as u64).max(COLLAB_FLOOR_BYTES_PER_SEC);
                let mut next = self.collab_next_free.lock().expect("pacer poisoned");
                let start = next.map_or(now, |t| t.max(now));
                *next = Some(start + Duration::from_secs_f64(size as f64 / share as f64));
                start - now
            }
        }
    }

    pub(crate) fn record_personal_at(&self, at: Instant, size: u64) {
        let mut w = self.personal_window.lock().expect("pacer poisoned");
        w.push_back((at, size));
        while w.front().is_some_and(|(t, _)| at.saturating_duration_since(*t) > PERSONAL_RATE_WINDOW) {
            w.pop_front();
        }
    }

    fn personal_rate_at(&self, now: Instant) -> u64 {
        let w = self.personal_window.lock().expect("pacer poisoned");
        let bytes: u64 = w.iter().filter(|(t, _)| now.saturating_duration_since(*t) <= PERSONAL_RATE_WINDOW).map(|(_, b)| b).sum();
        (bytes as f64 / PERSONAL_RATE_WINDOW.as_secs_f64()) as u64
    }

    pub fn personal_upload(self: &Arc<Self>) -> PersonalUploadGuard {
        self.personal_active.fetch_add(1, Ordering::Relaxed);
        PersonalUploadGuard { pacer: Arc::clone(self) }
    }

    pub fn personal_active(&self) -> usize {
        self.personal_active.load(Ordering::Relaxed)
    }
}

pub struct PersonalUploadGuard {
    pacer: Arc<UploadPacer>,
}

impl Drop for PersonalUploadGuard {
    fn drop(&mut self) {
        self.pacer.personal_active.fetch_sub(1, Ordering::Relaxed);
        *self.pacer.collab_next_free.lock().expect("pacer poisoned") = None;
    }
}
```

  In the personal consumer: the `Throttle` arm calls
  `pacer.reserve_class(m.inner.size, UploadClass::Personal)`, and the
  detached drain task of `GetRequestReceivedNotify` holds
  `let _active = pacer.personal_upload();` for its whole life (only for
  payload-carrying requests, `request_is_payload_carrying`, M:481).
  Behaviour of the personal path is unchanged: same bucket, same replies.

- [ ] **Step 5: The collab consumer** (`sharing/iroh/mod.rs`):

```rust
/// The collab store's own mask (plan P14/P15): `get: InterceptLog` so every
/// get is admitted or refused by the serve check. iroh-blobs 0.103 routes
/// get_many/push/observe through the SAME mask field (provider/events.rs:462),
/// so those arrive intercepted too and are answered explicitly below.
pub(crate) fn collab_provider_event_channel() -> (EventSender, mpsc::Receiver<ProviderMessage>) {
    EventSender::channel(
        PROVIDER_EVENT_CAPACITY,
        EventMask {
            connected: ConnectMode::Notify,
            get: RequestMode::InterceptLog,
            throttle: ThrottleMode::Intercept,
            ..EventMask::DEFAULT
        },
    )
}

pub(crate) fn spawn_collab_provider_events(
    mut rx: mpsc::Receiver<ProviderMessage>,
    pacer: Arc<UploadPacer>,
    oracle: SharedServeOracle,
    gauge: Arc<StreamGauge>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            match msg {
                ProviderMessage::GetRequestReceived(m) => {
                    let oracle = oracle.read().ok().and_then(|g| g.clone());
                    let gauge = Arc::clone(&gauge);
                    tokio::spawn(async move {
                        let hash = m.inner.request.hash;
                        let verdict = collab_serve_verdict(oracle.as_deref(), &hash.to_hex(), &gauge, true).await;
                        let permit = match verdict {
                            Ok(permit) => {
                                m.tx.send(Ok(())).await.ok();
                                permit
                            }
                            Err(reason) => {
                                m.tx.send(Err(reason)).await.ok();
                                None
                            }
                        };
                        // SAFETY RULE: drain the update stream for the whole transfer.
                        let mut updates = m.rx;
                        while let Ok(Some(_)) = updates.recv().await {}
                        drop(permit);
                    });
                }
                ProviderMessage::ObserveRequestReceived(m) => {
                    let oracle = oracle.read().ok().and_then(|g| g.clone());
                    let gauge = Arc::clone(&gauge);
                    tokio::spawn(async move {
                        let hash = m.inner.request.hash;
                        let verdict = collab_serve_verdict(oracle.as_deref(), &hash.to_hex(), &gauge, false).await;
                        m.tx.send(verdict.map(|_| ())).await.ok();
                        let mut updates = m.rx;
                        while let Ok(Some(_)) = updates.recv().await {}
                    });
                }
                ProviderMessage::PushRequestReceived(m) => {
                    tracing::warn!(connection_id = m.inner.connection_id, "push into the collab store refused");
                    m.tx.send(Err(AbortReason::Permission)).await.ok();
                    let mut updates = m.rx;
                    tokio::spawn(async move { while let Ok(Some(_)) = updates.recv().await {} });
                }
                ProviderMessage::GetManyRequestReceived(m) => {
                    tracing::debug!(connection_id = m.inner.connection_id, "get-many on the collab store refused");
                    m.tx.send(Err(AbortReason::Permission)).await.ok();
                    let mut updates = m.rx;
                    tokio::spawn(async move { while let Ok(Some(_)) = updates.recv().await {} });
                }
                ProviderMessage::Throttle(m) => {
                    let wait = pacer.reserve_class(m.inner.size, UploadClass::Collab);
                    if wait.is_zero() {
                        m.tx.send(Ok(())).await.ok();
                    } else {
                        let tx = m.tx;
                        tokio::spawn(async move {
                            tokio::time::sleep(wait).await;
                            tx.send(Ok(())).await.ok();
                        });
                    }
                }
                ProviderMessage::GetRequestReceivedNotify(m) => drain_detached(m.rx),
                ProviderMessage::GetManyRequestReceivedNotify(m) => drain_detached(m.rx),
                ProviderMessage::PushRequestReceivedNotify(m) => drain_detached(m.rx),
                ProviderMessage::ObserveRequestReceivedNotify(m) => drain_detached(m.rx),
                ProviderMessage::ClientConnected(m) => {
                    m.tx.send(Ok(())).await.ok();
                }
                ProviderMessage::ClientConnectedNotify(_) | ProviderMessage::ConnectionClosed(_) => {}
            }
        }
    })
}

/// The serve check, with the file stat off the consumer task.
async fn collab_serve_verdict(
    oracle: Option<&dyn crate::collab::serve::ServeOracle>,
    blake3_hex: &str,
    gauge: &Arc<StreamGauge>,
    count_stream: bool,
) -> Result<Option<StreamPermit>, AbortReason> {
    use crate::collab::serve::{decide, ServeDecision};
    let Some(oracle) = oracle else {
        tracing::warn!("collab get refused: no serve oracle installed");
        return Err(AbortReason::Permission);
    };
    let rec = oracle.lookup(blake3_hex);
    let observed = match &rec {
        Some(r) => {
            let path = r.path.clone();
            tokio::task::spawn_blocking(move || std::fs::metadata(&path).ok().map(|m| crate::collab::storage::sweep::Stamp::of(&m)))
                .await
                .unwrap_or(None)
        }
        None => None,
    };
    let limit = if count_stream { gauge.limit() } else { usize::MAX };
    match decide(rec.as_ref(), observed, oracle.serving(), gauge.in_use(), limit) {
        ServeDecision::Serve => Ok(if count_stream { gauge.try_acquire() } else { None }),
        ServeDecision::RefuseLimit => {
            tracing::debug!(streams = gauge.in_use(), "collab get refused: upload stream limit");
            Err(AbortReason::RateLimited)
        }
        ServeDecision::RefuseMismatch => {
            let r = rec.expect("a mismatch has a record");
            tracing::warn!(project_id = %r.project_id, frame_uuid = %r.frame_uuid, path = %r.path.display(), "collab get refused: file changed on disk; checking it now");
            oracle.on_mismatch(&r);
            Err(AbortReason::Permission)
        }
        ServeDecision::RefuseNotHeld => {
            tracing::debug!(blake3 = blake3_hex, "collab get refused: not a held current version");
            Err(AbortReason::Permission)
        }
        ServeDecision::RefuseUnavailable => {
            tracing::debug!("collab get refused: collaboration storage unavailable");
            Err(AbortReason::Permission)
        }
    }
}
```

  (`drain_detached(rx)` = the existing `drain_only!` body as a function.
  `StreamGauge::try_acquire` increments only when `in_use < limit`, so a race
  between two admitted requests never exceeds the limit: a request that
  loses the race after `decide` said Serve is refused with `RateLimited`.)

- [ ] **Step 6: Connection registry** (`CollabSlotBlobs`, M:2094): add
  `conns: Arc<std::sync::Mutex<HashMap<NodeId, Vec<iroh::endpoint::WeakConnectionHandle>>>>`;
  in `accept`, after the gate check, `conns.lock().entry(from).or_default().push(connection.weak_handle())`
  (pruning handles whose `upgrade()` is `None`) before delegating. The node
  keeps a clone of the `Arc`:

```rust
/// I11 (plan P16): close every accepted collab connection whose remote the
/// connect gate no longer admits. The gate reads membership live, so call
/// this right after a membership snapshot refresh.
pub fn close_collab_connections_not_admitted(&self) -> usize {
    let mut closed = 0;
    let mut conns = self.collab_conns.lock().expect("collab conns poisoned");
    conns.retain(|node, handles| {
        if connect_gate_admits(&self.connect_gate, node) {
            handles.retain(|h| h.upgrade().is_some());
            return !handles.is_empty();
        }
        for h in handles.drain(..) {
            if let Some(c) = h.upgrade() {
                c.close(0u32.into(), b"membership revoked");
                closed += 1;
            }
        }
        tracing::info!(peer = %hex32(node), "collab connections closed: no longer admitted");
        false
    });
    closed
}
```

- [ ] **Step 7: Wire the node** (`bind_with`, N:1122-1149): the collab event
  channel becomes `collab_provider_event_channel()`, its consumer
  `spawn_collab_provider_events(rx, pacer, collab_oracle.clone(), collab_gauge.clone())`;
  new fields `collab_oracle: SharedServeOracle` (starts `None` → every get
  refused until the live session installs the oracle), `collab_gauge:
  Arc<StreamGauge>` (starts at the provisional default 8),
  `collab_conns`. Setters per the Interfaces block;
  `#[cfg(test)] pub fn collab_stream_gauge_for_test(&self) -> Arc<StreamGauge>`.

- [ ] **Step 8: `DbServeOracle`** (`api/collab_live/serve_oracle.rs`): `lookup` =

```rust
fn lookup(&self, blake3_hex: &str) -> Option<ServeRecord> {
    let db = crate::api::db(&self.ctx).ok()?;
    let conn = db.conn();
    let row: Option<(String, String, String, Option<String>)> = conn
        .query_row(
            "SELECT project_id, frame_uuid, landed_path, size_mtime_seen FROM project_frames_local
             WHERE blake3 = ?1 AND local_state IN ('held','own_held') AND landed_path IS NOT NULL
             LIMIT 1",
            [blake3_hex],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "serve lookup failed; refusing");
            None
        });
    let (project_id, frame_uuid, path, stamp) = row?;
    Some(ServeRecord { project_id, frame_uuid, path: path.into(), stamp: Stamp::parse(stamp.as_deref()?)? })
}
```

  `serving()` = `self.guard.state().serving()`; `on_mismatch` sends
  `(project_id, frame_uuid)` on the unbounded channel (the storage engine's
  `local_check` consumes it, Task 15). Test: a `held` row with a stamp →
  `lookup` returns it; `wanted`/`quarantined` → `None`; a superseded hash
  (the row moved to v2) → `None`.

- [ ] **Step 9: Settings keys.** Add the two keys and defaults (with a doc
  comment "PROVISIONAL — collab v3 wave 3 plan P21; re-set from the relay
  measurement"), both pairs in `defaults::all()`, both keys in the
  `every_key_with_a_default_is_listed` list, and the clamped getters
  (`get_with_precedence(conn, keys::X, defaults::X)?.parse::<usize>()`,
  non-numeric → default with a `warn!`, clamped to the ranges). Test
  `collab_stream_limit_getters_default_and_clamp` mirroring the existing
  loss-guard getter test (settings/mod.rs:833).

- [ ] **Step 10: Run** `cargo test -p athenaeum-core --lib collab::serve sharing::iroh settings:: api::collab_live && cargo check --workspace --all-targets && cargo check -p athenaeum-core --no-default-features`. Expected: PASS, the whole personal-sync iroh suite included (`sharing::iroh::tests`).

- [ ] **Step 11: Commit**

```bash
rustfmt crates/athenaeum-core/src/collab/serve.rs crates/athenaeum-core/src/sharing/iroh/{mod,node,pacer,tests}.rs crates/athenaeum-core/src/settings/mod.rs crates/athenaeum-core/src/api/collab_live/*.rs
git add -A crates/athenaeum-core/src
git commit -m "feat(collab): serve check in the collab provider intercept (ERR_PERMISSION on a changed file, ERR_LIMIT past the stream limit, push refused), class-aware upload pacer, close collab connections of dropped members"
```

---
### Task 11: Landing — temp file, rename over the target, re-import; never over a quarantined file (§7.5, I1, I6, I7, L5, L7, P12, P13)

**Implementer:** rust-engineer.

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/iroh/blobs.rs` — new `athtmp_path`, `export_child_replacing` beside `export_child` (:696-727; `export_child` itself unchanged — personal sync).
- Create: `crates/athenaeum-core/src/api/collab_live/landing.rs` (`pub mod landing;`): the wave-2 landing moved from `api/collab_exchange.rs` — `land_frame` (:2951-3056), `landing_target` (:2900-2932), `new_landing_path` (:2863-2882), `fresh_row` (:2840-2859), `record_landing` (:3100-3140), `forget_stale_landing` (:3061-3088), `link_identical` (:3145-3218), `identical_landed` (:2479-2492), `holds_frame_content` (:2937-2943), `remove_landed` (:3091-3095). The originals are deleted from `collab_exchange.rs` in this task; `run_batch` (CE:2688) calls the moved functions until Task 15 removes it.
- Modify: `crates/athenaeum-core/src/db/collab_frames.rs` — `set_landed_if` (:316) fence gains `AND local_state = 'wanted'` and no longer writes `on_disk`/`local_state` (the caller runs `set_local_state` in the same transaction).
- Test: `landing.rs` tests on the Task 7/9 `test_support` rig; `blobs.rs` unit test.

**Interfaces:**
- Consumes: `add_path_child`, `ensure_child_readable`, `CopyRepair`, `ImportProgressMeter`, `export_source_vanished`, `in_flight_tag` (blobs.rs, KEEP); `set_local_state` (Task 1); `StoreGuard` (Task 7); `transition` (Task 9).
- Produces:
  ```rust
  // sharing/iroh/blobs.rs
  pub(crate) const ATHTMP_EXT: &str = "athtmp";
  pub(crate) fn athtmp_path(target: &Path) -> PathBuf;                          // "<target>.athtmp", same directory
  pub(crate) async fn export_child_replacing(store: &Store, hash: Hash, target: &Path, size: u64) -> Result<ExportOutcome>;
  // api/collab_live/landing.rs
  pub struct LandingEnv<'a> { pub ctx: &'a ServiceContext, pub node: &'a SharedIrohNode, pub store: &'a iroh_blobs::api::Store, pub project: &'a CollabProjectRow, pub collab_root: &'a Path, pub guard: &'a StoreGuard }
  #[derive(Debug, Clone, PartialEq, Eq)] pub enum Landed { Yes(PathBuf), AwaitingGc, Stale, Unavailable, Failed(String) }
  pub async fn land_frame(env: &LandingEnv<'_>, row: &LocalFrameRow, hash: iroh_blobs::Hash) -> Landed;
  pub async fn link_identical(env: &LandingEnv<'_>, row: &LocalFrameRow, src: &Path) -> Landed;
  pub fn identical_landed(ctx: &ServiceContext, row: &LocalFrameRow) -> Result<Option<PathBuf>, ApiError>;
  pub fn project_frame_in_flight_tag(project_id: &str, frame_uuid: &str, content_version: i32) -> String;
  #[cfg(test)] pub(crate) mod fault { pub fn fail_after_export_once(); }        // test hook: the next landing fails between export and rename
  ```

- [ ] **Step 1: Write the failing tests** (`api/collab_live/landing.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::collab_live::test_support as ts;
    use crate::db::collab_frames::{self as frames_db, LocalState};

    fn athtmp_files(root: &std::path::Path) -> usize {
        walkdir::WalkDir::new(root).into_iter().filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "athtmp")).count()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_new_version_replaces_the_old_file_atomically_and_the_store_serves_the_target() {
        let rig = ts::fetch_rig(1).await; // a provider with v1 and a receiver that fetched v1's blob into its collab store
        let (pid, uuid) = rig.frame(0);
        let v1_path = rig.land(0).await.expect("v1 lands");
        rig.publish_new_version(0).await;        // provider re-versions; receiver's row → wanted(v2), v1 file untouched
        assert_eq!(std::fs::read(&v1_path).unwrap(), rig.v1_bytes(0), "v1 stays until v2 replaces it (L7)");
        rig.fetch_blob(0).await;                  // v2 bytes into the receiver's store
        let v2_path = rig.land(0).await.expect("v2 lands");
        assert_eq!(v2_path, v1_path, "same path");
        assert_eq!(std::fs::read(&v2_path).unwrap(), rig.v2_bytes(0));
        assert_eq!(athtmp_files(&rig.root), 0);
        // P12: the store references the TARGET, not the dead temp name
        crate::sharing::iroh::blobs::probe_first_byte(&rig.receiver_store(), rig.v2_hash(0)).await.expect("readable through the target");
        let conn = crate::api::db(&rig.ctx).unwrap().conn();
        let row = frames_db::get(&conn, &pid, &uuid).unwrap().unwrap();
        assert_eq!((row.local_state, row.content_version), (LocalState::Held, 2));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_landing_interrupted_between_export_and_rename_keeps_the_old_file() {
        let rig = ts::fetch_rig(1).await;
        let v1_path = rig.land(0).await.unwrap();
        rig.publish_new_version(0).await;
        rig.fetch_blob(0).await;
        fault::fail_after_export_once();
        assert!(matches!(rig.land(0).await, Err(Landed::Failed(_))));
        assert_eq!(std::fs::read(&v1_path).unwrap(), rig.v1_bytes(0), "the old file is intact (I7)");
        // the retry reuses the completed temp file: no second export, no copy
        let again = rig.land(0).await.unwrap();
        assert_eq!(std::fs::read(&again).unwrap(), rig.v2_bytes(0));
        assert_eq!(athtmp_files(&rig.root), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nothing_lands_over_a_quarantined_file() {
        let rig = ts::fetch_rig(1).await;
        let (pid, uuid) = rig.frame(0);
        let path = rig.land(0).await.unwrap();
        ts::overwrite_same_size(&path);
        let edited = std::fs::read(&path).unwrap();
        {
            let conn = crate::api::db(&rig.ctx).unwrap().conn();
            frames_db::set_local_state(&conn, &pid, &uuid, LocalState::Quarantined).unwrap();
        }
        rig.publish_new_version(0).await;
        rig.fetch_blob(0).await;
        assert_eq!(rig.land(0).await, Err(Landed::Stale));
        assert_eq!(std::fs::read(&path).unwrap(), edited, "the user's edit is never overwritten (L5)");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_version_bump_during_the_landing_is_fenced() {
        let rig = ts::fetch_rig(1).await;
        rig.fetch_blob(0).await;
        rig.bump_manifest_version_locally(0); // the manifest moved while the bytes were in flight
        assert_eq!(rig.land(0).await, Err(Landed::Stale));
        assert_eq!(athtmp_files(&rig.root), 0);
    }
}
```

  `test_support::fetch_rig(n)` — two nodes (relay disabled, paired), a
  provider that publishes `n` real small frames, a receiver whose manifest is
  synced (rows `wanted`) and whose collab store receives a frame's blob
  through `fetch_blob(i)` (one `execute_get` from the provider over the
  collab ALPN into the receiver's store, under the frame's in-flight tag).
  `land(i)` calls `land_frame` and maps `Landed::Yes(p)` → `Ok(p)`, anything
  else → `Err(landed)`. `publish_new_version(i)` rewrites the provider's file,
  republishes, and syncs the receiver's manifest.

- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib api::collab_live::landing`. Expected: compile errors.

- [ ] **Step 3: `export_child_replacing`** (`sharing/iroh/blobs.rs`):

```rust
pub(crate) const ATHTMP_EXT: &str = "athtmp";

/// `<target>.athtmp` in the target's directory (the watcher ignores it).
pub(crate) fn athtmp_path(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_owned();
    s.push(".");
    s.push(ATHTMP_EXT);
    PathBuf::from(s)
}

/// Collab landing (spec §7.5, plan P12): export the store-owned data to
/// `<target>.athtmp`, rename it over `<target>` (an atomic replace on every
/// platform we ship: the old version stays whole until the new one is
/// complete, I7/L7), then re-import `<target>` by reference so the store's
/// first external path is the target and not the dead temp name
/// (iroh-blobs 0.103 records the EXPORT path as the entry's location,
/// store/fs.rs:1316-1320; an import merges the path lists, sorted, and
/// `<name>` sorts before `<name>.athtmp`).
///
/// A temp file left by a landing that crashed after its export and before
/// its rename is recognised by its size and reused: the re-import verifies
/// its BLAKE3, so a wrong file is refused, never served.
pub(crate) async fn export_child_replacing(store: &Store, hash: Hash, target: &Path, size: u64) -> Result<ExportOutcome> {
    let tmp = athtmp_path(target);
    let reuse = match tokio::fs::metadata(&tmp).await {
        Ok(m) if m.len() == size => true,
        Ok(_) => {
            // a partial copy of ours (cross-volume fallback cut short): the
            // store never referenced it, since export records the path only
            // after the copy completed
            if let Err(e) = tokio::fs::remove_file(&tmp).await {
                return Err(LocalFault(anyhow::Error::new(e).context(format!("remove partial temp {}", tmp.display()))).into());
            }
            false
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(LocalFault(anyhow::Error::new(e).context(format!("stat temp {}", tmp.display()))).into()),
    };
    if !reuse {
        if let Err(e) = store
            .blobs()
            .export_with_opts(ExportOptions { hash, mode: ExportMode::TryReference, target: tmp.clone() })
            .finish()
            .await
        {
            return Ok(Err(e));
        }
    }
    #[cfg(test)]
    if crate::api::collab_live::landing::fault::take() {
        return Err(LocalFault(anyhow::anyhow!("injected fault after export")).into());
    }
    if let Err(e) = tokio::fs::rename(&tmp, target).await {
        return Err(LocalFault(anyhow::Error::new(e).context(format!("rename {} over {}", tmp.display(), target.display()))).into());
    }
    let mut meter = ImportProgressMeter::new(None, size);
    let tt = add_path_child(store, target, size, ImportMode::TryReference, &mut meter).await?;
    if tt.hash() != hash {
        let e = anyhow::anyhow!("re-import of {} hashed {} instead of {}", target.display(), tt.hash(), hash);
        tracing::error!(path = %target.display(), error = %e, "landing re-import mismatch");
        return Err(LocalFault(e).into());
    }
    let _tt = ensure_child_readable(store, target, hash, size, ImportMode::TryReference, tt, CopyRepair::Refuse).await?;
    Ok(Ok(()))
}
```

  (`LocalFault` is the existing local-fault wrapper `export_child` uses; the
  cfg(test) fault hook is a thread-local `Cell<bool>` in
  `landing::fault`.) Unit test in `blobs.rs`: `athtmp_path("/c/a/x.fits")
  == "/c/a/x.fits.athtmp"`.

- [ ] **Step 4: `landing.rs`** — move the functions verbatim, then change:
  - `LandingEnv` replaces `FetchEnv` for landing (the client/token/relay
    fields are not needed any more).
  - `fresh_row` (the R20 fence re-read) additionally requires
    `row.local_state == LocalState::Wanted` (P13).
  - Before any disk write, `if !env.guard.check_now().fetching() { return Landed::Unavailable; }`.
  - `landing_target`: DELETE the R24 rename-aside (CE:2914-2926) — a
    quarantined row can no longer get here; keep "an existing landed path
    inside the root is the target" and the `holds` shortcut (C1: when the
    file already holds the frame, skip the export and just record).
  - `land_frame` calls `blobs::export_child_replacing(store, hash, &dest, row.byte_size as u64)`
    instead of `export_child`; `export_source_vanished` handling unchanged
    (`AwaitingGc`).
  - `record_landing`: in ONE transaction — `set_landed_if(..)` (fence incl.
    `local_state = 'wanted'`), then `set_local_state(Held)` (→ outbox `add`,
    `on_disk = 1`), then the `sync_history` row. 0 rows → `Stale` +
    `forget_stale_landing` (unchanged: it never removes a file a row
    references).
  - Every `info!`/`warn!` keeps its wave-2 phrase; new ones:
    `info!(project_id, frame_uuid, content_version, path, "frame landed")`.

- [ ] **Step 5: Run** `cargo test -p athenaeum-core --lib api::collab_live::landing sharing::iroh::blobs api::collab_exchange && cargo check --workspace --all-targets`. Expected: PASS (the surviving wave-2 replication tests now call the moved landing).

- [ ] **Step 6: Commit**

```bash
rustfmt crates/athenaeum-core/src/sharing/iroh/blobs.rs crates/athenaeum-core/src/api/collab_live/landing.rs crates/athenaeum-core/src/api/collab_exchange.rs crates/athenaeum-core/src/db/collab_frames.rs
git add -A crates/athenaeum-core/src
git commit -m "feat(collab): land by temp file + rename + re-import by reference (v1 intact until v2 is complete), no landing over a quarantined file"
```

---
### Task 12: Collab connection pool and the live assignment run (§7.2, §7.3, §8 receive side, P18–P20)

**Implementer:** rust-engineer.

**Files:**
- Create: `crates/athenaeum-core/src/sharing/iroh/collab_pool.rs`; `pub mod collab_pool;` in `sharing/iroh/mod.rs`.
- Modify: `crates/athenaeum-core/src/sharing/iroh/assign.rs` — `FetchItem` (:431-441), `open_pool` (:477-481), `TransferFault` (:489-495), `fetch_children_assigned` (:603), `fetch_items_assigned` (:650), `run_items` (:660-860), `run_child` (:873-1114), `transfer_once` (:1507-1570), `claim_provider` (:1577), `pick_provider` (:1615-1640), `earliest_wait` (:1642), `record_failure_at` (:1745-1773); new `run_live`.
- Modify: `crates/athenaeum-core/src/sharing/iroh/blobs.rs` — `fetch_blobs_assigned` (:1497-1576) builds `ProviderSet::Fixed` (it is removed in Task 15 with its caller).
- Test: `collab_pool.rs` tests; `sharing/iroh/tests.rs` live-run tests (the existing assign/hedge tests are the personal-path pin and must stay green unchanged).

**Interfaces:**
- Consumes: `COLLAB_BLOBS_ALPN`; the Task 10 provider intercept (refusals to test against); `test_support::{landed_rig, bare_node, pair}` (Tasks 7–10).
- Produces:
  ```rust
  // sharing/iroh/collab_pool.rs
  pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
  pub const KEEP_OPEN: Duration = Duration::from_secs(60);
  pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
  pub const KEEP_ALIVE: Duration = Duration::from_secs(5);
  #[derive(Debug, Clone, PartialEq, Eq)] pub enum PoolEvent { Closed { node: iroh::EndpointId, reason: String } }
  #[derive(Debug)] pub enum DialError { Timeout, Connect(String) }
  pub struct CollabPool { /* endpoint, entries, dial_locks, events, dials, connect_timeout */ }
  impl CollabPool {
      pub fn new(endpoint: iroh::Endpoint, events: tokio::sync::mpsc::UnboundedSender<PoolEvent>) -> Arc<Self>;
      #[cfg(test)] pub fn with_connect_timeout(endpoint: iroh::Endpoint, events: tokio::sync::mpsc::UnboundedSender<PoolEvent>, t: Duration) -> Arc<Self>;
      pub async fn get(self: &Arc<Self>, addr: iroh::EndpointAddr) -> Result<PooledConn, DialError>;
      pub fn close(&self, node: &iroh::EndpointId, reason: &[u8]);
      pub fn close_where(&self, drop_if: impl Fn(&iroh::EndpointId) -> bool) -> usize;
      pub fn dials(&self) -> u64;
  }
  pub struct PooledConn { pub conn: iroh::endpoint::Connection, /* touch on drop */ }
  // sharing/iroh/assign.rs
  pub(crate) const LIMIT_RETRY: Duration = Duration::from_secs(2);
  pub(crate) type ProviderAddrs = Arc<dyn Fn(&EndpointId) -> Option<iroh::EndpointAddr> + Send + Sync>;
  pub(crate) enum Dialer { Stock(ConnectionPool), Collab { pool: Arc<super::collab_pool::CollabPool>, addrs: ProviderAddrs } }
  #[derive(Clone)] pub(crate) enum ProviderSet { Fixed(Arc<Vec<EndpointId>>), Live(tokio::sync::watch::Receiver<Arc<Vec<EndpointId>>>) }
  impl ProviderSet { pub(crate) fn current(&self) -> Arc<Vec<EndpointId>>; }
  // FetchItem.providers: ProviderSet   (was Arc<Vec<EndpointId>>)
  pub(crate) struct LiveItem { pub item: FetchItem, pub cancel: tokio::sync::watch::Receiver<bool> }
  #[derive(Debug)] pub(crate) enum ItemOutcome { Done, Cancelled, Failed(anyhow::Error) }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub(crate) enum LiveVerdict { Refused { provider: EndpointId, hash: Hash }, Busy { provider: EndpointId }, Corrupt { provider: EndpointId, hash: Hash }, DialFailed { provider: EndpointId, error: String } }
  pub(crate) struct LiveRunOptions { pub stall_hard_limit: Duration, pub hedging: bool, pub telemetry: ProviderTelemetrySink, pub max_in_flight: Arc<std::sync::atomic::AtomicUsize>, pub unit_cap_bytes: u64 }
  pub(crate) async fn run_live(
      store: &Store,
      dialer: Dialer,
      items: tokio::sync::mpsc::Receiver<LiveItem>,
      opts: LiveRunOptions,
      done: tokio::sync::mpsc::UnboundedSender<(String, ItemOutcome)>,
      verdicts: tokio::sync::mpsc::UnboundedSender<LiveVerdict>,
      yield_now: tokio::sync::watch::Receiver<bool>,
  ) -> AssignmentReport;   // returns when `items` is closed and drained, or when yield is requested and nothing is in flight
  ```

- [ ] **Step 1: Write the failing tests.**

`sharing/iroh/collab_pool.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::collab_live::test_support as ts;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_connection_per_provider_reused_and_a_close_is_reported_at_once() {
        let rig = ts::landed_rig(1).await;
        let me = ts::bare_node().await;
        ts::pair(&me, &rig.node).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = CollabPool::new(me.endpoint(), tx);
        let a = pool.get(rig.node.endpoint_addr()).await.unwrap();
        let b = pool.get(rig.node.endpoint_addr()).await.unwrap();
        assert_eq!(a.conn.stable_id(), b.conn.stable_id());
        assert_eq!(pool.dials(), 1);
        drop((a, b));
        rig.node.shutdown().await;
        let ev = tokio::time::timeout(Duration::from_secs(40), rx.recv()).await.expect("closed() fires").unwrap();
        assert!(matches!(ev, PoolEvent::Closed { node, .. } if node == rig.node.endpoint_addr().id));
    }

    #[tokio::test]
    async fn an_unreachable_provider_times_out() {
        let me = ts::bare_node().await;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = CollabPool::with_connect_timeout(me.endpoint(), tx, Duration::from_millis(500));
        let nobody = iroh::EndpointAddr::new(iroh::SecretKey::from_bytes(&[42u8; 32]).public());
        assert!(matches!(pool.get(nobody).await, Err(DialError::Timeout) | Err(DialError::Connect(_))));
    }
}
```

`sharing/iroh/tests.rs` (live run):

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_item_waits_for_its_first_provider_then_completes() {
    let rig = crate::api::collab_live::test_support::landed_rig(1).await;
    let me = crate::api::collab_live::test_support::bare_node().await;
    crate::api::collab_live::test_support::pair(&me, &rig.node).await;
    let store = crate::api::collab_live::test_support::scratch_store().await;
    let provider = rig.node.endpoint_addr().id;
    let (prov_tx, prov_rx) = tokio::sync::watch::channel(Arc::new(Vec::<iroh::EndpointId>::new()));
    let (out, report) = crate::api::collab_live::test_support::run_one_live(&me, &store, &rig, 0, crate::sharing::iroh::assign::ProviderSet::Live(prov_rx), async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        prov_tx.send(Arc::new(vec![provider])).unwrap();   // a provider appears
    }).await;
    assert!(matches!(out, crate::sharing::iroh::assign::ItemOutcome::Done), "{out:?}");
    assert!(report.total_bytes() > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusing_provider_is_excluded_for_the_hash_without_a_strike() {
    // provider A edited its file (serve check refuses: ERR_PERMISSION), B serves
    let (a, b) = crate::api::collab_live::test_support::two_landed_providers().await;
    crate::api::collab_live::test_support::overwrite_same_size(&a.frames[0].2);
    let me = crate::api::collab_live::test_support::bare_node().await;
    crate::api::collab_live::test_support::pair(&me, &a.node).await;
    crate::api::collab_live::test_support::pair(&me, &b.node).await;
    let store = crate::api::collab_live::test_support::scratch_store().await;
    let (out, verdicts) = crate::api::collab_live::test_support::run_one_live_fixed(&me, &store, a.hash_of(0), a.frames[0].1.clone(), &[&a, &b]).await;
    assert!(matches!(out, crate::sharing::iroh::assign::ItemOutcome::Done), "{out:?}");
    assert!(verdicts.iter().any(|v| matches!(v, crate::sharing::iroh::assign::LiveVerdict::Refused { .. })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_busy_provider_is_retried_later_and_another_one_serves_now() {
    let (a, b) = crate::api::collab_live::test_support::two_landed_providers().await;
    a.node.set_collab_upload_limit(1);
    let _busy = a.node.collab_stream_gauge_for_test().try_acquire().unwrap();
    let me = crate::api::collab_live::test_support::bare_node().await;
    crate::api::collab_live::test_support::pair(&me, &a.node).await;
    crate::api::collab_live::test_support::pair(&me, &b.node).await;
    let store = crate::api::collab_live::test_support::scratch_store().await;
    let (out, verdicts) = crate::api::collab_live::test_support::run_one_live_fixed(&me, &store, a.hash_of(0), a.frames[0].1.clone(), &[&a, &b]).await;
    assert!(matches!(out, crate::sharing::iroh::assign::ItemOutcome::Done));
    assert!(verdicts.iter().any(|v| matches!(v, crate::sharing::iroh::assign::LiveVerdict::Busy { .. })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_stops_one_item_and_keeps_its_partial_bytes() {
    let rig = crate::api::collab_live::test_support::landed_rig_big(1, 64 * 1024 * 1024).await; // one 64 MiB frame
    rig.node.set_upload_limit(8 * 1024 * 1024); // slow it down: 8 MB/s
    let me = crate::api::collab_live::test_support::bare_node().await;
    crate::api::collab_live::test_support::pair(&me, &rig.node).await;
    let store = crate::api::collab_live::test_support::scratch_store().await;
    let out = crate::api::collab_live::test_support::run_one_live_cancel_after(&me, &store, &rig, 0, Duration::from_millis(1500)).await;
    assert!(matches!(out, crate::sharing::iroh::assign::ItemOutcome::Cancelled));
    assert!(matches!(store.blobs().status(rig.hash_of(0)).await.unwrap(), iroh_blobs::api::proto::BlobStatus::Partial { .. }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_stream_cap_bounds_concurrent_items_and_yield_stops_taking_new_ones() {
    let rig = crate::api::collab_live::test_support::landed_rig(4).await;
    let me = crate::api::collab_live::test_support::bare_node().await;
    crate::api::collab_live::test_support::pair(&me, &rig.node).await;
    let store = crate::api::collab_live::test_support::scratch_store().await;
    let stats = crate::api::collab_live::test_support::run_many_live(&me, &store, &rig, /*max_in_flight*/ 1, /*yield after first*/ true).await;
    assert_eq!(stats.max_concurrent, 1);
    assert_eq!(stats.completed, 1, "yield: the in-flight item finished, the queued ones were not taken");
    assert_eq!(stats.returned_early, true);
}
```

  `test_support` gains the run helpers used above: `run_one_live`,
  `run_one_live_fixed`, `run_one_live_cancel_after`, `run_many_live`,
  `two_landed_providers`, `landed_rig_big(n, bytes)`. Each builds a
  `CollabPool`, a `Dialer::Collab { pool, addrs }` whose `addrs` maps a node
  id to that test node's `endpoint_addr()`, and drives `run_live` with its
  channels; `run_many_live` counts concurrency with a telemetry sink that
  increments on `ProviderEvent::Trying` and decrements on completion.

- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib sharing::iroh::collab_pool sharing::iroh::tests::a_live`. Expected: compile errors.

- [ ] **Step 3: `collab_pool.rs`** — the `ControlPool` idiom (N:634-739), plus:

```rust
fn transport_config() -> iroh::endpoint::QuicTransportConfig {
    iroh::endpoint::QuicTransportConfig::builder()
        .keep_alive_interval(KEEP_ALIVE)
        .max_idle_timeout(Some(IDLE_TIMEOUT.try_into().expect("30 s is a valid QUIC idle timeout")))
        .build()
}

impl CollabPool {
    pub async fn get(self: &Arc<Self>, addr: iroh::EndpointAddr) -> Result<PooledConn, DialError> {
        let node = addr.id;
        if let Some(c) = self.live(&node) {
            return Ok(c);
        }
        // one dial per provider at a time
        let lock = self.dial_lock(&node);
        let _g = lock.lock().await;
        if let Some(c) = self.live(&node) {
            return Ok(c);
        }
        let opts = iroh::endpoint::ConnectOptions::new().with_transport_config(transport_config());
        let dial = async {
            let connecting = self
                .endpoint
                .connect_with_opts(addr, super::COLLAB_BLOBS_ALPN, opts)
                .await
                .map_err(|e| DialError::Connect(e.to_string()))?;
            connecting.await.map_err(|e| DialError::Connect(e.to_string()))
        };
        let conn = match tokio::time::timeout(self.connect_timeout, dial).await {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                tracing::debug!(provider = %node.fmt_short(), error = ?e, "collab dial failed");
                return Err(e);
            }
            Err(_) => {
                tracing::debug!(provider = %node.fmt_short(), timeout_ms = self.connect_timeout.as_millis() as u64, "collab dial timed out");
                return Err(DialError::Timeout);
            }
        };
        self.dials.fetch_add(1, Ordering::Relaxed);
        spawn_conn_path_diagnostics(&conn, "outgoing");
        let entry = self.insert(node, conn.clone());
        self.spawn_close_watcher(node, conn.clone());
        self.spawn_idle_reaper(node, conn.clone(), Arc::clone(&entry.last_used), Arc::clone(&entry.active));
        Ok(PooledConn::new(conn, entry.last_used, entry.active))
    }
}
```

  - `live(node)`: the entry exists and `conn.close_reason().is_none()` → a
    `PooledConn` (increments `active`, touches `last_used`).
  - `spawn_close_watcher`: `let reason = conn.closed().await;` → remove the
    entry iff it still holds this `stable_id` → send
    `PoolEvent::Closed { node, reason: reason.to_string() }` →
    `debug!(provider, reason, "collab connection closed")`.
  - `spawn_idle_reaper`: closes (`0`, `b"idle"`) once `active == 0` and
    `last_used + KEEP_OPEN` has passed.
  - `close_where(drop_if)` closes and evicts matching entries (I11), returns
    the count.

- [ ] **Step 4: `assign.rs`** — minimal, behaviour-preserving for the personal path:
  - `FetchItem.providers: ProviderSet`. `fetch_children_assigned` and
    `fetch_blobs_assigned` build `ProviderSet::Fixed(Arc::new(list))`.
    `run_items` builds the union of `providers.current()` for the initial
    states map; `claim_provider` inserts `ProviderState::default()` for a
    provider it has not seen (a live provider that appeared later);
    `pick_provider` treats a missing state as default instead of skipping it.
  - `Dialer` replaces the bare `ConnectionPool` argument of `run_items` /
    `run_child` / `transfer_once`: `Dialer::Stock(open_pool(endpoint, alpn))`
    for the existing entry points; `transfer_once` gets a connection through

```rust
async fn dial(dialer: &Dialer, provider: EndpointId) -> Result<DialedConn, TransferFault> {
    match dialer {
        Dialer::Stock(pool) => pool
            .get_or_connect(provider)
            .await
            .map(DialedConn::Stock)
            .map_err(|e| TransferFault::Failed { bytes: 0, error: anyhow::anyhow!("dial {}: {e}", provider.fmt_short()) }),
        Dialer::Collab { pool, addrs } => {
            let Some(addr) = addrs(&provider) else {
                return Err(TransferFault::Failed { bytes: 0, error: anyhow::anyhow!("no dial address for {}", provider.fmt_short()) });
            };
            pool.get(addr)
                .await
                .map(DialedConn::Collab)
                .map_err(|e| TransferFault::Failed { bytes: 0, error: anyhow::anyhow!("dial {}: {e:?}", provider.fmt_short()) })
        }
    }
}
```

    (`DialedConn` holds either the stock `ConnectionRef` or a `PooledConn`
    for the whole transfer and hands out `Connection` clones.)
  - `TransferFault` gains `Refused { bytes }`, `Busy { bytes }`,
    `Corrupt { bytes, error }`. In `transfer_once`,
    `GetProgressItem::Error(e)` goes through:

```rust
fn classify_get_error(e: &iroh_blobs::get::GetError, bytes: u64, provider: EndpointId) -> TransferFault {
    use iroh_blobs::get::{DecodeError, GetError};
    match e.iroh_error_code() {
        Some(c) if c == iroh_blobs::protocol::ERR_PERMISSION => return TransferFault::Refused { bytes },
        Some(c) if c == iroh_blobs::protocol::ERR_LIMIT => return TransferFault::Busy { bytes },
        _ => {}
    }
    if let GetError::Decode { source, .. } = e {
        if matches!(source, DecodeError::ParentHashMismatch { .. } | DecodeError::LeafHashMismatch { .. }) {
            return TransferFault::Corrupt { bytes, error: anyhow::anyhow!("verification failed from {}: {e}", provider.fmt_short()) };
        }
    }
    TransferFault::Failed { bytes, error: anyhow::anyhow!("get from {}: {e}", provider.fmt_short()) }
}
```

    (field names per `iroh-blobs-0.103.0/src/get/error.rs:13-48` and
    `src/get.rs:635-663`; adjust the pattern if a variant is a tuple.)
  - `run_child` keeps an item-local `excluded: HashSet<EndpointId>`:
    `Refused` and `Corrupt` add the provider to it (no failure strike;
    `Corrupt` also `warn!(provider, child_hash, "provider served bytes that failed verification")`);
    `Busy` sets that provider's `next_try = now + LIMIT_RETRY` without
    touching `failures`; `Failed` and `Stalled` keep today's
    `record_failure_at`. Candidate providers each round =
    `providers.current()` minus `excluded`. A `Live` set with no candidate
    waits on `watch::Receiver::changed()` or the earliest back-off, whichever
    first, and counts toward `MAX_BACKOFF_ROUNDS` only when a candidate
    existed (so an item never fails merely for waiting on its first
    provider). Verdicts are sent on the optional channel (`None` for the
    personal path).
  - `run_live`: the `run_items` loop with three differences —
    items arrive on the channel (`select!` over `items.recv()` when
    `in_flight < max_in_flight.load()` and yield is not requested, and
    `set.join_next()`); each child future is wrapped:

```rust
let key = live.item.key.clone();
let mut cancel = live.cancel.clone();
let handle = set.spawn(async move {
    tokio::select! {
        r = run_child(/* … */) => match r { Ok(()) => ItemOutcome::Done, Err(e) => ItemOutcome::Failed(e) },
        _ = async { while !*cancel.borrow() { if cancel.changed().await.is_err() { std::future::pending::<()>().await; } } } => ItemOutcome::Cancelled,
    }
});
```

    each completion is sent on `done` at once (the executor lands the frame
    immediately, not at the end of a batch); `FailMode` is always `Isolate`;
    when `yield_now` is `true`, no new item is taken, an in-flight item larger
    than `unit_cap_bytes` is cancelled once it has moved `unit_cap_bytes`
    since the yield (its verified ranges stay in the store for the resume),
    and the function returns as soon as nothing is in flight. Dropping a
    child future drops its `GetProgress` stream, which resets the QUIC stream
    — the module doc's cancellation note (A:71-92) already covers why that is
    enough.

- [ ] **Step 5: Run** `cargo test -p athenaeum-core --lib sharing::iroh && cargo check --workspace --all-targets`. Expected: PASS — the new tests and every existing assign/hedge/collection test unchanged.

- [ ] **Step 6: Commit**

```bash
rustfmt crates/athenaeum-core/src/sharing/iroh/{assign,blobs,collab_pool,mod,tests}.rs crates/athenaeum-core/src/api/collab_live/test_support.rs
git add -A crates/athenaeum-core/src
git commit -m "feat(collab): dedicated collab pool (connect_with_opts, 10 s connect, 60 s keep-open, closed() eviction) and a live assignment run — live providers, refusal codes, stream cap, per-item cancel, yield"
```

---
### Task 13: Two-class `ReceiveGate` with yield-on-demand (§8, L1, P18)

**Implementer:** rust-engineer.

**Files:**
- Modify: `crates/athenaeum-core/src/sync/receiver.rs` — `GateState` (:208-216), `ReceiveGate` (:258-340) rewritten in place; the personal call site (:2113) keeps `acquire()`; re-exports at `sync/mod.rs:98-100` gain `ReceivePermit`, `ReceiveClass`.
- Modify: `crates/athenaeum-core/src/api/collab_exchange.rs:2328-2331` — the wave-2 batch permit becomes `gate.acquire_collab()` (removed with its caller in Task 15).
- Test: new tests in `sync/receiver.rs`; the existing gate tests (receiver.rs:7465-7642, 8613-9189) stay green, adapted only where they name the old permit type.

**Interfaces:**
- Consumes: nothing new.
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum ReceiveClass { Personal, Collab }
  pub struct ReceiveGate { /* inner: Arc<Mutex<GateInner>>, yield_tx: watch::Sender<bool> */ }
  impl ReceiveGate {
      pub fn new(limit: usize) -> Self;                         // clamped 1..=8, unchanged
      pub async fn acquire(&self) -> ReceivePermit;             // personal (unchanged name and meaning)
      pub async fn acquire_collab(&self) -> ReceivePermit;      // collab: admitted only when no personal transfer waits
      pub fn yield_signal(&self) -> tokio::sync::watch::Receiver<bool>;   // true while a personal transfer waits and every lane is taken
      pub fn set_limit(&self, limit: usize);                    // live, never interrupts a holder (unchanged)
      pub fn limit(&self) -> usize;
      pub fn in_use(&self) -> usize;
  }
  pub struct ReceivePermit { /* releases on drop */ }
  impl ReceivePermit { pub fn class(&self) -> ReceiveClass; }
  ```

- [ ] **Step 1: Write the failing tests** (`sync/receiver.rs` tests):

```rust
#[tokio::test]
async fn a_waiting_personal_transfer_is_admitted_before_any_collab_unit() {
    let gate = std::sync::Arc::new(ReceiveGate::new(1));
    let held = gate.acquire_collab().await;
    let g1 = std::sync::Arc::clone(&gate);
    let collab_next = tokio::spawn(async move { g1.acquire_collab().await });
    tokio::task::yield_now().await;
    let g2 = std::sync::Arc::clone(&gate);
    let personal = tokio::spawn(async move { g2.acquire().await });
    tokio::task::yield_now().await;
    drop(held);
    let p = tokio::time::timeout(std::time::Duration::from_secs(1), personal).await.unwrap().unwrap();
    assert_eq!(p.class(), ReceiveClass::Personal);
    assert!(!collab_next.is_finished(), "the queued collab unit waits behind the personal transfer");
    drop(p);
    let c = tokio::time::timeout(std::time::Duration::from_secs(1), collab_next).await.unwrap().unwrap();
    assert_eq!(c.class(), ReceiveClass::Collab);
}

#[tokio::test]
async fn a_personal_waiter_raises_the_yield_signal_until_it_is_admitted() {
    let gate = std::sync::Arc::new(ReceiveGate::new(1));
    let mut signal = gate.yield_signal();
    let held = gate.acquire_collab().await;
    assert!(!*signal.borrow());
    let g = std::sync::Arc::clone(&gate);
    let personal = tokio::spawn(async move { g.acquire().await });
    tokio::time::timeout(std::time::Duration::from_secs(1), signal.wait_for(|y| *y)).await.unwrap().unwrap();
    drop(held); // the collab lane finished its frame and yielded
    let _p = personal.await.unwrap();
    assert!(!*gate.yield_signal().borrow());
}

#[tokio::test]
async fn no_collab_waiter_personal_only_behaves_as_before() {
    let gate = ReceiveGate::new(2);
    let a = gate.acquire().await;
    let b = gate.acquire().await;
    assert_eq!(gate.in_use(), 2);
    drop(a);
    let _c = gate.acquire().await;
    drop(b);
    assert_eq!(gate.in_use(), 1);
}

#[tokio::test]
async fn a_cancelled_acquire_never_leaks_a_lane() {
    let gate = std::sync::Arc::new(ReceiveGate::new(1));
    let held = gate.acquire().await;
    let g = std::sync::Arc::clone(&gate);
    let waiter = tokio::spawn(async move { g.acquire_collab().await });
    tokio::task::yield_now().await;
    waiter.abort();
    let _ = waiter.await;
    drop(held);
    // the lane is free again for the next caller
    let _again = tokio::time::timeout(std::time::Duration::from_secs(1), gate.acquire()).await.unwrap();
    assert_eq!(gate.in_use(), 1);
}

#[tokio::test]
async fn a_shrink_takes_effect_as_holders_finish() {
    let gate = std::sync::Arc::new(ReceiveGate::new(2));
    let a = gate.acquire().await;
    let b = gate.acquire_collab().await;
    gate.set_limit(1);
    let g = std::sync::Arc::clone(&gate);
    let next = tokio::spawn(async move { g.acquire().await });
    drop(a);
    tokio::task::yield_now().await;
    assert!(!next.is_finished(), "one holder left = the new limit");
    drop(b);
    let _n = tokio::time::timeout(std::time::Duration::from_secs(1), next).await.unwrap().unwrap();
    assert_eq!(gate.limit(), 1);
}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib sync::receiver::tests::a_waiting_personal`. Expected: compile errors (`acquire_collab`, `ReceiveClass` missing).

- [ ] **Step 3: Implement** (replace `GateState` + `ReceiveGate`; keep the clamp constants and the doc's "live, never interrupts a holder" contract):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiveClass {
    Personal,
    Collab,
}

struct Waiter {
    tx: tokio::sync::oneshot::Sender<ReceivePermit>,
}

struct GateInner {
    limit: usize,
    in_use: usize,
    personal: std::collections::VecDeque<Waiter>,
    collab: std::collections::VecDeque<Waiter>,
}

/// Live-resizable two-class admission for the fetch+ingest phase (W2 T2.2,
/// collab v3 wave 3 §8). A waiting personal transfer is always admitted
/// before any collab unit (L1); while one waits and every lane is taken,
/// `yield_signal` is true and the collab lane finishes its frame in flight,
/// releases and re-queues. Shrinking never interrupts a holder: grants stop
/// until `in_use` falls below the new limit (the old debt counter's effect,
/// without the bookkeeping).
pub struct ReceiveGate {
    inner: std::sync::Arc<std::sync::Mutex<GateInner>>,
    yield_tx: std::sync::Arc<tokio::sync::watch::Sender<bool>>,
}

pub struct ReceivePermit {
    inner: std::sync::Arc<std::sync::Mutex<GateInner>>,
    yield_tx: std::sync::Arc<tokio::sync::watch::Sender<bool>>,
    class: ReceiveClass,
}

impl ReceivePermit {
    pub fn class(&self) -> ReceiveClass {
        self.class
    }
}

impl Drop for ReceivePermit {
    fn drop(&mut self) {
        let grants = {
            let mut g = self.inner.lock().expect("receive gate poisoned");
            g.in_use -= 1;
            ReceiveGate::collect_grants(&mut g)
        };
        ReceiveGate::deliver(&self.inner, &self.yield_tx, grants);
    }
}

impl ReceiveGate {
    pub fn new(limit: usize) -> Self {
        let limit = limit.clamp(MIN_CONCURRENT_RECEIVES, MAX_CONCURRENT_RECEIVES);
        Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(GateInner {
                limit,
                in_use: 0,
                personal: Default::default(),
                collab: Default::default(),
            })),
            yield_tx: std::sync::Arc::new(tokio::sync::watch::channel(false).0),
        }
    }

    fn permit(&self, class: ReceiveClass) -> ReceivePermit {
        ReceivePermit { inner: std::sync::Arc::clone(&self.inner), yield_tx: std::sync::Arc::clone(&self.yield_tx), class }
    }

    /// Under the lock: reserve lanes for the next waiters, personal first.
    fn collect_grants(g: &mut GateInner) -> Vec<(Waiter, ReceiveClass)> {
        let mut out = Vec::new();
        while g.in_use < g.limit {
            let next = g
                .personal
                .pop_front()
                .map(|w| (w, ReceiveClass::Personal))
                .or_else(|| g.collab.pop_front().map(|w| (w, ReceiveClass::Collab)));
            let Some((w, class)) = next else { break };
            if w.tx.is_closed() {
                continue; // a cancelled waiter
            }
            g.in_use += 1;
            out.push((w, class));
        }
        out
    }

    /// Outside the lock: hand the reserved lanes over. A waiter cancelled in
    /// between gets its permit dropped here, which releases the lane again.
    fn deliver(
        inner: &std::sync::Arc<std::sync::Mutex<GateInner>>,
        yield_tx: &std::sync::Arc<tokio::sync::watch::Sender<bool>>,
        grants: Vec<(Waiter, ReceiveClass)>,
    ) {
        for (w, class) in grants {
            let permit = ReceivePermit { inner: std::sync::Arc::clone(inner), yield_tx: std::sync::Arc::clone(yield_tx), class };
            drop(w.tx.send(permit)); // Err(permit) → dropped → released
        }
        let want_yield = {
            let g = inner.lock().expect("receive gate poisoned");
            g.personal.iter().any(|w| !w.tx.is_closed()) && g.in_use >= g.limit
        };
        yield_tx.send_if_modified(|y| {
            if *y != want_yield { *y = want_yield; true } else { false }
        });
    }

    async fn acquire_class(&self, class: ReceiveClass) -> ReceivePermit {
        let rx = {
            let mut g = self.inner.lock().expect("receive gate poisoned");
            let may_jump = match class {
                ReceiveClass::Personal => g.personal.is_empty(),
                ReceiveClass::Collab => g.personal.is_empty() && g.collab.is_empty(),
            };
            if may_jump && g.in_use < g.limit {
                g.in_use += 1;
                drop(g);
                return self.permit(class);
            }
            let (tx, rx) = tokio::sync::oneshot::channel();
            match class {
                ReceiveClass::Personal => g.personal.push_back(Waiter { tx }),
                ReceiveClass::Collab => g.collab.push_back(Waiter { tx }),
            }
            rx
        };
        Self::deliver(&self.inner, &self.yield_tx, Vec::new()); // refresh the yield flag
        rx.await.expect("the gate outlives its waiters")
    }

    pub async fn acquire(&self) -> ReceivePermit {
        self.acquire_class(ReceiveClass::Personal).await
    }

    pub async fn acquire_collab(&self) -> ReceivePermit {
        self.acquire_class(ReceiveClass::Collab).await
    }

    pub fn yield_signal(&self) -> tokio::sync::watch::Receiver<bool> {
        self.yield_tx.subscribe()
    }

    pub fn set_limit(&self, limit: usize) {
        let limit = limit.clamp(MIN_CONCURRENT_RECEIVES, MAX_CONCURRENT_RECEIVES);
        let grants = {
            let mut g = self.inner.lock().expect("receive gate poisoned");
            g.limit = limit;
            Self::collect_grants(&mut g)
        };
        Self::deliver(&self.inner, &self.yield_tx, grants);
    }

    pub fn limit(&self) -> usize {
        self.inner.lock().expect("receive gate poisoned").limit
    }

    pub fn in_use(&self) -> usize {
        self.inner.lock().expect("receive gate poisoned").in_use
    }
}
```

  Why `rx.await.expect(..)` is sound: the sender side lives in the gate's
  waiter queues, and a queue entry is removed only by `collect_grants`,
  which sends; the gate lives as long as `InboundControl` (the same argument
  the wave-2 code made for its semaphore, receiver.rs:288-293). Cancel-safety:
  dropping the future drops `rx`; `collect_grants` skips a closed sender,
  and a grant racing the drop is released by `deliver`.

- [ ] **Step 4: Adapt callers** — receiver.rs:2113 holds the returned
  `ReceivePermit` exactly as it held the semaphore permit; CE:2328-2331 calls
  `acquire_collab()`. Adjust the existing gate tests only where they name
  `OwnedSemaphorePermit` or reach into `GateState`; their assertions
  (FIFO, live grow/shrink, cancel-safety) are the pin.

- [ ] **Step 5: Run** `cargo test -p athenaeum-core --lib sync::receiver api::collab_exchange && cargo check --workspace --all-targets`. Expected: PASS.

- [ ] **Step 6: Commit**

```bash
rustfmt crates/athenaeum-core/src/sync/receiver.rs crates/athenaeum-core/src/sync/mod.rs crates/athenaeum-core/src/api/collab_exchange.rs
git add -A crates/athenaeum-core/src
git commit -m "feat(sync): two-class ReceiveGate — personal transfers always admitted first, a yield signal for the collab lane"
```

---
### Task 14: Scheduler deterministic core and seeded simulation (§7.1–§7.4, §12, I1–I11)

**Implementer:** rust-engineer.

**Files:**
- Create: `crates/athenaeum-core/src/collab/scheduler/mod.rs` (`pub mod core; #[cfg(test)] mod sim_tests;`), `collab/scheduler/core.rs`, `collab/scheduler/sim_tests.rs`. Register `pub mod scheduler;` in `collab/mod.rs`.
- Test: unit tests in `core.rs`; the seeded simulation in `sim_tests.rs`.

**Interfaces:**
- Consumes: `collab::live::backoff::{BACKOFF_BASE, BACKOFF_CAP}` (Task 2), `collab::live::holders::{providers, ProjectHolders, FrameRef, Provider}` and `collab::live::presence::PresenceBook` (Tasks 4, 6) — the simulation derives providers with the production functions; `collab::live::cursor::step` (Task 5); `collab::serve::decide` (Task 10); `geometry::ransac::SplitMix64`.
- Produces:
  ```rust
  pub const WORK_UNIT_MAX_BYTES: u64 = 256 * 1024 * 1024;
  pub const STARVATION: Duration = Duration::from_secs(3600);
  pub type FrameKey = (String, String);                   // (project_id, frame_uuid)
  #[derive(Debug, Clone, PartialEq, Eq)] pub struct Want { pub key: FrameKey, pub content_version: i32, pub blake3: String, pub byte_size: i64, pub since_ms: i64 }
  #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)] pub struct ProviderRef { pub device: String, pub relay_url: Option<String> }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum FetchResult { Landed, Failed, Cancelled, AwaitingGc }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum CancelReason { NewVersion, NotWanted, ProjectGone, StorageUnavailable }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum Input {
      NeedSet { project_id: String, wants: Vec<Want> },           // the project's whole need set (replacement)
      Providers { key: FrameKey, providers: Vec<ProviderRef> },   // candidates of the CURRENT version (I4, I5)
      ProjectGone { project_id: String },
      Storage { fetching: bool },
      Slots(usize),                                               // collab.max_receive_streams
      Lane { admitted: bool },                                    // the collab ReceiveGate permit is held / was yielded
      Finished { key: FrameKey, content_version: i32, result: FetchResult },
      DialFailed { device: String },
      ConnectionClosed { device: String },
      DialOk { device: String },
      ClearBackoffs,                                              // Sync now (L10)
      Tick,
  }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum Command {
      Start { key: FrameKey, content_version: i32, blake3: String, byte_size: i64, providers: Vec<ProviderRef> },
      UpdateProviders { key: FrameKey, providers: Vec<ProviderRef> },
      Cancel { key: FrameKey, reason: CancelReason },
      RequestLane,
      ReleaseLane,
  }
  pub struct Core { /* see Step 3 */ }
  impl Core {
      pub fn new(seed: u64, slots: usize) -> Self;
      pub fn step(&mut self, now_ms: i64, input: Input) -> Vec<Command>;
      pub fn in_flight(&self) -> Vec<(FrameKey, i32)>;
      pub fn next_wake_ms(&self) -> Option<i64>;
      pub fn lane(&self) -> bool;
  }
  ```

- [ ] **Step 1: Write the failing unit tests** (`core.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn want(u: &str, cv: i32, since: i64) -> Want {
        Want { key: ("p1".into(), u.into()), content_version: cv, blake3: format!("b-{u}-{cv}"), byte_size: 10, since_ms: since }
    }
    fn prov(d: &str) -> ProviderRef { ProviderRef { device: d.into(), relay_url: None } }
    fn starts(cmds: &[Command]) -> Vec<String> {
        cmds.iter().filter_map(|c| match c { Command::Start { key, .. } => Some(key.1.clone()), _ => None }).collect()
    }

    fn primed(slots: usize) -> Core {
        let mut c = Core::new(1, slots);
        assert_eq!(c.step(0, Input::NeedSet { project_id: "p1".into(), wants: vec![want("a", 1, 0), want("b", 1, 0), want("c", 1, 0)] }), vec![]);
        c
    }

    #[test]
    fn providerless_frames_sleep_and_a_provider_wakes_them() {
        let mut c = primed(4);
        assert!(c.step(0, Input::Tick).is_empty());
        let cmds = c.step(1, Input::Providers { key: ("p1".into(), "b".into()), providers: vec![prov("X")] });
        assert_eq!(cmds, vec![Command::RequestLane]);
        let cmds = c.step(2, Input::Lane { admitted: true });
        assert_eq!(starts(&cmds), vec!["b".to_string()]);
    }

    #[test]
    fn rarest_first_then_random_and_the_slot_cap_holds() {
        let mut c = primed(1);
        c.step(0, Input::Providers { key: ("p1".into(), "a".into()), providers: vec![prov("X"), prov("Y")] });
        c.step(0, Input::Providers { key: ("p1".into(), "b".into()), providers: vec![prov("X")] });
        let cmds = c.step(0, Input::Lane { admitted: true });
        assert_eq!(starts(&cmds), vec!["b".to_string()], "the rarer frame goes first");
        assert_eq!(c.in_flight().len(), 1);
    }

    #[test]
    fn a_frame_waiting_an_hour_jumps_the_queue() {
        let mut c = Core::new(1, 1);
        c.step(0, Input::NeedSet { project_id: "p1".into(), wants: vec![want("old", 1, 0), want("rare", 1, 3_500_000)] });
        c.step(0, Input::Providers { key: ("p1".into(), "old".into()), providers: vec![prov("X"), prov("Y"), prov("Z")] });
        c.step(0, Input::Providers { key: ("p1".into(), "rare".into()), providers: vec![prov("X")] });
        let cmds = c.step(STARVATION.as_millis() as i64 + 1, Input::Lane { admitted: true });
        assert_eq!(starts(&cmds), vec!["old".to_string()]);
    }

    #[test]
    fn a_new_version_cancels_the_fetch_in_flight_in_the_same_step() {
        let mut c = primed(4);
        c.step(0, Input::Providers { key: ("p1".into(), "a".into()), providers: vec![prov("X")] });
        c.step(0, Input::Lane { admitted: true });
        let cmds = c.step(1, Input::NeedSet { project_id: "p1".into(), wants: vec![want("a", 2, 0), want("b", 1, 0), want("c", 1, 0)] });
        assert!(cmds.contains(&Command::Cancel { key: ("p1".into(), "a".into()), reason: CancelReason::NewVersion }));
        assert!(c.in_flight().is_empty());
    }

    #[test]
    fn exclusion_storage_loss_and_a_lost_project_cancel_at_once() {
        let mut c = primed(4);
        for u in ["a", "b"] {
            c.step(0, Input::Providers { key: ("p1".into(), u.into()), providers: vec![prov("X")] });
        }
        c.step(0, Input::Lane { admitted: true });
        let cmds = c.step(1, Input::NeedSet { project_id: "p1".into(), wants: vec![want("b", 1, 0)] });
        assert!(cmds.contains(&Command::Cancel { key: ("p1".into(), "a".into()), reason: CancelReason::NotWanted }));
        let cmds = c.step(2, Input::Storage { fetching: false });
        assert!(cmds.contains(&Command::Cancel { key: ("p1".into(), "b".into()), reason: CancelReason::StorageUnavailable }));
        assert!(starts(&c.step(3, Input::Tick)).is_empty());
        c.step(4, Input::Storage { fetching: true });
        let cmds = c.step(5, Input::ProjectGone { project_id: "p1".into() });
        assert!(starts(&cmds).is_empty());
        assert!(c.in_flight().is_empty());
    }

    #[test]
    fn a_failed_dial_backs_off_that_provider_and_sync_now_clears_it() {
        let mut c = primed(4);
        c.step(0, Input::Providers { key: ("p1".into(), "a".into()), providers: vec![prov("X")] });
        c.step(0, Input::Lane { admitted: true });
        let cmds = c.step(1, Input::DialFailed { device: "X".into() });
        assert!(cmds.contains(&Command::UpdateProviders { key: ("p1".into(), "a".into()), providers: vec![] }));
        assert!(c.next_wake_ms().is_some());
        c.step(2, Input::Finished { key: ("p1".into(), "a".into()), content_version: 1, result: FetchResult::Failed });
        assert!(starts(&c.step(3, Input::Tick)).is_empty(), "X is backing off");
        // the idle lane was released; clearing the back-offs asks for it again
        assert_eq!(c.step(4, Input::ClearBackoffs), vec![Command::RequestLane]);
        assert_eq!(starts(&c.step(5, Input::Lane { admitted: true })), vec!["a".to_string()]);
    }

    #[test]
    fn a_yielded_lane_takes_no_new_unit_and_is_released_when_idle() {
        let mut c = primed(4);
        c.step(0, Input::Providers { key: ("p1".into(), "a".into()), providers: vec![prov("X")] });
        c.step(0, Input::Lane { admitted: true });
        c.step(1, Input::Lane { admitted: false });
        c.step(1, Input::Providers { key: ("p1".into(), "b".into()), providers: vec![prov("X")] });
        assert!(starts(&c.step(2, Input::Tick)).is_empty());
        let cmds = c.step(3, Input::Finished { key: ("p1".into(), "a".into()), content_version: 1, result: FetchResult::Landed });
        assert!(cmds.contains(&Command::RequestLane), "work remains: ask for the lane again");
    }
}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib collab::scheduler`. Expected: compile errors.

- [ ] **Step 3: `collab/scheduler/core.rs`**

```rust
//! The receive-side scheduler as a pure, deterministic core (spec §7.1):
//! hub events, disk events, fetch results and timers in; fetch, cancel and
//! lane commands out. The executor (`api::collab_live::executor`) performs
//! the commands and re-checks every precondition against the database; the
//! landing fence stays a DB conditional (I1). BTreeMaps and a seeded RNG make
//! every run reproducible (§12).

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crate::collab::live::backoff::{BACKOFF_BASE, BACKOFF_CAP};
use crate::geometry::ransac::SplitMix64;

pub const WORK_UNIT_MAX_BYTES: u64 = 256 * 1024 * 1024;
pub const STARVATION: Duration = Duration::from_secs(3600);

// … the public types of the Interfaces block …

#[derive(Debug, Clone)]
struct InFlight {
    content_version: i32,
    blake3: String,
    providers: Vec<ProviderRef>,
}

pub struct Core {
    rng: SplitMix64,
    slots: usize,
    lane: bool,
    lane_requested: bool,
    storage_ok: bool,
    wants: BTreeMap<FrameKey, Want>,
    tiebreak: BTreeMap<FrameKey, u64>,
    providers: BTreeMap<FrameKey, Vec<ProviderRef>>,
    in_flight: BTreeMap<FrameKey, InFlight>,
    dial_backoff: BTreeMap<String, (i64, u32)>,   // device → (until_ms, attempt)
    frame_backoff: BTreeMap<FrameKey, (i64, u32)>,
}

impl Core {
    pub fn new(seed: u64, slots: usize) -> Self {
        Self {
            rng: SplitMix64(seed),
            slots: slots.max(1),
            lane: false,
            lane_requested: false,
            storage_ok: true,
            wants: BTreeMap::new(),
            tiebreak: BTreeMap::new(),
            providers: BTreeMap::new(),
            in_flight: BTreeMap::new(),
            dial_backoff: BTreeMap::new(),
            frame_backoff: BTreeMap::new(),
        }
    }

    fn jitter_ms(&mut self, attempt: u32) -> i64 {
        let upper = BACKOFF_CAP.min(BACKOFF_BASE.saturating_mul(1u32 << attempt.min(16))).as_millis() as f64;
        (upper * self.rng.next_f64()).max(100.0) as i64
    }

    fn available(&self, key: &FrameKey, now_ms: i64) -> Vec<ProviderRef> {
        self.providers
            .get(key)
            .map(|ps| {
                ps.iter()
                    .filter(|p| self.dial_backoff.get(&p.device).is_none_or(|(until, _)| *until <= now_ms))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn cancel(&mut self, key: &FrameKey, reason: CancelReason, out: &mut Vec<Command>) {
        if self.in_flight.remove(key).is_some() {
            out.push(Command::Cancel { key: key.clone(), reason });
        }
    }

    pub fn step(&mut self, now_ms: i64, input: Input) -> Vec<Command> {
        let mut out = Vec::new();
        match input {
            Input::NeedSet { project_id, wants } => {
                let incoming: BTreeMap<FrameKey, Want> = wants.into_iter().map(|w| (w.key.clone(), w)).collect();
                let old: Vec<FrameKey> = self.wants.keys().filter(|k| k.0 == project_id).cloned().collect();
                for k in old {
                    if !incoming.contains_key(&k) {
                        self.wants.remove(&k);
                        self.tiebreak.remove(&k);
                    }
                }
                let flying: Vec<FrameKey> = self.in_flight.keys().filter(|k| k.0 == project_id).cloned().collect();
                for k in flying {
                    match incoming.get(&k) {
                        None => self.cancel(&k, CancelReason::NotWanted, &mut out),
                        Some(w) => {
                            let f = &self.in_flight[&k];
                            if f.content_version != w.content_version || f.blake3 != w.blake3 {
                                self.cancel(&k, CancelReason::NewVersion, &mut out);
                                self.providers.remove(&k); // the old version's candidates
                            }
                        }
                    }
                }
                for (k, w) in incoming {
                    if self.wants.get(&k).is_some_and(|old| old.content_version != w.content_version) {
                        self.frame_backoff.remove(&k);
                    }
                    if !self.tiebreak.contains_key(&k) {
                        let r = self.rng.next_u64();
                        self.tiebreak.insert(k.clone(), r);
                    }
                    self.wants.insert(k, w);
                }
            }
            Input::Providers { key, providers } => {
                let mut ps = providers;
                ps.sort();
                ps.dedup();
                self.providers.insert(key.clone(), ps);
                if self.in_flight.contains_key(&key) {
                    let now = self.available(&key, now_ms);
                    let f = self.in_flight.get_mut(&key).expect("checked");
                    if f.providers != now {
                        f.providers = now.clone();
                        out.push(Command::UpdateProviders { key, providers: now });
                    }
                }
            }
            Input::ProjectGone { project_id } => {
                let flying: Vec<FrameKey> = self.in_flight.keys().filter(|k| k.0 == project_id).cloned().collect();
                for k in flying {
                    self.cancel(&k, CancelReason::ProjectGone, &mut out);
                }
                self.wants.retain(|k, _| k.0 != project_id);
                self.providers.retain(|k, _| k.0 != project_id);
                self.tiebreak.retain(|k, _| k.0 != project_id);
                self.frame_backoff.retain(|k, _| k.0 != project_id);
            }
            Input::Storage { fetching } => {
                self.storage_ok = fetching;
                if !fetching {
                    let flying: Vec<FrameKey> = self.in_flight.keys().cloned().collect();
                    for k in flying {
                        self.cancel(&k, CancelReason::StorageUnavailable, &mut out);
                    }
                }
            }
            Input::Slots(n) => self.slots = n.max(1),
            Input::Lane { admitted } => {
                self.lane = admitted;
                self.lane_requested = false;
            }
            Input::Finished { key, content_version, result } => {
                if self.in_flight.get(&key).is_some_and(|f| f.content_version == content_version) {
                    self.in_flight.remove(&key);
                }
                match result {
                    FetchResult::Landed | FetchResult::AwaitingGc => {
                        if self.wants.get(&key).is_some_and(|w| w.content_version == content_version) {
                            self.wants.remove(&key);
                        }
                        self.frame_backoff.remove(&key);
                    }
                    FetchResult::Failed => {
                        let attempt = self.frame_backoff.get(&key).map_or(0, |(_, a)| a + 1);
                        let until = now_ms + self.jitter_ms(attempt);
                        self.frame_backoff.insert(key, (until, attempt));
                    }
                    FetchResult::Cancelled => {}
                }
            }
            Input::DialFailed { device } | Input::ConnectionClosed { device } => {
                let attempt = self.dial_backoff.get(&device).map_or(0, |(_, a)| a + 1);
                let until = now_ms + self.jitter_ms(attempt);
                self.dial_backoff.insert(device.clone(), (until, attempt));
                let flying: Vec<FrameKey> = self.in_flight.keys().cloned().collect();
                for k in flying {
                    let now = self.available(&k, now_ms);
                    let f = self.in_flight.get_mut(&k).expect("listed");
                    if f.providers.iter().any(|p| p.device == device) {
                        f.providers = now.clone();
                        out.push(Command::UpdateProviders { key: k, providers: now });
                    }
                }
            }
            Input::DialOk { device } => {
                self.dial_backoff.remove(&device);
            }
            Input::ClearBackoffs => {
                self.dial_backoff.clear();
                self.frame_backoff.clear();
            }
            Input::Tick => {}
        }
        self.schedule(now_ms, &mut out);
        out
    }

    fn schedule(&mut self, now_ms: i64, out: &mut Vec<Command>) {
        if !self.storage_ok {
            return;
        }
        let starving = STARVATION.as_millis() as i64;
        let mut candidates: Vec<(bool, i64, usize, u64, FrameKey)> = self
            .wants
            .iter()
            .filter(|(k, _)| !self.in_flight.contains_key(*k))
            .filter(|(k, _)| self.frame_backoff.get(*k).is_none_or(|(until, _)| *until <= now_ms))
            .filter_map(|(k, w)| {
                let n = self.available(k, now_ms).len();
                (n > 0).then(|| (now_ms - w.since_ms < starving, if now_ms - w.since_ms >= starving { w.since_ms } else { 0 }, n, self.tiebreak[k], k.clone()))
            })
            .collect();
        // starving first (by age), then rarest, then the per-want random draw
        candidates.sort();
        if candidates.is_empty() {
            if self.lane && self.in_flight.is_empty() {
                self.lane = false;
                out.push(Command::ReleaseLane);
            }
            return;
        }
        if !self.lane {
            // a yielded lane re-queues only once its units in flight are done
            if !self.lane_requested && self.in_flight.is_empty() {
                self.lane_requested = true;
                out.push(Command::RequestLane);
            }
            return;
        }
        for (_, _, _, _, key) in candidates {
            if self.in_flight.len() >= self.slots {
                break;
            }
            let w = self.wants[&key].clone();
            let providers = self.available(&key, now_ms);
            self.in_flight.insert(key.clone(), InFlight { content_version: w.content_version, blake3: w.blake3.clone(), providers: providers.clone() });
            out.push(Command::Start { key, content_version: w.content_version, blake3: w.blake3, byte_size: w.byte_size, providers });
        }
    }

    pub fn in_flight(&self) -> Vec<(FrameKey, i32)> {
        self.in_flight.iter().map(|(k, f)| (k.clone(), f.content_version)).collect()
    }

    pub fn next_wake_ms(&self) -> Option<i64> {
        self.dial_backoff.values().map(|(u, _)| *u).chain(self.frame_backoff.values().map(|(u, _)| *u)).min()
    }

    pub fn lane(&self) -> bool {
        self.lane
    }
}
```

  (`Lane { admitted: false }` while units are in flight is the yield: the
  core takes no new unit, keeps the in-flight ones and, once they finish,
  asks for the lane again if work remains — the unit test above.)

- [ ] **Step 4: The seeded simulation** (`sim_tests.rs`). A small world
  model — hub manifest, claims, presence, membership, per-frame local state,
  in-flight fetches — driven by random events; the core is stepped with
  exactly what the executor would feed it; the invariants are asserted after
  EVERY step; a failure prints the seed and the step:

```rust
//! Seeded randomized interleavings of hub events (`project`/`holders` in
//! both orders), presence, disk events, fetch results and crashes, with
//! I1–I11 asserted after every step (spec §12). Reproduce one failing seed
//! with `COLLAB_SIM_SEED=<n> cargo test -p athenaeum-core --lib collab::scheduler::sim_tests -- --nocapture`.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::core::*;
use crate::collab::live::holders::{providers as derive_providers, FrameRef, ProjectHolders};
use crate::collab::live::presence::PresenceBook;
use crate::collab::live::wire::{HolderDeltaWire, PresenceChange, PresenceEvent};
use crate::geometry::ransac::SplitMix64;

const DEVICES: [&str; 5] = ["ME", "A", "B", "C", "D"];
const FRAMES: usize = 12;
const STEPS: usize = 400;

#[derive(Clone, Debug)]
struct HubFrame { cv: i32, published: bool, accepted: bool }

#[derive(Clone, Debug, PartialEq, Eq)]
enum Local { Wanted, Held(i32), Quarantined, NotKept, Idle }

struct World {
    rng: SplitMix64,
    now: i64,
    frames: BTreeMap<String, HubFrame>,          // uuid → current state (project "p1")
    holders: ProjectHolders,                     // the production holder map
    presence: PresenceBook,                      // the production presence book
    members: HashSet<String>,
    local: BTreeMap<String, Local>,
    storage_ok: bool,
    core: Core,
    flying: BTreeMap<String, (i32, Vec<String>)>, // uuid → (cv, providers) as the executor sees it
    slots: usize,
    project_live: bool,
}

impl World {
    fn new(seed: u64) -> World {
        let mut w = World {
            rng: SplitMix64(seed),
            now: 0,
            frames: (0..FRAMES).map(|i| (format!("f{i:02}"), HubFrame { cv: 1, published: true, accepted: true })).collect(),
            holders: ProjectHolders::default(),
            presence: PresenceBook::default(),
            members: DEVICES.iter().map(|d| d.to_string()).collect(),
            local: (0..FRAMES).map(|i| (format!("f{i:02}"), Local::Wanted)).collect(),
            storage_ok: true,
            core: Core::new(seed, 3),
            flying: BTreeMap::new(),
            slots: 3,
            project_live: true,
        };
        w.presence.apply_hello("p1", &[]);
        w
    }

    /// What the executor computes from the DB: the need set (I8).
    fn need_set(&self) -> Vec<Want> {
        if !self.project_live {
            return vec![];
        }
        self.frames
            .iter()
            .filter(|(u, f)| f.published && f.accepted && self.local.get(*u) == Some(&Local::Wanted))
            .map(|(u, f)| Want { key: ("p1".into(), u.clone()), content_version: f.cv, blake3: format!("b-{u}-{}", f.cv), byte_size: 10, since_ms: 0 })
            .collect()
    }

    fn seq_of(u: &str) -> i32 { u[1..].parse::<i32>().unwrap() + 1 }

    /// Providers with the PRODUCTION derivation (I4, I5).
    fn providers_of(&self, u: &str) -> Vec<ProviderRef> {
        let pubs: HashSet<String> = HashSet::new();
        let f = FrameRef { project_id: "p1", frame_seq: Self::seq_of(u), content_version: self.frames[u].cv, publisher_devices: &pubs };
        derive_providers(&self.holders, &self.presence, &self.members, "ME", &f)
            .into_iter()
            .map(|p| ProviderRef { device: p.device, relay_url: p.relay_url })
            .collect()
    }

    fn feed(&mut self, input: Input) {
        let cmds = self.core.step(self.now, input);
        self.execute(cmds);
    }

    fn resync_all(&mut self) {
        let wants = self.need_set();
        self.feed(Input::NeedSet { project_id: "p1".into(), wants });
        let us: Vec<String> = self.frames.keys().cloned().collect();
        for u in us {
            let providers = self.providers_of(&u);
            self.feed(Input::Providers { key: ("p1".into(), u), providers });
        }
    }

    fn execute(&mut self, cmds: Vec<Command>) {
        for c in cmds {
            match c {
                Command::Start { key, content_version, blake3, providers, .. } => {
                    // I1: the pinned target is the CURRENT manifest version
                    let f = &self.frames[&key.1];
                    assert_eq!(content_version, f.cv, "I1: start pins the current version");
                    assert_eq!(blake3, format!("b-{}-{}", key.1, f.cv), "I1: start pins the current hash");
                    // I8: only the replication set
                    assert!(f.published && f.accepted && self.project_live, "I8: start of a frame outside the replication set");
                    assert_eq!(self.local[&key.1], Local::Wanted, "start of a frame that is not wanted (quarantined/not kept/idle never fetch)");
                    // I5: every provider is a current candidate
                    let ok: BTreeSet<String> = self.providers_of(&key.1).into_iter().map(|p| p.device).collect();
                    for p in &providers {
                        assert!(ok.contains(&p.device), "I5: {} is not a candidate for {}", p.device, key.1);
                    }
                    assert!(!providers.is_empty(), "a providerless frame was started");
                    assert!(self.storage_ok, "a start while storage is unavailable");
                    self.flying.insert(key.1.clone(), (content_version, providers.into_iter().map(|p| p.device).collect()));
                }
                Command::UpdateProviders { key, providers } => {
                    if let Some(f) = self.flying.get_mut(&key.1) {
                        f.1 = providers.into_iter().map(|p| p.device).collect();
                    }
                }
                Command::Cancel { key, .. } => {
                    self.flying.remove(&key.1);
                }
                Command::RequestLane => {
                    let cmds = self.core.step(self.now, Input::Lane { admitted: true });
                    self.execute(cmds);
                }
                Command::ReleaseLane => {}
            }
        }
    }

    fn assert_invariants(&self) {
        // I6: nothing in flight for a superseded version
        for (u, (cv, _)) in &self.flying {
            assert_eq!(*cv, self.frames[u].cv, "I6: {u} fetching superseded v{cv}");
        }
        // the core and the executor agree on what is in flight
        let core: BTreeSet<String> = self.core.in_flight().into_iter().map(|(k, _)| k.1).collect();
        let exec: BTreeSet<String> = self.flying.keys().cloned().collect();
        assert_eq!(core, exec, "core and executor disagree on in-flight work");
        assert!(self.flying.len() <= self.slots, "slot cap exceeded");
        if !self.storage_ok {
            assert!(self.flying.is_empty(), "fetching continues on an unavailable store");
        }
        // I7: automation never drops a held replica — only landing (a newer
        // Held), a disk event or a user action changes it (checked in step())
    }

    /// One random event. Returns a label for the failure message.
    fn step(&mut self) -> &'static str {
        self.now += 1 + (self.rng.below(5_000) as i64);
        let us: Vec<String> = self.frames.keys().cloned().collect();
        let u = us[self.rng.below(us.len())].clone();
        let dev = DEVICES[1 + self.rng.below(DEVICES.len() - 1)].to_string();
        match self.rng.below(14) {
            0 | 1 => {
                // a holder claims a version (current, or an older one) — `holders` before or after `project`
                let cv = if self.rng.below(3) == 0 { (self.frames[&u].cv - 1).max(1) } else { self.frames[&u].cv };
                self.holders.apply_delta(&HolderDeltaWire { device: dev, add: vec![(Self::seq_of(&u), cv)], rm: vec![] });
                let providers = self.providers_of(&u);
                self.feed(Input::Providers { key: ("p1".into(), u), providers });
                "holders add"
            }
            2 => {
                self.holders.apply_delta(&HolderDeltaWire { device: dev, add: vec![], rm: vec![Self::seq_of(&u)] });
                let providers = self.providers_of(&u);
                self.feed(Input::Providers { key: ("p1".into(), u), providers });
                "holders rm"
            }
            3 | 4 => {
                let connected = self.rng.below(4) != 0;
                self.presence.apply_event(&PresenceEvent { project_id: "p1".into(), replace: false, changes: vec![PresenceChange { device: dev, connected, serving: connected && self.rng.below(5) != 0, relay_url: None }] });
                self.resync_all();
                "presence"
            }
            5 => {
                // I10: a CAS version bump (the hub only accepts expected == current)
                let f = self.frames.get_mut(&u).unwrap();
                f.cv += 1;
                if let Local::Held(_) = self.local[&u] {
                    self.local.insert(u.clone(), Local::Wanted); // Held(vN) → Wanted(vN+1); the vN file stays (L7)
                }
                self.resync_all();
                "new version"
            }
            6 => {
                let f = self.frames.get_mut(&u).unwrap();
                f.accepted = !f.accepted;
                self.local.insert(u.clone(), if f.accepted { Local::Wanted } else { Local::Idle });
                self.resync_all();
                "exclusion toggled"
            }
            7 => {
                // a fetch finishes; the landing fence only records the current version (I1)
                if let Some((fu, (cv, _))) = self.flying.iter().next().map(|(k, v)| (k.clone(), v.clone())) {
                    let result = if self.rng.below(4) == 0 { FetchResult::Failed } else { FetchResult::Landed };
                    self.flying.remove(&fu);
                    if result == FetchResult::Landed && cv == self.frames[&fu].cv && self.local[&fu] == Local::Wanted {
                        self.local.insert(fu.clone(), Local::Held(cv));
                        self.holders.apply_delta(&HolderDeltaWire { device: "ME".into(), add: vec![(Self::seq_of(&fu), cv)], rm: vec![] });
                    }
                    self.feed(Input::Finished { key: ("p1".into(), fu), content_version: cv, result });
                    let wants = self.need_set();
                    self.feed(Input::NeedSet { project_id: "p1".into(), wants });
                }
                "fetch finished"
            }
            8 => {
                self.storage_ok = !self.storage_ok;
                self.feed(Input::Storage { fetching: self.storage_ok });
                "storage toggled"
            }
            9 => {
                // a replica edited on disk → quarantined; it never fetches or lands (L5, P13)
                if let Local::Held(_) = self.local[&u] {
                    self.local.insert(u.clone(), Local::Quarantined);
                    let wants = self.need_set();
                    self.feed(Input::NeedSet { project_id: "p1".into(), wants });
                }
                "edited"
            }
            10 => {
                // I11: a member leaves; their devices stop being candidates in the same step
                self.members.remove(&dev);
                self.resync_all();
                for (fu, (_, provs)) in &self.flying {
                    assert!(provs.iter().all(|p| self.members.contains(p)), "I11: a removed member is still a provider of {fu}");
                }
                "member left"
            }
            11 => {
                self.feed(Input::DialFailed { device: dev });
                "dial failed"
            }
            12 => {
                // crash: a fresh core, re-fed from the model (C24: local state survives, Fetching does not)
                self.core = Core::new(self.rng.next_u64(), self.slots);
                self.flying.clear();
                self.feed(Input::Storage { fetching: self.storage_ok });
                self.resync_all();
                "crash"
            }
            _ => {
                self.feed(Input::Tick);
                "tick"
            }
        }
    }
}

fn run_seed(seed: u64) {
    let mut w = World::new(seed);
    w.resync_all();
    for i in 0..STEPS {
        let what = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let label = w.step();
            w.assert_invariants();
            label
        }));
        if let Err(e) = what {
            panic!("collab scheduler simulation failed: seed {seed}, step {i}: {:?}", e.downcast_ref::<String>().or(e.downcast_ref::<&str>().map(|s| s.to_string()).as_ref()));
        }
    }
}

#[test]
fn seeded_interleavings_keep_every_invariant() {
    if let Ok(s) = std::env::var("COLLAB_SIM_SEED") {
        run_seed(s.parse().expect("COLLAB_SIM_SEED is a number"));
        return;
    }
    for seed in 0..300u64 {
        run_seed(seed);
    }
}

/// I2/I3/I4 for the feed: random delivery order, duplicates and drops of
/// contiguous events, repaired by the catch-up path, always converge to the
/// hub's state and never apply an event twice.
#[test]
fn feed_cursors_converge_under_reordering_duplication_and_loss() {
    use crate::collab::live::cursor::{step as cursor_step, Step};
    for seed in 0..300u64 {
        let mut rng = SplitMix64(seed);
        let head = 40i64;
        let mut events: Vec<(i64, i64)> = (1..=head).map(|v| (v - 1, v)).collect(); // (prev, version)
        // shuffle, duplicate and drop
        for i in (1..events.len()).rev() {
            let j = rng.below(i + 1);
            events.swap(i, j);
        }
        let dups: Vec<(i64, i64)> = events.iter().filter(|_| rng.below(4) == 0).cloned().collect();
        events.extend(dups);
        events.retain(|_| rng.below(6) != 0);
        let mut cursor = 0i64;
        let mut applied: Vec<i64> = Vec::new();
        for (prev, v) in events {
            match cursor_step(cursor, prev, v) {
                Step::Apply => { applied.push(v); cursor = v; }
                Step::Ignore => {}
                Step::CatchUp => { cursor = v; applied.push(v); } // REST catch-up brings the state to v
            }
        }
        // the 60 s versions vector repairs whatever is still missing
        if cursor < head {
            cursor = head;
        }
        assert_eq!(cursor, head, "seed {seed}: cursor did not converge");
        let mut sorted = applied.clone();
        sorted.dedup();
        assert_eq!(sorted.len(), applied.len(), "seed {seed}: an event was applied twice");
        assert!(applied.windows(2).all(|w| w[0] < w[1]), "seed {seed}: cursor went backwards");
    }
}

/// I9: the serve decision never serves a changed or missing file.
#[test]
fn the_serve_check_never_serves_a_changed_file() {
    use crate::collab::serve::{decide, ServeDecision, ServeRecord};
    use crate::collab::storage::sweep::Stamp;
    let mut rng = SplitMix64(7);
    for _ in 0..10_000 {
        let rec = ServeRecord { project_id: "p".into(), frame_uuid: "u".into(), path: "/x".into(), stamp: Stamp { size: 100, mtime: 1_000 } };
        let observed = match rng.below(4) {
            0 => None,
            1 => Some(Stamp { size: 100, mtime: 1_000 + rng.below(3) as i64 }),
            2 => Some(Stamp { size: 101, mtime: 1_000 }),
            _ => Some(Stamp { size: 100, mtime: 1_003 + rng.below(100) as i64 }),
        };
        let d = decide(Some(&rec), observed, true, 0, 8);
        let intact = observed.is_some_and(|o| rec.stamp.matches(&o));
        assert_eq!(d == ServeDecision::Serve, intact);
    }
}
```

  After `resync_all`, every in-flight provider list was updated through
  `UpdateProviders`, so a removed member is gone in the same step (I11).

- [ ] **Step 5: Run** `cargo test -p athenaeum-core --lib collab::scheduler && cargo check -p athenaeum-core --no-default-features`. Expected: PASS; the simulation runs 300 seeds × 400 steps in a few seconds. A failure names its seed; fix the core (never the invariant) and add that seed as a named regression test in `sim_tests.rs`.

- [ ] **Step 6: Commit**

```bash
rustfmt crates/athenaeum-core/src/collab/scheduler/*.rs
git add -A crates/athenaeum-core/src/collab
git commit -m "feat(collab): deterministic scheduler core (need set, rarest→random→starvation, live providers, per-provider dial back-off, cancellation, lane/yield) with a seeded I1–I11 simulation"
```

---
### Task 15: The live orchestrator — session, executor, spawner, clean exit and sign-out; retire the wave-2 worker, pass, maintenance and loss guard (§4.1–§4.2, §4.6, §7, L3, L10, P18, P26–P28, P31, P33)

**Implementer:** rust-engineer.

**Files:**
- Create: `crates/athenaeum-core/src/api/collab_live/session.rs`, `api/collab_live/executor.rs`; extend `api/collab_live/mod.rs` (spawner, handle registry, status, events).
- Modify: `crates/athenaeum-core/src/api/sync.rs:1379` and `:1717` — `spawn_collab_auto_sync` → `spawn_collab_live` (same arguments).
- Modify: `crates/athenaeum-core/src/api/account.rs:333` (`sign_out`) → `collab_live::on_sign_out(ctx).await` first.
- Modify: `crates/athenaeum-tauri/src/lib.rs:521-540` and `crates/athenaeum-web/src/main.rs:360-390` — `athenaeum_core::api::collab_live::shutdown(&ctx).await` (bounded 2 s) before the iroh node shutdown.
- Modify (retire): `crates/athenaeum-core/src/api/collab_exchange.rs` — delete every item the audit marks REMOVE (worker/poll/pass/maintenance/disk truth/loss guard/holder reports/batch fetch/claims/cancel registry/`sync_project_now`/`refresh_collab_frames`, and their tests incl. the ones Task 3 `#[ignore]`d); `crates/athenaeum-core/src/collab/hub_client.rs` — delete `project_versions`, `put_holders`, `frame_holders`, `HolderWire`, `ProjectVersionWire`, `HolderRefWire`; `crates/athenaeum-core/src/sharing/iroh/blobs.rs` — delete `fetch_blobs_assigned`, `FrameFetch`; `crates/athenaeum-core/src/db/collab_frames.rs` — `LocalFrameRow` loses `locally_declined`, `holder_count`, delete `set_declined`, `set_missing` (→ `set_awaiting_gc`); `crates/athenaeum-core/src/settings/mod.rs` — loss-guard keys, defaults, getters, test entries; `crates/athenaeum-core/src/db/schema.rs` — P33 row delete; delete `crates/athenaeum-core/src/api/collab_v3_e2e_tests.rs` and its `mod` line (`api/mod.rs:103-104`; Task 18 writes the wave-3 e2e).
- Modify (hosts, the three removed core fns): `crates/athenaeum-tauri/src/commands/collab.rs` (`refresh_collab_frames` :125, `sync_project_now` :179, `resolve_collab_loss` :236) and their `invoke_handler` lines (`lib.rs:496-517`); `crates/athenaeum-web/src/routes/collab.rs` (:192, :245, :300, `LossArgs`) and `routes/mod.rs:329-350`. Task 16 adds the replacements.
- Test: `api/collab_live/mod.rs` integration tests (two instances, fake hub, relay disabled).

**Interfaces:**
- Consumes: everything from Tasks 1–14.
- Produces:
  ```rust
  // api/collab_live/mod.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
  pub enum LiveState { Off, Connecting, Live, Reconnecting, Unreachable, SignedOut, Outdated }
  #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
  pub enum StorageStateView { Available, ReadOnly, Unavailable, NotSet }
  #[derive(Debug, Clone, PartialEq, serde::Serialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
  pub struct CollabLiveStatus { pub state: LiveState, pub retry_in_secs: Option<u64>, pub since: String, pub storage: StorageStateView, pub storage_reason: Option<String>, pub watcher_degraded: bool, pub network_volume: bool }
  pub const COLLAB_LIVE_STATUS_EVENT: &str = "collab-live-status";
  pub const COLLAB_DELETION_CHOICE_EVENT: &str = "collab-deletion-choice";
  pub const COLLAB_FRAME_LOST_EVENT: &str = "collab-frame-lost";
  pub const COLLAB_FRAME_CHANGED_EVENT: &str = "collab-frame-changed";
  pub const COLLAB_ATTENTION_EVENT: &str = "collab-attention-changed";
  #[derive(Debug, Clone, serde::Serialize, ts_rs::TS)] #[serde(rename_all = "camelCase")] pub struct CollabDeletionChoice { pub count: usize, pub project_ids: Vec<String> }
  #[derive(Debug, Clone, serde::Serialize, ts_rs::TS)] #[serde(rename_all = "camelCase")] pub struct CollabFrameLost { pub project_id: String, pub frame_uuid: String, pub file_name: String }
  #[derive(Debug, Clone, serde::Serialize, ts_rs::TS)] #[serde(rename_all = "camelCase")] pub struct CollabFrameChanged { pub project_id: String, pub frame_uuid: String, pub file_name: String }
  #[derive(Debug, Clone, serde::Serialize, ts_rs::TS)] #[serde(rename_all = "camelCase")] pub struct CollabAttentionChanged { pub project_id: String }
  pub fn spawn_collab_live(ctx: Arc<ServiceContext>, sync: Arc<SyncRuntime>, emitter: Option<Arc<dyn ProgressEmitter>>) -> Option<tokio::task::JoinHandle<()>>;
  pub fn status(ctx: &ServiceContext) -> CollabLiveStatus;
  pub fn sync_now(ctx: &ServiceContext) -> Result<(), ApiError>;                 // L10, P26
  pub fn notify_local_change(ctx: &ServiceContext, project_id: &str);            // a command changed local state → recompute need set, note the outbox
  pub fn set_receive_streams(ctx: &ServiceContext, n: usize);
  pub async fn shutdown(ctx: &ServiceContext);                                   // clean exit: DELETE /me/presence (≤ 2 s), stop
  pub async fn on_sign_out(ctx: &ServiceContext);
  pub(crate) fn holder_view(ctx: &ServiceContext) -> Option<Arc<dyn storage_task::HolderView>>;
  // api/collab_live/executor.rs
  pub(crate) fn need_wants(conn: &rusqlite::Connection, project: &CollabProjectRow, storage_fetching: bool) -> anyhow::Result<Vec<crate::collab::scheduler::core::Want>>;
  // sharing/iroh/node.rs
  impl SharedIrohNode { pub fn admits(&self, node: &NodeId) -> bool; }       // the connect-gate predicate, for the collab pool's I11 sweep
  ```

- [ ] **Step 1: Write the failing integration tests** (`api/collab_live/mod.rs`),
  two in-process instances on the fake hub (Task 3), relay disabled, with the
  fake hub's timings shortened (`keepalive 200 ms, grace 300 ms, silence 1 s`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::collab_live::test_support as ts;
    use std::time::{Duration, Instant};

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_publish_is_fetched_without_any_poll() {
        let w = ts::two_instances().await; // A = send, B = send_receive; both live (spawn_collab_live), paired
        let t0 = Instant::now();
        let uuids = w.a_publishes(3).await;
        for u in &uuids {
            w.b.wait_state(u, crate::db::collab_frames::LocalState::Held, Duration::from_secs(20)).await;
        }
        assert!(t0.elapsed() < Duration::from_secs(20));
        assert_eq!(w.hub.requests_to("/me/project-versions"), 0);
        assert_eq!(w.hub.requests_matching("/frames/", "/holders"), 0, "no per-frame holder lookups");
        // B's landings were reported through the outbox: the hub counts B as a holder
        for u in &uuids {
            assert!(w.hub.holders_of(ts::PID, u).contains(&w.b.device()));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_clean_exit_goes_offline_at_once_and_a_restart_resumes_without_rereporting() {
        let w = ts::two_instances().await;
        w.a_publishes(2).await;
        w.b.wait_all_held(Duration::from_secs(20)).await;
        let writes = w.hub.holder_writes();
        shutdown(&w.b.ctx).await;
        assert!(!w.hub.connected(ts::PID).contains(&w.b.device()), "DELETE /me/presence took effect");
        w.b.restart_live().await;
        w.hub.wait_connected(ts::PID, &w.b.device(), Duration::from_secs(5)).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(w.hub.holder_writes(), writes, "digests match: zero re-reports");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_new_version_mid_download_cancels_and_lands_the_new_one() {
        let w = ts::two_instances_throttled(4 * 1024 * 1024).await; // A uploads at 4 MB/s
        let uuids = w.a_publishes_big(1, 32 * 1024 * 1024).await;
        w.b.wait_fetch_started(&uuids[0], Duration::from_secs(10)).await;
        w.a_republishes_changed(&uuids[0]).await;
        w.b.wait_state_version(&uuids[0], crate::db::collab_frames::LocalState::Held, 2, Duration::from_secs(60)).await;
        assert_eq!(w.b.file_bytes(&uuids[0]), w.a.file_bytes(&uuids[0]));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sync_now_reconnects_and_clears_back_offs() {
        let w = ts::two_instances().await;
        w.hub.kill_streams();
        sync_now(&w.b.ctx).unwrap();
        w.hub.wait_connected(ts::PID, &w.b.device(), Duration::from_secs(3)).await;
        assert_eq!(status(&w.b.ctx).state, LiveState::Live);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_personal_transfer_waits_at_most_one_frame() {
        let w = ts::two_instances_throttled(8 * 1024 * 1024).await;
        w.b.set_receive_limit(1).await; // one lane: collab holds it
        w.a_publishes_big(4, 16 * 1024 * 1024).await;
        w.b.wait_any_fetch_started(Duration::from_secs(10)).await;
        let admitted = w.b.personal_acquire_timed().await; // time until a personal permit is granted
        assert!(admitted < Duration::from_secs(8), "waited {admitted:?}: more than one 16 MiB frame at 8 MB/s");
    }
}
```

  `test_support` gains `two_instances()`, `two_instances_throttled(bps)`
  (A's upload limit), `Instance::{wait_state, wait_all_held,
  wait_state_version, wait_fetch_started, wait_any_fetch_started, device,
  restart_live, file_bytes, set_receive_limit, personal_acquire_timed}`,
  `World::{a_publishes, a_publishes_big, a_republishes_changed}`, and on
  `FakeHub`: `requests_to(path)`, `requests_matching(a, b)` (count
  `received_requests()` through the front's proxy), `wait_connected`.
  `set_receive_limit` calls `ReceiveGate::set_limit` on B's
  `sync.inbound_control()`; `personal_acquire_timed` awaits
  `receive_gate.acquire()` and returns the elapsed time.

- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib api::collab_live::tests`. Expected: compile errors.

- [ ] **Step 3: The session** (`session.rs`): one task per live instance.

```rust
/// The event channel's client side (spec §4.1–§4.2, §4.6): open the stream,
/// feed its events to the runtime, beat every 15 s (and at once when the
/// serving map or the home relay changes), reconnect with full-jitter
/// back-off, reopen at once on `session_gone`, send `DELETE /me/presence` on
/// a clean exit.
pub(crate) async fn run_session(
    rt: Arc<Runtime>,                       // shared runtime state (mod.rs)
    events: tokio::sync::mpsc::Sender<LiveEvent>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let client = crate::collab::live::stream::stream_http_client();
    let mut backoff = crate::collab::live::backoff::Backoff::new();
    let mut reset = crate::collab::live::backoff::reset_signal();
    let mut failures = 0u32;
    loop {
        if *stop.borrow() {
            return;
        }
        let Some((hub_url, token)) = rt.credentials() else {
            rt.set_state(LiveState::SignedOut, None);
            if wait_or_stop(Duration::from_secs(60), &mut stop, &mut reset).await { return; }
            continue;
        };
        rt.set_state(if failures == 0 { LiveState::Connecting } else { LiveState::Reconnecting }, None);
        match crate::collab::live::stream::open(&client, &hub_url, &token).await {
            Ok(resp) => {
                failures = 0;
                backoff.reset();
                rt.set_state(LiveState::Live, None);
                let (end_tx, mut end_rx) = tokio::sync::watch::channel(false);
                let beat = tokio::spawn(beat_loop(Arc::clone(&rt), end_rx.clone()));
                let mut cancel = stop.clone();
                let end = crate::collab::live::stream::pump(resp, &events, &mut cancel).await;
                let _ = end_tx.send(true);
                let _ = beat.await;
                let _ = end_rx.changed();
                tracing::info!(outcome = ?end, "event stream ended");
                if matches!(end, crate::collab::live::stream::StreamEnd::Cancelled) {
                    rt.leave().await; // DELETE /me/presence, bounded
                    return;
                }
            }
            Err(crate::collab::live::stream::OpenError::Unauthorized) => {
                tracing::warn!("event stream refused: signed out or device revoked");
                rt.set_state(LiveState::SignedOut, None);
                if wait_or_stop(Duration::from_secs(60), &mut stop, &mut reset).await { return; }
                continue;
            }
            Err(crate::collab::live::stream::OpenError::Outdated) => {
                crate::account::client::warn_collab_api_outdated_once();
                rt.set_state(LiveState::Outdated, None);
                if wait_or_stop(Duration::from_secs(3600), &mut stop, &mut reset).await { return; }
                continue;
            }
            Err(e) => {
                failures += 1;
                tracing::warn!(attempt = failures, error = ?e, "event stream connect failed");
            }
        }
        let delay = backoff.next_delay();
        let state = if failures >= 3 { LiveState::Unreachable } else { LiveState::Reconnecting };
        rt.set_state(state, Some(delay));
        if crate::collab::live::backoff::sleep_or_reset(delay, &mut reset).await {
            backoff.reset();
        }
    }
}
```

  - `beat_loop(rt, end)`: waits for the session id (`rt.session_id()`, set
    by the runtime when it applies `hello`), then `POST /me/presence` at
    once, every `BEAT_INTERVAL`, on a change of `rt.serving_map()` (a
    `watch` the runtime updates when storage state or projects change), and
    on `node.home_relay_watch()` changes. `SessionGone` → `rt.reconnect_now()`
    (drops the stream: the pump's cancel fires, the loop reconnects without
    back-off). Each tick compares wall-clock and monotonic elapsed time;
    `woke_from_sleep` → `reconnect_now()` (P28). Transport errors on a beat
    are logged at `debug` and retried on the next tick (a missed beat is the
    hub's 40 s rule's business).
  - `rt.leave()`: `presence_leave(session_id)` under
    `tokio::time::timeout(2 s)`; errors `warn!`.
  - `sync_now` (mod.rs): `backoff::reset_all()`, `reconnect_now()`, and a
    `LiveCommand::Reconcile` for the runtime (catch up every project, a
    digest check each, `StorageEngine::sweep`).

- [ ] **Step 4: The runtime loop** (`mod.rs`, `run_live`): one `select!` loop
  per instance owning `FeedApplier`, `Holdings`, `StorageEngine`, the
  scheduler `Core` and the executor state. Arms, in priority order:
  1. `stop` → return.
  2. `events.recv()` → `feed.apply(ev, &mut holdings)`; on `Hello` record the
     session id; `Err(ApiError::Conflict("epoch_changed"))` from the holder
     side → `feed.epoch_change`; every `FeedEffect` →
     - `NeedSetChanged(p)` / `ProjectJoined(p)` → `executor.refresh_need(p)`;
     - `ProvidersChanged(p)` → `executor.refresh_providers(p)`;
     - `MembersChanged(p)` → `node.close_collab_connections_not_admitted()`,
       `pool.close_where(|id| !node.admits(id.as_bytes()))`,
       `storage_task::apply_policy(ctx, p)`, then `refresh_need(p)` +
       `refresh_providers(p)`;
     - `ProjectGone(p)` → `core.step(ProjectGone)`;
     - `EpochChanged` → refresh everything.
  3. `storage.recv_signal()` → `storage.on_signal`.
  4. `checks.recv()` (the serve oracle's mismatch channel) →
     `storage.local_check(p, u)`.
  5. `executor.done.recv()` / `executor.verdicts.recv()` / `pool_events.recv()`
     / `lane_rx.recv()` → executor handlers (Step 5).
  6. `commands.recv()` (`LiveCommand::{Reconcile, LocalChange(p), SetStreams(n), Shutdown(tx), SignOut(tx)}`).
  7. `sleep_until(min(storage.next_deadline(), holdings.next_deadline(), core wake, 60 s GC probe, 1 h digest))`
     → `storage.tick`, `holdings.flush_due`, `holdings.hourly_digest_checks`,
     `core.step(Tick)`, the GC probe.

  Every `StorageEvent` is turned into: `StateChanged` → `holdings.note_append`
  + `refresh_need(p)`; `DeletionChoice` → emit `COLLAB_DELETION_CHOICE_EVENT`;
  `FrameLost` → emit `COLLAB_FRAME_LOST_EVENT`; `Quarantined` → emit
  `COLLAB_FRAME_CHANGED_EVENT`; `Availability(s)` → `core.step(Storage {
  fetching: s.fetching() })`, update the serving map (beat), the status;
  `WatcherDegraded(b)` → status. Every change of an attention list emits
  `COLLAB_ATTENTION_EVENT { projectId }` (Task 17's lists reload on it).

- [ ] **Step 5: The executor** (`executor.rs`).
  - `need_wants(conn, project, storage_fetching)`: empty unless
    `storage_fetching`, `role_allows_replication(&project.data_role,
    project.is_coordinator)` and `project.auto_replicate`; else every row
    with `origin = replica`, `local_state = 'wanted'`, `state = 'published'`,
    `accepted`, `awaiting_gc = 0`, `policy_matches(row, read_policy(project))`
    and the byte budget of `frame_need` (CE:1158-1203, moved here without its
    `paused` argument and without the holder-count sort) → `Want { key,
    content_version, blake3, byte_size, since_ms: state_changed_at }`.
  - `refresh_need(p)` → `core.step(NeedSet { project_id: p, wants })` →
    execute commands; `refresh_providers(p)` → for every want, the
    `holders::providers(...)` of Task 6 (`FrameRef` with the row's
    `frame_seq`, the publisher's devices from `member_devices` of the
    publisher account) → `core.step(Providers { key, providers })`.
  - Commands:
    - `RequestLane` → spawn `gate.acquire_collab()`; the permit arrives on
      `lane_rx` → keep it, `core.step(Lane { admitted: true })`, start a
      `run_live` task (Task 12) with `Dialer::Collab { pool, addrs }`,
      `max_in_flight = collab.max_receive_streams`,
      `unit_cap_bytes = WORK_UNIT_MAX_BYTES`, and `yield_now` = the gate's
      `yield_signal()`; a `true` yield → `core.step(Lane { admitted: false })`;
      when `run_live` returns → drop the permit.
    - `ReleaseLane` → close the run's item channel (it returns when idle).
    - `Start { key, cv, blake3, byte_size, providers }` → first the local
      shortcuts: `collab_blob_health(hash)` `Dead` → `set_awaiting_gc(true)`
      + unseed → `Finished { AwaitingGc }`; the row's landed path already
      holds the content (stamp equal to `size_mtime_seen`, else xxh3) →
      `adopt_by_hash` → `Finished { Landed }`; `identical_landed` → 
      `link_identical` → `Finished { Landed }`. Otherwise set the in-flight
      tag (`project_frame_in_flight_tag`), create the item's provider watch
      and cancel watch, and send `LiveItem { key: uuid, request:
      GetRequest::blob(hash), hash, size, providers: ProviderSet::Live(rx) }`.
    - `UpdateProviders` → the item's provider watch.
    - `Cancel` → the item's cancel watch (`true`); `NewVersion` also deletes
      the old in-flight tag once the item reports `Cancelled` (its partial
      bytes of the OLD hash are discarded, §7.4).
  - `done` → `(key, Done)` → `landing::land_frame` (Task 11) → `Yes` →
    `Finished { Landed }` + `holdings.note_append(p)` + a
    `collab-frames-landed` burst event (≤ one per second per project);
    `AwaitingGc` → `Finished { AwaitingGc }`; `Stale`/`Unavailable` →
    `Finished { Cancelled }`; `Failed(e)` → `set_error` + `Finished { Failed }`.
    `(key, Cancelled)` → `Finished { Cancelled }`. `(key, Failed(e))` →
    `warn!` + `Finished { Failed }`.
  - `verdicts` → `Refused`/`Corrupt` are logged (the engine already
    excluded the provider for that hash); `Busy` → nothing (the engine
    retries after `LIMIT_RETRY`); `DialFailed { provider }` →
    `core.step(DialFailed { device })`. `pool_events` `Closed { node }` →
    `core.step(ConnectionClosed { device })`.
  - The GC probe (every 60 s): rows with `awaiting_gc = 1` whose
    `collab_blob_health` is `Missing` or `Partial` → `set_awaiting_gc(false)`
    → `refresh_need(p)`.
  - `addrs` for the dialer: device (base64) → `NodeId` through
    `sync::pairing::node_id_from_pubkey_b64`; the live relay from the
    presence book (or the holder map's snapshot relay);
    `pairing::peer_dial_addr(node_id, Some(&EndpointAddrReport { home_relay_url, direct_addrs: vec![], .. }), &node.relay_urls(), true)`
    (S1: relay only across accounts).

- [ ] **Step 6: Spawner, handle, exit, sign-out** (`mod.rs`):

```rust
static ARMED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, LiveHandle>>> = std::sync::OnceLock::new();

fn registry() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, LiveHandle>> {
    ARMED.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner())
}

fn scope(ctx: &ServiceContext) -> Option<String> {
    crate::api::db(ctx).ok().map(|d| d.path().to_string_lossy().to_string())
}

/// Arm the live exchange once per catalog (the wave-2 once-per-process guard,
/// keyed so the in-process e2e can run three instances). Also arms the
/// auto-publish worker, as the wave-2 spawner did.
pub fn spawn_collab_live(ctx: Arc<ServiceContext>, sync: Arc<SyncRuntime>, emitter: Option<Arc<dyn ProgressEmitter>>) -> Option<tokio::task::JoinHandle<()>> {
    let key = scope(&ctx)?;
    let mut reg = registry();
    if reg.contains_key(&key) {
        return None;
    }
    let (handle, task) = LiveHandle::start(Arc::clone(&ctx), sync, emitter.clone());
    reg.insert(key, handle);
    drop(reg);
    crate::api::collab_autopublish::spawn_auto_publish_worker(ctx, emitter);
    tracing::info!("collab live exchange armed");
    Some(task)
}
```

  The runtime builds its `StorageEngine` with `StorageTimings::default()`,
  except under `#[cfg(test)]` where `test_support::set_storage_timings(t)`
  (a process-wide `OnceLock<Mutex<Option<StorageTimings>>>`) overrides them —
  the e2e shortens the 10 s aggregation and the 60 s settle.
  `LiveHandle { commands: mpsc::UnboundedSender<LiveCommand>, status:
  watch::Receiver<CollabLiveStatus>, stop: watch::Sender<bool> }`. The
  runtime waits for a bound node and a mounted collab store (re-checking
  every 5 s, `debug!`), installs the `DbServeOracle` and the upload stream
  limit on the node, then runs. `shutdown(ctx)`: send `Shutdown(tx)`, await
  it under `timeout(2 s)` (the runtime sends `DELETE /me/presence` and
  stops), remove the handle. `on_sign_out(ctx)`: the same, plus
  `node.set_collab_serve_oracle(None)`. `status(ctx)`: the handle's latest
  status, or `Off` when none. The status is emitted as
  `COLLAB_LIVE_STATUS_EVENT` on every change (`set_state` deduplicates).

  `SharedIrohNode::admits(&self, node: &NodeId) -> bool` =
  `connect_gate_admits(&self.connect_gate, node)`.

- [ ] **Step 7: Retire.** Delete, with their tests:
  - `collab_exchange.rs`: `AUTO_SYNC_KICK`/`auto_sync_kick` (:64-68),
    `COLLAB_VERSION_POLL_INTERVAL` (:81), `…_STARTUP_DELAY` (:84),
    `poll_scope` (:242-244) if unused, `poll_versions_once` (:399-486),
    `ChangeCollector` (:493-512), `refresh_collab_frames` (:519-533),
    `POLL_BACKOFF` family (:540-599), `kick_if_versions_moved` (:605-615),
    `POLL_DOWN` (:620), `version_poll_tick` (:626-652), `tick_loop`
    (:658-673), `HOLDERS_CHUNK` (:939), `FETCH_BATCH` (:945), `PassKind`
    (:958-970), `DiskTruth` (:1050-1072), `FetchOutcome` (:1076-1088),
    `frame_health`, `unseed_frame`, `collaboration_root_quiet`,
    `inside_root`, `disk_truth` (:1421-1597), `reseed_if_untagged`,
    `readmit`, `recheck_awaiting_gc`, `still_wanted`, `sweep_in_flight`
    (moved: run once when the store mounts — it already runs in
    `set_collab_root`, N:1640), `MaintenanceOutcome`, `run_maintenance`,
    `loss_guard`, `report_holders`, `FramePullClaim`, `FETCH_CANCELS`,
    `cancel_project_fetch` (callers: `set_project_auto_replicate` now calls
    `notify_local_change`; `api/collab.rs:1411` (lost project) now relies on
    the feed's `ProjectGone`), `fetch_key`, `record_frame_error`, `FetchEnv`,
    `fetch_frames`, `fetch_frames_gated`, `between_batches`, `Batch`,
    `holder_lookup_is_frame_level`, `prepare_batch`, `holder_ref`,
    `run_batch`, `fail_with_followers`, `drop_tag`, `OptEmitter`,
    `resolve_collab_loss`, `replica_file_lost`, `COLLAB_AUTO_SYNC_INTERVAL`,
    `COLLAB_AUTO_SYNC_STARTUP_DELAY`, `AUTO_SYNC_ARMED`,
    `AutoSyncPassOutcome`, `run_auto_sync_pass`, `replication_pass`,
    `auto_sync_pass`, `maintenance_tick`, `run_collab_auto_sync_loop`,
    `auto_sync_loop_inner`, `spawn_collab_auto_sync`, `sync_project_now`.
    `LossAction`, `CollabReplicationPaused` and `COLLAB_REPLICATION_PAUSED_EVENT`
    stay one more task (ts-rs types; Task 16 removes them with the surface).
    `frame_need` moves to `executor.rs` (Step 5).
  - `hub_client.rs`: the three deprecated methods and their wire structs and
    tests.
  - `blobs.rs`: `fetch_blobs_assigned`, `FrameFetch`.
  - `db/collab_frames.rs`: `locally_declined` and `holder_count` leave
    `LocalFrameRow`, `SELECT_COLS` and `row_from_sql` (the columns stay in
    the table, never read); `set_declined` and `set_missing` go;
    `set_awaiting_gc(conn, pid, uuid, on: bool) -> Result<usize>` replaces
    the latter's use in landing (`export_source_vanished`) and the executor.
    `ProjectFrameView::from_local_row` keeps compiling by reading
    `local_state` (Task 16 reshapes the DTO): `on_disk: row.on_disk,
    locally_declined: row.local_state == LocalState::NotKept, holder_count: 0`.
  - `settings/mod.rs`: `COLLAB_LOSS_GUARD_*` keys, defaults, the two
    `all()` pairs, the two getters, the two test-list entries and
    `collab_loss_guard_getters_default_and_clamp`.
  - `db/schema.rs`: P33 —
    `conn.execute("DELETE FROM settings WHERE key IN ('collab.loss_guard_fraction','collab.loss_guard_bytes')", [])?;`
    in the collab section (idempotent).
  - `api/collab_v3_e2e_tests.rs` and its two `mod` lines (`api/mod.rs:103-104`).
  - Hosts: the `refresh_collab_frames`, `sync_project_now`,
    `resolve_collab_loss` wrappers on BOTH hosts and their registrations
    (Tauri `lib.rs:496-517`, web `routes/mod.rs:329-350`, `LossArgs`).
  - Completeness check:

```bash
rg -n "poll_versions_once|run_auto_sync_pass|replication_pass|run_maintenance|disk_truth|loss_guard|resolve_collab_loss|sync_project_now|refresh_collab_frames|put_holders|frame_holders|project_versions|fetch_blobs_assigned|FrameFetch|COLLAB_AUTO_SYNC|POLL_BACKOFF|locally_declined|holder_count" crates/ src/ --glob '!src/types/models.ts'
```

    Expected hits: only `src/` frontend files (Task 17), the `LossAction`
    ts types (Task 16), the SQL column names in `db/schema.rs`, and the
    `locally_declined`/`holder_count` DTO fields of `ProjectFrameView`
    (Task 16).

- [ ] **Step 8: Wire exit and sign-out.**
  - Tauri `lib.rs` run-event handler: before taking the iroh node, 
    `athenaeum_core::api::collab_live::shutdown(&state.ctx).await` inside the
    same `block_on` (it is bounded to 2 s itself).
  - Web `main.rs` graceful shutdown: the same call before the node
    shutdown.
  - `api::account::sign_out`: `crate::api::collab_live::on_sign_out(ctx).await;`
    first (gated with `#[cfg(all(feature = "render", feature = "solver"))]`
    like the module).

- [ ] **Step 9: Run** `cargo test -p athenaeum-core --lib api::collab_live && cargo test -p athenaeum-core --lib && cargo check --workspace --all-targets && cargo check -p athenaeum-core --no-default-features`. Expected: PASS. The whole lib suite must be green: the retirement is large (owner rule 2026-09-18: a large deletion commits only on a green full suite).

- [ ] **Step 10: Commit**

```bash
rustfmt $(git diff --name-only -- '*.rs')
git add -A crates/
git commit -m "feat(collab)!: live exchange runtime — event session with beat and reconnect, feed/holdings/storage/scheduler in one loop, collab lane on the receive gate; the poll, the 20-minute pass, maintenance and the loss guard are removed"
```

---
### Task 16: Command surface on both hosts, DTOs, ts-rs (§14 app part, L6, L9, L10, L11, P21, P26, P27, P29)

**Implementer:** rust-engineer.

**Files:**
- Create: `crates/athenaeum-core/src/api/collab_live/surface.rs` (`pub mod surface;`): the command-facing functions and DTOs.
- Modify: `crates/athenaeum-core/src/api/collab_exchange.rs` — `ProjectFrameView` (:709-796) reshaped; delete `LossAction` (:1040-1046), `CollabReplicationPaused` (:1020-1025), `COLLAB_REPLICATION_PAUSED_EVENT` (:932).
- Modify: `crates/athenaeum-core/src/ts_export.rs` (models block :214-235), then regenerate `src/types/models.ts`.
- Modify: `crates/athenaeum-tauri/src/commands/collab.rs`, `crates/athenaeum-tauri/src/lib.rs` (`invoke_handler`, :496-517); `crates/athenaeum-web/src/routes/collab.rs`, `crates/athenaeum-web/src/routes/mod.rs` (:329-350).
- Test: `surface.rs` tests on the `test_support` rig; `tests/ts_contract.rs` (regenerated file).

**Interfaces:**
- Consumes: `collab_live::{status, sync_now, notify_local_change, set_receive_streams, holder_view}` (Task 15); `storage_task::{resolve_deletions, keep_again, resolve_changed_file, last_copy_report, DeletionAction, ChangedAction}` (Task 9); `replace::{replace_offer, replace_device}` (Task 7); `StoreGuard`/`check_store` (Task 7); `SharedIrohNode::set_collab_upload_limit` (Task 10); settings getters (Task 10).
- Produces (all `#[derive(serde::Serialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]`; argument enums also `Deserialize`):
  ```rust
  // surface.rs
  #[serde(rename_all = "snake_case")] pub enum LocalStateView { Wanted, Held, Missing, AwaitingChoice, Quarantined, NotKept, Idle, OwnHeld, OwnMissing, OwnChanged }
  pub struct ChangedFileView { pub frame_uuid: String, pub file_name: String, pub path: String, pub detected_at: String, pub new_version_waiting: bool }
  pub struct ChoiceFrameView { pub frame_uuid: String, pub file_name: String, pub holders_online: usize, pub holders_total: usize, pub at_risk: bool }
  pub struct NotKeptView { pub frame_uuid: String, pub file_name: String, pub content_version: i32 }
  pub struct ForeignFileView { pub path: String, pub seen_at: String }
  pub struct CollabAttention { pub changed: Vec<ChangedFileView>, pub awaiting_choice: Vec<ChoiceFrameView>, pub not_kept: Vec<NotKeptView>, pub other_files: Vec<ForeignFileView> }
  pub enum DeletionActionArg { Refetch, StopKeeping }                 // "refetch" | "stopKeeping"
  pub enum ChangedActionArg { RefetchOriginal, Delete }               // "refetchOriginal" | "delete"
  pub struct LastCopyView { pub frame_uuid: String, pub file_name: String, pub holders_online: usize, pub holders_total: usize, pub at_risk: bool }
  pub struct ChangedFileOutcome { pub trashed: bool }
  pub struct DeviceReplaceOfferView { pub device_id: String, pub device_name: String, pub last_seen_at: Option<String>, pub offline_days: Option<i64>, pub prompt: bool, pub propose_retire: bool }
  pub struct CollabStorageStatus { pub state: StorageStateView, pub reason: Option<String>, pub root: Option<String>, pub watcher_degraded: bool, pub network_volume: bool, pub replace: Option<DeviceReplaceOfferView> }
  pub struct ReplaceOutcomeView { pub scanned: usize, pub adopted: usize }
  pub fn list_collab_attention(ctx: &ServiceContext, project_id: &str) -> Result<CollabAttention, ApiError>;
  pub fn resolve_collab_deletions(ctx: &ServiceContext, project_id: &str, frame_uuids: Option<Vec<String>>, action: DeletionActionArg) -> Result<usize, ApiError>;
  pub fn preview_collab_stop_keeping(ctx: &ServiceContext, project_id: &str, frame_uuids: Vec<String>) -> Result<Vec<LastCopyView>, ApiError>;
  pub fn keep_collab_frames_again(ctx: &ServiceContext, project_id: &str, frame_uuids: Option<Vec<String>>) -> Result<usize, ApiError>;
  pub async fn resolve_collab_changed_file(ctx: &ServiceContext, project_id: &str, frame_uuid: &str, action: ChangedActionArg, confirmed_delete: bool) -> Result<ChangedFileOutcome, ApiError>;
  pub async fn get_collab_storage_status(ctx: &ServiceContext) -> Result<CollabStorageStatus, ApiError>;
  pub async fn collab_replace_device(ctx: &ServiceContext, device_id: &str) -> Result<ReplaceOutcomeView, ApiError>;
  pub fn collab_sync_now(ctx: &ServiceContext) -> Result<(), ApiError>;
  pub fn get_collab_live_status(ctx: &ServiceContext) -> CollabLiveStatus;
  pub async fn set_collab_max_upload_streams(ctx: &ServiceContext, n: usize) -> Result<(), ApiError>;
  pub fn set_collab_max_receive_streams(ctx: &ServiceContext, n: usize) -> Result<(), ApiError>;
  // ProjectFrameView (collab_exchange.rs) — replaces onDisk/awaitingGc/locallyDeclined/holderCount:
  //   pub local_state: LocalStateView, pub on_disk: bool, pub holders_online: usize, pub holders_total: usize,
  //   pub waiting_for_publisher: bool, pub new_version_waiting: bool
  ```

**Command table (every row on BOTH hosts; Tauri `#[tracing::instrument(skip_all, err)]`, web `#[tracing::instrument(skip_all, err(Debug))]`; `get_collab_live_status` at `level = "debug"`):**

| Command | Args (camelCase) | Returns | Core |
| ---- | ---- | ---- | ---- |
| `collab_sync_now` | — | `()` | `surface::collab_sync_now` |
| `get_collab_live_status` | — | `CollabLiveStatus` | `surface::get_collab_live_status` |
| `list_collab_attention` | `projectId` | `CollabAttention` | `surface::list_collab_attention` |
| `resolve_collab_deletions` | `projectId, frameUuids?: string[], action` | `number` | `surface::resolve_collab_deletions` |
| `preview_collab_stop_keeping` | `projectId, frameUuids` | `LastCopyView[]` | `surface::preview_collab_stop_keeping` |
| `keep_collab_frames_again` | `projectId, frameUuids?: string[]` | `number` | `surface::keep_collab_frames_again` |
| `resolve_collab_changed_file` | `projectId, frameUuid, action, confirmedDelete` | `ChangedFileOutcome` | `surface::resolve_collab_changed_file` |
| `get_collab_storage_status` | — | `CollabStorageStatus` | `surface::get_collab_storage_status` |
| `collab_replace_device` | `deviceId` | `ReplaceOutcomeView` | `surface::collab_replace_device` |
| `set_collab_max_upload_streams` | `maxUploadStreams` | `()` | `surface::set_collab_max_upload_streams` |
| `set_collab_max_receive_streams` | `maxReceiveStreams` | `()` | `surface::set_collab_max_receive_streams` |

Removed in Task 15 on both hosts: `refresh_collab_frames`, `sync_project_now`, `resolve_collab_loss`.

- [ ] **Step 1: Write the failing tests** (`surface.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::collab_live::test_support as ts;
    use crate::db::collab_frames::{self as frames_db, LocalState};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn attention_lists_every_kind_and_actions_move_frames_between_them() {
        let rig = ts::landed_rig(4).await;
        let pid = rig.frames[0].0.clone();
        {
            let conn = crate::api::db(&rig.ctx).unwrap().conn();
            frames_db::set_local_state(&conn, &pid, &rig.frames[1].1, LocalState::AwaitingChoice).unwrap();
            frames_db::set_local_state(&conn, &pid, &rig.frames[2].1, LocalState::NotKept).unwrap();
            crate::db::collab_frames::record_foreign_file(&conn, &rig.root.join("m31/stray.fits").to_string_lossy(), Some(&pid), None).unwrap();
        }
        ts::overwrite_same_size(&rig.frames[0].2);
        rig.engine().local_check(&pid, &rig.frames[0].1).await; // → quarantined
        let a = list_collab_attention(&rig.ctx, &pid).unwrap();
        assert_eq!((a.changed.len(), a.awaiting_choice.len(), a.not_kept.len(), a.other_files.len()), (1, 1, 1, 1));
        assert!(!a.changed[0].new_version_waiting);
        assert_eq!(resolve_collab_deletions(&rig.ctx, &pid, None, DeletionActionArg::StopKeeping).unwrap(), 1);
        assert_eq!(keep_collab_frames_again(&rig.ctx, &pid, None).unwrap(), 2);
        let a = list_collab_attention(&rig.ctx, &pid).unwrap();
        assert!(a.awaiting_choice.is_empty() && a.not_kept.is_empty());
    }

    #[tokio::test]
    async fn storage_status_reports_an_other_device_root_with_a_replace_offer() {
        let (_t, ctx, hub) = ts::signed_in_rig().await;
        let root = ts::collab_root(&ctx);
        hub.add_device("acc-me", "OLD-DEV", "old-id", "Old laptop", Some(chrono::Utc::now() - chrono::Duration::days(10)));
        crate::collab::storage::marker::write_marker(&root, &crate::collab::storage::marker::StoreMarker { store_id: "s".into(), device_id: "OLD-DEV".into() }).unwrap();
        let s = get_collab_storage_status(&ctx).await.unwrap();
        assert_eq!(s.state, StorageStateView::Unavailable);
        let offer = s.replace.expect("an offer for an account device");
        assert!(offer.prompt && !offer.propose_retire);
    }

    #[tokio::test]
    async fn stream_limits_persist_clamped_and_apply_live() {
        let (_t, ctx, _hub) = ts::signed_in_rig().await;
        set_collab_max_upload_streams(&ctx, 500).await.unwrap();
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert_eq!(ctx.settings.get_collab_max_upload_streams(&conn).unwrap(), 64);
        drop(conn);
        assert!(set_collab_max_receive_streams(&ctx, 0).is_err());
    }
}
```

- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib api::collab_live::surface`. Expected: compile errors.

- [ ] **Step 3: Implement `surface.rs`.**
  - `list_collab_attention`: `live_project` check; `changed` =
    `list_quarantine` joined with the row (`new_version_waiting =
    row.content_version > q.quarantined_version`); `awaiting_choice` = rows
    in `awaiting_choice` with `holder_view(ctx).other_holders(..)` →
    `at_risk = last_copy_warning(total)` (no live runtime → counts 0,
    `at_risk = true`); `not_kept` = rows in `not_kept`; `other_files` =
    NEW `db::collab_frames::list_foreign_files(conn, project_id) ->
    Vec<(String /*path*/, String /*seen_at*/)>` (`SELECT path, seen_at FROM
    collab_foreign_files WHERE project_id = ?1 OR project_id IS NULL ORDER BY path`).
  - `resolve_collab_deletions` / `keep_collab_frames_again`: call Task 9's
    functions, then `notify_local_change(ctx, pid)` (the runtime recomputes
    the need set and flushes the outbox rows those transitions wrote).
  - `preview_collab_stop_keeping`: `last_copy_report` with the live holder
    view.
  - `resolve_collab_changed_file`: the bound node from `ctx.iroh_node`
    (none → `ApiError::Invalid("collaboration is not running")`), Task 9's
    function, then `notify_local_change`.
  - `get_collab_storage_status`: the Collaboration root
    (`require_collaboration_root`; none → `NotSet`); `check_store(root,
    recorded, me)`; `Unavailable(OtherDevice { device_id })` →
    `replace_offer(ctx, &device_id)`; `watcher_degraded`/`network_volume`
    from the live status. Reasons render as stable snake_case strings:
    `path_missing`, `not_a_directory`, `marker_missing`, `marker_mismatch`,
    `other_device`.
  - `collab_replace_device`: `replace::replace_device` → then
    `notify_local_change` for every live project.
  - `set_collab_max_upload_streams(n)`: validate `COLLAB_UPLOAD_STREAMS_RANGE`
    by clamping (the Settings codec already clamps; a stored value is always
    in range), `set_setting(keys::COLLAB_MAX_UPLOAD_STREAMS)`, then
    `node.set_collab_upload_limit(n)`. `set_collab_max_receive_streams(n)`:
    `n` outside `COLLAB_RECEIVE_STREAMS_RANGE` → `ApiError::Invalid`, else
    persist + `collab_live::set_receive_streams(ctx, n)` (the runtime stores
    it in the live run's `max_in_flight` atomic and `core.step(Slots(n))`).
  - `ProjectFrameView::from_local_row` takes the holder view: `local_state`,
    `on_disk`, `holders_online`/`holders_total` from `other_holders`,
    `waiting_for_publisher` from Task 6's `waiting_for_publisher`,
    `new_version_waiting` for a `quarantined` row whose content version is
    above its quarantine row's.

- [ ] **Step 4: ts-rs.** In `ts_export.rs` remove `LossAction` and
  `CollabReplicationPaused`; add `LocalStateView`, `ChangedFileView`,
  `ChoiceFrameView`, `NotKeptView`, `ForeignFileView`, `CollabAttention`,
  `DeletionActionArg`, `ChangedActionArg`, `LastCopyView`,
  `ChangedFileOutcome`, `DeviceReplaceOfferView`, `CollabStorageStatus`,
  `ReplaceOutcomeView`, `LiveState`, `StorageStateView`, `CollabLiveStatus`,
  `CollabDeletionChoice`, `CollabFrameLost`, `CollabFrameChanged`,
  `CollabAttentionChanged`. Then:

```bash
TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract
cargo test -p athenaeum-core --test ts_contract
```

  Expected: the regenerated `src/types/models.ts` differs only in the collab
  types. The frontend does not compile against it until Task 17 (it still
  imports `LossAction`); that is expected between these two tasks, and Task
  17's gate restores `npx tsc --noEmit`.

- [ ] **Step 5: Hosts.** One wrapper per table row, the wave-2 pattern:

```rust
// crates/athenaeum-tauri/src/commands/collab.rs
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn resolve_collab_deletions(
    state: State<'_, AppState>,
    project_id: String,
    frame_uuids: Option<Vec<String>>,
    action: athenaeum_core::api::collab_live::surface::DeletionActionArg,
) -> Result<usize, String> {
    athenaeum_core::api::collab_live::surface::resolve_collab_deletions(&state.ctx, &project_id, frame_uuids, action)
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, level = "debug")]
pub async fn get_collab_live_status(state: State<'_, AppState>) -> Result<athenaeum_core::api::collab_live::CollabLiveStatus, String> {
    Ok(athenaeum_core::api::collab_live::surface::get_collab_live_status(&state.ctx))
}
```

```rust
// crates/athenaeum-web/src/routes/collab.rs
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletionsArgs {
    project_id: String,
    #[serde(default)]
    frame_uuids: Option<Vec<String>>,
    action: athenaeum_core::api::collab_live::surface::DeletionActionArg,
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn resolve_collab_deletions(
    State(state): State<WebAppState>,
    Json(args): Json<DeletionsArgs>,
) -> Result<Json<usize>, (axum::http::StatusCode, String)> {
    athenaeum_core::api::collab_live::surface::resolve_collab_deletions(&state.ctx, &args.project_id, args.frame_uuids, args.action)
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, level = "debug")]
pub async fn get_collab_live_status(State(state): State<WebAppState>) -> Json<athenaeum_core::api::collab_live::CollabLiveStatus> {
    Json(athenaeum_core::api::collab_live::surface::get_collab_live_status(&state.ctx))
}
```

  Register every command in `invoke_handler![]` (Tauri) and as
  `.route("/api/<name>", post(collab::<name>))` in `build_router` (web).
  Parity check:

```bash
diff <(rg -o 'pub async fn (\w+)' -r '$1' crates/athenaeum-tauri/src/commands/collab.rs | sort) \
     <(rg -o 'pub async fn (\w+)' -r '$1' crates/athenaeum-web/src/routes/collab.rs | sort)
```

  Expected: no output.

- [ ] **Step 6: Run** `cargo test -p athenaeum-core --lib api::collab_live && cargo test -p athenaeum-core --test ts_contract && cargo check --workspace --all-targets`. Expected: PASS.

- [ ] **Step 7: Commit**

```bash
rustfmt $(git diff --name-only -- '*.rs')
git add -A crates/ src/types/models.ts
git commit -m "feat(collab): live-exchange command surface on both hosts — sync now, live and storage status, attention lists, deletion choice, keep again, changed files, device replace, stream limits"
```

---
### Task 17: Frontend — live status, Sync now, attention lists, choices, last-copy warning, device replace, Settings stream limits, notifications (§14 app part, L4–L6, L9–L11)

**Implementer:** frontend-dev.

**Files:**
- Create: `src/components/collab/CollabLiveStatus.tsx` (+ `.test.tsx`), `src/components/collab/CollabAttention.tsx` (+ `.test.tsx`), `src/components/collab/DeviceReplaceDialog.tsx` (+ `.test.tsx`).
- Modify: `src/components/collab/ReceiveTab.tsx` (paused banner :154-177, `resolve_collab_loss` :95, `sync_project_now` :79, paused listener :60, `OnDiskBadge` :253) + `ReceiveTab.test.tsx` (:74, :97, :115); `src/components/collab/AutoReplicateBar.tsx` (`sync_project_now` :84) + its test; `src/hooks/useCollabNotifications.ts` (paused listener :141) + test; `src/hooks/useProjects.ts` (`refresh_collab_frames` :95) + test; `src/pages/ProjectDetail.tsx` (header near `<UpdateRequired/>` :318, `<ReceiveTab>` :435); `src/pages/Projects.tsx` (:28); `src/components/Layout.tsx` (next to `CollabNotificationsListener` :48-49, :184); `src/settings/registry.ts` (Transfers sections :723-848); `src/components/settings/TransfersSection.tsx` (:354-365 pattern); `src/components/settings/tabs/TransfersTab.tsx` (`TRANSFERS_SECTIONS`).
- Test: the Vitest files above, `src/settings/registry.test.ts`, `src/components/settings/tabs/registry-coverage.test.tsx`.

**Interfaces:**
- Consumes (Task 16, `src/types/models.ts`): `CollabLiveStatus`, `LiveState`, `StorageStateView`, `CollabAttention`, `ChangedFileView`, `ChoiceFrameView`, `NotKeptView`, `ForeignFileView`, `LastCopyView`, `ChangedFileOutcome`, `CollabStorageStatus`, `DeviceReplaceOfferView`, `ReplaceOutcomeView`, `LocalStateView`, `ProjectFrameView` (reshaped), `CollabDeletionChoice`, `CollabFrameLost`, `CollabFrameChanged`, `CollabAttentionChanged`; commands of Task 16's table; events `collab-live-status`, `collab-attention-changed`, `collab-deletion-choice`, `collab-frame-lost`, `collab-frame-changed`, `collab-frames-landed` (unchanged).
- Produces:
  ```ts
  export default function CollabLiveStatus(props: { compact?: boolean }): JSX.Element;
  export default function CollabAttention(props: { projectId: string }): JSX.Element | null;
  export default function DeviceReplaceDialog(): JSX.Element | null;   // mounted once in Layout
  export function liveStatusLabel(s: CollabLiveStatus, nowMs: number): string;   // pure, tested
  ```

UI copy (sentence case, English):

| State | Text |
| ---- | ---- |
| `live` | "Live" |
| `connecting` | "Connecting…" |
| `reconnecting` | "Reconnecting in {n} s" (counts down from `retryInSecs`) |
| `unreachable` | "Hub unreachable — retrying" |
| `signedOut` | "Signed out" |
| `outdated` | "Update required" |
| `off` | "Collaboration is off" |
| storage `unavailable` | "Online · storage unavailable" (+ reason: "the Collaboration folder is missing" / "another disk is mounted there" / "this folder belongs to another device") |
| storage `readOnly` | "Online · read-only storage (serving, not downloading)" |
| `watcherDegraded` or `networkVolume` | "Changes are seen by periodic check only" |

- [ ] **Step 1: Write the failing tests.**

`src/components/collab/CollabLiveStatus.test.tsx`:

```tsx
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { NotificationProvider } from '../../contexts/NotificationContext';
import CollabLiveStatus, { liveStatusLabel } from './CollabLiveStatus';
import { api } from '../../api';
import type { CollabLiveStatus as Status } from '../../types/models';

vi.mock('../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

const base: Status = { state: 'live', retryInSecs: null, since: '2026-09-25T10:00:00Z', storage: 'available', storageReason: null, watcherDegraded: false, networkVolume: false };
let emit: ((p: Status) => void) | undefined;

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((cmd: string) =>
    cmd === 'get_collab_live_status' ? Promise.resolve(base) : Promise.resolve(null)) as never);
  vi.mocked(api.listen).mockImplementation(((name: string, h: (p: Status) => void) => {
    if (name === 'collab-live-status') emit = h;
    return Promise.resolve(() => {});
  }) as never);
});

const renderIt = () => render(<MemoryRouter><NotificationProvider><CollabLiveStatus /></NotificationProvider></MemoryRouter>);

describe('CollabLiveStatus', () => {
  it('labels every state', () => {
    expect(liveStatusLabel(base, 0)).toBe('Live');
    expect(liveStatusLabel({ ...base, state: 'reconnecting', retryInSecs: 12 }, 0)).toBe('Reconnecting in 12 s');
    expect(liveStatusLabel({ ...base, state: 'unreachable' }, 0)).toBe('Hub unreachable — retrying');
    expect(liveStatusLabel({ ...base, storage: 'unavailable', storageReason: 'other_device' }, 0)).toContain('storage unavailable');
  });

  it('follows the live event and Sync now calls the global command', async () => {
    renderIt();
    expect(await screen.findByText('Live')).toBeInTheDocument();
    act(() => emit?.({ ...base, state: 'reconnecting', retryInSecs: 5 }));
    expect(await screen.findByText(/Reconnecting in \d+ s/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Sync now' }));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('collab_sync_now'));
  });

  it('says when changes are seen by periodic check only', async () => {
    vi.mocked(api.invoke).mockImplementation(((cmd: string) =>
      cmd === 'get_collab_live_status' ? Promise.resolve({ ...base, watcherDegraded: true }) : Promise.resolve(null)) as never);
    renderIt();
    expect(await screen.findByText('Changes are seen by periodic check only')).toBeInTheDocument();
  });
});
```

`src/components/collab/CollabAttention.test.tsx`:

```tsx
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { NotificationProvider } from '../../contexts/NotificationContext';
import CollabAttention from './CollabAttention';
import { api } from '../../api';
import type { CollabAttention as Attention } from '../../types/models';

vi.mock('../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

const attention: Attention = {
  changed: [{ frameUuid: 'u1', fileName: 'c_a.fits', path: '/c/m31/o/c_a.fits', detectedAt: '2026-09-25 10:00:00', newVersionWaiting: true }],
  awaitingChoice: [{ frameUuid: 'u2', fileName: 'c_b.fits', holdersOnline: 0, holdersTotal: 1, atRisk: true }],
  notKept: [{ frameUuid: 'u3', fileName: 'c_c.fits', contentVersion: 1 }],
  otherFiles: [{ path: '/c/m31/stray.fits', seenAt: '2026-09-25 09:00:00' }],
};

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((cmd: string) => {
    switch (cmd) {
      case 'list_collab_attention': return Promise.resolve(attention);
      case 'preview_collab_stop_keeping': return Promise.resolve([{ frameUuid: 'u2', fileName: 'c_b.fits', holdersOnline: 0, holdersTotal: 1, atRisk: true }]);
      case 'resolve_collab_changed_file': return Promise.reject(new Error('trash_unavailable: the system trash is not available — confirm to delete the changed file'));
      default: return Promise.resolve(1);
    }
  }) as never);
  vi.mocked(api.listen).mockImplementation((() => Promise.resolve(() => {})) as never);
});

const renderIt = () => render(<MemoryRouter><NotificationProvider><CollabAttention projectId="p1" /></NotificationProvider></MemoryRouter>);

describe('CollabAttention', () => {
  it('lists the four kinds', async () => {
    renderIt();
    expect(await screen.findByText('Changed files')).toBeInTheDocument();
    expect(screen.getByText('A new version is waiting')).toBeInTheDocument();
    expect(screen.getByText('Waiting for your choice')).toBeInTheDocument();
    expect(screen.getByText('Not kept')).toBeInTheDocument();
    expect(screen.getByText('Other files')).toBeInTheDocument();
  });

  it('stop keeping warns about the last copy before it acts', async () => {
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Stop keeping all' }));
    expect(await screen.findByText(/fewer than 2 other copies/)).toBeInTheDocument();
    expect(screen.getByText(/0 online, 1 in total/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Stop keeping' }));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('resolve_collab_deletions', { projectId: 'p1', frameUuids: null, action: 'stopKeeping' }));
  });

  it('re-fetch original falls back to a confirmed delete when there is no trash', async () => {
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Re-fetch original' }));
    expect(await screen.findByText(/system trash is not available/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Delete and re-fetch' }));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('resolve_collab_changed_file', { projectId: 'p1', frameUuid: 'u1', action: 'refetchOriginal', confirmedDelete: true }));
  });

  it('keep again works per frame and for all', async () => {
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Keep again' }));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('keep_collab_frames_again', { projectId: 'p1', frameUuids: ['u3'] }));
    fireEvent.click(screen.getByRole('button', { name: 'Keep all again' }));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('keep_collab_frames_again', { projectId: 'p1', frameUuids: null }));
  });
});
```

`src/components/collab/DeviceReplaceDialog.test.tsx`:

```tsx
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { NotificationProvider } from '../../contexts/NotificationContext';
import DeviceReplaceDialog from './DeviceReplaceDialog';
import { api } from '../../api';

vi.mock('../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.listen).mockImplementation((() => Promise.resolve(() => {})) as never);
});

describe('DeviceReplaceDialog', () => {
  it('offers the replace for a device offline more than 7 days and confirms it', async () => {
    vi.mocked(api.invoke).mockImplementation(((cmd: string) => {
      if (cmd === 'get_collab_storage_status') return Promise.resolve({ state: 'unavailable', reason: 'other_device', root: '/c', watcherDegraded: false, networkVolume: false,
        replace: { deviceId: 'old-id', deviceName: 'Old laptop', lastSeenAt: '2026-09-10T00:00:00Z', offlineDays: 15, prompt: true, proposeRetire: false } });
      if (cmd === 'collab_replace_device') return Promise.resolve({ scanned: 12, adopted: 12 });
      return Promise.resolve(null);
    }) as never);
    render(<MemoryRouter><NotificationProvider><DeviceReplaceDialog /></NotificationProvider></MemoryRouter>);
    expect(await screen.findByText('This device replaces Old laptop')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Replace Old laptop' }));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('collab_replace_device', { deviceId: 'old-id' }));
  });

  it('stays closed when there is nothing to replace', async () => {
    vi.mocked(api.invoke).mockImplementation((() => Promise.resolve({ state: 'available', reason: null, root: '/c', watcherDegraded: false, networkVolume: false, replace: null })) as never);
    const { container } = render(<MemoryRouter><NotificationProvider><DeviceReplaceDialog /></NotificationProvider></MemoryRouter>);
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('get_collab_storage_status'));
    expect(container).toBeEmptyDOMElement();
  });
});
```

  `useCollabNotifications.test.tsx` — replace the paused test with three:
  a `collab-deletion-choice` payload `{count: 15, projectIds: ['p1']}` →
  one `warning` notification "15 replicas were deleted — choose what to do"
  linking `/projects/p1?tab=receive`; a `collab-frame-lost` payload → an
  `error`-tone notification "c_b.fits is lost everywhere — restore it from
  the Trash"; a `collab-frame-changed` payload → a `warning` "c_a.fits
  changed on disk and was set aside". `ReceiveTab.test.tsx` — replace the
  restore/stopHolding tests with: the state badges ("On disk", "Waiting",
  "Changed", "Not kept", "Waiting for your choice", "v{n} waiting for the
  publisher") and "Sync now" calling `collab_sync_now`.

- [ ] **Step 2: Run** `npx vitest run src/components/collab src/hooks`. Expected: FAIL (components missing, old commands).

- [ ] **Step 3: `CollabLiveStatus.tsx`**

```tsx
import { useEffect, useState } from 'react';
import { Loader2, RefreshCw, Wifi, WifiOff } from 'lucide-react';
import { api } from '../../api';
import { useNotifications } from '../../contexts/NotificationContext';
import type { CollabLiveStatus as Status } from '../../types/models';

const REASON: Record<string, string> = {
  path_missing: 'the Collaboration folder is missing',
  not_a_directory: 'the Collaboration folder is missing',
  marker_missing: 'the Collaboration folder is missing',
  marker_mismatch: 'another disk is mounted there',
  other_device: 'this folder belongs to another device',
};

export function liveStatusLabel(s: Status, elapsedSecs: number): string {
  switch (s.state) {
    case 'live':
      if (s.storage === 'unavailable') return `Online · storage unavailable${s.storageReason ? ` (${REASON[s.storageReason] ?? s.storageReason})` : ''}`;
      if (s.storage === 'readOnly') return 'Online · read-only storage (serving, not downloading)';
      return 'Live';
    case 'connecting': return 'Connecting…';
    case 'reconnecting': return `Reconnecting in ${Math.max(0, (s.retryInSecs ?? 0) - elapsedSecs)} s`;
    case 'unreachable': return 'Hub unreachable — retrying';
    case 'signedOut': return 'Signed out';
    case 'outdated': return 'Update required';
    default: return 'Collaboration is off';
  }
}

export default function CollabLiveStatus({ compact = false }: { compact?: boolean }) {
  const { notify } = useNotifications();
  const [status, setStatus] = useState<Status | null>(null);
  const [elapsed, setElapsed] = useState(0);
  const [syncing, setSyncing] = useState(false);

  useEffect(() => {
    let cancelled = false;
    api.invoke<Status>('get_collab_live_status')
      .then((s) => { if (!cancelled) setStatus(s); })
      .catch((err) => console.error('[collab] get_collab_live_status failed:', err));
    return () => { cancelled = true; };
  }, []);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api.listen<Status>('collab-live-status', (s) => { if (cancelled) return; setStatus(s); setElapsed(0); })
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
      .catch((err) => console.error('[collab] live-status listen failed:', err));
    return () => { cancelled = true; unlisten?.(); };
  }, []);

  useEffect(() => {
    if (status?.state !== 'reconnecting') return;
    const t = setInterval(() => setElapsed((e) => e + 1), 1000);
    return () => clearInterval(t);
  }, [status?.state, status?.since]);

  const syncNow = async () => {
    setSyncing(true);
    try {
      await api.invoke('collab_sync_now');
    } catch (err) {
      console.error('[collab] collab_sync_now failed:', err);
      notify({ title: 'Sync now failed', detail: err instanceof Error ? err.message : String(err), kind: 'project', tone: 'warning', hasErrors: true });
    } finally {
      setSyncing(false);
    }
  };

  if (!status) return null;
  const live = status.state === 'live';
  const tone = live && status.storage === 'available' ? 'text-success' : status.state === 'unreachable' || status.storage === 'unavailable' ? 'text-error' : 'text-content-muted';
  return (
    <div className="flex items-center gap-3 text-sm">
      <span className={`flex items-center gap-1.5 ${tone}`}>
        {live ? <Wifi className="w-4 h-4" /> : <WifiOff className="w-4 h-4" />}
        {liveStatusLabel(status, elapsed)}
      </span>
      {(status.watcherDegraded || status.networkVolume) && !compact && (
        <span className="text-content-muted">Changes are seen by periodic check only</span>
      )}
      <button type="button" onClick={syncNow} disabled={syncing}
        className="flex items-center gap-1 px-2 py-1 rounded bg-surface-hover text-content hover:brightness-110 disabled:opacity-50">
        {syncing ? <Loader2 className="w-4 h-4 animate-spin" /> : <RefreshCw className="w-4 h-4" />}
        Sync now
      </button>
    </div>
  );
}
```

  (Use the design tokens that exist in the Tailwind config — `text-success`
  if present, else `text-accent`; `rg -n "success" tailwind.config.*` decides.)

- [ ] **Step 4: `CollabAttention.tsx`** — four sections (hidden when empty), a
  reload on mount and on `collab-attention-changed` (filtered by
  `projectId`, StrictMode-safe pattern), `ConfirmDialog` (`src/components/ConfirmDialog.tsx`)
  for every destructive step:
  - **Changed files** (header "Changed files"; per row the file name, the
    path, `detectedAt` via `formatTimestamp`, "A new version is waiting"
    when `newVersionWaiting`): "Re-fetch original" →
    `resolve_collab_changed_file {projectId, frameUuid, action: 'refetchOriginal', confirmedDelete: false}`;
    an error whose message starts with `trash_unavailable` opens a confirm
    ("The system trash is not available. Delete the changed file and
    re-fetch the original?" / confirm "Delete and re-fetch") that calls
    again with `confirmedDelete: true`. "Delete" → confirm ("Delete the
    changed file? It will not be re-fetched." / "Delete") →
    `action: 'delete', confirmedDelete: true`. A successful re-fetch with
    `trashed: true` notifies "c_a.fits moved to the Trash; the original is
    being re-fetched" (discrete outcome).
  - **Waiting for your choice** ("Waiting for your choice"; counts and a
    note "Nothing else is paused"): "Re-fetch all" →
    `resolve_collab_deletions {projectId, frameUuids: null, action: 'refetch'}`;
    "Stop keeping all" → `preview_collab_stop_keeping {projectId, frameUuids: [all]}`
    → when any row `atRisk`, a confirm listing each at-risk frame "fewer
    than 2 other copies: {online} online, {total} in total" (confirm
    button "Stop keeping"), else act directly →
    `resolve_collab_deletions {projectId, frameUuids: null, action: 'stopKeeping'}`.
    Per-row buttons do the same for one frame (`frameUuids: [uuid]`).
  - **Not kept** ("Not kept"): per row "Keep again" →
    `keep_collab_frames_again {projectId, frameUuids: [uuid]}`; header
    button "Keep all again" → `frameUuids: null`.
  - **Other files** ("Other files"; read-only list with the note "Files in
    the Collaboration folder that belong to no project frame. The app never
    deletes them.").
  - Every failure: `console.error` + one `notify({ kind: 'project', tone: 'warning', hasErrors: true, … })`.

- [ ] **Step 5: `DeviceReplaceDialog.tsx`** — on mount and on every
  `collab-live-status` event whose `storage === 'unavailable'`, call
  `get_collab_storage_status`; when `replace?.prompt`, show a modal
  (title "This device replaces {deviceName}", text "{deviceName} has been
  offline for {offlineDays} days. Replacing it retires that device and
  adopts the files already in this folder — nothing is downloaded again.",
  plus "{deviceName} can be retired (offline for more than 30 days)." when
  `proposeRetire`), buttons "Replace {deviceName}" →
  `collab_replace_device {deviceId}` → notify "Adopted {adopted} of
  {scanned} files" and close; "Not now" closes until the next app start.
  When `replace` exists but `prompt` is false, `CollabLiveStatus` shows a
  "Replace a device…" link (storage reason `other_device`) that opens the
  same dialog through a small context (`DeviceReplaceContext` in the same
  file). Mount `<DeviceReplaceDialog />` in `Layout.tsx` next to
  `CollabNotificationsListener`.

- [ ] **Step 6: Existing components.**
  - `ReceiveTab.tsx`: delete the paused state, its listener (:60) and
    banner (:154-177), the `resolve_collab_loss` handler (:95); "Sync now"
    → `collab_sync_now` (no args); `OnDiskBadge` → `LocalStateBadge`
    mapping `localState`: `held` "On disk", `wanted` "Waiting" (or "v{n}
    waiting for the publisher" when `waitingForPublisher`), `missing`
    "Missing", `awaiting_choice` "Waiting for your choice", `quarantined`
    "Changed" (+ "new version waiting" when `newVersionWaiting`), `not_kept`
    "Not kept", `idle` "Not replicated"; the holders column shows
    "{holdersOnline} online / {holdersTotal}". Render `<CollabAttention
    projectId={projectId} />` above the frame list.
  - `AutoReplicateBar.tsx`: "Sync now" → `api.invoke('collab_sync_now')`;
    update its test's mock to reject `collab_sync_now`.
  - `useProjects.ts`: the manual refresh calls `collab_sync_now` instead of
    `refresh_collab_frames` (changes arrive as events).
  - `useCollabNotifications.ts`: delete the `collab-replication-paused`
    listener (:141) and the `CollabReplicationPaused` import; add three
    listeners (same pattern, `kind: 'project'`, no `dedupeKey` — final
    review I4):

```ts
api.listen<CollabDeletionChoice>('collab-deletion-choice', (p) => {
  if (cancelled) return;
  notify({
    title: `${p.count} replicas were deleted — choose what to do`,
    detail: p.projectIds.map(titleFor).join(', '),
    kind: 'project', tone: 'warning', hasErrors: true,
    link: p.projectIds.length === 1 ? `/projects/${p.projectIds[0]}?tab=receive` : '/projects',
  });
})
```

    `collab-frame-lost` → `{ title: `${p.fileName} is lost everywhere — restore it from the Trash`, detail: titleFor(p.projectId), tone: 'error', hasErrors: true, link: `/projects/${p.projectId}?tab=receive` }`;
    `collab-frame-changed` → `{ title: `${p.fileName} changed on disk and was set aside`, tone: 'warning', link: … }`.
  - `ProjectDetail.tsx` and `Projects.tsx`: `<CollabLiveStatus />` in the
    page header (compact on Projects).

- [ ] **Step 7: Settings → Transfers stream limits** (docs/settings/README.md contract).
  - `registry.ts`, after `transfers.receiving`:

```ts
{
  id: 'transfers.collabStreams',
  tab: 'transfers',
  title: 'Collaboration streams',
  fields: [
    {
      id: 'uploadStreams',
      label: 'Simultaneous collaboration uploads',
      help: 'How many frames this device serves to collaborators at once. Further requests are sent to other holders.',
      keywords: ['collab.max_upload_streams', 'set_collab_max_upload_streams', 'swarm', 'seeding'],
    },
    {
      id: 'receiveStreams',
      label: 'Simultaneous collaboration downloads',
      help: 'How many frames one collaboration download fetches at once. Personal transfers always go first.',
      keywords: ['collab.max_receive_streams', 'set_collab_max_receive_streams', 'swarm'],
    },
  ],
},
```

  - `TransfersSection.tsx`:

```tsx
<SettingsSection id="transfers.collabStreams">
  <SettingNumber
    section="transfers.collabStreams"
    field="uploadStreams"
    settingKey="collab.max_upload_streams"
    codec={intCodec(1, 64)}
    min={1}
    max={64}
    step={1}
    write={(n) => api.invoke('set_collab_max_upload_streams', { maxUploadStreams: n })}
  />
  <SettingNumber
    section="transfers.collabStreams"
    field="receiveStreams"
    settingKey="collab.max_receive_streams"
    codec={intCodec(1, 32)}
    min={1}
    max={32}
    step={1}
    write={(n) => api.invoke('set_collab_max_receive_streams', { maxReceiveStreams: n })}
  />
</SettingsSection>
```

  - `tabs/TransfersTab.tsx`: add `{ sectionId: 'transfers.collabStreams', element: transfersSection }`
    after `transfers.receiving`. Defaults come from `get_settings_defaults`
    (Task 10 registered them) — never restated in TS.

- [ ] **Step 8: Run** `npx tsc --noEmit && npx vitest run`. Expected: PASS (the TS break Task 16 left is fixed here: no `LossAction`/`CollabReplicationPaused` import remains — `rg -n "LossAction|CollabReplicationPaused|resolve_collab_loss|sync_project_now|refresh_collab_frames|locallyDeclined|awaitingGc|holderCount" src/` prints nothing).

- [ ] **Step 9: Commit**

```bash
git add src/
git commit -m "feat(collab-ui): live status with Sync now, attention lists (changed / waiting for your choice / not kept / other files), last-copy warning, device replace prompt, stream-limit settings, new notifications"
```

---
### Task 18: Three-instance e2e with latency bounds and the disk ledger, relay stream measurement, docs, final gates (§12 app part, §15, P21)

**Implementer:** rust-engineer (docs included; the frontend is untouched here).

**Files:**
- Create: `crates/athenaeum-core/src/api/collab_v3_live_e2e_tests.rs`; `api/mod.rs` gains `#[cfg(all(test, unix, feature = "render", feature = "solver"))] mod collab_v3_live_e2e_tests;` (the wave-2 gate, `api/mod.rs:103-104`).
- Modify: `crates/athenaeum-core/src/api/relay_live_tests.rs` — a second `#[ignore]`d owner-run test.
- Modify (only if the measurement ran): `crates/athenaeum-core/src/settings/mod.rs` — the two stream-limit defaults.
- Modify (docs): `docs/transfers/README.md` (new section after the wave-2 collab section, L33-62), `docs/superpowers/open-items.md` (new section under "## Unverified by hand", above "### Collab v3 wave 2", L201), `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` (a collab v3 wave-3 entry after the wave-2 entry, L168-186), `docs/frontend/notifications.md` (the kind list at L32-35 is stale — replace it with the union of `src/contexts/NotificationContext.tsx:35-52`), `CLAUDE.md` (the collab bullet under "Transfers / personal sync" and the command count in "Module Map").

**Interfaces:**
- Consumes: everything; `test_support` (Tasks 7–15), `FakeHub` (Task 3).
- Produces: the e2e module; one ignored relay test
  `collab_stream_limits_throughput_on_real_relay`.

**The e2e scenarios** — spec §12's table, in process: three instances
(contributor A `send`, processor B `send_receive`, coordinator C
`send_receive` + moderator), real iroh nodes with the relay disabled, paired,
one fake hub with the SSE front, the collab store GC armed at 100 ms
(`test_gc`), storage timings shortened through
`test_support::set_storage_timings({aggregate: 200 ms, settle: 1 s, ...})`.
"Starts fetching" = the executor's first `Start` for that frame (a
`#[cfg(test)]` timestamp map in `executor.rs`,
`test_support::first_start_at(uuid)`). Hub-owned timings (grace, silence)
run at their real values where the bound is about them.

| # | Scenario (spec §12) | Assertion in process | Bound |
| ---- | ---- | ---- | ---- |
| 1 | A publishes → B starts fetching | `first_start_at(u) − announce_returned_at` | ≤ 2 s |
| 2 | B lands → C, already connected to B, can fetch it | C's `Start` naming B as a provider after B's landing | ≤ 3 s |
| 3 | B quits cleanly → C stops dialing B | after `shutdown(B)`, B absent from C's presence book / C's providers | ≤ 2 s |
| 4 | B is killed (runtime aborted, node shut, no DELETE) | C drops B | ≤ 50 s after B's last beat (real 10 s grace) |
| 5 | B restarts | B is a provider again after connect; `hub.holder_writes()` unchanged | ≤ 3 s; 0 re-reports |
| 6 | v2 published mid-download of v1 | v1 fetch cancelled; v2 lands at the same path; a later `GetRequest::blob(v1_hash)` to B is refused (`ERR_PERMISSION`) | — |
| 7 | v2 landing interrupted (`landing::fault::fail_after_export_once`) | v1 bytes intact at the target; the retry lands v2; no `.athtmp` left | — |
| 8 | a replica edited in place | the next get to B refused; B's row `quarantined`; a later v2 does not land over it | — |
| 9 | `touch` on a replica | after the rehash, B serves again | — |
| 10 | a single delete on B | `wanted` after the settle; re-landed from C (GC hook) | settle + GC |
| 11 | 15 deletes at once on B | one `collab-deletion-choice`; B keeps fetching another frame meanwhile | — |
| 12 | storage unmounted (rename B's root away and back) | B's `serving` false at the hub; zero state changes; back → serving, no re-fetch | — |
| 13 | hub restart (`hub.kill_streams()` mid-transfer) | the transfer completes; after reconnect the digest check matches | — |
| 14 | epoch rotation (`hub.forget_frames` + `rotate_epoch`) | full resync; A's frames re-announced under the same uuids | — |
| 15 | device revoked mid-transfer (`hub.revoke_device(C)`) | B closes C's collab connection (`close_collab_connections_not_admitted`) | — |
| 16 | personal transfer during a collab fetch | a personal `ReceiveGate::acquire` is granted after at most one frame | ≤ one frame's time |

Plus the disk ledger, kept from wave 2 (R32 baselines): after the run,
every Collaboration root holds payload + < 1 %, each working dir grew
≤ 64 KiB (the fixed 1 MiB `blobs.db` exempt), no `*.athtmp` anywhere, and
each collab store's `data/` did not grow by the payload (landing is a rename).

- [ ] **Step 1: Write the e2e** — one `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]`
  per group so a failure names its scenario; every wait is a
  `tokio::time::timeout` around a condition poll (50 ms), never a bare sleep.
  Skeleton:

```rust
//! Collab v3 wave 3 — three instances, live exchange, spec §12 latency table
//! and the one-copy disk ledger. The run against the real test hub and the
//! test relay is owed (docs/superpowers/open-items.md).

use std::time::{Duration, Instant};

use crate::api::collab_live::test_support as ts;
use crate::db::collab_frames::LocalState;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publish_fetch_and_serve_onward_within_bounds() {
    let w = ts::three_instances().await;
    let (uuids, announced_at) = w.a_publishes_timed(3).await;
    for u in &uuids {
        let started = w.b.first_start_at(u, Duration::from_secs(10)).await;
        assert!(started.duration_since(announced_at) <= Duration::from_secs(2), "scenario 1: {u}");
    }
    w.b.wait_state(&uuids[0], LocalState::Held, Duration::from_secs(30)).await;
    let landed_at = Instant::now();
    let c_start = w.c.first_start_with_provider(&uuids[0], &w.b.device(), Duration::from_secs(10)).await;
    assert!(c_start.duration_since(landed_at) <= Duration::from_secs(3), "scenario 2");
    w.assert_disk_ledger();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clean_exit_kill_and_restart() {
    let w = ts::three_instances_all_held(3).await;
    let t = Instant::now();
    crate::api::collab_live::shutdown(&w.b.ctx).await;
    w.c.wait_not_a_provider(&w.b.device(), Duration::from_secs(2)).await;
    assert!(t.elapsed() <= Duration::from_secs(2), "scenario 3");
    w.b.restart_live().await;
    let writes = w.hub.holder_writes();
    let back = Instant::now();
    w.c.wait_provider(&w.b.device(), Duration::from_secs(3)).await;
    assert!(back.elapsed() <= Duration::from_secs(3), "scenario 5");
    assert_eq!(w.hub.holder_writes(), writes, "scenario 5: zero re-reports");
    let last_beat = w.hub.last_beat(&w.b.device());
    w.b.kill().await;
    w.c.wait_not_a_provider(&w.b.device(), Duration::from_secs(50)).await;
    assert!(last_beat.elapsed() <= Duration::from_secs(50), "scenario 4");
}
```

  and likewise `versions_mid_download_and_interrupted_landing` (6, 7),
  `edited_touched_and_deleted_replicas` (8, 9, 10, 11),
  `storage_hub_restart_epoch_and_revocation` (12, 13, 14, 15),
  `personal_priority` (16). `test_support` gains `three_instances()`,
  `three_instances_all_held(n)`, `World::{a_publishes_timed, assert_disk_ledger}`,
  `Instance::{first_start_at, first_start_with_provider, wait_provider,
  wait_not_a_provider, kill}`, and `FakeHub::last_beat(device) -> Instant`.

- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib api::collab_v3_live_e2e_tests -- --nocapture`. Fix what it finds in the owning task's code (commits named after that task); never loosen a bound.

- [ ] **Step 3: The relay stream measurement** (`api/relay_live_tests.rs`), same
  skip-unless-env pattern as the existing canary (L1-60):

```rust
/// Owner-run: pick the collab stream-limit defaults from a measurement on the
/// real relay (plan P21). Two endpoints on `ATHENAEUM_TEST_RELAY`, relay only;
/// the provider holds 16 frames of 32 MiB in its collab store; the fetcher
/// runs the live assignment run with max_in_flight ∈ {1, 2, 4, 8, 16} and
/// prints MB/s for each.
///
/// ATHENAEUM_TEST_RELAY=https://test-relay.artfrom.space:8443 \
///   cargo test -p athenaeum-core --lib -- --ignored --exact \
///   api::relay_live_tests::collab_stream_limits_throughput_on_real_relay --nocapture
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires ATHENAEUM_TEST_RELAY=<relay-url>; owner-run against test-relay.artfrom.space"]
async fn collab_stream_limits_throughput_on_real_relay() {
    let Ok(relay_url) = std::env::var("ATHENAEUM_TEST_RELAY") else {
        eprintln!("skip: set ATHENAEUM_TEST_RELAY");
        return;
    };
    let pair = crate::api::collab_live::test_support::relay_pair(&relay_url, 16, 32 * 1024 * 1024).await;
    for n in [1usize, 2, 4, 8, 16] {
        let (bytes, elapsed) = pair.fetch_all_fresh(n).await; // a fresh receiver store per run
        eprintln!("streams={n} bytes={bytes} secs={:.1} MB/s={:.1}", elapsed.as_secs_f64(), bytes as f64 / 1e6 / elapsed.as_secs_f64());
    }
}
```

  Run it now if the relay is reachable
  (`curl -s -o /dev/null -w '%{http_code}' https://test-relay.artfrom.space:8443/` answers).
  Rule for the defaults: `collab.max_receive_streams` = the smallest `n`
  reaching ≥ 90 % of the best MB/s; `collab.max_upload_streams` = the same
  value (one fetcher can saturate one provider). Write both into
  `settings/mod.rs` (dropping "PROVISIONAL" from the doc comment) and paste
  the table into the commit body. If the relay is unreachable, keep 8/8 and
  record the owed run in open-items (Step 4).

- [ ] **Step 4: Docs.**
  - `docs/transfers/README.md` — a section "## Collab v3 — live exchange (wave 3, 2026-09-25)":
    one bullet each for the event channel (SSE, hello, cursors, catch-up,
    epoch), presence (beat 15 s, clean exit, hub timing rules), holdings
    (claims, `report_seq`, outbox, digest, implicit claims, refused), the
    scheduler (need set, rarest → random → 1 h, live providers, cancel
    rules, one collab lane, yield), the transport (collab pool, refusal
    codes, serve check, stream limits, class-aware pacer), local storage
    (marker, watcher/settle/canary, sweep, the state list, L4 window, L5
    quarantine, L6 keep again, L9 device replace), landing (`.athtmp` +
    rename + re-import, P12), and what was retired (poll, pass,
    maintenance, loss guard, per-frame holder lookups, full holder chunks,
    three commands). Link the spec and this plan.
  - `docs/superpowers/open-items.md` — "### Collab v3 wave 3 — the app on the live exchange (2026-09-25)":
    - **Owed — three-machine acceptance on the test hub + test relay**
      (after the hub wave-3 deploy): the §12 table with real timings, the
      §15 acceptance with its two edits, `du -sh` of each Collaboration root
      and working-dir `blobs/` before/after, the relay-byte fraction; exact
      steps listed.
    - **Owed — relay stream measurement** if Step 3 did not run.
    - **Owed — desktop click-through**: status line, Sync now, the four
      lists, the last-copy dialog, the device-replace prompt, Settings →
      Transfers → Collaboration streams.
    - **Owed — Windows**: rename over an open served file, `trash::delete`
      on the shell API, UNC network-volume detection.
    - **Owed — one-week soak** (spec §15): hourly digest checks all match.
    - **Owner decisions** (from this plan): P15 personal store accepts
      inbound pushes (library quirk) — fix in its own change?; P18 one
      collab lane; P21 defaults; P28 no sleep hook; P30 no serving/pause
      switches; P31 single-delete latency bounded by the 900 s GC.
    - **Do NOT re-flag**: P12's extra BLAKE3 read per landing is the price
      of a store that references the landed path; `holder_count` and
      `locally_declined` columns remain in `project_frames_local` unread
      (SQLite column drops avoided); `ReceiveGate` debt counter replaced by
      grant-on-release (same semantics).
  - Logging dictionary — the collab v3 wave-3 entry: `epoch`, `version`,
    `prev`, `holder_seq`, `report_seq`, `session_id`, `device`,
    `from_state`, `to_state`, `streams`, `digest_match`, `refused`,
    `retry_in_ms`, `store_id`, `window_count`, `class`, each with its type
    and meaning; `retry_secs` recorded as retired with the wave-2 poll.
  - `docs/frontend/notifications.md` — the kind list = files, update, merge,
    scan, export, analysis, platesolve, autofind, archive, fileop,
    registration, masterbuild, calibration, stacking, sync, project,
    generic.
  - `CLAUDE.md` — the "Collab v3" bullet: "event-driven: one SSE stream per
    device (`/me/events`), claims + outbox + digest, a pure scheduler core,
    a serve check per request, a per-frame local state machine
    (`project_frames_local.local_state`); no poll, no periodic pass";
    and the command count, recounted:
    `rg -c '#\[tauri::command\]' crates/athenaeum-tauri/src/commands/*.rs | awk -F: '{s+=$2} END {print s}'`.

- [ ] **Step 5: Final gates** (all green; paste the summary lines into the ledger):

```bash
cargo test -p athenaeum-core 2>&1 | grep -E '^test result' | sort | uniq -c
cargo test --workspace 2>&1 | grep -E '^test result' | sort | uniq -c
cargo check -p athenaeum-core --no-default-features
npx tsc --noEmit
npx vitest run 2>&1 | tail -5
rg -n 'println!|eprintln!' crates/athenaeum-core/src --glob '!**/*tests*.rs' | rg -v '#\[cfg\(test\)\]'
rg -n "poll_versions_once|replication_pass|run_maintenance|resolve_collab_loss|sync_project_now|refresh_collab_frames|LossAction|collab-replication-paused|loss_guard" crates/ src/ docs/transfers
```

  Expected: every `test result` ok; no-default-features compiles; tsc and
  vitest clean; the `println!` grep prints only test-module lines (inspect
  each hit); the retirement grep prints nothing.

- [ ] **Step 6: Commit**

```bash
git add -A crates/ docs/ CLAUDE.md
git commit -m "test(collab): three-instance live-exchange e2e with the spec latency table and the one-copy disk ledger; relay stream measurement; docs and open-items"
```

No push, no deploy. The branch is merged to local `main` only after the
final whole-branch review (superpowers:finishing-a-development-branch).

---
## Self-review

**1. Spec coverage (app side)**

| Spec | Requirement | Task |
| ---- | ---- | ---- |
| §3 I1 | pinned targets; landing fence as a DB conditional | 11 (fence + `local_state`), 14 (sim), 15 (executor re-checks) |
| §3 I2, I3 | hub-ordered cursors; `prev`-checked deltas; snapshot/epoch reload | 5 (`cursor.rs`, applier), 6 (holder cursor), 14 (feed convergence sim) |
| §3 I4 | claims keyed by version; providers derived, never cleared | 6 (`holders.rs`), 14 (sim uses the production derivation) |
| §3 I5 | presence adds candidates only; in-flight ended by the connection | 4 (presence book), 6, 12 (only `closed()`/dial failures evict), 14 |
| §3 I6 | superseded versions never served or fetched | 9 (NewVersion edges), 10 (serve lookup by current blake3), 14 (cancel in the same step) |
| §3 I7 | no automated deletion; replacement by rename; last-copy warning | 9 (no delete path, L4 warning), 11 (rename), 17 (warning dialog) |
| §3 I8 | replication set | 9 (`apply_policy`, Idle), 15 (`need_wants`), 14 |
| §3 I9 | the disk decides servability; rehash resolves drift | 8 (sweep), 9 (recheck), 10 (serve check) |
| §3 I10 | CAS versions | 2 (client), 6 (batch versions in publish), 3 (fake hub) |
| §3 I11 | members bump → gate rebuild → close connections | 5 (`MembersChanged`), 10 (registry + close), 12 (pool `close_where`), 15 (wiring) |
| §4.1 | dedicated streaming client; 50 s read timeout; full-jitter reconnect 1–60 s; silent-stream warn | 2 (back-off), 4 (stream), 15 (session loop) |
| §4.2 | hello/session; beat 15 s + on serving/relay change; `409 session_gone` → reopen; DELETE on quit/sign-out; sleep best effort | 4 (presence, home-relay watch), 15 (beat loop, exit, sign-out, wake detection; P28) |
| §4.3 | the seven events; inline frames / `more`; `versions` self-heal | 4 (types), 5 (applier) |
| §4.4 | epoch change: reload snapshots, full report, re-announce lost own frames | 5 (`epoch_change`, `reannounce_lost_own_frames`), 6 (`reload`) |
| §4.6 | retry every hub call; 401/403 handling; no pass-level abort; Sync now | 2 (`with_retry`), 6 (flush back-off), 15 (`sync_now`), 16 (command) |
| §6.1 | persisted holder map; delta resume; snapshot only when needed | 1 (tables), 6 |
| §6.2 | journal + outbox in the state change's transaction; flush per `nextFlushMs` / 500 | 1 (`set_local_state` → outbox), 6 (flush clock) |
| §6.3 | digest (vectors pinned); full report on mismatch; hourly check; implicit and refused claims | 2 (digest + vectors), 6 |
| §7.1 | deterministic core + executor | 14, 15 |
| §7.2 | need set; rarest → random → 1 h; sleeping providerless frames; one-frame units; live providers; refusal codes | 12 (live set, codes), 14, 15 |
| §7.3 | one connection per provider; collab pool with `connect_with_opts` (10 s / 30 s / 60 s); dial back-off; DecodeError eviction | 12, 14 (dial back-off) |
| §7.4 | cancellation on version/exclusion/lost/membership/policy/auto-off/storage | 9 (`apply_policy`), 14, 15 |
| §7.5 | `.athtmp` + rename; no delete-first; never over quarantine | 11 (P12, P13) |
| §8 | two-class gate with yield; class-aware pacer; upload stream limit (ERR_LIMIT); receive stream cap; settings with measured defaults | 10, 12, 13, 16, 17, 18 (measurement) |
| §9.1 | marker, states, never recreate the root, refuse another device's root | 7 |
| §9.2 | watcher (10 s / 60 s), canary, sweep hourly ±25 % / 5 min, 2 s tolerance, own frames outside the root | 8, 9 |
| §9.3 | serve check in the intercept → immediate local check | 10, 15 (check channel → `local_check`) |
| §9.4 | the per-frame state machine exactly; L4/L5/L6; unknown files; own-frame states | 9 (`states.rs`, engine), 16 (lists), 17 (UI) |
| §9.5 | device replace prompt (7 d), proposal (30 d), retire, re-adopt by hash | 7, 16, 17 |
| §11 | reuse audit + disk-copy ledger | § Reuse audit; 18 (ledger asserted) |
| §12 app | seeded simulation; unit tests; three-instance e2e with the latency table; real-hub run owed | 14, every task, 18 |
| §14 app | status line, Sync now, lists, choice, warning, lost notification, replace prompt, stream limits, both hosts | 15, 16, 17 |
| §15 | acceptance edits (15 deletes → one choice; fetch within 2 s of announce) | 18 (scenarios 1, 11; owed real run) |
| §2.1 overrides | loss-guard settings removed; poll/pass/maintenance/R-rulings retired | 15 (P33), 16, 17 |

Nothing app-side of the spec is left without a task. Hub-side items (§5,
§12 hub tests, the load check) belong to the hub plan.

**2. Placeholder scan.** Every code step carries code or an exact
transformation with `file:line`. Remaining "verify at execution" points —
API spellings the research read but did not compile against — are named
here so an implementer checks them first, not guesses:
- `iroh::endpoint::QuicTransportConfig::builder()` export path and the
  `max_idle_timeout(Some(IDLE_TIMEOUT.try_into()?))` conversion (Task 12;
  `iroh-1.2.0/src/endpoint/quic.rs:129-211`).
- `iroh_blobs::get::GetError::Decode { .. }` field name and the
  `DecodeError::{ParentHashMismatch, LeafHashMismatch}` shapes (Task 12;
  `get/error.rs:13-48`, `get.rs:635-663`).
- Whether `GetProgress`/`PushProgress` implement `IntoFuture` (`.await` in
  Task 10/12 tests); if not, drive `.stream()` to its terminal item as
  `transfer_once` does.
- The in-memory store type for `test_support::scratch_store()`
  (`iroh_blobs::store::mem::MemStore` → `Store`).
- The irpc update receiver's `recv()` signature in the collab consumer's
  drain loops (reuse the existing `drain_only!` body verbatim).
- The Tailwind token for the "live" colour (`text-success` if the config
  defines it, else `text-accent`) (Task 17).

**3. Name and type consistency with the hub wire contract.**
- `device` = standard padded base64 of the node id everywhere: presence
  book keys, holder map keys, `member_devices`, the marker's `deviceId`,
  the collab pool's `addrs` (decoded with `node_id_from_pubkey_b64`).
- Wire field names match § Hub contract: `HelloProject { version,
  holder_seq, claim_count, claim_digest, report_seq, presence }`,
  `HoldersReportWire { report_seq, full, add: [{uuid, contentVersion}],
  remove, digest, count }` → `HoldersReportReplyWire { holder_seq,
  digest_match, next_flush_ms, refused }`, `HoldersSnapshotWire { epoch,
  holder_seq, version, frames: [{seq, uuid, contentVersion}], devices:
  [{device, displayName, relayUrl, claims: [[start, len, cv]]}] }`,
  `HolderDeltaPageWire { epoch, holder_seq, floor, deltas, has_more, next:
  {since, after} }`, `VersionInWire { uuid, expected_version, … }` →
  `VersionResultWire { uuid, status: ok|conflict|not_found|forbidden,
  content_version }`, `BeatWire { session_id, serving, relay_url }`.
  Error strings: `session_gone`, `version_conflict`, `collab_api_outdated`,
  `holders_below_floor`, `holders_cursor_ahead`, `epoch_changed` — each
  classified in exactly one place (`hub_client::classify`, `stream::open`).
- The digest vectors are pinned verbatim (Task 2) — the same table as the
  hub plan's `src/claims/digest.rs` tests.
- Cross-task names: `set_local_state` (1) is the only writer of `on_disk`
  and the only producer of outbox rows besides `record_claim_change` (1)
  and `add_implicit_claim` (1); `FeedApplier`/`HolderSide` (5) ↔
  `Holdings` (6); `StoreGuard` (7) is read by `DbServeOracle` (10),
  landing (11) and the engine (9); `StorageEngine`/`HolderView` (9) ↔
  runtime (15) ↔ surface (16); `Core`/`Input`/`Command` (14) ↔ executor
  (15); `run_live`/`LiveItem`/`ItemOutcome`/`LiveVerdict` (12) ↔ executor
  (15); `ReceiveGate::{acquire, acquire_collab, yield_signal}` (13) ↔
  executor (15); `CollabLiveStatus` and the event structs (15) ↔ surface
  (16) ↔ UI (17).

**4. Contradictions found (spec ↔ hub wire contract ↔ libraries) and their resolution.**
- **Spec ↔ hub wire contract:** no conflicting wire shape. The hub plan adds
  fields the spec did not name (`hello.accountId`, `claimCount`,
  `claimDigest`, `reportSeq`; `refused` in the report reply; typed 409/410
  bodies; the `[start, len, cv]` run encoding; implicit claims on
  announce/version). The app codes against the hub plan (§ Hub contract).
- **Spec §7.5 ↔ iroh-blobs 0.103:** a bare "export to `.athtmp`, rename"
  leaves the store referencing the dead temp path. Resolved by P12 (re-import
  the target by reference after the rename).
- **Spec §9.3 ↔ iroh-blobs 0.103:** the mask routes push/get-many/observe
  through `get`; resolved by P15 in the collab consumer; the personal store's
  existing push acceptance goes to the owner.
- **Spec §4.2 "sleep hook where the shell has one" ↔ tauri 2.11.5:** there is
  none on desktop; resolved by P28 (40 s rule + wake detection).
- **Spec §8 "admission per unit" ↔ swarm concurrency ↔ the shared lane cap:**
  resolved by P18 (one collab lane, units admitted into it, yield at unit
  boundaries); flagged to the owner.
- **Spec L4 "single deletions re-fetched after settling" ↔ no public blob
  delete in iroh-blobs 0.103:** the re-fetch waits for the collab GC; P31.
