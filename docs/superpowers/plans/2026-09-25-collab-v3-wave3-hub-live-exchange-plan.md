# Collab v3 — Wave 3: hub live exchange — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The hub becomes the only authority for presence and change propagation.
It keeps holdings as durable, device-owned claims under a gapless holder cursor
with a per-device digest. It pushes every committed change over one SSE stream
per device, in contiguous cursor order. It tracks live presence (session beat,
grace, silence, flap damping, warm-up). It versions frames by compare-and-set,
one at a time or in batches of 500. Revocation reaches every provider. The epoch
survives a database restore. The deploy repo lets a thousand streams live behind
the write-once vhost.

**Architecture:** One migration (`0023_live_exchange.sql`) adds frame ordinals, the holder
cursor, claim columns and the digest table. A `claims` module owns every holder
write: the cursor lock, a pure report planner, the digest math and one bulk
upsert. A `feed` module owns the event channel: a clock abstraction, ordered
coalescers, the presence registry, `FeedHub` (broadcast per project, mailbox
per session), `FeedBatch` + `commit_and_publish` (commit and publish inside one
spawned task), the 50 ms driver and the epoch. `routes/events.rs` serves
`GET /me/events` and `POST`/`DELETE /me/presence`. The holder routes grow the
snapshot and delta reads. Versions gain compare-and-set and a batch route.
Revocation gains the I11 effects. The two poll-era routes are retired behind
`409 collab_api_outdated`.

**Tech Stack:** Rust 2021 (toolchain 1.96). axum 0.8.9 (`axum::response::sse`).
sqlx 0.8.6 / Postgres 16. tokio 1.52 (features `sync`, `time` added). tokio-stream
0.1.18 (feature `sync`, for `BroadcastStream`, `StreamMap`, `ReceiverStream`).
blake3 1.8.5 (new). tower-http 0.6.11 (`TraceLayer::make_span_with`).
`#[sqlx::test]`, one database per test. The React/TS portal with vitest. Ansible
in the astronet repo.

**Spec:** `docs/superpowers/specs/2026-09-25-collab-v3-live-exchange-design.md`
(approved 2026-09-25). This plan implements the hub side of it:
- §4 (event channel, presence, schema, epoch, `FeedHub`);
- §5 (tables, lock order, behaviour changes, endpoints, infrastructure);
- §6.3 (digest);
- §12 (hub tests and the load check);
- the hub and portal parts of §14.

The parent spec is `2026-09-23-collab-v3-per-frame-model-design.md` (amendment
A4). The app plan follows separately and codes against **§ Wire contract** below.

## Global Constraints

- **Repos and branches.**
  - Hub repo `/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-hub`. Branch `collab-v3-wave3` from local `main` @ `6127951` (`git checkout -b collab-v3-wave3 6127951`, in Task 1).
  - The portal is the hub repo's own `portal/` directory. `/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-hub-portal` is a stale git **worktree of the hub repo** on branch `collab-v2-portal` (pre-v3 package model). Nothing in this plan touches it.
  - Deploy repo `/Volumes/BigMac/Users/astrobureau/Documents/astronet`, branch `main`.
- **Test gate.**
  - `docker compose up -d postgres`, then `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test`. The whole suite is green at the end of every task.
  - `cargo build --release 2>&1 | grep -c warning` prints `0`.
  - Portal: `cd portal && npx vitest run && npx tsc -b`.
- **Handlers.**
  - Every new or changed handler carries `#[tracing::instrument(skip_all)]`. Per-flush and per-beat routes carry `level = "debug"` (`PUT …/holders/self`, `POST /me/presence`, `GET …/holders?since=`). Ruling P31 explains why there is no `err`.
  - A refusal names the rule it applies. `ApiError::Internal` is never swallowed.
  - Every background task (driver, pump, prune) logs a failure at `error!` or `warn!` before it continues.
- **Logging.**
  - `tracing` is the only logging API. `println!`/`eprintln!` appear only in `#[cfg(test)]` and `tests/`.
  - The message is a short stable phrase and data goes in snake_case fields: `tracing::info!(project_id = %id, count, "frames announced")`.
  - Reuse the hub's existing field names: `project_id`, `account_id`, `device_id`, `count`, `error`, `status`, `reason`, `full`, `added`, `removed`, `requested`, `written`.
  - New field names, introduced once here: `session_id`, `holder_seq`, `version`, `prev`, `lagged`, `epoch`, `retire`, `refused`, `pruned`, `projects`, `known`.
- **Rule S1 is unchanged.** Across accounts only `relayUrl` travels, never `directAddrs`.
- **Timing constants.** Copied verbatim from spec §4. They live in code as named constants, never as inline literals:

  | Constant | Value | Spec |
  | ---- | ---- | ---- |
  | beat interval (client) | 15 s | §4.2 |
  | beat silence → offline | 40 s | §4.2 |
  | grace after stream close | 10 s | §4.2 |
  | flap damping | 3 offline→online transitions within 5 min → offline broadcast delayed 60 s | §4.2 |
  | presence warm-up after hub start | 30 s | §4.2 |
  | `project` coalescing | ≤ 250 ms | §4.3 |
  | `holders` and `presence` coalescing | 1 s | §4.3 |
  | gap buffer before `resync` | 2 s | §4.3 |
  | `versions` state vector | every 60 s | §4.3 |
  | keepalive comment | every 20 s | §4.1 |
  | SSE `retry:` | 3000 ms | §4.1 |
  | broadcast capacity per project | 512 | §4.5 |
  | frame rows inlined per `project` event | ≤ 50 | §4.3 |
  | `nextFlushMs` default | 1000 ms | §6.2 |
  | tombstone retention | 7 days | §5.1 |
  | high-water marks file | every 60 s | §4.4 |
  | Postgres pool | 20 connections (was 5) | §5.4 |

- **Lock order (spec §5.1).** It is `projects` → `project_holder_cursor(project_id)` → `project_members` → frame rows (`project_frames`, then `frame_holders`). A writer that touches several projects locks every `projects` row in id order first, then every cursor row in id order. A holder report locks only the cursor row.
- **Serde.** Wire names are camelCase (`#[serde(rename_all = "camelCase")]`); SQL is snake_case.
- **Cross-plan contract.** § Wire contract is what the app plan codes against. Any change to it during implementation edits that section in the same commit and is reported to the controller.
- **Naming.** No third-party product names in code, comments, docs or commit messages. Our own dependencies (axum, sqlx, tokio, blake3, iroh, nginx, systemd, Postgres) may be named.
- **Commits.**
  - Commit as the configured git user `eg013ra1n <vilen.sharifov@gmail.com>`.
  - Every commit message ends with exactly these two lines:

    ```
    Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
    Claude-Session: https://claude.ai/code/session_01RYd9tanU4ehWLDXkPv159r
    ```

  - The trailers are omitted from the `git commit -m` lines below only for brevity. Always add them with a second `-m`.
- **No push and no deploy.** The astronet task changes files and verifies them with `--syntax-check` only. A deploy to the test hub or production happens only on the owner's word, as a separate procedure.
- **Dependencies (Task 1 adds them all).** In `Cargo.toml`:
  - `blake3 = "1"`, which resolves to 1.8.5, already in the local registry;
  - `tokio-stream = { version = "0.1", features = ["sync"] }`, already in `Cargo.lock` transitively;
  - tokio features `"sync", "time"` added to the existing list.

  No new dev-dependencies. Integration tests read SSE bodies through `axum::body::Body::into_data_stream()` and `tokio_stream::StreamExt`.

---

## Wire contract (what the app codes against)

All paths are under `https://<hub>/api/v1`. `deviceToken` is
`Authorization: Bearer <token>` from `POST /auth/verify`. Errors come in three
shapes:
- `{"error": "<message>"}`;
- an empty body, for 401 and 403;
- the typed bodies listed per route below.

A malformed JSON body gets axum's own rejection: 400 for a syntax error, 415
without `Content-Type: application/json`, 422 for a shape mismatch, each with a
plain-text body.

### Identifiers and encodings

| Name | Encoding |
| ---- | ---- |
| `device` | base64 (standard alphabet, padded) of the device's 32-byte public key: the same string as `GET /devices` → `pubkey` and the membership snapshot's `nodes[]`. Hub device UUIDs never cross accounts. |
| `projectId`, `uuid`, `frameUuid`, `accountId` | lowercase hyphenated UUID strings. |
| `frameSeq` | integer ≥ 1. A dense per-project frame ordinal, assigned at announce and never reused. |
| `contentVersion` | integer ≥ 1. |
| `version` | the project's `projects.version` (int64). It orders frames, metadata and membership changes. |
| `holderSeq` | the project's holder cursor (int64, starts at 0). It orders holdings. |
| `epoch` | an opaque string (today a UUID). Compare it for equality only. |
| `sessionId` | 32 lowercase hex characters (128 random bits). |
| `digest` | 32 lowercase hex characters (16 bytes). |

### The claim digest (spec §6.3)

- **Per claim.** `key(uuid, cv)` is the first 16 bytes of `blake3(input)`, where
  `input` is 20 bytes:
  - bytes 0–15: the frame UUID's 16 bytes in RFC 4122 order, i.e. the
    hyphenated hex read left to right (`Uuid::as_bytes()`);
  - bytes 16–19: `contentVersion` as an unsigned 32-bit big-endian integer.
- **Per claim set.** `digest(set)` is the byte-wise XOR of `key` over the
  device's own **non-removed** claims in the project, whatever version is
  current. `count` is the number of those claims.
- **Empty set.** The digest is `00000000000000000000000000000000` and `count` is 0.
- **Worked vectors.** The implementation must reproduce them. They are pinned
  in `src/claims/digest.rs` tests, and the app must pin the same.

| Input | Value |
| ---- | ---- |
| `a = 00000000-0000-4000-8000-000000000001`, cv 1: input bytes | `0000000000004000800000000000000100000001` |
| `blake3(input)` (full 32 bytes) | `129cef69895583884285d1404f99e181afc3b63d273a5314902d2ec7bf24183f` |
| `key(a, 1)` | `129cef69895583884285d1404f99e181` |
| `key(b = …000000000002, 1)` | `c3ef44a7f50605a0dee0abca26ecf959` |
| `key(c = …000000000003, 2)` | `40fc3b9d29d73f9101f6293d9c3b5cc4` |
| `key(c, 1)` | `b8c7cf6630052129879bab6949f47d5b` |
| `{(a,1), (b,1)}` → count 2 | `d173abce7c5386289c657a8a697518d8` |
| `{(a,1), (b,1), (c,2)}` → count 3 | `918f90535584b9b99d9353b7f54e441c` |
| `{(a,1), (b,1), (c,1)}` → count 3 | `69b464a84c56a7011bfed1e320816583` |

### Run-length claims (snapshot `devices[].claims`)

- **Shape.** `claims` is a JSON array of runs `[startSeq, runLength, contentVersion]`, all integers. It is sorted by `startSeq`. Runs never overlap.
- **Meaning.** A run says the device claims every `frameSeq` in `startSeq ..= startSeq + runLength − 1` at `contentVersion`. A frame outside every run is not claimed by that device.
- **Construction.** Consecutive `frameSeq` values with the same `contentVersion` form one run. Removed claims are never listed.
- **Example.** Claims `{1:v1, 2:v1, 3:v1, 5:v2, 6:v1}` encode as `[[1,3,1],[5,1,2],[6,1,1]]`.

### Event stream — `GET /me/events`

- **Auth.** `deviceToken` of a device with capability `athenaeum`.
  - 401 (empty body): missing, unknown, revoked or blocked token.
  - 403 (empty body): a `perseus` device.
  - A portal session cannot open the stream.
- **Response.** `200`, `Content-Type: text/event-stream`, `Cache-Control: no-cache`, `X-Accel-Buffering: no`.
- **Framing.** Each event is `event: <name>\ndata: <one-line JSON>\n\n`.
  - A keepalive comment `:\n\n` comes every 20 s.
  - The first event (`hello`) also carries `retry: 3000`.
- **One stream per device.** A second stream from the same device ends the older one: its body closes.
- **Hub-side close.** The hub closes the stream on revoke/retire, account block, `DELETE /me/presence` and hub shutdown.
- **Client transport (spec §4.1).** No total timeout, a 50 s read timeout, and reconnect with full-jitter exponential back-off (1 s base, 60 s cap).

| Event | `data` JSON | When |
| ---- | ---- | ---- |
| `hello` | `{"sessionId":"<32hex>","epoch":"<str>","accountId":"<uuid>","projects":{"<projectId>":{"version":42,"holderSeq":17,"claimCount":3,"claimDigest":"<32hex>","reportSeq":120,"presence":[{"device":"<b64>","serving":true,"relayUrl":"https://…"}]}}}` | first event of every stream |
| `project` | `{"projectId":"<uuid>","prev":41,"version":42,"kinds":["frames","members"],"frames":[FrameEvent,…],"more":false}` | after a committed bump, coalesced ≤ 250 ms |
| `holders` | `{"projectId":"<uuid>","prev":16,"seq":17,"deltas":[{"device":"<b64>","add":[[12,1],[13,1]],"rm":[7]}]}` | holder changes, coalesced 1 s |
| `presence` | `{"projectId":"<uuid>","replace":false,"changes":[{"device":"<b64>","connected":true,"serving":true,"relayUrl":"https://…"}]}` | coalesced 1 s; `replace: true` once at the end of the hub's warm-up |
| `account` | `{"kind":"joined","projectId":"<uuid>"}` (or `"left"`) | the caller's own account joined or left a project |
| `resync` | `{"projectId":"<uuid>","what":"project"}` (or `"holders"`) | a gap not filled within 2 s, or this stream lagged |
| `versions` | `{"<projectId>":[42,17],…}`, i.e. `[version, holderSeq]` per project of this stream | every 60 s |

**Field rules.**

- **`hello.projects`** lists exactly the projects the account is a member of when the stream opens. A cached project missing from it means the account left while disconnected.
  - **`claimCount` / `claimDigest`** are the hub's view of **this device's** claims in that project.
  - **`reportSeq`** is the highest `reportSeq` the hub has stored for this device in that project, 0 if none.
  - **`presence`** lists the connected devices of that project's members, this device included. It is `[]` during the hub's 30 s warm-up.
- **`project`.**
  - **`kinds`** is a subset of `frames`, `meta`, `members`, `thresholds`, `dictionary`, `grid`, always in that order.
  - **`frames`** is present only when every changed frame row is inlined. That requires ≤ 50 rows, all with `state = "published"`.
  - **`more: true`** means frames changed but were not inlined: pull `GET /projects/{id}/manifest?since=<your cursor>`. `more` is always present.
  - **The `members` kind** covers member added, removed, left, role, flags and trust, plus device added, revoked or retired. On it, refetch `GET /projects/{id}/membership` and rebuild the connect gate.
- **`FrameEvent`** is the manifest's `FrameView` without `own`: `{frameUuid, frameSeq, publisherAccountId, publisherDisplayName, fileName, contentVersion, blake3, byteSize, xxh3, filterRaw, filterCanonical, channel, exptimeSec, dateObs|null, meta, gateVersion, accepted, acceptedReason|null, state, rejectReason|null, manifestVersion, createdAt}`. Derive `own` as `publisherAccountId == hello.accountId`.
- **`holders`.**
  - **`add`** entries are `[frameSeq, contentVersion]`: the device now claims that version.
  - **`rm`** entries are `frameSeq` values: the claim ended.
  - Within one event a `frameSeq` appears at most once per device.
  - A claim on a `frameSeq` your manifest does not know yet (a pending frame you cannot see) is kept. It resolves when the manifest delivers that `frameSeq`.
- **`presence`.**
  - **`connected: false`** carries `serving: false` and the last known `relayUrl`.
  - **`serving`** is per project.
  - A device is a candidate provider for a frame only when all of these hold: its claimed `contentVersion` equals the manifest's current one, it is `connected` and `serving` in this project, it is a member, and it is not you (I4, I5).
  - Presence only adds candidates. It never cancels an open transfer.
- **Cursor rules (I3), per project.**
  - The client keeps `(epoch, version)` and `(epoch, holderSeq)`.
  - Apply a `project` or `holders` event when `prev == cursor`, then set `cursor = version` / `seq`. Ignore it when `version`/`seq` ≤ cursor.
  - Otherwise catch up over REST: `GET …/manifest?since=`, plus the small documents for the listed kinds, or `GET …/holders?since=`. Then continue.
  - On `resync`, catch up the named side over REST.
  - On `versions`: a head above the cursor means catch up; a head below the cursor means an epoch change (below).
- **Epoch change.** Treat `hello.epoch ≠ stored epoch`, or any head below the stored cursor, as an epoch change (spec §4.4). Then:
  - reload every snapshot;
  - reconcile holdings (`full: true` report);
  - re-announce your own frames the hub no longer lists, under their existing uuids.

### Presence beat — `POST /me/presence`, `DELETE /me/presence`

Both routes sit outside token auth. The hub authenticates the `sessionId` in
memory and makes no database query.

| Route | Request | Success | Errors |
| ---- | ---- | ---- | ---- |
| `POST /me/presence` | `{"sessionId":"<32hex>","serving":{"<projectId>":true,…},"relayUrl":"https://…"\|null}` | `204` | `409 {"error":"session_gone"}`: unknown, replaced or expired session; reopen the stream at once. `400 {"error":"sessionId must be 32 lowercase hex chars"}`. `400 {"error":"homeRelayUrl must use https"}` (and the other relay-URL messages of `PUT /devices/self/address`). |
| `DELETE /me/presence` | `{"sessionId":"<32hex>"}` | `204`, also for an unknown session | `400` bad `sessionId` format |

- **Fields.** `sessionId` is required. `serving` (object of `projectId → bool`) defaults to `{}`. `relayUrl` (string or null) defaults to `null`.
- **When to beat.** Send `POST` right after `hello`, every 15 s, and at once when serving state or home relay changes. `serving` keys that are not projects of the account are ignored. A project missing from the map counts as `false`.
- **When the hub marks you offline:**
  - `DELETE` → offline at once, and the stream is closed;
  - stream closed → offline 10 s later, unless a beat with that session arrives in those 10 s. A session kept alive by beats lives on the 40 s rule alone;
  - no beat for 40 s → offline (the stream opening counts as a beat).
- **Flap damping.** From the 3rd online transition within 5 min, an offline broadcast is delayed 60 s. A return inside the delay broadcasts nothing.
- **After a hub restart.** Everyone starts offline. For 30 s no presence diffs are sent. Then every open stream gets one `presence` event per project with `replace: true`, and `hello` carries full presence from then on.
- **Relay URL.** It comes from the latest beat. Until the first beat it is the device's stored `homeRelayUrl` from `PUT /devices/self/address`.

### Holders — reads

**`GET /projects/{id}/holders/snapshot`.**

- **Auth.** `deviceToken` or portal session. The caller must be a member (403 otherwise, empty body). It is one REPEATABLE READ transaction.
- **Response:**

  ```json
  {"epoch":"<str>","holderSeq":17,"version":42,
   "frames":[{"seq":1,"uuid":"<uuid>","contentVersion":2}],
   "devices":[{"device":"<b64>","displayName":"Anna","relayUrl":"https://…"|null,"claims":[[1,3,1],[5,1,2]]}]}
  ```

- **`frames`.** Every frame visible to the caller, ordered by `seq`. That means published frames, plus the caller's own frames in any state, plus every frame for a `data.moderate` holder.
- **`devices`.** Every non-revoked `athenaeum` device of a current member, the caller's own included, ordered by `device`.
  - `claims` is that device's non-removed claims in run-length form, any `contentVersion`, including `frameSeq` values not in `frames`.
  - `relayUrl` is the live presence relay when the device is connected, else its stored `homeRelayUrl`.
- **Consistency.** `holderSeq` and `version` are the cursors this snapshot reflects.

**`GET /projects/{id}/holders?since=S[&after=A][&limit=N][&epoch=E]`.**

- **Auth.** Member (`deviceToken` or portal session). `limit` defaults to 5000 and is clamped to 1..=5000.
- **Response:**

  ```json
  {"epoch":"<str>","holderSeq":17,"floor":3,
   "deltas":[{"device":"<b64>","add":[[12,1]],"rm":[7]}],
   "hasMore":false,"next":null}
  ```

  When `hasMore` is true, `next` is `{"since":S,"after":"<opaque>"}`. Pass both back verbatim.

- **Semantics.**
  - Every claim whose state changed after `S`, in its **current** state.
  - Each `(device, frameSeq)` appears once per page.
  - Paged in `(changedSeq, device, frameSeq)` order.
  - After a multi-page catch-up, set your cursor to the **first** page's `holderSeq`. A later page may repeat newer changes, and applying them twice is harmless.
- **410 cases (reload the snapshot):**
  - `{"error":"holders_below_floor","floor":F,"holderSeq":H}` when `S < floor`;
  - `{"error":"holders_cursor_ahead","floor":F,"holderSeq":H}` when `S > holderSeq`;
  - `{"error":"epoch_changed","epoch":"<current>"}` when `E` is given and differs.

### Holders — `PUT /projects/{id}/holders/self`

- **Auth.** `deviceToken`, member. A portal session gets `400 {"error":"a device token is required to report holdings"}`.
- **Body limit.** 8 MB.
- **Request:**

  ```json
  {"reportSeq":120,"full":false,
   "add":[{"uuid":"<uuid>","contentVersion":1}],
   "remove":["<uuid>"],
   "digest":"<32hex>","count":42}
  ```

- **Response:** `200 {"holderSeq":18,"digestMatch":true,"nextFlushMs":1000,"refused":["<uuid>"]}`.

**Rules.**

- **Fields.** `reportSeq` (int64), `digest` (string) and `count` (int64) are required. `full` (bool, default `false`), `add` (default `[]`) and `remove` (default `[]`) may be omitted. `contentVersion` is an int32.
- **`reportSeq` (≥ 1).**
  - It comes from one monotonically increasing counter per device, never reused, that survives restarts. It is the highest journal sequence this report includes. After a data loss, raise it above `hello.projects[*].reportSeq`.
  - Every entry of the report is stamped with it.
  - An entry applies only if `reportSeq` is greater than the value stored for that `(device, frame)`.
  - So duplicated or reordered deliveries are harmless (C19, C20). Coalesce your outbox per frame (last op wins) before sending.
- **`full: true`.** `add` is the complete claim set as of `reportSeq` R.
  - The hub tombstones every live claim of this device in the project that is unlisted and has a stored reportSeq below R.
  - Entries with a stored reportSeq ≥ R are untouched. They are newer deltas.
  - `remove` must be empty.
- **Claims are durable.** They never expire, and they are stored whatever version is current (I4).
- **A claim is refused** (listed in `refused`, not stored, and any live hub claim for that frame tombstoned) when any of these holds:
  - the frame is not in this project;
  - `contentVersion < 1`, or `contentVersion` is above the frame's current version;
  - the member may not hold it: the publisher may claim its own frames in any state; others need `send_receive` or `data.moderate` for a published frame, and `data.moderate` for a pending one.

  Drop refused frames from your claim set.
- **`remove` of a frame the hub has no claim for** is recorded as an invisible tombstone, so that a delayed older `add` cannot resurrect it.
- **`digest` / `count`.** They describe your whole claim set in this project **after** this report.
  - `digestMatch` compares the hub's set with yours, after removing the `refused` entries' keys from yours.
  - On `false`, send one `full: true` report (spec §6.3).
- **An empty report** (`full: false`, no `add`, no `remove`) is a pure digest check. It takes no lock and writes nothing.
- **`holderSeq`** is the project's holder cursor after the report.
- **Flush timing.** Flush after `nextFlushMs`, or at once at 500 entries.

**Errors.**

| Status | Body | Cause |
| ---- | ---- | ---- |
| 400 | `{"error":"reportSeq must be >= 1"}` | invalid `reportSeq` |
| 400 | `{"error":"add/remove must each contain at most 100000 items"}` | list too long |
| 400 | `{"error":"duplicate uuid in add: <uuid>"}` or `{"error":"duplicate uuid in remove: <uuid>"}` | duplicate entry |
| 400 | `{"error":"uuid <uuid> present in both add and remove"}` | contradictory entry |
| 400 | `{"error":"remove must be empty when full is true"}` | `full` with `remove` |
| 400 | `{"error":"digest must be 32 lowercase hex chars"}` | bad digest |
| 400 | `{"error":"count must be >= 0"}` | bad count |
| 409 | `{"error":"collab_api_outdated"}` | a body without `reportSeq` (a wave-2 app) |
| 403 | empty | not a member |

**Implicit claims.** A successful announce (`POST /projects/{id}/frames`) claims `(uuid, 1)` for the calling device, per frame. A successful version (`POST …/{uuid}/version` or an `ok` entry of `POST …/frames/versions`) claims `(uuid, newContentVersion)`.
- The hub writes these claims in the same transaction and stamps them with this device's highest stored `reportSeq` in the project, so a delayed older report can never roll them back (amended during execution, hub Task 2 review).
- Add them to your claim set and digest without reporting them.
- Flush any pending outbox entry for such a frame **before** calling version.

### Frames — versions

**`POST /projects/{id}/frames/{uuid}/version`.**

- **Auth.** `deviceToken`, publisher only.
- **Request:** `{"expectedVersion":1,"blake3":"<64hex>","byteSize":104000000,"xxh3":"<16hex>"}`.
- **Response:** `200 {"contentVersion":2,"projectVersion":43}`.

| Status | Body | Cause |
| ---- | ---- | ---- |
| 409 | `{"error":"version_conflict","contentVersion":3}` | the current version ≠ `expectedVersion` |
| 409 | `{"error":"collab_api_outdated"}` | no `expectedVersion` |
| 409 | `{"error":"project is closed"}` | project closed |
| 403 | empty | not the publisher |
| 404 | `{"error":"frame not found"}` | unknown frame |
| 400 | `{"error":"blake3 must be 64 lowercase hex chars"}` (and the other hex/size messages) | bad hash or size |

No holder row is deleted. Older claims simply stop validating.

**`POST /projects/{id}/frames/versions` (new).**

- **Auth.** `deviceToken`, member. The batch is 1..=500 entries and runs in one transaction.
- **Request:** `{"versions":[{"uuid":"<uuid>","expectedVersion":1,"blake3":"<64hex>","byteSize":1,"xxh3":"<16hex>"}]}`.
- **Response:** `200 {"projectVersion":43,"results":[{"uuid":"<uuid>","status":"ok","contentVersion":2}]}`. `results` is in request order.
- **Fields.** Every request field is required: `expectedVersion` int32, `byteSize` int64 > 0, `blake3` 64 lowercase hex, `xxh3` 16 lowercase hex.
- **Per-entry `status`:**
  - `ok`: versioned, and `contentVersion` is the new version;
  - `conflict`: `contentVersion` is the current version;
  - `not_found`: `contentVersion` is 0;
  - `forbidden`: you are not the publisher; `contentVersion` is 0.
- **Version bump.** `projectVersion` moves only when at least one entry is `ok`.
- **Whole-batch errors:**
  - 400 for bad hex or size, an empty batch, more than 500 entries, or a duplicate uuid;
  - 409 `{"error":"project is closed"}`;
  - 403 for a non-member.

### Changed shapes elsewhere

- **Manifest `FrameView`** (`GET /projects/{id}/manifest`) gains `frameSeq` (int) and loses `holderCount`. Everything else is unchanged.
- **`POST /devices/{id}/revoke`** takes an optional body `{"retire":true}` and answers `204` (`404 {"error":"device not found"}`).
  - Revoke and retire have identical effects. The device's claims are tombstoned in every project. Every project of the account gets a `project` event with kind `members`. The device's stream is closed.
  - A retired device is simply absent from `GET /devices`.
- **`POST /auth/verify`** for an `athenaeum` device bumps kind `members` in every project of the account.
- **Retired routes.** `GET /me/project-versions` and `GET /projects/{id}/frames/{uuid}/holders` answer `409 {"error":"collab_api_outdated"}`. So does every request of the wave-2 shapes above.

---

## Plan rulings (decided here; cite as P1…)

Each ruling names the decision, why, and the cost if it is wrong.

- **P1 — Commit and publish in one spawned task.** Every writer replaces
  `tx.commit(); state.versions.invalidate(id)` with
  `commit_and_publish(&state, tx, feed)`. It moves the `Transaction<'static>`
  into a `tokio::spawn` that commits, invalidates the version cache and calls
  `FeedHub::publish`.
  - *Why:* spec §4.5. A dropped request future can no longer commit without publishing.
  - *If wrong:* a lost wake-up. The 60 s `versions` vector heals it.
  - The `VersionCache` keeps being invalidated there until Task 10 retires its only reader.
- **P2 — One holder-cursor bump per transaction, and only for a visible change.**
  - A transaction that changes a claim's `(content_version, removed)` stamps every changed row with `changed_seq = seq + 1` and bumps the cursor once in `HolderWrite::finish`.
  - A write that changes only `report_seq` (an ordering pin) does not bump.
  - *Why:* gapless, commit-ordered cursors (I2), and zero cursor writes for an idle hub.
  - *If wrong:* a delta reader misses a change. Pinned by the Task 1 watermark test.
- **P3 — Devices are identified by base64 pubkey on the wire**, never by hub UUID.
  - *Why:* the app dials node ids and verifies them against the signed snapshot. UUIDs are internal.
  - *If wrong:* one extra mapping on the app side.
- **P4 — Frame ordinals.** They are 1-based and dense. `projects.next_frame_seq` is the next one to assign; announce reserves N in one `UPDATE … RETURNING`.
  - Migration 0023 numbers existing rows in `(project_frames.created_at, frame_uuid)` order. `created_at` is the spec's "announced_at": `project_frame_versions.announced_at` for v1 is written in the same transaction with the same `now()`.
  - *If wrong:* only the order of old test rows changes.
- **P5 — Claims on frames the caller cannot see.** They are broadcast and listed by `frameSeq`. The snapshot's `frames` list is visibility-filtered.
  - *Why:* dense ordinals already reveal that a pending frame exists; a `frameSeq` carries no name, hash or content. Filtering the broadcast per viewer would need per-stream frame state.
  - *If wrong:* a member learns that some pending frame has holders.
- **P6 — Claim permission and refusal.**
  - A publisher may claim its own frame in any state. Others need `send_receive`, or `data.moderate`, on a published frame. `data.moderate` also covers pending frames.
  - Unknown frames, versions above current or below 1, and non-holdable frames are **refused**: listed, not stored, and any live hub claim is tombstoned.
  - `digestMatch` removes refused keys from the device's digest before comparing.
  - *Why:* both sides must reach the same set from the same report, or the digest check loops forever.
  - *If wrong:* a mismatch loop. Pinned by a Task 2 test.
- **P7 — Claims are stored regardless of currency** (`1 ≤ cv ≤ current`). This replaces wave 1's `a.v = f.content_version` guard (`holders.rs:148`). *Why:* I4 and spec §5.2; availability is derived.
- **P8 — A `remove` of an unknown claim writes an invisible tombstone.** The tombstone has `changed_seq = 0` and `removed_at = now()` and is pruned after 7 days.
  - *Why:* without it a delayed older `add` resurrects the claim (C19).
  - *If wrong:* a few stray rows for 7 days.
- **P9 — Full-report semantics.** Listed entries are upserted when stored `report_seq < R`. Unlisted live claims with `report_seq < R` are tombstoned. Everything with `report_seq ≥ R` is untouched. *Why:* spec §6.3, "deltas with a sequence above R follow as usual".
- **P10 — Digest encoding** is exactly § Wire contract.
  - The digest of claims migrated from 0022 is **not** backfilled in SQL, because Postgres has no blake3.
  - `hello.claimDigest` reports the hub's value, 0/zeros for those rows. A device with claims sees a mismatch and sends one full report, which the hub applies and re-digests.
  - *If wrong:* one extra full report per device after the migration. Only local dev databases hold 0022 rows: the test hub runs migrations ≤ 21 and prod has had publishing blocked since 2026-08-31.
- **P11 — `hello` extensions beyond spec §4.3:**
  - `accountId`, so that clients derive `own` for inlined frames;
  - per project `claimCount`, `claimDigest`, so the spec's "after hello, the hub returns whether (count, digest) matches" needs no extra request;
  - per project `reportSeq` (the highest stored), so a device whose local journal was lost can move above the hub's pins instead of being ignored forever.
  - *If wrong:* three unused fields.
- **P12 — Inline frames.** Only `published` rows are inlined. More than 50 rows, or any non-published row in the coalesced event, means `frames` is omitted and `more: true`.
  - `FrameEvent` is the manifest `FrameView` without `own`.
  - *Why:* one serialised payload per project is shared by every stream. Visibility of pending rows differs per viewer.
  - *If wrong:* an extra manifest delta read for moderators on pending announces.
- **P13 — First-sight coalescer start.** After a hub start, a project's emitted cursor starts at the `prev` of the first event seen. An older event that arrives later is dropped.
  - *Why:* the hub keeps no per-project cursor in memory across restarts.
  - *If wrong:* a client sees `prev ≠ cursor` and catches up over REST, which is the I3 path anyway.
- **P14 — Session rules.**
  - The stream opening counts as a beat.
  - A detached session expires 10 s after the close unless a beat arrives after the close. Such a session then lives on the 40 s rule.
  - A replacing session inherits the device's serving map and relay.
  - `DELETE` means immediate offline, no damping, and the stream is closed. A `DELETE` of an unknown session answers 204.
  - A kick (revoke, block) means immediate offline, no damping.
  - *Why:* spec §4.2 leaves "a beat during the grace keeps the session alive" open-ended. This is the simplest reading that keeps a roaming client live.
- **P15 — Flap damping.** It counts online transitions, the first connect included. The offline broadcast of a device with ≥ 3 online transitions in the last 5 min is delayed 60 s. A return inside the delay emits nothing. `DELETE` and kicks are never damped.
- **P16 — Presence follows membership.** When an account joins a project, its connected devices appear in that project's presence. When it leaves, they get `connected: false` there. *Why:* presence is visible only within projects the device is a member of (§4.2).
- **P17 — Who may open the stream.**
  - `/me/events` needs an `athenaeum` device token. Perseus gets 403, because it never joins project exchange (`snapshots.rs:81`).
  - When `HUB_EVENTS_PROBE_TOKEN` is set, that exact bearer opens a **probe stream**: `event: hello` with `{"probe":true,"epoch":…}`, closed after 10 s, with no session and no presence.
  - *Why:* spec §5.4's "probe token provisioned for the deploy" without registering a fake device.
- **P18 — Device add, revoke and retire bump `projects.version` with kind `members`**, for every project of the account. They do **not** bump `membership_version`.
  - *Why:* its doc (`snapshots.rs:6-9`) defines it as membership-only, and clients apply every verified snapshot by content. Fewer test changes.
  - Retire is revoke plus `retire = true` in the log. It is not a second device state (§5.2).
  - A perseus device changes no snapshot, so it bumps nothing.
- **P19 — Leaving tombstones claims.** A member who leaves or is removed has every one of their account's devices' claims in that project tombstoned, under `projects` → cursor → `project_members`.
  - *Why:* I4, "a holding ends only by … the member leaving". §5.2 lists only revoke.
  - *If wrong:* ex-member claims linger. The snapshot filters them out anyway.
- **P20 — Block and project delete.** Blocking an account closes its streams but bumps nothing, because the signed snapshot carries no block state. The operator's `delete_project` sends `account left` to every member.
- **P21 — One 60 s read serves both the heads vector and the marks.** `SELECT p.id, p.version, c.seq FROM projects p JOIN project_holder_cursor c …` over all projects, once a minute, feeds the `versions` vector and the high-water marks file.
- **P22 — The epoch.**
  - It is an opaque UUID string in `hub_meta['epoch']`.
  - The marks file is `$STATE_DIRECTORY/feed-marks.json`, written atomically (`.tmp` + rename).
  - Startup rotates the epoch on: no epoch row; an unreadable marks file; epoch mismatch; any project head below its mark. A project missing from the database is ignored, because deletion is legitimate.
  - `athenaeum-hub rotate-epoch` (new subcommand) stands in for the spec's `migrate --reset`, which does not exist: the hub has no migrate CLI.
  - `STATE_DIRECTORY` unset (dev, tests) disables the marks with one `warn!`.
- **P23 — Graceful shutdown closes every stream first.**
  - *Why:* `axum::serve(..).with_graceful_shutdown` waits for all connections, and SSE streams never end by themselves. systemd would SIGKILL after 90 s.
- **P24 — The clock abstraction.** `feed::clock::Clock` has `SystemClock` and `ManualClock`. Every presence and coalescer timer is evaluated in `FeedHub::tick()`: the driver calls it every 50 ms, and tests call it after `clock.advance(..)`.
  - tokio's paused time is not used. Under auto-advance the sqlx pool's acquire timeout fires while a test waits on real database I/O.
- **P25 — The API bump** (spec §5.3). All of these answer `409 collab_api_outdated`:
  - the two retired routes;
  - a `PUT …/holders/self` without `reportSeq`;
  - a `POST …/version` without `expectedVersion`.

  The wave-2 app on local `main` (hub client since `4d3f963c`, contained in no release tag) calls exactly these four.
- **P26 — `holders?since` pages current rows, not a log.**
  - The keyset is `(changed_seq, device_id, frame_seq)`, carried in an opaque `after` string.
  - After a catch-up the cursor is the first page's `holderSeq`.
  - 410 for `S < floor`, `S > head`, or an epoch mismatch.
  - *Why:* the claims table is the log. A row that moves forward reappears later, and a row is never skipped.
- **P27 — Snapshot membership of `devices`.** It lists every non-revoked athenaeum device of current members, even with no claims. `relayUrl` is the live presence relay if connected, else the stored one.
- **P28 — `PUT …/holders/self` size.** Each list holds up to 100,000 entries, and the route carries an 8 MB body limit. *Why:* a full report of a large project must fit in one PUT (§6.3). The upgrade path above ~100,000 frames is range reconciliation, out of scope.
- **P29 — The migration tombstones holds that nobody can count on:** those of revoked devices and of accounts that are no longer members. Live rows get `changed_seq = 1`, and those projects' cursor `seq = 1`.
- **P30 — The load check runs in-process against real Postgres**, through a 20-connection pool (`tests/load_check.rs`, `#[ignore]`).
  - Spec §12 says "on the test hub". A run there needs 100 device tokens minted on that hub by OTP mail, so it belongs to the owner's deploy acceptance.
  - The harness prints the same measurements.
- **P31 — Instrument precedent.** Hub handlers use `#[tracing::instrument(skip_all)]` without `err`, 61 of them today. `ApiError::into_response` logs every failure itself: `error!` for 5xx, `warn!` for 429, `debug!` for refusals (`error.rs:54-81`). Adding `err` would double-log.
- **P32 — Revoke tombstones claims in the union of** the account's projects and the projects where the device still has live claims. It locks all those project rows in id order, then all their cursor rows in id order.
  - *Why:* a claim can outlive a membership, as a migrated row or a race. The union guarantees nothing survives.
  - *If wrong:* one extra lock.
- **P33 — Migration and code are one file each.** `0023_live_exchange.sql` is written once in Task 1 and never edited by a later task, because sqlx checksums applied migrations. Every schema object any task needs is in it.

---

## File structure

- **Create**
  - `migrations/0023_live_exchange.sql` (Task 1).
  - `src/claims/mod.rs`, `src/claims/cursor.rs` (Task 1); `src/claims/digest.rs`, `src/claims/plan.rs`, `src/claims/store.rs` (Task 2).
  - `src/feed/mod.rs`, `src/feed/clock.rs`, `src/feed/coalesce.rs`, `src/feed/presence.rs`, `src/feed/wire.rs`, `src/feed/hub.rs` (Task 3).
  - `src/feed/publish.rs` (Task 4); `src/feed/driver.rs` (Task 5); `src/feed/epoch.rs` (Task 9).
  - `src/routes/events.rs` (Task 5).
  - Tests:
    - `tests/claims.rs` (Tasks 1–2), `tests/feed_publish.rs` (Task 4), `tests/events.rs` (Task 5);
    - `tests/holders_read.rs` (Task 6), `tests/versions_cas.rs` (Task 7), `tests/revocation.rs` (Task 8);
    - `tests/epoch.rs` (Task 9), `tests/load_check.rs` (Task 13).
- **Modify**
  - Wiring and startup: `Cargo.toml`, `src/lib.rs`, `src/main.rs`, `src/db.rs`, `src/config.rs`, `src/error.rs`, `src/auth_mw.rs`, `src/collab_auth.rs`, `src/project_version.rs`, `src/scheduler.rs`.
  - Routes: `src/routes/{mod,frames,holders,devices,projects,profiles,members,join_requests,invites,operator,thresholds,dictionary,auth,compat}.rs`.
  - Tests: `tests/common/mod.rs`, `tests/{holders,frames,coverage,compat,collab_schema}.rs`.
  - Portal: `portal/src/types.ts`, `portal/src/pages/ProjectPage.test.tsx`.
  - `README.md`.
- **Delete** `src/routes/versions.rs` and `tests/versions.rs` (Task 10).
- **astronet** `templates/athenaeum-hub.service.j2`, `templates/hub.env.j2`, `templates/nginx-projects.artfrom.space.conf.j2`, `deploy_athenaeum_hub.yml` (Task 12).
- **App repo** `docs/superpowers/open-items.md` (Task 14).

---
### Task 1: Migration 0023, frame ordinals, the holder cursor and the lock order

**Files:**
- Modify: `Cargo.toml:23` (tokio features), `Cargo.toml` `[dependencies]` (add `blake3`, `tokio-stream`).
- Create: `migrations/0023_live_exchange.sql`.
- Create: `src/claims/mod.rs`, `src/claims/cursor.rs`. Add `pub mod claims;` to `src/lib.rs:10-24`.
- Modify: `src/routes/frames.rs`:
  - `:1-29` (module doc, lock order);
  - `:250-333` (`announce`: cursor lock, ordinals, claim columns);
  - `:352-477` (`FrameRow`, `FrameView`, `frame_select_sql`: add `frame_seq`, drop `holder_count`);
  - `:581-652` (`new_version`: no holder deletes);
  - `:908-940` (`reject`: claim-based foreign-holder check, no holder deletes).
- Modify: `src/routes/holders.rs`:
  - `:17-33` (drop `HOLDER_FRESH_SQL` and `fresh_holders_of_device_sql`; keep `HOLDER_ONLINE_SQL` until Task 11);
  - `:127-199` (interim claim-column writes; Task 2 replaces them);
  - `:250-258` (per-frame GET reads live claims).
- Modify: `src/routes/projects.rs:18, 908-963` (coverage on claims), `src/routes/projects.rs:269-400` (`create_project_core` inserts the cursor row).
- Modify: `src/routes/profiles.rs:26, 529-545` (`holding_count` on claims), `src/collab_auth.rs:104-115` (`lock_project_row` doc names the cursor).
- Test: `tests/claims.rs` (new), `tests/collab_schema.rs` (extend), and fix-ups in `tests/holders.rs:237-300, 415-430, 484-560` and `tests/frames.rs:169-186`.

**Interfaces:**
- Produces (`src/claims/cursor.rs`):
  ```rust
  #[derive(Debug, Clone, Default, PartialEq, Eq)]
  pub struct DeviceDelta { pub add: BTreeMap<i32, i32>, pub rm: BTreeSet<i32> }   // frame_seq -> content_version; frame_seq
  impl DeviceDelta { pub fn add(&mut self, frame_seq: i32, content_version: i32); pub fn rm(&mut self, frame_seq: i32); pub fn merge(&mut self, later: &DeviceDelta); pub fn is_empty(&self) -> bool }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct HolderBump { pub project_id: Uuid, pub prev: i64, pub seq: i64, pub deltas: BTreeMap<String, DeviceDelta> }   // key = device pubkey, base64
  pub struct HolderWrite { /* private */ }
  impl HolderWrite {
      pub async fn lock(conn: &mut PgConnection, project_id: Uuid) -> Result<Self, sqlx::Error>;
      pub fn project_id(&self) -> Uuid;
      pub fn base_seq(&self) -> i64;          // cursor seq when locked
      pub fn floor(&self) -> i64;
      pub fn seq(&self) -> i64;               // base_seq + 1: the changed_seq every visibly changed row gets
      pub fn record_add(&mut self, device_id: Uuid, frame_seq: i32, content_version: i32);
      pub fn record_rm(&mut self, device_id: Uuid, frame_seq: i32);
      pub fn touch(&mut self);                // Task 1 interim PUT only — deleted in Task 2
      pub async fn finish(self, conn: &mut PgConnection) -> Result<Option<HolderBump>, sqlx::Error>;
  }
  ```
- Produces (`src/claims/mod.rs`): `pub(crate) const VALID_CLAIM_SQL: &str;`.
- Wire:
  - `FrameView` gains `frameSeq`; `holderCount` is gone.
  - New version and reject no longer delete holder rows.

- [ ] **Step 1: Branch and dependencies**

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-hub
git checkout -b collab-v3-wave3 6127951
```

`Cargo.toml`: change line 23 to

```toml
tokio = { version = "1", features = ["macros", "rt-multi-thread", "signal", "net", "sync", "time"] }
```

and add under `[dependencies]` (after `tower-http`):

```toml
# Event channel (collab v3 wave 3): BroadcastStream/StreamMap/ReceiverStream for
# the per-device SSE pump.
tokio-stream = { version = "0.1", features = ["sync"] }
# The order-independent claim digest (spec 2026-09-25 §6.3): first 16 bytes of
# blake3(frame_uuid ‖ content_version), XOR-folded per device and project.
blake3 = "1"
```

Run: `cargo build 2>&1 | tail -2` → `Finished`.

- [ ] **Step 2: Write the failing tests**

Append to `tests/collab_schema.rs`:

```rust
#[sqlx::test]
async fn live_exchange_tables_and_columns_exist(pool: PgPool) {
    for table in ["project_holder_cursor", "device_project_digest"] {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL").bind(table).fetch_one(&pool).await.unwrap();
        assert!(exists, "{table} missing");
    }
    let cols: Vec<String> = sqlx::query_scalar(
        "SELECT table_name || '.' || column_name FROM information_schema.columns \
         WHERE (table_name = 'frame_holders' AND column_name IN ('report_seq','changed_seq','removed','removed_at','reported_at')) \
            OR (table_name = 'project_frames' AND column_name = 'frame_seq') \
            OR (table_name = 'projects' AND column_name = 'next_frame_seq') \
            OR (table_name = 'device_project_digest' AND column_name = 'max_report_seq') \
         ORDER BY 1",
    ).fetch_all(&pool).await.unwrap();
    assert_eq!(cols, vec![
        "device_project_digest.max_report_seq",
        "frame_holders.changed_seq",
        "frame_holders.removed",
        "frame_holders.removed_at",
        "frame_holders.report_seq",
        "project_frames.frame_seq",
        "projects.next_frame_seq",
    ], "reported_at is dropped (spec §5.1)");
}
```

Create `tests/claims.rs`:

```rust
//! Holder claims and the holder cursor (collab v3 wave 3, spec 2026-09-25
//! §5.1, §5.2, §6, invariants I2 and I4).
mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use athenaeum_hub::claims::HolderWrite;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

/// A project owned by `coord@example.com` (device seed 1) with `members`
/// extra `send_receive` members (device seeds 10..). Returns the router,
/// mailer, coordinator token, project id and `(token, device_id)` per member.
pub async fn project_with_members(
    pool: &PgPool,
    members: u8,
) -> (axum::Router, CaptureMailer, String, String, Vec<(String, Uuid)>) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap().to_string();
    let mut devices = Vec::new();
    for n in 0..members {
        let (token, device_id) = register_device(&app, &mailer, &format!("m{n}@example.com"), 10 + n, "PC").await;
        join_and_approve(&app, &coord, &token, &id, &format!("M{n}"), "send_receive").await;
        devices.push((token, Uuid::parse_str(&device_id).unwrap()));
    }
    (app, mailer, coord, id, devices)
}

pub async fn cursor(pool: &PgPool, project: Uuid) -> (i64, i64) {
    sqlx::query_as("SELECT seq, floor FROM project_holder_cursor WHERE project_id = $1")
        .bind(project)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn announce_assigns_dense_frame_seqs_and_the_manifest_carries_them(pool: PgPool) {
    let (app, _m, coord, id, _) = project_with_members(&pool, 0).await;
    let pid = Uuid::parse_str(&id).unwrap();
    assert_eq!(announce_frames(&app, &coord, &id, 1, 4).await.0, StatusCode::OK);
    assert_eq!(announce_frames(&app, &coord, &id, 4, 6).await.0, StatusCode::OK);
    let (_, body) = send(&app, get(&format!("/api/v1/projects/{id}/manifest?since=0"), Some(&coord))).await;
    let rows = as_json(&body)["rows"].as_array().unwrap().clone();
    let seqs: Vec<i64> = rows.iter().map(|r| r["frameSeq"].as_i64().unwrap()).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4, 5]);
    assert!(rows.iter().all(|r| r.get("holderCount").is_none()), "holderCount left the manifest (spec §5.2)");
    let next: i32 = sqlx::query_scalar("SELECT next_frame_seq FROM projects WHERE id = $1").bind(pid).fetch_one(&pool).await.unwrap();
    assert_eq!(next, 6);
    // Two announces = two holder-writing transactions (the publisher's own claims, I4).
    assert_eq!(cursor(&pool, pid).await, (2, 0));
    let (changed, removed, report_seq): (i64, bool, i64) = sqlx::query_as(
        "SELECT changed_seq, removed, report_seq FROM frame_holders WHERE project_id = $1 AND frame_uuid = $2",
    )
    .bind(pid)
    .bind(Uuid::parse_str(&frame_uuid(4)).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((changed, removed, report_seq), (2, false, 0));
}

#[sqlx::test]
async fn holder_cursor_is_gapless_and_commit_ordered_under_concurrent_writers(pool: PgPool) {
    let (app, _m, coord, id, devices) = project_with_members(&pool, 8).await;
    assert_eq!(announce_frames(&app, &coord, &id, 1, 129).await.0, StatusCode::OK);
    let pid = Uuid::parse_str(&id).unwrap();
    let (start, _) = cursor(&pool, pid).await;
    let frames: Vec<(Uuid, i32)> =
        sqlx::query_as("SELECT frame_uuid, frame_seq FROM project_frames WHERE project_id = $1 ORDER BY frame_seq")
            .bind(pid)
            .fetch_all(&pool)
            .await
            .unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let mut writers = Vec::new();
    for (w, (_, device_id)) in devices.iter().enumerate() {
        let pool = pool.clone();
        let mine: Vec<(Uuid, i32)> = frames.iter().skip(w * 16).take(16).cloned().collect();
        let device_id = *device_id;
        writers.push(tokio::spawn(async move {
            for (frame_uuid, frame_seq) in mine {
                let mut tx = pool.begin().await.unwrap();
                let mut hw = HolderWrite::lock(&mut tx, pid).await.unwrap();
                sqlx::query(
                    "INSERT INTO frame_holders (project_id, frame_uuid, device_id, content_version, report_seq, changed_seq) \
                     VALUES ($1, $2, $3, 1, 1, $4)",
                )
                .bind(pid)
                .bind(frame_uuid)
                .bind(device_id)
                .bind(hw.seq())
                .execute(&mut *tx)
                .await
                .unwrap();
                hw.record_add(device_id, frame_seq, 1);
                hw.finish(&mut tx).await.unwrap().expect("a changed claim bumps the cursor");
                tx.commit().await.unwrap();
            }
        }));
    }
    let watcher = {
        let pool = pool.clone();
        let done = done.clone();
        tokio::spawn(async move {
            loop {
                let finished = done.load(Ordering::SeqCst);
                let seen: Vec<i64> = sqlx::query_scalar(
                    "SELECT changed_seq FROM frame_holders WHERE project_id = $1 AND changed_seq > $2 ORDER BY changed_seq",
                )
                .bind(pid)
                .bind(start)
                .fetch_all(&pool)
                .await
                .unwrap();
                let expected: Vec<i64> = (start + 1..=start + seen.len() as i64).collect();
                assert_eq!(seen, expected, "committed changed_seq values must always be a gapless prefix (I2, C15)");
                if finished {
                    return seen.len();
                }
            }
        })
    };
    for w in writers {
        w.await.unwrap();
    }
    done.store(true, Ordering::SeqCst);
    assert_eq!(watcher.await.unwrap(), 128);
    assert_eq!(cursor(&pool, pid).await.0, start + 128);
}

#[sqlx::test]
async fn new_version_and_reject_keep_holder_rows(pool: PgPool) {
    let (app, _m, coord, id, devices) = project_with_members(&pool, 1).await;
    let pid = Uuid::parse_str(&id).unwrap();
    let (member, member_device) = devices[0].clone();
    assert_eq!(announce_frames(&app, &member, &id, 1, 2).await.0, StatusCode::OK);
    let uuid = frame_uuid(1);
    // The coordinator claims v1 (the Task 1 body; Task 2 changes it).
    let (status, _) = send(&app, put(&format!("/api/v1/projects/{id}/holders/self"),
        &json!({"full": false, "add": [{"frameUuid": uuid, "contentVersion": 1}], "remove": []}), Some(&coord))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = send(&app, post(&format!("/api/v1/projects/{id}/frames/{uuid}/version"),
        &json!({"blake3": format!("{:064x}", 77), "byteSize": 1, "xxh3": format!("{:016x}", 77)}), Some(&member))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let rows: Vec<(Uuid, i32, bool)> = sqlx::query_as(
        "SELECT device_id, content_version, removed FROM frame_holders WHERE project_id = $1 AND frame_uuid = $2 ORDER BY content_version",
    )
    .bind(pid)
    .bind(Uuid::parse_str(&uuid).unwrap())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2, "the coordinator's v1 claim survives the new version (spec §5.2)");
    assert_eq!(rows[0].1, 1);
    assert_eq!((rows[1].0, rows[1].1, rows[1].2), (member_device, 2, false));

    // Reject on an approval project keeps the publisher's claim row.
    let project = create_project_via(&app, &coord, "Q", true).await;
    let qid = project["id"].as_str().unwrap().to_string();
    join_and_approve(&app, &coord, &member, &qid, "M", "send").await;
    assert_eq!(announce_frames(&app, &member, &qid, 10, 11).await.0, StatusCode::OK);
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{qid}/frames/{}/reject", frame_uuid(10)),
        &json!({"reason": "blurred"}), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK);
    let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM frame_holders WHERE project_id = $1 AND NOT removed")
        .bind(Uuid::parse_str(&qid).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kept, 1, "reject no longer deletes holder rows (spec §5.2)");
}

const SEED_0022: &str = r#"
INSERT INTO accounts (id, email) VALUES
  ('a0000000-0000-4000-8000-000000000001', 'coord@example.com'),
  ('a0000000-0000-4000-8000-000000000002', 'bob@example.com'),
  ('a0000000-0000-4000-8000-000000000003', 'gone@example.com');
INSERT INTO devices (id, account_id, pubkey, token_hash, revoked_at) VALUES
  ('d0000000-0000-4000-8000-000000000001', 'a0000000-0000-4000-8000-000000000001', decode(repeat('01', 32), 'hex'), 't1', NULL),
  ('d0000000-0000-4000-8000-000000000002', 'a0000000-0000-4000-8000-000000000002', decode(repeat('02', 32), 'hex'), 't2', NULL),
  ('d0000000-0000-4000-8000-000000000003', 'a0000000-0000-4000-8000-000000000002', decode(repeat('03', 32), 'hex'), 't3', now()),
  ('d0000000-0000-4000-8000-000000000004', 'a0000000-0000-4000-8000-000000000003', decode(repeat('04', 32), 'hex'), 't4', NULL);
INSERT INTO projects (id, slug, title, target_name, target_ra_deg, target_dec_deg, target_radius_deg, created_by, version) VALUES
  ('b0000000-0000-4000-8000-000000000001', 'p', 'P', 'M101', 210.8, 54.35, 1.5, 'a0000000-0000-4000-8000-000000000001', 5),
  ('b0000000-0000-4000-8000-000000000002', 'q', 'Q', 'M31', 10.7, 41.3, 1.0, 'a0000000-0000-4000-8000-000000000001', 1);
INSERT INTO project_members (project_id, account_id, display_name, data_role, is_coordinator) VALUES
  ('b0000000-0000-4000-8000-000000000001', 'a0000000-0000-4000-8000-000000000001', 'Coord', 'send_receive', true),
  ('b0000000-0000-4000-8000-000000000001', 'a0000000-0000-4000-8000-000000000002', 'Bob', 'send_receive', false),
  ('b0000000-0000-4000-8000-000000000002', 'a0000000-0000-4000-8000-000000000001', 'Coord', 'send_receive', true);
INSERT INTO project_frames (project_id, frame_uuid, publisher, file_name, blake3, byte_size, xxh3, filter_raw, filter_canonical, exptime_sec, gate_version, state, manifest_version, created_at) VALUES
  ('b0000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000002', 'a0000000-0000-4000-8000-000000000001', 'two.fits', repeat('b', 64), 10, repeat('b', 16), 'L', 'L', 60, 0, 'published', 5, '2026-09-01T00:00:05Z'),
  ('b0000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000003', 'a0000000-0000-4000-8000-000000000001', 'three.fits', repeat('c', 64), 10, repeat('c', 16), 'L', 'L', 60, 0, 'published', 5, '2026-09-01T00:00:01Z'),
  ('b0000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000001', 'a0000000-0000-4000-8000-000000000001', 'one.fits', repeat('a', 64), 10, repeat('a', 16), 'L', 'L', 60, 0, 'published', 5, '2026-09-01T00:00:01Z');
INSERT INTO frame_holders (project_id, frame_uuid, device_id, content_version) VALUES
  ('b0000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000001', 'd0000000-0000-4000-8000-000000000001', 1),
  ('b0000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000002', 'd0000000-0000-4000-8000-000000000002', 1),
  ('b0000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000001', 'd0000000-0000-4000-8000-000000000003', 1),
  ('b0000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000003', 'd0000000-0000-4000-8000-000000000004', 1);
"#;

/// Migration 0023 against the rows migration 0022 created (spec §14:
/// "ordinals and claim conversion for the rows migration 0022 created").
/// `Migrator::migrations` is `#[doc(hidden)] pub` in sqlx 0.8.6 (pinned in
/// Cargo.lock); a test-only use.
#[sqlx::test(migrations = false)]
async fn migration_0023_numbers_frames_and_converts_holds(pool: PgPool) {
    let full = sqlx::migrate!("./migrations");
    let mut upto_22 = sqlx::migrate!("./migrations");
    upto_22.migrations = std::borrow::Cow::Owned(full.migrations.iter().filter(|m| m.version <= 22).cloned().collect());
    upto_22.run(&pool).await.unwrap();
    sqlx::raw_sql(SEED_0022).execute(&pool).await.unwrap();
    full.run(&pool).await.unwrap();

    let p = Uuid::parse_str("b0000000-0000-4000-8000-000000000001").unwrap();
    let q = Uuid::parse_str("b0000000-0000-4000-8000-000000000002").unwrap();
    let seqs: Vec<(String, i32)> = sqlx::query_as("SELECT file_name, frame_seq FROM project_frames WHERE project_id = $1 ORDER BY frame_seq")
        .bind(p).fetch_all(&pool).await.unwrap();
    assert_eq!(seqs, vec![("one.fits".into(), 1), ("three.fits".into(), 2), ("two.fits".into(), 3)],
        "ordinals follow (created_at, frame_uuid) — ruling P4");
    let next: Vec<(Uuid, i32)> = sqlx::query_as("SELECT id, next_frame_seq FROM projects ORDER BY id").fetch_all(&pool).await.unwrap();
    assert_eq!(next, vec![(p, 4), (q, 1)]);
    assert_eq!(cursor(&pool, p).await, (1, 0));
    assert_eq!(cursor(&pool, q).await, (0, 0));
    let holds: Vec<(Uuid, bool, bool, i64, i64)> = sqlx::query_as(
        "SELECT device_id, removed, removed_at IS NOT NULL, changed_seq, report_seq FROM frame_holders WHERE project_id = $1 ORDER BY device_id",
    ).bind(p).fetch_all(&pool).await.unwrap();
    let d = |n: u32| Uuid::parse_str(&format!("d0000000-0000-4000-8000-{n:012x}")).unwrap();
    assert_eq!(holds, vec![
        (d(1), false, false, 1, 0),
        (d(2), false, false, 1, 0),
        (d(3), true, true, 1, 0),  // revoked device → tombstone (P29)
        (d(4), true, true, 1, 0),  // not a member → tombstone (P29)
    ]);
    let digests: i64 = sqlx::query_scalar("SELECT count(*) FROM device_project_digest").fetch_one(&pool).await.unwrap();
    assert_eq!(digests, 0, "digests are not backfilled in SQL — ruling P10");
}
```

- [ ] **Step 3: Run the tests and watch them fail**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --test claims --test collab_schema`
Expected: FAIL. The build fails first on `athenaeum_hub::claims` (unresolved import).

- [ ] **Step 4: Write the migration**

`migrations/0023_live_exchange.sql`:

```sql
-- 0023_live_exchange — collab v3 wave 3 (spec 2026-09-25 §5.1): frame
-- ordinals, device-owned durable holder claims, the gapless holder cursor
-- and the per-device claim digest. Written once; later tasks never edit it
-- (sqlx checksums applied migrations — plan ruling P33).

-- Frame ordinals: dense per project, assigned at announce, never reused.
-- They index the holder maps on the wire (`frameSeq`). `next_frame_seq` is
-- the NEXT ordinal to assign; announce reserves N with one UPDATE … RETURNING
-- under the project row lock it already holds.
ALTER TABLE projects ADD COLUMN IF NOT EXISTS next_frame_seq integer NOT NULL DEFAULT 1;
ALTER TABLE project_frames ADD COLUMN IF NOT EXISTS frame_seq integer;
-- Existing rows get ordinals in (announce time, frame_uuid) order.
-- `created_at` IS the announce time: `project_frame_versions.announced_at`
-- for content_version 1 is written in the same transaction with the same
-- now() (plan ruling P4).
WITH ordered AS (
    SELECT project_id, frame_uuid,
           row_number() OVER (PARTITION BY project_id ORDER BY created_at, frame_uuid) AS seq
    FROM project_frames
)
UPDATE project_frames f SET frame_seq = o.seq
FROM ordered o
WHERE f.project_id = o.project_id AND f.frame_uuid = o.frame_uuid;
ALTER TABLE project_frames ALTER COLUMN frame_seq SET NOT NULL;
ALTER TABLE project_frames ADD CONSTRAINT project_frames_frame_seq_key UNIQUE (project_id, frame_seq);
ALTER TABLE project_frames ADD CONSTRAINT project_frames_frame_seq_positive CHECK (frame_seq >= 1);
UPDATE projects p
SET next_frame_seq = COALESCE((SELECT max(f.frame_seq) + 1 FROM project_frames f WHERE f.project_id = p.id), 1);

-- The gapless holder counter (I2). `seq` is bumped once per committed
-- transaction that visibly changes a claim, under this row's lock; `floor`
-- rises when tombstones older than 7 days are pruned — a delta read below it
-- answers 410 "reload the snapshot".
CREATE TABLE IF NOT EXISTS project_holder_cursor (
    project_id uuid   PRIMARY KEY REFERENCES projects (id) ON DELETE CASCADE,
    seq        bigint NOT NULL DEFAULT 0 CHECK (seq >= 0),
    floor      bigint NOT NULL DEFAULT 0 CHECK (floor >= 0 AND floor <= seq)
);

-- Claims: device-owned, durable, keyed by version (I4). `report_seq` is the
-- device's own ordering stamp (an upsert applies only above it); `changed_seq`
-- is the holder-cursor value of the transaction that last changed the
-- claim's visible state; a removal is a tombstone (`removed`), pruned after
-- 7 days. Freshness (`reported_at`) is gone — holdings never expire (L2).
ALTER TABLE frame_holders
    ADD COLUMN IF NOT EXISTS report_seq  bigint      NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS changed_seq bigint      NOT NULL DEFAULT 1,
    ADD COLUMN IF NOT EXISTS removed     boolean     NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS removed_at  timestamptz;
-- Holds nobody can count on migrate as tombstones (plan ruling P29): a
-- revoked device's, and a device whose account is no longer a member.
UPDATE frame_holders h SET removed = true, removed_at = now()
WHERE EXISTS (SELECT 1 FROM devices d WHERE d.id = h.device_id AND d.revoked_at IS NOT NULL)
   OR NOT EXISTS (
        SELECT 1 FROM devices d
        JOIN project_members pm ON pm.account_id = d.account_id AND pm.project_id = h.project_id
        WHERE d.id = h.device_id);
ALTER TABLE frame_holders ALTER COLUMN changed_seq DROP DEFAULT;
ALTER TABLE frame_holders ADD CONSTRAINT frame_holders_tombstone_stamp CHECK (removed = (removed_at IS NOT NULL));
ALTER TABLE frame_holders ADD CONSTRAINT frame_holders_content_version_positive CHECK (content_version >= 1);
DROP INDEX IF EXISTS frame_holders_reported;
ALTER TABLE frame_holders DROP COLUMN IF EXISTS reported_at;
CREATE INDEX IF NOT EXISTS frame_holders_changed ON frame_holders (project_id, changed_seq);
CREATE INDEX IF NOT EXISTS frame_holders_device_live ON frame_holders (device_id, project_id) WHERE NOT removed;
CREATE INDEX IF NOT EXISTS frame_holders_prunable ON frame_holders (removed_at) WHERE removed;

-- One cursor row per project; projects whose 0022 rows migrated start at 1
-- (every migrated row carries changed_seq = 1).
INSERT INTO project_holder_cursor (project_id, seq)
SELECT p.id, CASE WHEN EXISTS (SELECT 1 FROM frame_holders h WHERE h.project_id = p.id) THEN 1 ELSE 0 END
FROM projects p
ON CONFLICT (project_id) DO NOTHING;

-- The per-(device, project) claim digest (spec §6.3), updated in the same
-- transaction as every claim change. `digest` is the XOR of the first 16
-- bytes of blake3(frame_uuid ‖ content_version BE u32) over the device's
-- non-removed claims; `max_report_seq` is the highest report_seq stored for
-- the device in the project (hello's `reportSeq`, plan ruling P11). Not
-- backfilled here — Postgres has no blake3; a migrated device's first full
-- report re-digests it (plan ruling P10).
CREATE TABLE IF NOT EXISTS device_project_digest (
    project_id     uuid    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    device_id      uuid    NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    count          integer NOT NULL CHECK (count >= 0),
    digest         bytea   NOT NULL CHECK (length(digest) = 16),
    max_report_seq bigint  NOT NULL DEFAULT 0,
    PRIMARY KEY (project_id, device_id)
);
```

- [ ] **Step 5: Write the claims module**

`src/claims/mod.rs`:

```rust
//! Holder claims (collab v3 wave 3, spec 2026-09-25 §5–§6). A claim
//! `(device, frame, content_version)` is a durable fact written only by that
//! device (I4) — the hub writes the publisher's first claim itself, in the
//! announce/version transaction. Claims never expire with time; they end by
//! the device's report, device revocation/retirement, or the member leaving.
//! Every claim write runs under the project's holder-cursor lock
//! ([`HolderWrite`]) so `changed_seq` is gapless and in commit order (I2).

pub mod cursor;

pub use cursor::{DeviceDelta, HolderBump, HolderWrite};

/// A `frame_holders` row (aliased `h`) joined to its frame (`f`), its device
/// (`d`) and the device account's membership row (`pm`) is a VALID claim —
/// one that counts toward redundancy — when it is live, on the frame's
/// current content version, on a non-revoked device of a current member.
/// Only server-side aggregates use it (the portal coverage, profile
/// numbers); no device-facing count exists any more (spec §5.2).
pub(crate) const VALID_CLAIM_SQL: &str =
    "NOT h.removed AND h.content_version = f.content_version AND d.revoked_at IS NULL AND pm.account_id IS NOT NULL";
```

`src/claims/cursor.rs`:

```rust
//! The per-project holder cursor (spec 2026-09-25 §5.1, I2). Every
//! transaction that changes claims locks `project_holder_cursor` for its
//! project FIRST among claim rows ([`HolderWrite::lock`]), stamps every
//! visibly changed row with `changed_seq = seq + 1`, and bumps the cursor
//! once in [`HolderWrite::finish`] — so the committed `changed_seq` values
//! are always a gapless prefix, in commit order (plan ruling P2). A write
//! that changes only `report_seq` (an ordering pin) is not a visible change
//! and bumps nothing, which is what keeps an idle hub at zero cursor writes.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use sqlx::PgConnection;
use uuid::Uuid;

/// One device's net holder change: `add` maps `frame_seq` → the claimed
/// `content_version`, `rm` lists `frame_seq`s whose claim ended. A
/// `frame_seq` is in at most one of the two.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceDelta {
    pub add: BTreeMap<i32, i32>,
    pub rm: BTreeSet<i32>,
}

impl DeviceDelta {
    pub fn add(&mut self, frame_seq: i32, content_version: i32) {
        self.rm.remove(&frame_seq);
        self.add.insert(frame_seq, content_version);
    }

    pub fn rm(&mut self, frame_seq: i32) {
        self.add.remove(&frame_seq);
        self.rm.insert(frame_seq);
    }

    /// Fold a LATER delta into this one: per `frame_seq`, the later op wins.
    pub fn merge(&mut self, later: &DeviceDelta) {
        for (seq, cv) in &later.add {
            self.add(*seq, *cv);
        }
        for seq in &later.rm {
            self.rm(*seq);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.add.is_empty() && self.rm.is_empty()
    }
}

/// What one committed holder-writing transaction changed — the feed's
/// `holders` input (spec §4.3). `deltas` is keyed by the device's base64
/// pubkey (plan ruling P3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolderBump {
    pub project_id: Uuid,
    pub prev: i64,
    pub seq: i64,
    pub deltas: BTreeMap<String, DeviceDelta>,
}

/// A transaction's hold on one project's holder cursor.
pub struct HolderWrite {
    project_id: Uuid,
    base_seq: i64,
    floor: i64,
    changed: bool,
    deltas: BTreeMap<Uuid, DeviceDelta>,
}

impl HolderWrite {
    /// Lock the project's cursor row (lock #2 of spec §5.1 — after `projects`
    /// when the writer holds it, alone for a holder report). The row exists
    /// for every project: migration 0023 created it for existing projects and
    /// `projects::create_project_core` inserts it for new ones.
    pub async fn lock(conn: &mut PgConnection, project_id: Uuid) -> Result<Self, sqlx::Error> {
        let (base_seq, floor): (i64, i64) =
            sqlx::query_as("SELECT seq, floor FROM project_holder_cursor WHERE project_id = $1 FOR UPDATE")
                .bind(project_id)
                .fetch_one(conn)
                .await?;
        Ok(Self { project_id, base_seq, floor, changed: false, deltas: BTreeMap::new() })
    }

    pub fn project_id(&self) -> Uuid {
        self.project_id
    }

    pub fn base_seq(&self) -> i64 {
        self.base_seq
    }

    pub fn floor(&self) -> i64 {
        self.floor
    }

    /// The `changed_seq` every row this transaction visibly changes carries.
    pub fn seq(&self) -> i64 {
        self.base_seq + 1
    }

    pub fn record_add(&mut self, device_id: Uuid, frame_seq: i32, content_version: i32) {
        self.changed = true;
        self.deltas.entry(device_id).or_default().add(frame_seq, content_version);
    }

    pub fn record_rm(&mut self, device_id: Uuid, frame_seq: i32) {
        self.changed = true;
        self.deltas.entry(device_id).or_default().rm(frame_seq);
    }

    /// Mark the cursor as advanced without a delta. Only the Task 1 interim
    /// holder report uses it; Task 2 deletes both.
    pub fn touch(&mut self) {
        self.changed = true;
    }

    /// Bump the cursor when anything visibly changed and return the feed
    /// input; `None` (and no write) otherwise.
    pub async fn finish(self, conn: &mut PgConnection) -> Result<Option<HolderBump>, sqlx::Error> {
        if !self.changed {
            return Ok(None);
        }
        let seq = self.seq();
        sqlx::query("UPDATE project_holder_cursor SET seq = $2 WHERE project_id = $1")
            .bind(self.project_id)
            .bind(seq)
            .execute(&mut *conn)
            .await?;
        let ids: Vec<Uuid> = self.deltas.keys().copied().collect();
        let keys: Vec<(Uuid, Vec<u8>)> = sqlx::query_as("SELECT id, pubkey FROM devices WHERE id = ANY($1)")
            .bind(&ids)
            .fetch_all(&mut *conn)
            .await?;
        let by_id: HashMap<Uuid, String> =
            keys.into_iter().map(|(id, pk)| (id, crate::security::encode_pubkey(&pk))).collect();
        let mut deltas = BTreeMap::new();
        for (device_id, delta) in self.deltas {
            match by_id.get(&device_id) {
                Some(pubkey) => {
                    deltas.insert(pubkey.clone(), delta);
                }
                None => tracing::warn!(project_id = %self.project_id, device_id = %device_id, "holder delta for an unknown device dropped"),
            }
        }
        Ok(Some(HolderBump { project_id: self.project_id, prev: self.base_seq, seq, deltas }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn later_op_wins_per_frame_seq() {
        let mut a = DeviceDelta::default();
        a.add(1, 1);
        a.add(2, 1);
        let mut b = DeviceDelta::default();
        b.rm(1);
        b.add(3, 2);
        a.merge(&b);
        assert_eq!(a.add, BTreeMap::from([(2, 1), (3, 2)]));
        assert_eq!(a.rm, BTreeSet::from([1]));
        let mut c = DeviceDelta::default();
        c.add(1, 2);
        a.merge(&c);
        assert_eq!(a.add.get(&1), Some(&2));
        assert!(!a.rm.contains(&1));
    }
}
```

Add `pub mod claims;` to `src/lib.rs` (alphabetically, after `pub mod auth_mw;`).

- [ ] **Step 6: Announce reserves ordinals and writes claim columns**

In `src/routes/frames.rs::announce` (`:250-333`):
- Right after the `SELECT status, require_approval … FOR UPDATE` (projects lock, `:251-256`), take the cursor lock:

```rust
    // Lock order (module doc): projects (above) → the holder cursor → frame rows.
    let mut hw = crate::claims::HolderWrite::lock(&mut tx, id).await?;
```

- Replace the insert loop (`:294-315`) with:

```rust
    let version = bump_project_version_tx(&mut tx, id).await?;
    let (first_seq,): (i32,) = sqlx::query_as(
        "UPDATE projects SET next_frame_seq = next_frame_seq + $2 WHERE id = $1 RETURNING next_frame_seq - $2",
    )
    .bind(id)
    .bind(body.frames.len() as i32)
    .fetch_one(&mut *tx)
    .await?;
    for (i, f) in body.frames.iter().enumerate() {
        let frame_seq = first_seq + i as i32;
        sqlx::query(
            "INSERT INTO project_frames (project_id, frame_uuid, frame_seq, publisher, publisher_device_id, file_name, blake3, byte_size, xxh3, \
                 filter_raw, filter_canonical, channel, exptime_sec, date_obs, meta, gate_version, state, manifest_version) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)",
        )
        .bind(id).bind(f.frame_uuid).bind(frame_seq).bind(auth.account_id).bind(device_id).bind(&f.file_name).bind(&f.blake3)
        .bind(f.byte_size).bind(&f.xxh3).bind(f.filter_raw.trim()).bind(&f.filter_canonical).bind(&f.channel)
        .bind(f.exptime_sec).bind(f.date_obs).bind(&f.meta).bind(f.gate_version).bind(state_str).bind(version)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO project_frame_versions (project_id, frame_uuid, content_version, blake3, byte_size, xxh3) VALUES ($1,$2,1,$3,$4,$5)")
            .bind(id).bind(f.frame_uuid).bind(&f.blake3).bind(f.byte_size).bind(&f.xxh3)
            .execute(&mut *tx)
            .await?;
        // I4: the publisher's first claim is written by the hub, in this transaction.
        sqlx::query(
            "INSERT INTO frame_holders (project_id, frame_uuid, device_id, content_version, report_seq, changed_seq) VALUES ($1,$2,$3,1,0,$4)",
        )
        .bind(id).bind(f.frame_uuid).bind(device_id).bind(hw.seq())
        .execute(&mut *tx)
        .await?;
        hw.record_add(device_id, frame_seq, 1);
    }
```

- Before `tx.commit()` add `let _holders = hw.finish(&mut tx).await?; // published to the feed from Task 4`.

- [ ] **Step 7: Manifest rows, new version, reject**

`FrameRow` (`:352-376`): add `pub frame_seq: i32,` after `frame_uuid` and delete `pub holder_count: i64,`. `FrameView` (`:378-404`): the same (`pub frame_seq: i32,`, no `holder_count`). `into_view` (`:406-436`): add `frame_seq: self.frame_seq,` and delete `holder_count: self.holder_count,`.

Replace `frame_select_sql` and its doc comment (`:438-477`) with:

```rust
/// The columns + joins every frame read shares. `$1` project, `$2` viewer
/// account, `$3` viewer-is-moderator. Visibility: published rows to every
/// member, pending/rejected rows only to their publisher and moderators.
/// No holder count: redundancy is derived by clients from the holder map
/// (spec 2026-09-25 §5.2); server-side aggregates use
/// [`crate::claims::VALID_CLAIM_SQL`].
pub(crate) fn frame_select_sql(extra_where: &str, order_limit: &str) -> String {
    format!(
        "SELECT f.frame_uuid, f.frame_seq, f.publisher, {PUBLISHER_NAME_SQL} AS publisher_display_name, f.file_name, f.content_version, \
                f.blake3, f.byte_size, f.xxh3, f.filter_raw, f.filter_canonical, f.channel, f.exptime_sec, f.date_obs, f.meta, \
                f.gate_version, f.accepted, f.accepted_reason, f.state, f.reject_reason, f.manifest_version, f.created_at \
         FROM project_frames f \
         LEFT JOIN project_members pm ON pm.project_id = f.project_id AND pm.account_id = f.publisher \
         LEFT JOIN project_alumni al ON al.project_id = f.project_id AND al.account_id = f.publisher \
         WHERE f.project_id = $1 AND (f.state = 'published' OR f.publisher = $2 OR $3) {extra_where} {order_limit}"
    )
}
```

`new_version` (`:581-652`):
- Replace the doc comment's second sentence with "Older claims stay; they carry their own `content_version` and simply stop validating (spec 2026-09-25 §5.2)."
- After `lock_project_row(&mut tx, id).await?;` insert `let mut hw = crate::claims::HolderWrite::lock(&mut tx, id).await?;`.
- Select `f.frame_seq` too: `let row: Option<(Uuid, i32, String, i32)> = sqlx::query_as("SELECT f.publisher, f.content_version, p.status, f.frame_seq FROM …")` and destructure `(publisher, current, status, frame_seq)`.
- Replace the `DELETE FROM frame_holders …` plus `INSERT INTO frame_holders …` pair (`:631-637`) with:

```rust
    // I4: the publisher's claim on the new version, written by the hub. Other
    // devices' claims on older versions stay — they stop validating (§5.2).
    sqlx::query(
        "INSERT INTO frame_holders (project_id, frame_uuid, device_id, content_version, report_seq, changed_seq) \
         VALUES ($1, $2, $3, $4, 0, $5) \
         ON CONFLICT (project_id, frame_uuid, device_id) DO UPDATE SET \
             content_version = EXCLUDED.content_version, changed_seq = EXCLUDED.changed_seq, removed = false, removed_at = NULL",
    )
    .bind(id).bind(frame_uuid).bind(device_id).bind(next).bind(hw.seq())
    .execute(&mut *tx)
    .await?;
    hw.record_add(device_id, frame_seq, next);
```

- Before commit add `let _holders = hw.finish(&mut tx).await?;`.

`reject` (`:908-940`):
- Replace the foreign-holder query's `AND {FRESH}` with `AND NOT h.removed AND d.revoked_at IS NULL` (drop the `FRESH = …` format argument), and update its comment to "live claims (spec 2026-09-25 §5.1) on non-revoked devices".
- Delete the `DELETE FROM frame_holders …` statement (`:936-940`), and in the doc comment replace "fresh holder" with "live claim".

- [ ] **Step 8: Readers, the interim holder report, the cursor row for new projects**

`src/routes/holders.rs`:
- Delete `HOLDER_FRESH_SQL` and `fresh_holders_of_device_sql` (`:17-33`). Keep `HOLDER_ONLINE_SQL` with its doc; Task 11 removes it.
- Change the module doc (`:1-3`) to: "Who holds which frame (collab v3 wave 3, spec 2026-09-25 §5–§6): durable device-owned claims; relay URL is the only address that crosses accounts (rule S1)."
- `frame_holders` (`:250-258`): replace `AND {HOLDER_FRESH_SQL}` with `AND NOT h.removed`. It stays until Task 10 retires the route.
- In `put_holders_self`, replace the transaction body (`:127-190`) with the interim claim-column version below. Task 2 replaces the whole handler.

```rust
    let mut tx = state.db.begin().await?;
    // Task 1 interim: the wave-1 body mapped onto claim columns. Task 2
    // replaces this handler with the reportSeq/digest protocol.
    let mut hw = crate::claims::HolderWrite::lock(&mut tx, id).await?;
    let add_uuids: Vec<Uuid> = body.add.iter().map(|f| f.frame_uuid).collect();
    let add_versions: Vec<i32> = body.add.iter().map(|f| f.content_version).collect();
    let written = sqlx::query(
        "INSERT INTO frame_holders (project_id, frame_uuid, device_id, content_version, report_seq, changed_seq) \
         SELECT f.project_id, f.frame_uuid, $2, a.v, 0, $8 \
         FROM unnest($3::uuid[], $4::int[]) AS a(u, v) \
         JOIN project_frames f ON f.project_id = $1 AND f.frame_uuid = a.u AND a.v = f.content_version \
         WHERE (f.state = 'published' OR (f.state = 'pending' AND ($6 OR f.publisher = $7))) \
           AND ($5 OR f.publisher = $7) \
         ON CONFLICT (project_id, frame_uuid, device_id) \
         DO UPDATE SET content_version = EXCLUDED.content_version, changed_seq = EXCLUDED.changed_seq, removed = false, removed_at = NULL",
    )
    .bind(id).bind(device_id).bind(&add_uuids).bind(&add_versions).bind(any_frame).bind(moderator).bind(auth.account_id).bind(hw.seq())
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if written as usize != body.add.len() {
        tracing::warn!(project_id = %id, requested = body.add.len(), written, "holder report: some frames skipped (unknown or not holdable by this member)");
    }
    let tombstoned = if body.full {
        sqlx::query("UPDATE frame_holders SET removed = true, removed_at = now(), changed_seq = $4 WHERE project_id = $1 AND device_id = $2 AND NOT removed AND NOT (frame_uuid = ANY($3))")
            .bind(id).bind(device_id).bind(&add_uuids).bind(hw.seq())
            .execute(&mut *tx).await?.rows_affected()
    } else if !body.remove.is_empty() {
        sqlx::query("UPDATE frame_holders SET removed = true, removed_at = now(), changed_seq = $4 WHERE project_id = $1 AND device_id = $2 AND NOT removed AND frame_uuid = ANY($3)")
            .bind(id).bind(device_id).bind(&body.remove).bind(hw.seq())
            .execute(&mut *tx).await?.rows_affected()
    } else {
        0
    };
    if written + tombstoned > 0 {
        hw.touch();
    }
    let _holders = hw.finish(&mut tx).await?;
    tx.commit().await?;
```

`src/routes/projects.rs`:
- Line 18: `use crate::routes::holders::HOLDER_ONLINE_SQL;`.
- In the replication query (`:930-934`), replace `{HOLDER_FRESH_SQL}` inside the `FILTER` with `NOT h.removed`. The `LEFT JOIN frame_holders … AND h.content_version = f.content_version` stays.
- In the `online_holders` query (`:955-962`), replace `AND {HOLDER_FRESH_SQL}` with `AND NOT h.removed`.
- Rewrite the two comments above them (`:908-929`, `:945-954`): "fresh" → "live claim (spec 2026-09-25 §5.1: claims never expire; a removal is a tombstone)". Drop every "75 minutes" and "freshness window" sentence.
- In `create_project_core`, right after the `INSERT INTO project_members …` (`:386-396`), insert:

```rust
    // The holder cursor row every claim write locks (spec 2026-09-25 §5.1).
    sqlx::query("INSERT INTO project_holder_cursor (project_id) VALUES ($1)")
        .bind(project.id)
        .execute(&mut *tx)
        .await?;
```

`src/routes/profiles.rs`:
- Delete the `use crate::routes::holders::fresh_holders_of_device_sql;` line (`:26`).
- Replace `holding_count` (`:529-545`) with:

```rust
/// Distinct frames this account currently holds on their CURRENT content
/// version, across every project — live claims on non-revoked devices
/// (spec 2026-09-25 §5.1). A claim on a superseded version doesn't count.
async fn holding_count(db: &PgPool, account_id: Uuid) -> Result<i64, ApiError> {
    Ok(sqlx::query_scalar(
        "SELECT count(DISTINCT (h.project_id, h.frame_uuid)) FROM frame_holders h \
         JOIN devices d ON d.id = h.device_id \
         JOIN project_frames f ON f.project_id = h.project_id AND f.frame_uuid = h.frame_uuid \
             AND f.content_version = h.content_version \
         WHERE d.account_id = $1 AND d.revoked_at IS NULL AND NOT h.removed",
    )
    .bind(account_id)
    .fetch_one(db)
    .await?)
}
```

`src/collab_auth.rs:104-115` (`lock_project_row` doc): replace "`projects` → `project_members` → `project_frames`" with "`projects` → `project_holder_cursor` → `project_members` → frame rows (spec 2026-09-25 §5.1)".

`src/routes/frames.rs:1-29`: replace the module doc's lock-order paragraph with:

```rust
//! **Lock order (collab v3 wave 3, spec 2026-09-25 §5.1).** Every collab
//! writer that touches more than one of `projects`, `project_holder_cursor`,
//! `project_members`, `project_frames`, `frame_holders` inside one
//! transaction locks them in this fixed order:
//!
//! 1. `projects` — [`crate::collab_auth::lock_project_row`] (or the writer's
//!    own `SELECT … FOR UPDATE` on the row) right after `begin()`;
//! 2. `project_holder_cursor` for that project — [`crate::claims::HolderWrite::lock`],
//!    taken only by writers that change claims;
//! 3. `project_members` row(s) — [`crate::collab_auth::lock_member_rows`];
//! 4. frame rows: `project_frames`, then `frame_holders` (and
//!    `device_project_digest`, which only claim writers touch, always under
//!    the cursor lock).
//!
//! A holder report (`holders::put_holders_self`) locks ONLY the cursor row,
//! never `projects`, so a hundred devices' 1 s flushes never queue behind an
//! announce on the project row. A writer that spans several projects locks
//! every `projects` row in id order in one statement, then every cursor row
//! in id order. Locking `projects` first in every multi-table writer makes
//! them mutually exclusive on that one row, so no two can then acquire the
//! rest in conflicting orders. The deadlock this closed: `frames::approve`
//! with `trust: true` (bumps `projects`, then flips the publisher's
//! `project_members` row) racing `members::put_trust` on the same publisher,
//! which used to lock `project_members` first. The version bump
//! (`bump_project_version_tx`/`bump_membership_version_tx`) is a write to
//! the already-locked row, not a fresh lock acquisition.
//!
//! Writers and their locks:
//! - `announce`, `new_version`: projects → cursor → frames.
//! - `patch_frame`, `approve`, `reject`, `members::patch_member`, `handover`,
//!   `put_trust`, `operator::appoint_coordinator`: projects → members → frames.
//! - `dictionary::put_dictionary`, `thresholds::post_thresholds`,
//!   `projects::update_project`/`put_grid`: projects only.
//! - `holders::put_holders_self`: cursor only.
```

- [ ] **Step 9: Fix the tests the model change moves**

- `tests/holders.rs:237-300` (`publisher_refreshes_own_pending_hold…`): replace both `SELECT reported_at …` reads, and the `after > before` assertion, with one read after the PUT:

```rust
    let live: bool = sqlx::query_scalar(
        "SELECT NOT removed FROM frame_holders WHERE project_id = $1 AND frame_uuid = $2 AND device_id = $3",
    )
    .bind(project_uuid).bind(frame1).bind(anna_device_uuid)
    .fetch_one(&pool).await.unwrap();
    assert!(live, "the publisher's own pending-frame claim stays live");
```

- `tests/holders.rs:421`: the hand-insert becomes `INSERT INTO frame_holders (project_id, frame_uuid, device_id, content_version, report_seq, changed_seq) VALUES ($1, $2, $3, 1, 0, 1)`.
- `tests/holders.rs:484-560` (`ex_members_stray_holder_rows_never_count_anywhere`): delete the `holder_count_via_manifest` closure and every assertion on it. Keep the coverage assertions.
- `tests/frames.rs:169-186`:
  - Rename the test to `new_content_version_keeps_old_claims_and_only_the_publisher_holds_v2`.
  - Keep its holders-GET assertion. The GET filters to the current version, so it still sees 1.
  - Change its first line to `let (app, _m, coord, anna, id) = setup(pool.clone(), false).await;` (`setup` takes the pool by value).
  - Append this assertion:

```rust
    let old: i64 = sqlx::query_scalar("SELECT count(*) FROM frame_holders WHERE frame_uuid = $1 AND content_version = 1 AND NOT removed")
        .bind(uuid::Uuid::parse_str(uuid).unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(old, 1, "the coordinator's v1 claim survives (spec §5.2)");
```

- [ ] **Step 10: Run the whole suite**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test 2>&1 | tail -15`
Expected: every test binary `ok`, and `claims`/`collab_schema` pass. Then `cargo build --release 2>&1 | grep -c warning` → `0`.

- [ ] **Step 11: Commit**

```bash
git add Cargo.toml Cargo.lock migrations/0023_live_exchange.sql src/lib.rs src/claims src/routes/frames.rs src/routes/holders.rs \
        src/routes/projects.rs src/routes/profiles.rs src/collab_auth.rs tests/claims.rs tests/collab_schema.rs tests/holders.rs tests/frames.rs
git commit -m "feat(hub): claims model — frame ordinals, gapless holder cursor, claim columns and tombstones; lock order projects → cursor → members → frames" \
           -m "Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RYd9tanU4ehWLDXkPv159r"
```

---

### Task 2: Claims core — digest, report planner, `PUT holders/self` v3

**Files:**
- Create: `src/claims/digest.rs`, `src/claims/plan.rs`, `src/claims/store.rs`. Add `pub mod digest; pub mod plan; pub mod store;` to `src/claims/mod.rs`.
- Modify: `src/claims/cursor.rs` (delete `touch`).
- Modify: `src/routes/holders.rs:35-200` (new request/response types and handler), `src/routes/compat.rs` (add `outdated()`).
- Modify: `src/routes/mod.rs:256-259` (the holders/self route gets an 8 MB body limit).
- Modify: `src/routes/frames.rs` (`announce` and `new_version` write claims through `store::hub_claims_tx`).
- Test:
  - `tests/common/mod.rs` gains `digest_of` and `report_holders`.
  - `tests/claims.rs` gains the protocol tests.
  - Port every old-body call in `tests/holders.rs`, `tests/frames.rs`, `tests/claims.rs`, `tests/collab_flow.rs:70-79`, `tests/profiles.rs:535-543`. `tests/account_auth.rs:77-85` stays: a session still gets 400.

**Interfaces:**
- Consumes: `HolderWrite` and `DeviceDelta` (Task 1).
- Produces:
  ```rust
  // claims::digest
  pub const ZERO_HEX: &str = "00000000000000000000000000000000";
  pub fn claim_key(frame_uuid: &Uuid, content_version: i32) -> [u8; 16];
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)] pub struct Digest { pub count: i64, pub bytes: [u8; 16] }
  impl Digest { pub fn add(&mut self, u: &Uuid, cv: i32); pub fn remove(&mut self, u: &Uuid, cv: i32); pub fn to_hex(&self) -> String;
                pub fn parse(count: i64, digest_hex: &str) -> Option<Digest>; pub fn from_row(count: i32, bytes: &[u8]) -> Digest }
  // claims::plan (pure)
  pub struct StoredClaim { pub content_version: i32, pub report_seq: i64, pub removed: bool }
  pub struct FrameInfo { pub frame_seq: i32, pub content_version: i32, pub publisher: Uuid, pub state: String }
  pub struct Perms { pub account_id: Uuid, pub any_frame: bool, pub moderator: bool }
  pub struct Report { pub report_seq: i64, pub full: bool, pub add: Vec<(Uuid, i32)>, pub remove: Vec<Uuid> }
  pub struct ClaimWrite { pub frame_uuid: Uuid, pub content_version: i32, pub report_seq: i64, pub removed: bool, pub visible: bool }
  pub struct DigestDelta { pub add: Vec<(Uuid, i32)>, pub remove: Vec<(Uuid, i32)> }
  pub struct ReportPlan { pub writes: Vec<ClaimWrite>, pub refused: Vec<(Uuid, i32)>, pub digest: DigestDelta }
  pub fn holdable(perms: &Perms, frame: &FrameInfo) -> bool;
  pub fn plan_report(stored: &HashMap<Uuid, StoredClaim>, frames: &HashMap<Uuid, FrameInfo>, perms: &Perms, report: &Report) -> ReportPlan;
  pub fn plan_hub_claims(stored: &HashMap<Uuid, StoredClaim>, claims: &[(Uuid, i32)]) -> ReportPlan;
  pub fn plan_tombstone_all(stored: &HashMap<Uuid, StoredClaim>) -> ReportPlan;
  // claims::store
  pub async fn load_stored(conn: &mut PgConnection, project_id: Uuid, device_id: Uuid, only: Option<&[Uuid]>) -> Result<HashMap<Uuid, StoredClaim>, sqlx::Error>;
  pub async fn load_frames(conn: &mut PgConnection, project_id: Uuid, uuids: &[Uuid]) -> Result<HashMap<Uuid, FrameInfo>, sqlx::Error>;
  pub async fn apply(conn: &mut PgConnection, hw: &mut HolderWrite, device_id: Uuid, plan: &ReportPlan) -> Result<Digest, sqlx::Error>;
  pub async fn read_digest(conn: &mut PgConnection, project_id: Uuid, device_id: Uuid) -> Result<(Digest, i64), sqlx::Error>; // (digest, max_report_seq)
  pub async fn hub_claims_tx(conn: &mut PgConnection, hw: &mut HolderWrite, device_id: Uuid, claims: &[(Uuid, i32)]) -> Result<(), sqlx::Error>;
  pub async fn tombstone_device_tx(conn: &mut PgConnection, hw: &mut HolderWrite, device_id: Uuid) -> Result<usize, sqlx::Error>;
  // routes::holders
  pub const NEXT_FLUSH_MS: u64 = 1000;
  pub(crate) const MAX_REPORT_ITEMS: usize = 100_000;
  // routes::compat
  pub(crate) fn outdated() -> ApiError;   // 409 {"error":"collab_api_outdated"}
  ```
- Wire: `PUT /projects/{id}/holders/self` exactly as in § Wire contract.

- [ ] **Step 1: Write the digest module with its vectors (test first)**

`src/claims/digest.rs`:

```rust
//! The order-independent claim-set digest (spec 2026-09-25 §6.3). One key
//! per claim — the first 16 bytes of `blake3(frame_uuid.as_bytes() ‖
//! content_version as u32 big-endian)` — XOR-folded over the device's own
//! non-removed claims in one project, whatever version is current, plus a
//! count. It depends only on the device's report stream, so the hub and the
//! device maintain it from identical inputs; a hub version bump never
//! changes it; claims are unique per (device, frame), so the XOR never
//! cancels a duplicate. The byte layout is the cross-plan wire contract.

use uuid::Uuid;

pub const ZERO_HEX: &str = "00000000000000000000000000000000";

pub fn claim_key(frame_uuid: &Uuid, content_version: i32) -> [u8; 16] {
    let mut input = [0u8; 20];
    input[..16].copy_from_slice(frame_uuid.as_bytes());
    input[16..].copy_from_slice(&(content_version as u32).to_be_bytes());
    let hash = blake3::hash(&input);
    let mut key = [0u8; 16];
    key.copy_from_slice(&hash.as_bytes()[..16]);
    key
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Digest {
    pub count: i64,
    pub bytes: [u8; 16],
}

impl Digest {
    pub fn add(&mut self, frame_uuid: &Uuid, content_version: i32) {
        xor_into(&mut self.bytes, &claim_key(frame_uuid, content_version));
        self.count += 1;
    }

    pub fn remove(&mut self, frame_uuid: &Uuid, content_version: i32) {
        xor_into(&mut self.bytes, &claim_key(frame_uuid, content_version));
        self.count -= 1;
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.bytes)
    }

    /// A device-sent `(count, digest)`; `None` unless the digest is exactly
    /// 32 lowercase hex chars.
    pub fn parse(count: i64, digest_hex: &str) -> Option<Digest> {
        if digest_hex.len() != 32 || !digest_hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        let mut bytes = [0u8; 16];
        hex::decode_to_slice(digest_hex, &mut bytes).ok()?;
        Some(Digest { count, bytes })
    }

    /// A `device_project_digest` row. The column's CHECK pins 16 bytes; a
    /// different length is logged and reads as the empty digest, which makes
    /// the next digest check fail and the device send a full report.
    pub fn from_row(count: i32, bytes: &[u8]) -> Digest {
        match <[u8; 16]>::try_from(bytes) {
            Ok(b) => Digest { count: i64::from(count), bytes: b },
            Err(_) => {
                tracing::error!(count, len = bytes.len(), "claim digest row has a wrong length, treated as empty");
                Digest::default()
            }
        }
    }
}

fn xor_into(acc: &mut [u8; 16], key: &[u8; 16]) {
    for (a, k) in acc.iter_mut().zip(key) {
        *a ^= k;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(n: u32) -> Uuid {
        Uuid::parse_str(&format!("00000000-0000-4000-8000-{n:012x}")).unwrap()
    }

    /// The vectors of the plan's § Wire contract — the app pins the same.
    #[test]
    fn digest_matches_the_published_vectors() {
        assert_eq!(hex::encode(claim_key(&u(1), 1)), "129cef69895583884285d1404f99e181");
        assert_eq!(hex::encode(claim_key(&u(2), 1)), "c3ef44a7f50605a0dee0abca26ecf959");
        assert_eq!(hex::encode(claim_key(&u(3), 2)), "40fc3b9d29d73f9101f6293d9c3b5cc4");
        assert_eq!(hex::encode(claim_key(&u(3), 1)), "b8c7cf6630052129879bab6949f47d5b");
        let mut d = Digest::default();
        assert_eq!((d.count, d.to_hex().as_str()), (0, ZERO_HEX));
        d.add(&u(1), 1);
        d.add(&u(2), 1);
        assert_eq!((d.count, d.to_hex().as_str()), (2, "d173abce7c5386289c657a8a697518d8"));
        d.add(&u(3), 2);
        assert_eq!((d.count, d.to_hex().as_str()), (3, "918f90535584b9b99d9353b7f54e441c"));
        d.remove(&u(3), 2);
        d.add(&u(3), 1);
        assert_eq!((d.count, d.to_hex().as_str()), (3, "69b464a84c56a7011bfed1e320816583"));
        d.remove(&u(3), 1);
        d.remove(&u(2), 1);
        d.remove(&u(1), 1);
        assert_eq!(d, Digest::default(), "order-independent and self-inverse");
    }

    #[test]
    fn parse_accepts_only_lowercase_32_hex() {
        assert!(Digest::parse(0, ZERO_HEX).is_some());
        assert!(Digest::parse(0, "D173ABCE7C5386289C657A8A697518D8").is_none());
        assert!(Digest::parse(0, "d173").is_none());
        assert_eq!(Digest::parse(2, "d173abce7c5386289c657a8a697518d8").unwrap().count, 2);
    }
}
```

Run: `cargo test --lib claims::digest` → PASS. These are pure functions over pinned vectors, and the vectors were computed with blake3 1.8.5 before the plan was written.

- [ ] **Step 2: Write the planner tests**

`src/claims/plan.rs` (the tests block first; the implementation in Step 4):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn u(n: u32) -> Uuid {
        Uuid::parse_str(&format!("00000000-0000-4000-8000-{n:012x}")).unwrap()
    }
    fn me() -> Uuid {
        Uuid::parse_str("a0000000-0000-4000-8000-000000000001").unwrap()
    }
    fn other() -> Uuid {
        Uuid::parse_str("a0000000-0000-4000-8000-000000000002").unwrap()
    }
    fn frame(seq: i32, cv: i32, publisher: Uuid, state: &str) -> FrameInfo {
        FrameInfo { frame_seq: seq, content_version: cv, publisher, state: state.into() }
    }
    fn receiver() -> Perms {
        Perms { account_id: me(), any_frame: true, moderator: false }
    }
    fn live(cv: i32, rs: i64) -> StoredClaim {
        StoredClaim { content_version: cv, report_seq: rs, removed: false }
    }
    fn report(rs: i64, full: bool, add: &[(u32, i32)], remove: &[u32]) -> Report {
        Report { report_seq: rs, full, add: add.iter().map(|(n, v)| (u(*n), *v)).collect(), remove: remove.iter().map(|n| u(*n)).collect() }
    }

    #[test]
    fn a_report_older_than_the_stored_one_changes_nothing() {
        let stored = HashMap::from([(u(1), StoredClaim { content_version: 1, report_seq: 12, removed: true })]);
        let frames = HashMap::from([(u(1), frame(1, 1, other(), "published"))]);
        let plan = plan_report(&stored, &frames, &receiver(), &report(10, false, &[(1, 1)], &[]));
        assert!(plan.writes.is_empty() && plan.refused.is_empty());
    }

    #[test]
    fn remove_of_an_unknown_claim_pins_an_invisible_tombstone() {
        let frames = HashMap::from([(u(1), frame(1, 3, other(), "published"))]);
        let plan = plan_report(&HashMap::new(), &frames, &receiver(), &report(12, false, &[], &[1]));
        assert_eq!(plan.writes, vec![ClaimWrite { frame_uuid: u(1), content_version: 3, report_seq: 12, removed: true, visible: false }]);
        assert_eq!(plan.digest, DigestDelta::default());
    }

    #[test]
    fn full_tombstones_only_unlisted_claims_below_its_report_seq() {
        let stored = HashMap::from([(u(1), live(1, 5)), (u(2), live(1, 5)), (u(3), live(1, 9))]);
        let frames = HashMap::from([(u(1), frame(1, 1, other(), "published"))]);
        let plan = plan_report(&stored, &frames, &receiver(), &report(8, true, &[(1, 1)], &[]));
        assert_eq!(plan.writes, vec![
            ClaimWrite { frame_uuid: u(1), content_version: 1, report_seq: 8, removed: false, visible: false },
            ClaimWrite { frame_uuid: u(2), content_version: 1, report_seq: 8, removed: true, visible: true },
        ], "u3 (report_seq 9 ≥ 8) is a newer delta and stays");
        assert_eq!(plan.digest.remove, vec![(u(1), 1), (u(2), 1)]);
        assert_eq!(plan.digest.add, vec![(u(1), 1)]);
    }

    #[test]
    fn refusals_are_listed_and_a_live_refused_claim_is_tombstoned() {
        let stored = HashMap::from([(u(4), live(1, 1))]);
        let frames = HashMap::from([
            (u(2), frame(2, 1, other(), "published")),
            (u(3), frame(3, 1, other(), "pending")),
            (u(4), frame(4, 1, other(), "published")),
        ]);
        let send_only = Perms { account_id: me(), any_frame: false, moderator: false };
        let plan = plan_report(&stored, &frames, &send_only, &report(2, false, &[(1, 1), (2, 5), (3, 1), (4, 1)], &[]));
        assert_eq!(plan.refused, vec![(u(1), 1), (u(2), 5), (u(3), 1), (u(4), 1)],
            "unknown frame, version above current, pending without moderate, foreign frame for a send member");
        assert_eq!(plan.writes, vec![ClaimWrite { frame_uuid: u(4), content_version: 1, report_seq: 2, removed: true, visible: true }]);
    }

    #[test]
    fn a_publisher_may_claim_its_own_frame_in_any_state() {
        let frames = HashMap::from([(u(1), frame(1, 1, me(), "rejected")), (u(2), frame(2, 1, me(), "pending"))]);
        let send_only = Perms { account_id: me(), any_frame: false, moderator: false };
        let plan = plan_report(&HashMap::new(), &frames, &send_only, &report(1, false, &[(1, 1), (2, 1)], &[]));
        assert!(plan.refused.is_empty());
        assert_eq!(plan.writes.len(), 2);
    }

    #[test]
    fn claims_are_stored_whatever_version_is_current() {
        let stored = HashMap::from([(u(1), live(1, 3))]);
        let frames = HashMap::from([(u(1), frame(1, 2, other(), "published"))]);
        let plan = plan_report(&stored, &frames, &receiver(), &report(4, false, &[(1, 2)], &[]));
        assert_eq!(plan.writes, vec![ClaimWrite { frame_uuid: u(1), content_version: 2, report_seq: 4, removed: false, visible: true }]);
        assert_eq!(plan.digest, DigestDelta { add: vec![(u(1), 2)], remove: vec![(u(1), 1)] });
        // An older-version claim is also stored (P7).
        let plan = plan_report(&HashMap::new(), &frames, &receiver(), &report(5, false, &[(1, 1)], &[]));
        assert!(plan.refused.is_empty());
    }

    #[test]
    fn hub_claims_keep_the_stored_report_seq_and_tombstone_all_keeps_it_too() {
        let stored = HashMap::from([(u(1), StoredClaim { content_version: 1, report_seq: 7, removed: true })]);
        let plan = plan_hub_claims(&stored, &[(u(1), 2), (u(2), 1)]);
        assert_eq!(plan.writes, vec![
            ClaimWrite { frame_uuid: u(1), content_version: 2, report_seq: 7, removed: false, visible: true },
            ClaimWrite { frame_uuid: u(2), content_version: 1, report_seq: 0, removed: false, visible: true },
        ]);
        let stored = HashMap::from([(u(1), live(2, 7)), (u(2), StoredClaim { content_version: 1, report_seq: 3, removed: true })]);
        let plan = plan_tombstone_all(&stored);
        assert_eq!(plan.writes, vec![ClaimWrite { frame_uuid: u(1), content_version: 2, report_seq: 7, removed: true, visible: true }]);
        assert_eq!(plan.digest.remove, vec![(u(1), 2)]);
    }
}
```

- [ ] **Step 3: Write the failing integration tests**

Append to `tests/common/mod.rs`:

```rust
/// `(count, digestHex)` of a claim set, computed with the hub's own digest
/// code (the wire definition, § Wire contract).
pub fn digest_of(claims: &[(&str, i32)]) -> (i64, String) {
    let mut d = athenaeum_hub::claims::digest::Digest::default();
    for (u, v) in claims {
        d.add(&uuid::Uuid::parse_str(u).unwrap(), *v);
    }
    (d.count, d.to_hex())
}

/// `PUT /projects/{id}/holders/self` with the v3 body. `after` is the
/// device's WHOLE claim set in the project after this report (the digest it
/// sends).
pub async fn report_holders(
    app: &axum::Router,
    token: &str,
    project_id: &str,
    report_seq: i64,
    full: bool,
    add: &[(&str, i32)],
    remove: &[&str],
    after: &[(&str, i32)],
) -> (StatusCode, Value) {
    let (count, digest) = digest_of(after);
    let body = json!({
        "reportSeq": report_seq,
        "full": full,
        "add": add.iter().map(|(u, v)| json!({"uuid": u, "contentVersion": v})).collect::<Vec<_>>(),
        "remove": remove,
        "digest": digest,
        "count": count,
    });
    let (status, bytes) = send(app, put(&format!("/api/v1/projects/{project_id}/holders/self"), &body, Some(token))).await;
    (status, if bytes.is_empty() { Value::Null } else { as_json(&bytes) })
}
```

Append to `tests/claims.rs`:

```rust
async fn live_claim(pool: &PgPool, project: &str, device: Uuid, frame: &str) -> Option<(i32, i64, bool)> {
    sqlx::query_as("SELECT content_version, report_seq, removed FROM frame_holders WHERE project_id = $1 AND device_id = $2 AND frame_uuid = $3")
        .bind(Uuid::parse_str(project).unwrap())
        .bind(device)
        .bind(Uuid::parse_str(frame).unwrap())
        .fetch_optional(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn report_seq_orders_adds_and_removes(pool: PgPool) {
    let (app, _m, coord, id, devices) = project_with_members(&pool, 1).await;
    let (member, member_device) = devices[0].clone();
    assert_eq!(announce_frames(&app, &coord, &id, 1, 4).await.0, StatusCode::OK);
    let f1 = frame_uuid(1);
    let (seq0, _) = cursor(&pool, Uuid::parse_str(&id).unwrap()).await;

    // rs 12 removes a claim the hub never saw: an invisible tombstone pins the order (P8).
    let (status, body) = report_holders(&app, &member, &id, 12, false, &[], &[&f1], &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["digestMatch"], true);
    assert_eq!(body["holderSeq"], seq0, "an invisible tombstone does not move the cursor (P2)");
    assert_eq!(live_claim(&pool, &id, member_device, &f1).await, Some((1, 12, true)));

    // The delayed rs 10 add arrives after it: ignored (C19).
    let (_, body) = report_holders(&app, &member, &id, 10, false, &[(&f1, 1)], &[], &[(&f1, 1)]).await;
    assert_eq!(body["digestMatch"], false, "the hub kept the newer removal");
    assert_eq!(live_claim(&pool, &id, member_device, &f1).await.unwrap().2, true);

    // rs 13 adds it for real.
    let (_, body) = report_holders(&app, &member, &id, 13, false, &[(&f1, 1)], &[], &[(&f1, 1)]).await;
    assert_eq!(body["digestMatch"], true);
    assert_eq!(body["holderSeq"], seq0 + 1);
    assert_eq!(body["nextFlushMs"], 1000);
    assert_eq!(live_claim(&pool, &id, member_device, &f1).await, Some((1, 13, false)));
}

#[sqlx::test]
async fn full_report_replaces_claims_as_of_its_report_seq(pool: PgPool) {
    let (app, _m, coord, id, devices) = project_with_members(&pool, 1).await;
    let (member, member_device) = devices[0].clone();
    assert_eq!(announce_frames(&app, &coord, &id, 1, 4).await.0, StatusCode::OK);
    let (f1, f2, f3) = (frame_uuid(1), frame_uuid(2), frame_uuid(3));
    report_holders(&app, &member, &id, 5, false, &[(&f1, 1), (&f2, 1)], &[], &[(&f1, 1), (&f2, 1)]).await;
    report_holders(&app, &member, &id, 9, false, &[(&f3, 1)], &[], &[(&f1, 1), (&f2, 1), (&f3, 1)]).await;
    let (status, body) = report_holders(&app, &member, &id, 8, true, &[(&f1, 1)], &[], &[(&f1, 1)]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["digestMatch"], false, "f3 (rs 9) is newer than the full report (rs 8) and stays (P9)");
    assert_eq!(live_claim(&pool, &id, member_device, &f1).await, Some((1, 8, false)));
    assert_eq!(live_claim(&pool, &id, member_device, &f2).await.unwrap().2, true);
    assert_eq!(live_claim(&pool, &id, member_device, &f3).await, Some((1, 9, false)));
}

#[sqlx::test]
async fn refused_claims_are_tombstoned_and_excluded_from_the_digest_check(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, anna_device) = register_device(&app, &mailer, "anna@example.com", 2, "Laptop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap().to_string();
    join_and_approve(&app, &coord, &anna, &id, "Anna", "send").await;
    assert_eq!(announce_frames(&app, &coord, &id, 1, 2).await.0, StatusCode::OK);
    let f1 = frame_uuid(1);
    let unknown = frame_uuid(99);
    // Anna (send) claims the coordinator's frame and an unknown one: both refused.
    let (status, body) = report_holders(&app, &anna, &id, 1, false, &[(&f1, 1), (&unknown, 1)], &[], &[(&f1, 1), (&unknown, 1)]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut refused: Vec<String> = body["refused"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    refused.sort();
    assert_eq!(refused, vec![f1.clone(), unknown.clone()]);
    assert_eq!(body["digestMatch"], true, "refused keys are removed from the device's digest before comparing (P6)");
    assert!(live_claim(&pool, &id, Uuid::parse_str(&anna_device).unwrap(), &f1).await.is_none());
    // A version above current is refused too.
    let (_, body) = report_holders(&app, &coord, &id, 1, false, &[(&f1, 5)], &[], &[(&f1, 5)]).await;
    assert_eq!(body["refused"], json!([f1]));
}

#[sqlx::test]
async fn an_empty_report_is_a_lock_free_digest_check_that_writes_nothing(pool: PgPool) {
    let (app, _m, coord, id, _) = project_with_members(&pool, 0).await;
    assert_eq!(announce_frames(&app, &coord, &id, 1, 3).await.0, StatusCode::OK);
    let pid = Uuid::parse_str(&id).unwrap();
    let before = cursor(&pool, pid).await;
    let (f1, f2) = (frame_uuid(1), frame_uuid(2));
    let (status, body) = report_holders(&app, &coord, &id, 1, false, &[], &[], &[(&f1, 1), (&f2, 1)]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["digestMatch"], true, "announce claims are in the hub digest (I4)");
    assert_eq!(body["holderSeq"], before.0);
    let (_, body) = report_holders(&app, &coord, &id, 1, false, &[], &[], &[(&f1, 1)]).await;
    assert_eq!(body["digestMatch"], false);
    assert_eq!(cursor(&pool, pid).await, before, "an idle digest check never writes (spec §12 load check)");
}

#[sqlx::test]
async fn wave_two_body_is_collab_api_outdated_and_malformed_reports_are_400(pool: PgPool) {
    let (app, _m, coord, id, _) = project_with_members(&pool, 0).await;
    let (status, body) = send(&app, put(&format!("/api/v1/projects/{id}/holders/self"),
        &json!({"full": false, "add": [{"frameUuid": frame_uuid(1), "contentVersion": 1}], "remove": []}), Some(&coord))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(as_json(&body)["error"], "collab_api_outdated");
    let u = frame_uuid(1);
    let zero = athenaeum_hub::claims::digest::ZERO_HEX;
    for (body, needle) in [
        (json!({"reportSeq": 0, "digest": zero, "count": 0}), "reportSeq must be >= 1"),
        (json!({"reportSeq": 1, "add": [{"uuid": u, "contentVersion": 1}, {"uuid": u, "contentVersion": 1}], "digest": zero, "count": 0}), "duplicate uuid in add"),
        (json!({"reportSeq": 1, "remove": [u, u], "digest": zero, "count": 0}), "duplicate uuid in remove"),
        (json!({"reportSeq": 1, "add": [{"uuid": u, "contentVersion": 1}], "remove": [u], "digest": zero, "count": 0}), "present in both add and remove"),
        (json!({"reportSeq": 1, "full": true, "remove": [u], "digest": zero, "count": 0}), "remove must be empty when full is true"),
        (json!({"reportSeq": 1, "digest": "ABC", "count": 0}), "digest must be 32 lowercase hex chars"),
        (json!({"reportSeq": 1, "digest": zero, "count": -1}), "count must be >= 0"),
    ] {
        let (status, bytes) = send(&app, put(&format!("/api/v1/projects/{id}/holders/self"), &body, Some(&coord))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(String::from_utf8_lossy(&bytes).contains(needle), "{}", String::from_utf8_lossy(&bytes));
    }
}
```

In `tests/claims.rs::new_version_and_reject_keep_holder_rows`, replace the coordinator's old-body PUT with:

```rust
    let (status, _) = report_holders(&app, &coord, &id, 1, false, &[(&uuid, 1)], &[], &[(&uuid, 1)]).await;
    assert_eq!(status, StatusCode::OK);
```

Run: `DATABASE_URL=… cargo test --test claims` → FAIL (404 or 422 on the v3 body; `digest_of` does not exist).

- [ ] **Step 4: Implement the planner and the store**

`src/claims/plan.rs` (above its tests block):

```rust
//! The pure holder-report planner (spec 2026-09-25 §6.2–§6.3; plan rulings
//! P6–P9). Given the device's stored claims, the frames the report names
//! and the member's permissions, it decides every row write, the refusals,
//! the digest change and the feed delta — no I/O, so every ordering case is
//! a unit test.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::claims::digest::Digest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredClaim {
    pub content_version: i32,
    pub report_seq: i64,
    pub removed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameInfo {
    pub frame_seq: i32,
    pub content_version: i32,
    pub publisher: Uuid,
    pub state: String,
}

#[derive(Debug, Clone, Copy)]
pub struct Perms {
    pub account_id: Uuid,
    /// `send_receive` or `data.moderate`.
    pub any_frame: bool,
    /// `data.moderate`.
    pub moderator: bool,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub report_seq: i64,
    pub full: bool,
    pub add: Vec<(Uuid, i32)>,
    pub remove: Vec<Uuid>,
}

/// One row write. `visible` = the claim's `(content_version, removed)`
/// changes: the row takes the transaction's `changed_seq` and the feed
/// carries it. An invisible write only moves `report_seq` (an ordering pin).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimWrite {
    pub frame_uuid: Uuid,
    pub content_version: i32,
    pub report_seq: i64,
    pub removed: bool,
    pub visible: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DigestDelta {
    pub add: Vec<(Uuid, i32)>,
    pub remove: Vec<(Uuid, i32)>,
}

impl DigestDelta {
    pub fn apply(&self, digest: &mut Digest) {
        for (u, v) in &self.remove {
            digest.remove(u, *v);
        }
        for (u, v) in &self.add {
            digest.add(u, *v);
        }
    }

    fn record(&mut self, old: Option<&StoredClaim>, new: &ClaimWrite) {
        if let Some(o) = old.filter(|o| !o.removed) {
            self.remove.push((new.frame_uuid, o.content_version));
        }
        if !new.removed {
            self.add.push((new.frame_uuid, new.content_version));
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportPlan {
    pub writes: Vec<ClaimWrite>,
    pub refused: Vec<(Uuid, i32)>,
    pub digest: DigestDelta,
}

impl ReportPlan {
    fn push(&mut self, old: Option<&StoredClaim>, write: ClaimWrite) {
        self.digest.record(old, &write);
        self.writes.push(write);
    }
}

/// P6: a publisher may claim its own frame in any state; anyone else needs
/// `send_receive` or `data.moderate` for a published frame, and
/// `data.moderate` for a pending one.
pub fn holdable(perms: &Perms, frame: &FrameInfo) -> bool {
    frame.publisher == perms.account_id
        || (perms.any_frame && (frame.state == "published" || (frame.state == "pending" && perms.moderator)))
}

pub fn plan_report(
    stored: &HashMap<Uuid, StoredClaim>,
    frames: &HashMap<Uuid, FrameInfo>,
    perms: &Perms,
    report: &Report,
) -> ReportPlan {
    let rs = report.report_seq;
    let mut plan = ReportPlan::default();
    let mut listed = HashSet::with_capacity(report.add.len());
    for &(uuid, cv) in &report.add {
        listed.insert(uuid);
        let old = stored.get(&uuid);
        let valid = frames
            .get(&uuid)
            .is_some_and(|f| cv >= 1 && cv <= f.content_version && holdable(perms, f));
        if !valid {
            plan.refused.push((uuid, cv));
            // A refused frame is not held on either side: tombstone a live
            // hub claim the report is newer than (P6).
            if let Some(s) = old.filter(|s| !s.removed && s.report_seq < rs) {
                plan.push(old, ClaimWrite { frame_uuid: uuid, content_version: s.content_version, report_seq: rs, removed: true, visible: true });
            }
            continue;
        }
        match old {
            Some(s) if s.report_seq >= rs => {} // a newer report already ruled on this frame
            Some(s) => plan.push(old, ClaimWrite {
                frame_uuid: uuid,
                content_version: cv,
                report_seq: rs,
                removed: false,
                visible: s.removed || s.content_version != cv,
            }),
            None => plan.push(None, ClaimWrite { frame_uuid: uuid, content_version: cv, report_seq: rs, removed: false, visible: true }),
        }
    }
    for &uuid in &report.remove {
        let old = stored.get(&uuid);
        match old {
            Some(s) if s.report_seq >= rs => {}
            Some(s) => plan.push(old, ClaimWrite {
                frame_uuid: uuid,
                content_version: s.content_version,
                report_seq: rs,
                removed: true,
                visible: !s.removed,
            }),
            // P8: pin the order so a delayed older add cannot resurrect it.
            None => {
                if let Some(f) = frames.get(&uuid) {
                    plan.push(None, ClaimWrite { frame_uuid: uuid, content_version: f.content_version, report_seq: rs, removed: true, visible: false });
                }
            }
        }
    }
    if report.full {
        let mut unlisted: Vec<(&Uuid, &StoredClaim)> = stored
            .iter()
            .filter(|(u, s)| !listed.contains(*u) && !s.removed && s.report_seq < rs)
            .collect();
        unlisted.sort_by_key(|(u, _)| **u);
        for (uuid, s) in unlisted {
            plan.push(Some(s), ClaimWrite { frame_uuid: *uuid, content_version: s.content_version, report_seq: rs, removed: true, visible: true });
        }
    }
    plan
}

/// The hub's own claim for the publishing device (announce, version — I4):
/// always applied, keeping the stored `report_seq` (0 for a new row).
pub fn plan_hub_claims(stored: &HashMap<Uuid, StoredClaim>, claims: &[(Uuid, i32)]) -> ReportPlan {
    let mut plan = ReportPlan::default();
    for &(uuid, cv) in claims {
        let old = stored.get(&uuid);
        plan.push(old, ClaimWrite {
            frame_uuid: uuid,
            content_version: cv,
            report_seq: old.map_or(0, |s| s.report_seq),
            removed: false,
            visible: old.map_or(true, |s| s.removed || s.content_version != cv),
        });
    }
    plan
}

/// Every live claim ends (revocation, retirement, the member leaving — I4).
pub fn plan_tombstone_all(stored: &HashMap<Uuid, StoredClaim>) -> ReportPlan {
    let mut live: Vec<(&Uuid, &StoredClaim)> = stored.iter().filter(|(_, s)| !s.removed).collect();
    live.sort_by_key(|(u, _)| **u);
    let mut plan = ReportPlan::default();
    for (uuid, s) in live {
        plan.push(Some(s), ClaimWrite { frame_uuid: *uuid, content_version: s.content_version, report_seq: s.report_seq, removed: true, visible: true });
    }
    plan
}
```

`src/claims/store.rs`:

```rust
//! The claim writes behind the planner: one bulk upsert per plan, the digest
//! row updated in the same transaction (spec 2026-09-25 §6.3), the feed
//! delta recorded on the [`HolderWrite`]. Every caller holds the project's
//! holder-cursor lock, so no other transaction touches these rows meanwhile.

use std::collections::HashMap;

use sqlx::PgConnection;
use uuid::Uuid;

use crate::claims::digest::Digest;
use crate::claims::plan::{plan_hub_claims, plan_tombstone_all, FrameInfo, ReportPlan, StoredClaim};
use crate::claims::HolderWrite;

pub async fn load_stored(
    conn: &mut PgConnection,
    project_id: Uuid,
    device_id: Uuid,
    only: Option<&[Uuid]>,
) -> Result<HashMap<Uuid, StoredClaim>, sqlx::Error> {
    let rows: Vec<(Uuid, i32, i64, bool)> = sqlx::query_as(
        "SELECT frame_uuid, content_version, report_seq, removed FROM frame_holders \
         WHERE project_id = $1 AND device_id = $2 AND ($3::uuid[] IS NULL OR frame_uuid = ANY($3))",
    )
    .bind(project_id)
    .bind(device_id)
    .bind(only.map(|s| s.to_vec()))
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(u, cv, rs, removed)| (u, StoredClaim { content_version: cv, report_seq: rs, removed }))
        .collect())
}

pub async fn load_frames(
    conn: &mut PgConnection,
    project_id: Uuid,
    uuids: &[Uuid],
) -> Result<HashMap<Uuid, FrameInfo>, sqlx::Error> {
    let rows: Vec<(Uuid, i32, i32, Uuid, String)> = sqlx::query_as(
        "SELECT frame_uuid, frame_seq, content_version, publisher, state FROM project_frames \
         WHERE project_id = $1 AND frame_uuid = ANY($2)",
    )
    .bind(project_id)
    .bind(uuids)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(u, seq, cv, publisher, state)| (u, FrameInfo { frame_seq: seq, content_version: cv, publisher, state }))
        .collect())
}

pub async fn read_digest(
    conn: &mut PgConnection,
    project_id: Uuid,
    device_id: Uuid,
) -> Result<(Digest, i64), sqlx::Error> {
    let row: Option<(i32, Vec<u8>, i64)> = sqlx::query_as(
        "SELECT count, digest, max_report_seq FROM device_project_digest WHERE project_id = $1 AND device_id = $2",
    )
    .bind(project_id)
    .bind(device_id)
    .fetch_optional(conn)
    .await?;
    Ok(match row {
        Some((count, bytes, max_rs)) => (Digest::from_row(count, &bytes), max_rs),
        None => (Digest::default(), 0),
    })
}

/// Execute a plan: one bulk upsert, the feed delta, the digest row.
/// Returns the hub's digest for the device after the write.
pub async fn apply(
    conn: &mut PgConnection,
    hw: &mut HolderWrite,
    device_id: Uuid,
    plan: &ReportPlan,
) -> Result<Digest, sqlx::Error> {
    let project_id = hw.project_id();
    let (mut digest, max_rs) = read_digest(conn, project_id, device_id).await?;
    if plan.writes.is_empty() {
        return Ok(digest);
    }
    let uuids: Vec<Uuid> = plan.writes.iter().map(|w| w.frame_uuid).collect();
    let versions: Vec<i32> = plan.writes.iter().map(|w| w.content_version).collect();
    let report_seqs: Vec<i64> = plan.writes.iter().map(|w| w.report_seq).collect();
    let removed: Vec<bool> = plan.writes.iter().map(|w| w.removed).collect();
    let visible: Vec<bool> = plan.writes.iter().map(|w| w.visible).collect();
    sqlx::query(
        "INSERT INTO frame_holders (project_id, frame_uuid, device_id, content_version, report_seq, changed_seq, removed, removed_at) \
         SELECT $1, w.u, $2, w.v, w.rs, CASE WHEN w.vis THEN $3 ELSE 0 END, w.rm, CASE WHEN w.rm THEN now() END \
         FROM unnest($4::uuid[], $5::int[], $6::bigint[], $7::bool[], $8::bool[]) AS w(u, v, rs, rm, vis) \
         ON CONFLICT (project_id, frame_uuid, device_id) DO UPDATE SET \
             content_version = EXCLUDED.content_version, \
             report_seq      = EXCLUDED.report_seq, \
             removed         = EXCLUDED.removed, \
             removed_at      = CASE WHEN EXCLUDED.removed THEN COALESCE(frame_holders.removed_at, now()) ELSE NULL END, \
             changed_seq     = CASE WHEN EXCLUDED.changed_seq > 0 THEN EXCLUDED.changed_seq ELSE frame_holders.changed_seq END",
    )
    .bind(project_id)
    .bind(device_id)
    .bind(hw.seq())
    .bind(&uuids)
    .bind(&versions)
    .bind(&report_seqs)
    .bind(&removed)
    .bind(&visible)
    .execute(&mut *conn)
    .await?;
    let visible_uuids: Vec<Uuid> = plan.writes.iter().filter(|w| w.visible).map(|w| w.frame_uuid).collect();
    if !visible_uuids.is_empty() {
        let seqs: HashMap<Uuid, i32> =
            sqlx::query_as::<_, (Uuid, i32)>("SELECT frame_uuid, frame_seq FROM project_frames WHERE project_id = $1 AND frame_uuid = ANY($2)")
                .bind(project_id)
                .bind(&visible_uuids)
                .fetch_all(&mut *conn)
                .await?
                .into_iter()
                .collect();
        for w in plan.writes.iter().filter(|w| w.visible) {
            let Some(frame_seq) = seqs.get(&w.frame_uuid).copied() else {
                tracing::error!(project_id = %project_id, frame_uuid = %w.frame_uuid, "claim write for a frame without an ordinal");
                continue;
            };
            if w.removed {
                hw.record_rm(device_id, frame_seq);
            } else {
                hw.record_add(device_id, frame_seq, w.content_version);
            }
        }
    }
    plan.digest.apply(&mut digest);
    let new_max_rs = plan.writes.iter().map(|w| w.report_seq).max().unwrap_or(0).max(max_rs);
    sqlx::query(
        "INSERT INTO device_project_digest (project_id, device_id, count, digest, max_report_seq) VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (project_id, device_id) DO UPDATE SET count = EXCLUDED.count, digest = EXCLUDED.digest, max_report_seq = EXCLUDED.max_report_seq",
    )
    .bind(project_id)
    .bind(device_id)
    .bind(digest.count as i32)
    .bind(digest.bytes.as_slice())
    .bind(new_max_rs)
    .execute(&mut *conn)
    .await?;
    Ok(digest)
}

/// The hub-written claims of I4 (announce, version) for `device_id`.
pub async fn hub_claims_tx(
    conn: &mut PgConnection,
    hw: &mut HolderWrite,
    device_id: Uuid,
    claims: &[(Uuid, i32)],
) -> Result<(), sqlx::Error> {
    let uuids: Vec<Uuid> = claims.iter().map(|c| c.0).collect();
    let stored = load_stored(conn, hw.project_id(), device_id, Some(&uuids)).await?;
    apply(conn, hw, device_id, &plan_hub_claims(&stored, claims)).await?;
    Ok(())
}

/// Tombstone every live claim of `device_id` in the locked project.
pub async fn tombstone_device_tx(
    conn: &mut PgConnection,
    hw: &mut HolderWrite,
    device_id: Uuid,
) -> Result<usize, sqlx::Error> {
    let stored = load_stored(conn, hw.project_id(), device_id, None).await?;
    let plan = plan_tombstone_all(&stored);
    apply(conn, hw, device_id, &plan).await?;
    Ok(plan.writes.len())
}
```

`src/claims/mod.rs`: add `pub mod digest; pub mod plan; pub mod store;`. In `cursor.rs`, delete `touch()`.

- [ ] **Step 5: The v3 holder report handler**

`src/routes/compat.rs`: add the helper below and make `retired()` return `Err(outdated())`.

```rust
/// The one structured "update required" answer (collab v3 §11, wave 3 P25).
pub(crate) fn outdated() -> ApiError {
    ApiError::Message(StatusCode::CONFLICT, "collab_api_outdated".into())
}
```

`src/routes/holders.rs`: replace `MAX_DELTA`, `HeldFrame`, `HoldersSelf` and `put_holders_self` (`:35-200`) with:

```rust
/// P28: one full report of a large project must fit in one PUT.
pub(crate) const MAX_REPORT_ITEMS: usize = 100_000;
/// Spec §6.2: the outbox flushes once this has passed (the hub may raise it).
pub const NEXT_FLUSH_MS: u64 = 1000;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimIn {
    pub uuid: Uuid,
    pub content_version: i32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldersReport {
    pub report_seq: i64,
    #[serde(default)]
    pub full: bool,
    #[serde(default)]
    pub add: Vec<ClaimIn>,
    #[serde(default)]
    pub remove: Vec<Uuid>,
    pub digest: String,
    pub count: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldersReportResponse {
    pub holder_seq: i64,
    pub digest_match: bool,
    pub next_flush_ms: u64,
    pub refused: Vec<Uuid>,
}

fn validate_report(body: &HoldersReport) -> Result<(), ApiError> {
    if body.report_seq < 1 {
        return Err(ApiError::bad_request("reportSeq must be >= 1"));
    }
    if body.add.len() > MAX_REPORT_ITEMS || body.remove.len() > MAX_REPORT_ITEMS {
        return Err(ApiError::bad_request(format!("add/remove must each contain at most {MAX_REPORT_ITEMS} items")));
    }
    if body.count < 0 {
        return Err(ApiError::bad_request("count must be >= 0"));
    }
    if body.full && !body.remove.is_empty() {
        return Err(ApiError::bad_request("remove must be empty when full is true"));
    }
    let mut add_set = std::collections::HashSet::with_capacity(body.add.len());
    for c in &body.add {
        if !add_set.insert(c.uuid) {
            return Err(ApiError::bad_request(format!("duplicate uuid in add: {}", c.uuid)));
        }
    }
    let mut remove_set = std::collections::HashSet::with_capacity(body.remove.len());
    for u in &body.remove {
        if !remove_set.insert(*u) {
            return Err(ApiError::bad_request(format!("duplicate uuid in remove: {u}")));
        }
        if add_set.contains(u) {
            return Err(ApiError::bad_request(format!("uuid {u} present in both add and remove")));
        }
    }
    Ok(())
}

/// `PUT /api/v1/projects/{id}/holders/self` — the device's holder report
/// (spec 2026-09-25 §6.2–§6.3; plan rulings P6–P9, P28). Entries apply only
/// above the stored `report_seq`; `full` replaces claims as of `reportSeq`;
/// refused claims are listed and tombstoned; an empty report is a lock-free
/// digest check. Locks ONLY the project's holder cursor (spec §5.1).
#[tracing::instrument(skip_all, level = "debug")]
pub async fn put_holders_self(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthAccount>,
    Path(id): Path<Uuid>,
    Json(raw): Json<serde_json::Value>,
) -> Result<Json<HoldersReportResponse>, ApiError> {
    let Some(device_id) = auth.device_id else {
        return Err(ApiError::bad_request("a device token is required to report holdings"));
    };
    if raw.get("reportSeq").is_none() {
        return Err(crate::routes::compat::outdated()); // a wave-2 body (P25)
    }
    let body: HoldersReport = serde_json::from_value(raw)
        .map_err(|e| ApiError::bad_request(format!("invalid holder report: {e}")))?;
    validate_report(&body)?;
    let device_digest = Digest::parse(body.count, &body.digest)
        .ok_or_else(|| ApiError::bad_request("digest must be 32 lowercase hex chars"))?;
    let member = require_member(&state.db, id, auth.account_id).await?;
    let perms = Perms {
        account_id: auth.account_id,
        any_frame: member.data_role == "send_receive" || member.has_cap("data.moderate"),
        moderator: member.has_cap("data.moderate"),
    };

    if !body.full && body.add.is_empty() && body.remove.is_empty() {
        // The idle path: no lock, no write — one digest read, one cursor read.
        let mut conn = state.db.acquire().await?;
        let (hub, _) = store::read_digest(&mut conn, id, device_id).await?;
        let (holder_seq,): (i64,) = sqlx::query_as("SELECT seq FROM project_holder_cursor WHERE project_id = $1")
            .bind(id)
            .fetch_one(&mut *conn)
            .await?;
        return Ok(Json(HoldersReportResponse { holder_seq, digest_match: hub == device_digest, next_flush_ms: NEXT_FLUSH_MS, refused: vec![] }));
    }

    let report = Report {
        report_seq: body.report_seq,
        full: body.full,
        add: body.add.iter().map(|c| (c.uuid, c.content_version)).collect(),
        remove: body.remove.clone(),
    };
    let named: Vec<Uuid> = report.add.iter().map(|a| a.0).chain(report.remove.iter().copied()).collect();
    let mut tx = state.db.begin().await?;
    let mut hw = HolderWrite::lock(&mut tx, id).await?;
    let stored = store::load_stored(&mut tx, id, device_id, if report.full { None } else { Some(&named) }).await?;
    let frames = store::load_frames(&mut tx, id, &named).await?;
    let plan = plan::plan_report(&stored, &frames, &perms, &report);
    let hub = store::apply(&mut tx, &mut hw, device_id, &plan).await?;
    let base = hw.base_seq();
    let bump = hw.finish(&mut tx).await?;
    let holder_seq = bump.as_ref().map_or(base, |b| b.seq);
    tx.commit().await?; // Task 4 routes the bump through commit_and_publish

    let mut expected = device_digest;
    for (u, v) in &plan.refused {
        expected.remove(u, *v);
    }
    if !plan.refused.is_empty() {
        tracing::warn!(project_id = %id, device_id = %device_id, refused = plan.refused.len(), "holder report: claims refused");
    }
    let added = plan.writes.iter().filter(|w| w.visible && !w.removed).count();
    let removed = plan.writes.iter().filter(|w| w.visible && w.removed).count();
    tracing::debug!(project_id = %id, device_id = %device_id, added, removed, full = report.full, holder_seq, "holders reported");
    Ok(Json(HoldersReportResponse {
        holder_seq,
        digest_match: hub == expected,
        next_flush_ms: NEXT_FLUSH_MS,
        refused: plan.refused.iter().map(|r| r.0).collect(),
    }))
}
```

The imports at the top of `holders.rs` gain:

```rust
use crate::claims::digest::Digest;
use crate::claims::plan::{self, Perms, Report};
use crate::claims::{store, HolderWrite};
```

`src/routes/mod.rs:256-259`: the route becomes

```rust
        .route(
            "/api/v1/projects/{id}/holders/self",
            // A full report of a large project (P28: ≤ 100,000 claims) exceeds axum's 2 MB default.
            axum::routing::put(holders::put_holders_self).layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
```

`src/routes/frames.rs`:
- In `announce`, replace the raw `INSERT INTO frame_holders …` plus `hw.record_add(…)` inside the loop with nothing.
- After the loop add:

```rust
    let claims: Vec<(Uuid, i32)> = body.frames.iter().map(|f| (f.frame_uuid, 1)).collect();
    crate::claims::store::hub_claims_tx(&mut tx, &mut hw, device_id, &claims).await?;
```

- In `new_version`, replace the upsert plus `hw.record_add(…)` with `crate::claims::store::hub_claims_tx(&mut tx, &mut hw, device_id, &[(frame_uuid, next)]).await?;`. `frame_seq` is then unused in `new_version`: drop it from the `SELECT` again.

- [ ] **Step 6: Port the old-body calls in the existing tests**

Replace every `put(…/holders/self, json!({"full": …, "add": [{"frameUuid": U, "contentVersion": V}], "remove": R}), …)` with `report_holders(&app, TOKEN, ID, RS, FULL, &[(U, V)…], &[R…], AFTER)`:
- Use a per-token increasing `RS` (1, 2, 3…).
- Set `AFTER` to that device's whole claim set after the call. For a publishing device, that includes its announce claims `(frame_uuid(n), 1)`.
- Expect `StatusCode::OK` instead of `NO_CONTENT`.

The call sites:
- `tests/holders.rs`: `holder_delta_add_remove_and_full_resync` (3 calls), `send_members_hold_only_their_own_frames…` (Anna's claim: assert `body["refused"] == json!([uuid_n(1)])`), `holder_delta_refuses_duplicate_or_contradictory_frame_ids`, `publisher_refreshes_own_pending_hold…`, `holder_report_and_read_are_pinned_to_the_current_content_version`, `ex_members_stray_holder_rows_never_count_anywhere`.
- `tests/frames.rs`: the two calls in `new_content_version_keeps_old_claims…` and `reject_is_refused_once_a_stripped_delegate_still_holds_the_frame`.
- `tests/collab_flow.rs:70-79`.
- `tests/profiles.rs:535-543`.

Two assertions change meaning:
- **`holder_delta_refuses_duplicate_or_contradictory_frame_ids`.** Build the raw JSON bodies with `"uuid"` keys and `reportSeq`/`digest`/`count`. The expected messages become `duplicate uuid in add`, `present in both add and remove` and `remove must be empty when full is true`.
- **`holder_report_and_read_are_pinned_to_the_current_content_version`.** The coordinator's re-report of v1 after v2 is now **stored**, because claims are kept regardless of currency (P7). Assert status 200 with an empty `refused`. The per-frame holders GET still shows only the publisher, because it filters `h.content_version = f.content_version`. Keep that assertion, and drop the comment sentence "the API itself refuses to write one".

`tests/account_auth.rs:77-85` is unchanged: a session gets 400 before the body is inspected.

- [ ] **Step 7: Run the whole suite**

Run: `DATABASE_URL=… cargo test 2>&1 | tail -15 && cargo build --release 2>&1 | grep -c warning`
Expected: all `ok`; `0` warnings.

- [ ] **Step 8: Commit**

```bash
git add src/claims src/routes/holders.rs src/routes/compat.rs src/routes/mod.rs src/routes/frames.rs tests/common/mod.rs \
        tests/claims.rs tests/holders.rs tests/frames.rs tests/collab_flow.rs tests/profiles.rs
git commit -m "feat(hub): holder reports v3 — reportSeq ordering, full replace, refused claims, per-device claim digest (blake3/XOR)" \
           -m "Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RYd9tanU4ehWLDXkPv159r"
```

---

### Task 3: Feed core — clock, ordered coalescers, presence registry, `FeedHub`

This task is new code only: no route changes, and every file is unit-tested
without a database.

**Files:**
- Create: `src/feed/mod.rs`, `src/feed/clock.rs`, `src/feed/wire.rs`, `src/feed/coalesce.rs`, `src/feed/presence.rs`, `src/feed/hub.rs`. Add `pub mod feed;` to `src/lib.rs`.
- Modify: `src/routes/mod.rs:43-100`. `AppState` gains `pub feed: crate::feed::FeedHub` (`FeedHub::system()` in `new`) and `with_feed(FeedHub)`.

**Interfaces:**
- Consumes: `claims::{DeviceDelta, HolderBump}` (Task 1).
- Produces:
  ```rust
  // feed::clock
  pub trait Clock: Send + Sync + 'static { fn now(&self) -> Instant; }
  pub struct SystemClock;
  #[derive(Clone, Default)] pub struct ManualClock; impl ManualClock { pub fn new() -> Self; pub fn advance(&self, by: Duration); }
  // feed::wire — every wire struct of § Wire contract, plus
  pub const INLINE_FRAMES: usize = 50;
  pub enum Kind { Frames, Meta, Members, Thresholds, Dictionary, Grid }   // Ord in this order; serde lowercase
  pub enum AccountKind { Joined, Left }  pub enum ResyncWhat { Project, Holders }
  pub struct WireEvent { pub name: &'static str, pub data: Arc<str> }
  impl WireEvent { pub fn new<T: Serialize>(name: &'static str, value: &T) -> WireEvent; pub fn resync(project_id: Uuid, what: ResyncWhat) -> WireEvent }
  // feed::coalesce
  pub trait Mergeable { fn merge(&mut self, later: Self); }
  pub struct Run<T> { pub prev: i64, pub cur: i64, pub payload: T }
  pub enum Flush<T> { Emit(Run<T>), Resync }
  impl<T: Mergeable> Coalescer<T> { pub fn new(window: Duration, gap_wait: Duration) -> Self; pub fn offer(&mut self, run: Run<T>, now: Instant); pub fn poll(&mut self, now: Instant) -> Vec<Flush<T>> }
  // feed::presence
  pub struct PresenceConfig { pub grace, pub silence, pub flap_window, pub flap_count: usize, pub flap_delay, pub warmup }   // 10 s, 40 s, 5 min, 3, 60 s, 30 s
  #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)] pub struct SessionId(pub [u8; 16]); impl SessionId { pub fn random() -> Self; pub fn to_hex(&self) -> String; pub fn parse(s: &str) -> Option<Self> }  // + Display
  pub struct SessionGone;  pub enum Note { Online(Uuid), Offline(Uuid), Changed(Uuid) }
  impl Presence { pub fn tick(&mut self, now: Instant) -> Vec<SessionId>; /* expired sessions */ … }
  // feed::hub
  pub struct FeedConfig { pub project_window, pub holder_window, pub presence_window, pub gap_wait, pub channel_capacity: usize, pub presence: PresenceConfig }
  pub type HeadsMap = BTreeMap<Uuid, [i64; 2]>;
  pub enum Control { Close, Joined(Uuid), Left(Uuid) }
  pub struct StreamHandle { pub session: SessionId, pub mailbox: mpsc::Receiver<Control>, pub projects: Vec<(Uuid, broadcast::Receiver<Arc<WireEvent>>)>, pub heads: broadcast::Receiver<Arc<HeadsMap>> }
  pub struct ProjectPayload { pub kinds: BTreeSet<Kind>, pub frames: Vec<(Uuid, serde_json::Value)>, pub more: bool }
  pub struct ProjectBump { pub project_id: Uuid, pub prev: i64, pub version: i64, pub payload: ProjectPayload }
  pub struct HolderPayload(pub BTreeMap<String, DeviceDelta>);
  impl FeedHub {
      pub fn new(clock: Arc<dyn Clock>, cfg: FeedConfig) -> FeedHub;  pub fn system() -> FeedHub;
      pub fn epoch(&self) -> String;  pub fn set_epoch(&self, epoch: String);
      pub fn subscribe(&self, project_id: Uuid) -> broadcast::Receiver<Arc<WireEvent>>;
      pub fn open_stream(&self, device_id: Uuid, account_id: Uuid, pubkey: String, relay_url: Option<String>, projects: &[Uuid]) -> StreamHandle;
      pub fn detach(&self, session: SessionId);
      pub fn beat(&self, session: SessionId, serving: HashMap<Uuid, bool>, relay_url: Option<String>) -> Result<(), SessionGone>;
      pub fn leave(&self, session: SessionId) -> bool;
      pub fn close_device(&self, device_id: Uuid);  pub fn close_account(&self, account_id: Uuid);
      pub fn account_event(&self, account_id: Uuid, kind: AccountKind, project_id: Uuid);
      pub fn offer_project(&self, bump: ProjectBump);  pub fn offer_holders(&self, bump: HolderBump);
      pub fn broadcast_heads(&self, heads: Arc<HeadsMap>);
      pub fn presence_for(&self, project_id: Uuid) -> Vec<PresenceEntry>;   // [] during warm-up
      pub fn connected_devices(&self) -> HashSet<Uuid>;
      pub fn live_relay(&self, device_id: Uuid) -> Option<Option<String>>;  // Some(relay) while connected
      pub fn tick(&self);  pub fn shutdown(&self);
  }
  ```

- [ ] **Step 1: Clock and wire types**

`src/feed/mod.rs`:

```rust
//! The event channel core (collab v3 wave 3, spec 2026-09-25 §4): ordered
//! coalescing of committed changes, live presence and the per-project
//! broadcast every device stream subscribes to. The hub is the only
//! authority for presence and change propagation (L12).

pub mod clock;
pub mod coalesce;
pub mod hub;
pub mod presence;
pub mod wire;

pub use hub::{Control, FeedConfig, FeedHub, HeadsMap, HolderPayload, ProjectBump, ProjectPayload, StreamHandle};
```

`src/feed/clock.rs`:

```rust
//! The clock every presence and coalescer timer reads (plan ruling P24).
//! Production uses the monotonic system clock; tests advance a manual one
//! and call `FeedHub::tick()` — tokio's paused time is not used, because its
//! auto-advance fires the sqlx pool's acquire timeout while a test waits on
//! real database I/O.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> Instant;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[derive(Clone)]
pub struct ManualClock {
    base: Instant,
    offset: Arc<Mutex<Duration>>,
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl ManualClock {
    pub fn new() -> Self {
        Self { base: Instant::now(), offset: Arc::new(Mutex::new(Duration::ZERO)) }
    }

    pub fn advance(&self, by: Duration) {
        *self.offset.lock().unwrap_or_else(|p| p.into_inner()) += by;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.base + *self.offset.lock().unwrap_or_else(|p| p.into_inner())
    }
}
```

`src/feed/wire.rs`:

```rust
//! Wire shapes of the event channel (spec 2026-09-25 §4.3; the plan's
//! § Wire contract — a cross-plan contract with the app). Each event is
//! serialised ONCE and shared by every stream of the project as a
//! [`WireEvent`].

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use uuid::Uuid;

/// Frame rows inlined per `project` event (spec §4.3); more → `more: true`.
pub const INLINE_FRAMES: usize = 50;

/// What a `project` event changed. Declaration order is the wire order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Frames,
    Meta,
    Members,
    Thresholds,
    Dictionary,
    Grid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountKind {
    Joined,
    Left,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ResyncWhat {
    Project,
    Holders,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectEvent {
    pub project_id: Uuid,
    pub prev: i64,
    pub version: i64,
    pub kinds: Vec<Kind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frames: Option<Vec<serde_json::Value>>,
    pub more: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HolderDeltaWire {
    pub device: String,
    pub add: Vec<(i32, i32)>,
    pub rm: Vec<i32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldersEvent {
    pub project_id: Uuid,
    pub prev: i64,
    pub seq: i64,
    pub deltas: Vec<HolderDeltaWire>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceChange {
    pub device: String,
    pub connected: bool,
    pub serving: bool,
    pub relay_url: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceEvent {
    pub project_id: Uuid,
    pub replace: bool,
    pub changes: Vec<PresenceChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceEntry {
    pub device: String,
    pub serving: bool,
    pub relay_url: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountEvent {
    pub kind: AccountKind,
    pub project_id: Uuid,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResyncEvent {
    pub project_id: Uuid,
    pub what: ResyncWhat,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloProject {
    pub version: i64,
    pub holder_seq: i64,
    pub claim_count: i64,
    pub claim_digest: String,
    pub report_seq: i64,
    pub presence: Vec<PresenceEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloEvent {
    pub session_id: String,
    pub epoch: String,
    pub account_id: Uuid,
    pub projects: BTreeMap<Uuid, HelloProject>,
}

/// One pre-serialised SSE event: `event: <name>` + `data: <data>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireEvent {
    pub name: &'static str,
    pub data: Arc<str>,
}

impl WireEvent {
    pub fn new<T: Serialize>(name: &'static str, value: &T) -> WireEvent {
        let data = serde_json::to_string(value).unwrap_or_else(|err| {
            tracing::error!(event = name, error = %err, "feed event failed to serialize, sent empty");
            "{}".to_string()
        });
        WireEvent { name, data: Arc::from(data) }
    }

    pub fn resync(project_id: Uuid, what: ResyncWhat) -> WireEvent {
        WireEvent::new("resync", &ResyncEvent { project_id, what })
    }
}
```

- [ ] **Step 2: Coalescer — tests first**

`src/feed/coalesce.rs`:

```rust
//! Ordered coalescing per project and cursor (spec 2026-09-25 §4.3):
//! commits enter tagged `(prev, cur]`, only contiguous ranges leave, merged
//! over a window (250 ms for `project`, 1 s for `holders`). A gap — two
//! commits whose publishing tasks ran out of order — is held up to
//! `gap_wait` (2 s) and then becomes one `Resync`. After a hub start the
//! emitted cursor begins at the first event's `prev` (plan ruling P13).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub trait Mergeable {
    /// Fold a LATER, contiguous payload into this one.
    fn merge(&mut self, later: Self);
}

#[derive(Debug, Clone, PartialEq)]
pub struct Run<T> {
    pub prev: i64,
    pub cur: i64,
    pub payload: T,
}

#[derive(Debug, PartialEq)]
pub enum Flush<T> {
    Emit(Run<T>),
    Resync,
}

pub struct Coalescer<T> {
    window: Duration,
    gap_wait: Duration,
    emitted: Option<i64>,
    ready: Option<(Run<T>, Instant)>,
    pending: BTreeMap<i64, Run<T>>,
    gap_since: Option<Instant>,
}

impl<T: Mergeable> Coalescer<T> {
    pub fn new(window: Duration, gap_wait: Duration) -> Self {
        Self { window, gap_wait, emitted: None, ready: None, pending: BTreeMap::new(), gap_since: None }
    }

    fn tail(&self) -> Option<i64> {
        self.ready.as_ref().map(|(r, _)| r.cur).or(self.emitted)
    }

    fn append(&mut self, run: Run<T>, now: Instant) {
        match &mut self.ready {
            Some((r, _)) => {
                r.cur = run.cur;
                r.payload.merge(run.payload);
            }
            None => self.ready = Some((run, now)),
        }
    }

    pub fn offer(&mut self, run: Run<T>, now: Instant) {
        let tail = match self.tail() {
            Some(t) => t,
            None => {
                self.emitted = Some(run.prev);
                run.prev
            }
        };
        if run.cur <= tail {
            tracing::debug!(prev = run.prev, version = run.cur, tail, "feed event already covered, dropped");
            return;
        }
        if run.prev < tail {
            tracing::warn!(prev = run.prev, version = run.cur, tail, "feed event overlaps the emitted range, dropped");
            return;
        }
        if run.prev > tail {
            self.pending.insert(run.prev, run);
            self.gap_since.get_or_insert(now);
            return;
        }
        self.append(run, now);
        while let Some(next) = self.tail().and_then(|t| self.pending.remove(&t)) {
            self.append(next, now);
        }
        if self.pending.is_empty() {
            self.gap_since = None;
        }
    }

    pub fn poll(&mut self, now: Instant) -> Vec<Flush<T>> {
        let mut out = Vec::new();
        if let Some(since) = self.gap_since {
            if now.duration_since(since) >= self.gap_wait {
                if let Some((r, _)) = self.ready.take() {
                    self.emitted = Some(r.cur);
                    out.push(Flush::Emit(r));
                }
                if let Some(max) = self.pending.values().map(|r| r.cur).max() {
                    self.emitted = Some(max);
                }
                self.pending.clear();
                self.gap_since = None;
                out.push(Flush::Resync);
                return out;
            }
        }
        if self.ready.as_ref().is_some_and(|(_, first)| now.duration_since(*first) >= self.window) {
            if let Some((r, _)) = self.ready.take() {
                self.emitted = Some(r.cur);
                out.push(Flush::Emit(r));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Default)]
    struct Tags(Vec<i64>);
    impl Mergeable for Tags {
        fn merge(&mut self, later: Self) {
            self.0.extend(later.0);
        }
    }
    fn run(prev: i64, cur: i64) -> Run<Tags> {
        Run { prev, cur, payload: Tags(vec![cur]) }
    }
    fn emit(prev: i64, cur: i64, tags: &[i64]) -> Flush<Tags> {
        Flush::Emit(Run { prev, cur, payload: Tags(tags.to_vec()) })
    }
    const W: Duration = Duration::from_millis(250);
    const G: Duration = Duration::from_secs(2);
    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn contiguous_commits_merge_within_the_window() {
        let t0 = Instant::now();
        let mut c = Coalescer::new(W, G);
        c.offer(run(4, 5), t0);
        c.offer(run(5, 6), t0 + ms(100));
        assert!(c.poll(t0 + ms(200)).is_empty());
        assert_eq!(c.poll(t0 + ms(250)), vec![emit(4, 6, &[5, 6])]);
        c.offer(run(6, 7), t0 + ms(300));
        assert_eq!(c.poll(t0 + ms(550)), vec![emit(6, 7, &[7])]);
    }

    #[test]
    fn an_out_of_order_commit_waits_for_its_predecessor() {
        let t0 = Instant::now();
        let mut c = Coalescer::new(W, G);
        c.offer(run(4, 5), t0);
        c.offer(run(6, 7), t0 + ms(10));
        c.offer(run(5, 6), t0 + ms(20));
        assert_eq!(c.poll(t0 + ms(250)), vec![emit(4, 7, &[5, 6, 7])]);
    }

    #[test]
    fn a_gap_older_than_two_seconds_becomes_one_resync() {
        let t0 = Instant::now();
        let mut c = Coalescer::new(W, G);
        c.offer(run(4, 5), t0);
        c.offer(run(6, 7), t0 + ms(10));
        assert_eq!(c.poll(t0 + ms(250)), vec![emit(4, 5, &[5])], "the contiguous part still flows");
        assert!(c.poll(t0 + ms(1000)).is_empty());
        assert_eq!(c.poll(t0 + ms(2010)), vec![Flush::Resync]);
        c.offer(run(5, 6), t0 + ms(3000));
        assert!(c.poll(t0 + ms(4000)).is_empty(), "the late commit is covered by the resync");
        c.offer(run(7, 8), t0 + ms(4000));
        assert_eq!(c.poll(t0 + ms(4250)), vec![emit(7, 8, &[8])]);
    }

    #[test]
    fn a_duplicate_publish_is_dropped() {
        let t0 = Instant::now();
        let mut c = Coalescer::new(W, G);
        c.offer(run(4, 5), t0);
        c.offer(run(4, 5), t0);
        assert_eq!(c.poll(t0 + W), vec![emit(4, 5, &[5])]);
    }
}
```

Run: `cargo test --lib feed::coalesce` → FAIL (module not declared), then PASS once `feed/mod.rs` exists. Write this file before `hub.rs`, so the module tree compiles with `hub.rs` still missing: comment out `pub mod hub;` and the `pub use` until Step 4.

- [ ] **Step 3: Presence registry — tests first**

`src/feed/presence.rs`:

```rust
//! Live presence (spec 2026-09-25 §4.2; plan rulings P14–P16). The hub's
//! monotonic clock drives every timeout; no device timestamp orders
//! anything. Facets: connected (this registry), serving (self-reported per
//! project, carried by the beat), reachable (never asserted — the fetcher
//! dials). Everyone starts offline after a hub start.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::time::{Duration, Instant};

use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct PresenceConfig {
    pub grace: Duration,
    pub silence: Duration,
    pub flap_window: Duration,
    pub flap_count: usize,
    pub flap_delay: Duration,
    pub warmup: Duration,
}

impl Default for PresenceConfig {
    fn default() -> Self {
        Self {
            grace: Duration::from_secs(10),
            silence: Duration::from_secs(40),
            flap_window: Duration::from_secs(300),
            flap_count: 3,
            flap_delay: Duration::from_secs(60),
            warmup: Duration::from_secs(30),
        }
    }
}

/// A stream session: 128 random bits, 32 lowercase hex chars on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(pub [u8; 16]);

impl SessionId {
    pub fn random() -> Self {
        SessionId(rand::random())
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn parse(s: &str) -> Option<Self> {
        if s.len() != 32 || !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        let mut bytes = [0u8; 16];
        hex::decode_to_slice(s, &mut bytes).ok()?;
        Some(SessionId(bytes))
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionGone;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Note {
    Online(Uuid),
    Offline(Uuid),
    Changed(Uuid),
}

impl Note {
    pub fn device_id(&self) -> Uuid {
        match self {
            Note::Online(d) | Note::Offline(d) | Note::Changed(d) => *d,
        }
    }
}

/// What the rest of the hub sees of a device. It outlives sessions: a
/// replacing session inherits `serving`/`relay_url` (P14).
#[derive(Debug, Clone, Default)]
pub struct Track {
    pub account_id: Uuid,
    pub pubkey: String,
    pub visible: bool,
    pub serving: HashMap<Uuid, bool>,
    pub relay_url: Option<String>,
    onlines: VecDeque<Instant>,
    offline_due: Option<Instant>,
}

struct Session {
    device_id: Uuid,
    last_beat: Instant,
    detached_at: Option<Instant>,
}

pub struct Presence {
    cfg: PresenceConfig,
    started: Instant,
    sessions: HashMap<SessionId, Session>,
    by_device: HashMap<Uuid, SessionId>,
    tracks: HashMap<Uuid, Track>,
    notes: Vec<Note>,
}

fn is_expired(s: &Session, now: Instant, cfg: &PresenceConfig) -> bool {
    if now.duration_since(s.last_beat) >= cfg.silence {
        return true;
    }
    matches!(s.detached_at, Some(d) if s.last_beat <= d && now.duration_since(d) >= cfg.grace)
}

impl Presence {
    pub fn new(cfg: PresenceConfig, now: Instant) -> Self {
        Self { cfg, started: now, sessions: HashMap::new(), by_device: HashMap::new(), tracks: HashMap::new(), notes: Vec::new() }
    }

    /// Spec §4.2: no presence diffs for the first 30 s after a hub start.
    pub fn warm(&self, now: Instant) -> bool {
        now.duration_since(self.started) >= self.cfg.warmup
    }

    /// A stream opened (counts as a beat). A second stream of the device
    /// replaces the first; the replaced session id is returned so its stream
    /// can be closed.
    pub fn open(&mut self, device_id: Uuid, account_id: Uuid, pubkey: String, relay_url: Option<String>, now: Instant) -> (SessionId, Option<SessionId>) {
        let replaced = self.by_device.remove(&device_id);
        if let Some(old) = replaced {
            self.sessions.remove(&old);
        }
        let id = SessionId::random();
        self.sessions.insert(id, Session { device_id, last_beat: now, detached_at: None });
        self.by_device.insert(device_id, id);
        let track = self.tracks.entry(device_id).or_default();
        track.account_id = account_id;
        track.pubkey = pubkey;
        if !track.visible {
            track.relay_url = relay_url; // the stored endpoint until the first beat
        }
        if track.offline_due.take().is_some() {
            // Back inside the damping delay: others never saw it leave (P15).
        } else if !track.visible {
            track.visible = true;
            track.onlines.push_back(now);
            self.notes.push(Note::Online(device_id));
        }
        (id, replaced)
    }

    pub fn beat(&mut self, session: SessionId, serving: HashMap<Uuid, bool>, relay_url: Option<String>, now: Instant) -> Result<(), SessionGone> {
        let s = self.sessions.get_mut(&session).ok_or(SessionGone)?;
        s.last_beat = now;
        let device_id = s.device_id;
        let track = self.tracks.get_mut(&device_id).ok_or(SessionGone)?;
        if track.serving != serving || track.relay_url != relay_url {
            track.serving = serving;
            track.relay_url = relay_url;
            if track.visible {
                self.notes.push(Note::Changed(device_id));
            }
        }
        Ok(())
    }

    /// The stream closed: the 10 s grace starts (P14).
    pub fn detach(&mut self, session: SessionId, now: Instant) {
        if let Some(s) = self.sessions.get_mut(&session) {
            s.detached_at.get_or_insert(now);
        }
    }

    /// Clean exit (`DELETE /me/presence`): offline at once, never damped.
    pub fn leave(&mut self, session: SessionId, now: Instant) -> Option<Uuid> {
        let s = self.sessions.remove(&session)?;
        self.by_device.remove(&s.device_id);
        self.offline(s.device_id, now, true);
        Some(s.device_id)
    }

    /// Revocation or block: offline at once, never damped.
    pub fn kick(&mut self, device_id: Uuid, now: Instant) -> Option<SessionId> {
        let session = self.by_device.remove(&device_id);
        if let Some(s) = session {
            self.sessions.remove(&s);
        }
        self.offline(device_id, now, true);
        session
    }

    /// Expire sessions and fire due damped offlines. Returns the expired
    /// sessions so the hub closes their streams (a silent device's stream
    /// must not linger on an orphaned session).
    pub fn tick(&mut self, now: Instant) -> Vec<SessionId> {
        let expired: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, s)| is_expired(s, now, &self.cfg))
            .map(|(id, _)| *id)
            .collect();
        for id in &expired {
            if let Some(s) = self.sessions.remove(id) {
                self.by_device.remove(&s.device_id);
                self.offline(s.device_id, now, false);
            }
        }
        let due: Vec<Uuid> = self
            .tracks
            .iter()
            .filter(|(_, t)| t.offline_due.is_some_and(|d| now >= d))
            .map(|(id, _)| *id)
            .collect();
        for device_id in due {
            let reconnected = self.by_device.contains_key(&device_id);
            if let Some(t) = self.tracks.get_mut(&device_id) {
                t.offline_due = None;
                if t.visible && !reconnected {
                    t.visible = false;
                    self.notes.push(Note::Offline(device_id));
                }
            }
        }
        expired
    }

    fn offline(&mut self, device_id: Uuid, now: Instant, immediate: bool) {
        let flap_window = self.cfg.flap_window;
        let flap_count = self.cfg.flap_count;
        let flap_delay = self.cfg.flap_delay;
        let Some(t) = self.tracks.get_mut(&device_id) else { return };
        if !t.visible {
            return;
        }
        while t.onlines.front().is_some_and(|at| now.duration_since(*at) > flap_window) {
            t.onlines.pop_front();
        }
        if !immediate && t.onlines.len() >= flap_count {
            t.offline_due.get_or_insert(now + flap_delay);
            return;
        }
        t.visible = false;
        t.offline_due = None;
        self.notes.push(Note::Offline(device_id));
    }

    pub fn drain(&mut self) -> Vec<Note> {
        std::mem::take(&mut self.notes)
    }

    pub fn track(&self, device_id: Uuid) -> Option<&Track> {
        self.tracks.get(&device_id)
    }

    pub fn visible_devices(&self) -> impl Iterator<Item = (&Uuid, &Track)> {
        self.tracks.iter().filter(|(_, t)| t.visible)
    }

    /// `(session, device)` for every live session of the account.
    pub fn live_sessions_of(&self, account_id: Uuid) -> Vec<(SessionId, Uuid)> {
        self.by_device
            .iter()
            .filter(|(device, _)| self.tracks.get(device).is_some_and(|t| t.account_id == account_id))
            .map(|(device, session)| (*session, *device))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }
    fn setup() -> (Presence, Instant, Uuid, Uuid) {
        let t0 = Instant::now();
        (Presence::new(PresenceConfig::default(), t0), t0, Uuid::from_u128(1), Uuid::from_u128(100))
    }

    #[test]
    fn a_closed_stream_goes_offline_after_the_grace() {
        let (mut p, t0, dev, acct) = setup();
        let (sid, _) = p.open(dev, acct, "pk".into(), None, t0);
        assert_eq!(p.drain(), vec![Note::Online(dev)]);
        p.detach(sid, t0 + s(1));
        p.tick(t0 + s(10));
        assert!(p.drain().is_empty(), "inside the 10 s grace");
        p.tick(t0 + s(11));
        assert_eq!(p.drain(), vec![Note::Offline(dev)]);
    }

    #[test]
    fn a_beat_during_the_grace_keeps_the_session_on_the_silence_rule() {
        let (mut p, t0, dev, acct) = setup();
        let (sid, _) = p.open(dev, acct, "pk".into(), None, t0);
        p.drain();
        p.detach(sid, t0 + s(1));
        p.beat(sid, HashMap::new(), None, t0 + s(5)).unwrap();
        p.tick(t0 + s(12));
        assert!(p.drain().is_empty());
        p.tick(t0 + s(45));
        assert_eq!(p.drain(), vec![Note::Offline(dev)], "40 s after the last beat");
    }

    #[test]
    fn beat_silence_of_40s_is_offline_with_the_stream_open() {
        let (mut p, t0, dev, acct) = setup();
        let (sid, _) = p.open(dev, acct, "pk".into(), None, t0);
        p.drain();
        p.beat(sid, HashMap::new(), None, t0 + s(15)).unwrap();
        p.tick(t0 + s(54));
        assert!(p.drain().is_empty());
        p.tick(t0 + s(55));
        assert_eq!(p.drain(), vec![Note::Offline(dev)]);
        assert_eq!(p.beat(sid, HashMap::new(), None, t0 + s(56)), Err(SessionGone));
    }

    #[test]
    fn a_second_stream_replaces_the_first_and_inherits_serving() {
        let (mut p, t0, dev, acct) = setup();
        let project = Uuid::from_u128(7);
        let (s1, _) = p.open(dev, acct, "pk".into(), None, t0);
        p.beat(s1, HashMap::from([(project, true)]), Some("https://r".into()), t0 + s(1)).unwrap();
        let (s2, replaced) = p.open(dev, acct, "pk".into(), None, t0 + s(2));
        assert_eq!(replaced, Some(s1));
        assert_eq!(p.beat(s1, HashMap::new(), None, t0 + s(3)), Err(SessionGone));
        assert_ne!(s1, s2);
        let t = p.track(dev).unwrap();
        assert_eq!(t.serving.get(&project), Some(&true));
        assert_eq!(t.relay_url.as_deref(), Some("https://r"), "the live relay survives, not the stored one");
        assert_eq!(p.drain(), vec![Note::Online(dev), Note::Changed(dev)], "no second Online");
    }

    #[test]
    fn flap_damping_delays_the_third_offline_by_60s() {
        let (mut p, t0, dev, acct) = setup();
        let mut t = t0;
        let mut notes = Vec::new();
        for _ in 0..3 {
            let (sid, _) = p.open(dev, acct, "pk".into(), None, t);
            p.detach(sid, t + s(1));
            p.tick(t + s(11));
            notes.extend(p.drain());
            t += s(20);
        }
        assert_eq!(notes, vec![Note::Online(dev), Note::Offline(dev), Note::Online(dev), Note::Offline(dev), Note::Online(dev)],
            "the third drop (3 onlines in 5 min) is damped");
        let third_drop = t0 + s(40 + 11);
        p.tick(third_drop + s(59));
        assert!(p.drain().is_empty());
        p.tick(third_drop + s(60));
        assert_eq!(p.drain(), vec![Note::Offline(dev)]);
    }

    #[test]
    fn a_return_inside_the_damping_delay_emits_nothing() {
        let (mut p, t0, dev, acct) = setup();
        let mut t = t0;
        for _ in 0..3 {
            let (sid, _) = p.open(dev, acct, "pk".into(), None, t);
            p.detach(sid, t + s(1));
            p.tick(t + s(11));
            t += s(20);
        }
        p.drain();
        p.open(dev, acct, "pk".into(), None, t0 + s(70));
        p.tick(t0 + s(200));
        assert!(p.drain().is_empty(), "never seen leaving, so no Online either");
        assert!(p.track(dev).unwrap().visible);
    }

    #[test]
    fn leave_and_kick_are_immediate_and_never_damped() {
        let (mut p, t0, dev, acct) = setup();
        let mut t = t0;
        for _ in 0..3 {
            let (sid, _) = p.open(dev, acct, "pk".into(), None, t);
            p.detach(sid, t + s(1));
            p.tick(t + s(11));
            t += s(20);
        }
        p.drain();
        let (sid, _) = p.open(dev, acct, "pk".into(), None, t0 + s(100));
        p.leave(sid, t0 + s(101));
        assert_eq!(p.drain(), vec![Note::Offline(dev)]);
        p.open(dev, acct, "pk".into(), None, t0 + s(102));
        p.drain();
        p.kick(dev, t0 + s(103));
        assert_eq!(p.drain(), vec![Note::Offline(dev)]);
    }

    #[test]
    fn warm_up_is_30s() {
        let (p, t0, _, _) = setup();
        assert!(!p.warm(t0 + s(29)));
        assert!(p.warm(t0 + s(30)));
    }

    #[test]
    fn session_ids_round_trip_as_32_lowercase_hex() {
        let id = SessionId::random();
        assert_eq!(SessionId::parse(&id.to_hex()), Some(id));
        assert_eq!(SessionId::parse("ABCDEF"), None);
    }
}
```

Run: `cargo test --lib feed::presence` → PASS.

- [ ] **Step 4: `FeedHub` — tests first**

`src/feed/hub.rs`:

```rust
//! `FeedHub` — the in-process relay of the event channel (spec 2026-09-25
//! §4.5). It holds:
//! - a lazily created `broadcast` channel per project (capacity 512);
//! - a bounded mailbox per stream session, for account events and closure;
//! - the presence registry;
//! - the ordered coalescers (250 ms `project`, 1 s `holders`/`presence`).
//!
//! Every timer is evaluated in [`FeedHub::tick`] against the injected clock
//! (plan ruling P24). The relay is single-instance by design: a second hub
//! instance would add LISTEN/NOTIFY as a wake-up and a shared presence store
//! (spec §4.5 — noted, not built).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

use crate::claims::{DeviceDelta, HolderBump};
use crate::feed::clock::{Clock, SystemClock};
use crate::feed::coalesce::{Coalescer, Flush, Mergeable, Run};
use crate::feed::presence::{Note, Presence, PresenceConfig, SessionGone, SessionId};
use crate::feed::wire::{
    AccountKind, HolderDeltaWire, HoldersEvent, Kind, PresenceChange, PresenceEntry, PresenceEvent, ProjectEvent, ResyncWhat,
    WireEvent, INLINE_FRAMES,
};

const MAILBOX: usize = 64;
const HEADS_CAPACITY: usize = 4;

#[derive(Debug, Clone)]
pub struct FeedConfig {
    pub project_window: Duration,
    pub holder_window: Duration,
    pub presence_window: Duration,
    pub gap_wait: Duration,
    pub channel_capacity: usize,
    pub presence: PresenceConfig,
}

impl Default for FeedConfig {
    fn default() -> Self {
        Self {
            project_window: Duration::from_millis(250),
            holder_window: Duration::from_secs(1),
            presence_window: Duration::from_secs(1),
            gap_wait: Duration::from_secs(2),
            channel_capacity: 512,
            presence: PresenceConfig::default(),
        }
    }
}

/// `project_id → [version, holderSeq]` — the 60 s `versions` state vector.
pub type HeadsMap = BTreeMap<Uuid, [i64; 2]>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Close,
    Joined(Uuid),
    Left(Uuid),
}

pub struct StreamHandle {
    pub session: SessionId,
    pub mailbox: mpsc::Receiver<Control>,
    pub projects: Vec<(Uuid, broadcast::Receiver<Arc<WireEvent>>)>,
    pub heads: broadcast::Receiver<Arc<HeadsMap>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectPayload {
    pub kinds: BTreeSet<Kind>,
    /// `(frame_uuid, FrameEvent JSON)` — published rows only (P12).
    pub frames: Vec<(Uuid, serde_json::Value)>,
    pub more: bool,
}

impl Mergeable for ProjectPayload {
    fn merge(&mut self, later: Self) {
        self.kinds.extend(later.kinds);
        if self.more || later.more {
            self.more = true;
            self.frames.clear();
            return;
        }
        for (uuid, row) in later.frames {
            match self.frames.iter_mut().find(|(u, _)| *u == uuid) {
                Some(slot) => slot.1 = row,
                None => self.frames.push((uuid, row)),
            }
        }
        if self.frames.len() > INLINE_FRAMES {
            self.frames.clear();
            self.more = true;
        }
    }
}

/// One committed project-version bump entering the feed.
#[derive(Debug, Clone)]
pub struct ProjectBump {
    pub project_id: Uuid,
    pub prev: i64,
    pub version: i64,
    pub payload: ProjectPayload,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct HolderPayload(pub BTreeMap<String, DeviceDelta>);

impl Mergeable for HolderPayload {
    fn merge(&mut self, later: Self) {
        for (device, delta) in later.0 {
            self.0.entry(device).or_default().merge(&delta);
        }
    }
}

#[derive(Default)]
struct PresenceBuffer {
    first_at: Option<Instant>,
    pending: BTreeMap<String, PresenceChange>,
    sent: HashMap<String, PresenceChange>,
}

struct Inner {
    presence: Presence,
    capacity: usize,
    channels: HashMap<Uuid, broadcast::Sender<Arc<WireEvent>>>,
    project_co: HashMap<Uuid, Coalescer<ProjectPayload>>,
    holder_co: HashMap<Uuid, Coalescer<HolderPayload>>,
    presence_buf: HashMap<Uuid, PresenceBuffer>,
    mailboxes: HashMap<SessionId, mpsc::Sender<Control>>,
    account_projects: HashMap<Uuid, BTreeSet<Uuid>>,
    warm_announced: bool,
}

impl Inner {
    fn channel(&mut self, project_id: Uuid) -> &broadcast::Sender<Arc<WireEvent>> {
        let capacity = self.capacity;
        self.channels.entry(project_id).or_insert_with(|| broadcast::channel(capacity).0)
    }

    fn send(&self, project_id: Uuid, event: WireEvent) {
        if let Some(tx) = self.channels.get(&project_id) {
            // No receiver is not an error: nobody streams this project now.
            let _ = tx.send(Arc::new(event));
        }
    }

    fn deliver(&mut self, session: SessionId, control: Control) {
        if let Some(tx) = self.mailboxes.get(&session) {
            if let Err(err) = tx.try_send(control) {
                tracing::warn!(session_id = %session, error = %err, "stream mailbox refused a control message, stream closed");
                // Dropping the sender ends the stream; the client reconnects.
                self.mailboxes.remove(&session);
            }
        }
    }

    fn close_session(&mut self, session: SessionId) {
        if let Some(tx) = self.mailboxes.remove(&session) {
            // A full or closed mailbox is fine: dropping the sender closes the stream too.
            let _ = tx.try_send(Control::Close);
        }
    }

    fn change_for(&self, device_id: Uuid, project_id: Uuid) -> Option<PresenceChange> {
        let t = self.presence.track(device_id)?;
        Some(PresenceChange {
            device: t.pubkey.clone(),
            connected: t.visible,
            serving: t.visible && t.serving.get(&project_id).copied().unwrap_or(false),
            relay_url: t.relay_url.clone(),
        })
    }

    fn queue_presence(&mut self, project_id: Uuid, change: PresenceChange, now: Instant) {
        let buf = self.presence_buf.entry(project_id).or_default();
        buf.first_at.get_or_insert(now);
        buf.pending.insert(change.device.clone(), change);
    }

    /// Fan the registry's fresh notes out to the projects' presence buffers
    /// at the moment they happen, so the 1 s window starts at the event.
    /// Before the warm-up replace went out, notes are dropped: the replace
    /// carries the state (spec §4.2).
    fn absorb_notes(&mut self, now: Instant) {
        let notes = self.presence.drain();
        if !self.warm_announced {
            return;
        }
        for note in &notes {
            self.fan_out(note, now);
        }
    }

    fn fan_out(&mut self, note: &Note, now: Instant) {
        let device_id = note.device_id();
        let Some(account_id) = self.presence.track(device_id).map(|t| t.account_id) else { return };
        let projects: Vec<Uuid> = self.account_projects.get(&account_id).map(|s| s.iter().copied().collect()).unwrap_or_default();
        for project_id in projects {
            if let Some(change) = self.change_for(device_id, project_id) {
                self.queue_presence(project_id, change, now);
            }
        }
    }

    /// Connected devices of the project's members, ordered by device.
    fn members_present(&self, project_id: Uuid) -> Vec<PresenceChange> {
        let mut out: Vec<PresenceChange> = self
            .presence
            .visible_devices()
            .filter(|(_, t)| self.account_projects.get(&t.account_id).is_some_and(|s| s.contains(&project_id)))
            .filter_map(|(id, _)| self.change_for(*id, project_id))
            .collect();
        out.sort_by(|a, b| a.device.cmp(&b.device));
        out
    }
}

fn project_event(project_id: Uuid, run: Run<ProjectPayload>) -> WireEvent {
    let p = run.payload;
    let frames = (!p.more && !p.frames.is_empty()).then(|| p.frames.into_iter().map(|(_, row)| row).collect());
    WireEvent::new("project", &ProjectEvent { project_id, prev: run.prev, version: run.cur, kinds: p.kinds.into_iter().collect(), frames, more: p.more })
}

fn holders_event(project_id: Uuid, run: Run<HolderPayload>) -> WireEvent {
    let deltas = run
        .payload
        .0
        .into_iter()
        .filter(|(_, d)| !d.is_empty())
        .map(|(device, d)| HolderDeltaWire { device, add: d.add.into_iter().collect(), rm: d.rm.into_iter().collect() })
        .collect();
    WireEvent::new("holders", &HoldersEvent { project_id, prev: run.prev, seq: run.cur, deltas })
}

#[derive(Clone)]
pub struct FeedHub {
    inner: Arc<Mutex<Inner>>,
    clock: Arc<dyn Clock>,
    cfg: FeedConfig,
    heads_tx: broadcast::Sender<Arc<HeadsMap>>,
    epoch: Arc<RwLock<String>>,
}

impl FeedHub {
    pub fn new(clock: Arc<dyn Clock>, cfg: FeedConfig) -> FeedHub {
        let inner = Inner {
            presence: Presence::new(cfg.presence.clone(), clock.now()),
            capacity: cfg.channel_capacity,
            channels: HashMap::new(),
            project_co: HashMap::new(),
            holder_co: HashMap::new(),
            presence_buf: HashMap::new(),
            mailboxes: HashMap::new(),
            account_projects: HashMap::new(),
            // No warm-up configured (tests) → nothing to replace later.
            warm_announced: cfg.presence.warmup.is_zero(),
        };
        FeedHub {
            inner: Arc::new(Mutex::new(inner)),
            clock,
            cfg,
            heads_tx: broadcast::channel(HEADS_CAPACITY).0,
            epoch: Arc::new(RwLock::new(String::new())),
        }
    }

    /// The production hub: system clock, spec defaults. `run()` sets the epoch.
    pub fn system() -> FeedHub {
        FeedHub::new(Arc::new(SystemClock), FeedConfig::default())
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| {
            tracing::error!("feed state lock poisoned, continuing with its state");
            poisoned.into_inner()
        })
    }

    pub fn epoch(&self) -> String {
        self.epoch.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn set_epoch(&self, epoch: String) {
        *self.epoch.write().unwrap_or_else(|p| p.into_inner()) = epoch;
    }

    pub fn subscribe(&self, project_id: Uuid) -> broadcast::Receiver<Arc<WireEvent>> {
        self.lock().channel(project_id).subscribe()
    }

    pub fn open_stream(&self, device_id: Uuid, account_id: Uuid, pubkey: String, relay_url: Option<String>, projects: &[Uuid]) -> StreamHandle {
        let now = self.clock.now();
        let mut g = self.lock();
        g.account_projects.insert(account_id, projects.iter().copied().collect());
        let (session, replaced) = g.presence.open(device_id, account_id, pubkey, relay_url, now);
        if let Some(old) = replaced {
            g.close_session(old);
            tracing::info!(device_id = %device_id, session_id = %old, "event stream replaced by a newer one");
        }
        let (tx, rx) = mpsc::channel(MAILBOX);
        g.mailboxes.insert(session, tx);
        let receivers = projects.iter().map(|p| (*p, g.channel(*p).subscribe())).collect();
        g.absorb_notes(now);
        StreamHandle { session, mailbox: rx, projects: receivers, heads: self.heads_tx.subscribe() }
    }

    pub fn detach(&self, session: SessionId) {
        let now = self.clock.now();
        let mut g = self.lock();
        g.presence.detach(session, now);
        g.mailboxes.remove(&session);
    }

    pub fn beat(&self, session: SessionId, serving: HashMap<Uuid, bool>, relay_url: Option<String>) -> Result<(), SessionGone> {
        let now = self.clock.now();
        let mut g = self.lock();
        let result = g.presence.beat(session, serving, relay_url, now);
        g.absorb_notes(now);
        result
    }

    pub fn leave(&self, session: SessionId) -> bool {
        let now = self.clock.now();
        let mut g = self.lock();
        let known = g.presence.leave(session, now).is_some();
        g.close_session(session);
        g.absorb_notes(now);
        known
    }

    pub fn close_device(&self, device_id: Uuid) {
        let now = self.clock.now();
        let mut g = self.lock();
        if let Some(session) = g.presence.kick(device_id, now) {
            g.close_session(session);
        }
        g.absorb_notes(now);
    }

    pub fn close_account(&self, account_id: Uuid) {
        let now = self.clock.now();
        let mut g = self.lock();
        for (session, device_id) in g.presence.live_sessions_of(account_id) {
            g.presence.kick(device_id, now);
            g.close_session(session);
        }
        g.absorb_notes(now);
    }

    /// The caller's own account joined or left a project (spec §4.3 `account`;
    /// presence follows membership, P16).
    pub fn account_event(&self, account_id: Uuid, kind: AccountKind, project_id: Uuid) {
        let now = self.clock.now();
        let mut g = self.lock();
        match kind {
            AccountKind::Joined => {
                g.account_projects.entry(account_id).or_default().insert(project_id);
            }
            AccountKind::Left => {
                if let Some(set) = g.account_projects.get_mut(&account_id) {
                    set.remove(&project_id);
                }
            }
        }
        let warm = g.warm_announced;
        for (session, device_id) in g.presence.live_sessions_of(account_id) {
            g.deliver(session, match kind {
                AccountKind::Joined => Control::Joined(project_id),
                AccountKind::Left => Control::Left(project_id),
            });
            if let Some(mut change) = g.change_for(device_id, project_id).filter(|c| warm && c.connected) {
                if kind == AccountKind::Left {
                    change.connected = false;
                    change.serving = false;
                }
                g.queue_presence(project_id, change, now);
            }
        }
    }

    pub fn offer_project(&self, bump: ProjectBump) {
        let now = self.clock.now();
        let (window, gap) = (self.cfg.project_window, self.cfg.gap_wait);
        let mut payload = bump.payload;
        if payload.kinds.contains(&Kind::Frames) && payload.frames.is_empty() {
            payload.more = true; // changed frames not inlined → pull the manifest delta
        }
        self.lock()
            .project_co
            .entry(bump.project_id)
            .or_insert_with(|| Coalescer::new(window, gap))
            .offer(Run { prev: bump.prev, cur: bump.version, payload }, now);
    }

    pub fn offer_holders(&self, bump: HolderBump) {
        let now = self.clock.now();
        let (window, gap) = (self.cfg.holder_window, self.cfg.gap_wait);
        self.lock()
            .holder_co
            .entry(bump.project_id)
            .or_insert_with(|| Coalescer::new(window, gap))
            .offer(Run { prev: bump.prev, cur: bump.seq, payload: HolderPayload(bump.deltas) }, now);
    }

    pub fn broadcast_heads(&self, heads: Arc<HeadsMap>) {
        // No receiver = no open stream; nothing to repair.
        let _ = self.heads_tx.send(heads);
    }

    pub fn presence_for(&self, project_id: Uuid) -> Vec<PresenceEntry> {
        let now = self.clock.now();
        let g = self.lock();
        if !g.presence.warm(now) {
            return Vec::new();
        }
        g.members_present(project_id)
            .into_iter()
            .map(|c| PresenceEntry { device: c.device, serving: c.serving, relay_url: c.relay_url })
            .collect()
    }

    pub fn connected_devices(&self) -> HashSet<Uuid> {
        self.lock().presence.visible_devices().map(|(id, _)| *id).collect()
    }

    pub fn live_relay(&self, device_id: Uuid) -> Option<Option<String>> {
        self.lock().presence.track(device_id).filter(|t| t.visible).map(|t| t.relay_url.clone())
    }

    /// Close every stream (graceful shutdown, P23).
    pub fn shutdown(&self) {
        let mut g = self.lock();
        let sessions: Vec<SessionId> = g.mailboxes.keys().copied().collect();
        for session in &sessions {
            g.close_session(*session);
        }
        tracing::info!(count = sessions.len(), "event streams closed for shutdown");
    }

    /// Evaluate every timer: presence expiry and damping, the warm-up end,
    /// the presence/project/holder coalescing windows, the gap buffer.
    pub fn tick(&self) {
        let now = self.clock.now();
        let mut g = self.lock();
        let inner = &mut *g;
        for session in inner.presence.tick(now) {
            inner.close_session(session);
        }
        if !inner.warm_announced && inner.presence.warm(now) {
            inner.warm_announced = true;
            let projects: Vec<Uuid> = inner.channels.keys().copied().collect();
            for project_id in &projects {
                let present = inner.members_present(*project_id);
                let buf = inner.presence_buf.entry(*project_id).or_default();
                buf.pending.clear();
                buf.first_at = None;
                buf.sent = present.iter().map(|c| (c.device.clone(), c.clone())).collect();
                inner.send(*project_id, WireEvent::new("presence", &PresenceEvent { project_id: *project_id, replace: true, changes: present }));
            }
            tracing::info!(projects = projects.len(), "presence warm-up ended");
        }
        inner.absorb_notes(now);
        let mut out: Vec<(Uuid, WireEvent)> = Vec::new();
        let window = self.cfg.presence_window;
        for (project_id, buf) in inner.presence_buf.iter_mut() {
            let Some(first) = buf.first_at else { continue };
            if now.duration_since(first) < window {
                continue;
            }
            buf.first_at = None;
            let mut changes = Vec::new();
            for (device, change) in std::mem::take(&mut buf.pending) {
                let differs = match buf.sent.get(&device) {
                    Some(prev) => prev != &change,
                    None => change.connected,
                };
                if differs {
                    buf.sent.insert(device, change.clone());
                    changes.push(change);
                }
            }
            if !changes.is_empty() {
                out.push((*project_id, WireEvent::new("presence", &PresenceEvent { project_id: *project_id, replace: false, changes })));
            }
        }
        for (project_id, co) in inner.project_co.iter_mut() {
            for flush in co.poll(now) {
                out.push((*project_id, match flush {
                    Flush::Emit(run) => project_event(*project_id, run),
                    Flush::Resync => WireEvent::resync(*project_id, ResyncWhat::Project),
                }));
            }
        }
        for (project_id, co) in inner.holder_co.iter_mut() {
            for flush in co.poll(now) {
                out.push((*project_id, match flush {
                    Flush::Emit(run) => holders_event(*project_id, run),
                    Flush::Resync => WireEvent::resync(*project_id, ResyncWhat::Holders),
                }));
            }
        }
        for (project_id, event) in out {
            inner.send(project_id, event);
        }
        inner.channels.retain(|_, tx| tx.receiver_count() > 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::clock::ManualClock;
    use serde_json::{json, Value};

    fn hub(warmup: Duration) -> (FeedHub, ManualClock) {
        let clock = ManualClock::new();
        let mut cfg = FeedConfig::default();
        cfg.presence.warmup = warmup;
        (FeedHub::new(Arc::new(clock.clone()), cfg), clock)
    }
    fn next(rx: &mut broadcast::Receiver<Arc<WireEvent>>) -> Option<(String, Value)> {
        rx.try_recv().ok().map(|e| (e.name.to_string(), serde_json::from_str(&e.data).unwrap()))
    }
    fn advance(h: &FeedHub, c: &ManualClock, by: Duration) {
        c.advance(by);
        h.tick();
    }
    const P: Uuid = Uuid::from_u128(0xA);
    const DEV: Uuid = Uuid::from_u128(1);
    const ACCT: Uuid = Uuid::from_u128(100);

    #[test]
    fn project_bumps_coalesce_into_one_event_per_window() {
        let (h, c) = hub(Duration::ZERO);
        let mut rx = h.subscribe(P);
        let row = json!({"frameUuid": "u1", "frameSeq": 1});
        h.offer_project(ProjectBump { project_id: P, prev: 1, version: 2,
            payload: ProjectPayload { kinds: BTreeSet::from([Kind::Frames]), frames: vec![(Uuid::from_u128(9), row.clone())], more: false } });
        h.offer_project(ProjectBump { project_id: P, prev: 2, version: 3,
            payload: ProjectPayload { kinds: BTreeSet::from([Kind::Members]), frames: vec![], more: false } });
        h.tick();
        assert!(next(&mut rx).is_none(), "inside the 250 ms window");
        advance(&h, &c, Duration::from_millis(250));
        let (name, data) = next(&mut rx).unwrap();
        assert_eq!(name, "project");
        assert_eq!(data, json!({"projectId": P, "prev": 1, "version": 3, "kinds": ["frames", "members"], "frames": [row], "more": false}));
    }

    #[test]
    fn a_frames_bump_without_rows_says_more() {
        let (h, c) = hub(Duration::ZERO);
        let mut rx = h.subscribe(P);
        h.offer_project(ProjectBump { project_id: P, prev: 4, version: 5,
            payload: ProjectPayload { kinds: BTreeSet::from([Kind::Frames]), frames: vec![], more: false } });
        advance(&h, &c, Duration::from_millis(250));
        let (_, data) = next(&mut rx).unwrap();
        assert_eq!(data["more"], true);
        assert!(data.get("frames").is_none());
    }

    #[test]
    fn holder_bumps_merge_per_device_later_op_winning() {
        let (h, c) = hub(Duration::ZERO);
        let mut rx = h.subscribe(P);
        let mut d1 = DeviceDelta::default();
        d1.add(1, 1);
        let mut d2 = DeviceDelta::default();
        d2.rm(1);
        d2.add(2, 1);
        h.offer_holders(HolderBump { project_id: P, prev: 0, seq: 1, deltas: BTreeMap::from([("pkA".to_string(), d1)]) });
        h.offer_holders(HolderBump { project_id: P, prev: 1, seq: 2, deltas: BTreeMap::from([("pkA".to_string(), d2)]) });
        advance(&h, &c, Duration::from_secs(1));
        let (name, data) = next(&mut rx).unwrap();
        assert_eq!(name, "holders");
        assert_eq!(data, json!({"projectId": P, "prev": 0, "seq": 2, "deltas": [{"device": "pkA", "add": [[2, 1]], "rm": [1]}]}));
    }

    #[test]
    fn presence_reaches_the_projects_of_the_account_coalesced_per_second() {
        let (h, c) = hub(Duration::ZERO);
        let handle = h.open_stream(DEV, ACCT, "pk".into(), Some("https://r1".into()), &[P]);
        let mut rx = h.subscribe(P);
        h.tick();
        advance(&h, &c, Duration::from_secs(1));
        let (name, data) = next(&mut rx).unwrap();
        assert_eq!(name, "presence");
        assert_eq!(data, json!({"projectId": P, "replace": false, "changes": [{"device": "pk", "connected": true, "serving": false, "relayUrl": "https://r1"}]}));
        h.beat(handle.session, HashMap::from([(P, true)]), Some("https://r2".into())).unwrap();
        h.tick();
        advance(&h, &c, Duration::from_secs(1));
        let (_, data) = next(&mut rx).unwrap();
        assert_eq!(data["changes"][0]["serving"], true);
        assert_eq!(data["changes"][0]["relayUrl"], "https://r2");
        assert_eq!(h.presence_for(P), vec![PresenceEntry { device: "pk".into(), serving: true, relay_url: Some("https://r2".into()) }]);
    }

    #[test]
    fn warm_up_suppresses_diffs_then_sends_one_replace() {
        let (h, c) = hub(Duration::from_secs(30));
        let _handle = h.open_stream(DEV, ACCT, "pk".into(), None, &[P]);
        let mut rx = h.subscribe(P);
        h.tick();
        advance(&h, &c, Duration::from_secs(1));
        assert!(next(&mut rx).is_none(), "no diffs during the warm-up");
        assert!(h.presence_for(P).is_empty());
        advance(&h, &c, Duration::from_secs(29));
        let (_, data) = next(&mut rx).unwrap();
        assert_eq!(data["replace"], true);
        assert_eq!(data["changes"][0]["device"], "pk");
    }

    #[test]
    fn account_events_reach_the_mailbox() {
        let (h, _c) = hub(Duration::ZERO);
        let q = Uuid::from_u128(0xB);
        let mut handle = h.open_stream(DEV, ACCT, "pk".into(), None, &[P]);
        h.account_event(ACCT, AccountKind::Joined, q);
        assert_eq!(handle.mailbox.try_recv().unwrap(), Control::Joined(q));
        h.account_event(ACCT, AccountKind::Left, P);
        assert_eq!(handle.mailbox.try_recv().unwrap(), Control::Left(P));
    }

    #[test]
    fn close_device_ends_the_stream_and_broadcasts_offline() {
        let (h, c) = hub(Duration::ZERO);
        let mut handle = h.open_stream(DEV, ACCT, "pk".into(), None, &[P]);
        let mut rx = h.subscribe(P);
        h.tick();
        advance(&h, &c, Duration::from_secs(1));
        next(&mut rx).unwrap();
        h.close_device(DEV);
        assert_eq!(handle.mailbox.try_recv().unwrap(), Control::Close);
        h.tick();
        advance(&h, &c, Duration::from_secs(1));
        let (_, data) = next(&mut rx).unwrap();
        assert_eq!(data["changes"][0]["connected"], false);
        assert!(h.connected_devices().is_empty());
    }

    #[test]
    fn a_second_stream_closes_the_first() {
        let (h, _c) = hub(Duration::ZERO);
        let mut first = h.open_stream(DEV, ACCT, "pk".into(), None, &[P]);
        let second = h.open_stream(DEV, ACCT, "pk".into(), None, &[P]);
        assert_eq!(first.mailbox.try_recv().unwrap(), Control::Close);
        assert_ne!(first.session, second.session);
        assert_eq!(h.beat(first.session, HashMap::new(), None), Err(SessionGone));
    }
}
```

Uncomment `pub mod hub;` and the `pub use` in `feed/mod.rs`. Add `pub mod feed;` to `src/lib.rs`.

- [ ] **Step 5: `AppState.feed`**

`src/routes/mod.rs`:
- Add the field to `AppState` (after `versions`):

```rust
    /// The event channel relay (collab v3 wave 3, spec 2026-09-25 §4.5):
    /// presence, per-project broadcast, ordered coalescers. `run()` gives it
    /// the epoch and spawns its driver; tests inject a manual clock via
    /// [`AppState::with_feed`].
    pub feed: crate::feed::FeedHub,
```

- Add `feed: crate::feed::FeedHub::system(),` in `AppState::new`, and add the builder:

```rust
    /// Replace the feed relay (tests: a `ManualClock`-driven hub).
    pub fn with_feed(mut self, feed: crate::feed::FeedHub) -> Self {
        self.feed = feed;
        self
    }
```

- [ ] **Step 6: Run and commit**

Run: `cargo test --lib feed && DATABASE_URL=… cargo test 2>&1 | tail -5 && cargo build --release 2>&1 | grep -c warning`
Expected: all green; `0` warnings.

```bash
git add src/feed src/lib.rs src/routes/mod.rs
git commit -m "feat(hub): feed core — manual/system clock, ordered coalescers with a 2 s gap buffer, presence registry (grace, silence, flap damping, warm-up), FeedHub" \
           -m "Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RYd9tanU4ehWLDXkPv159r"
```

---

### Task 4: Publish after commit — `FeedBatch`, `commit_and_publish`, every bump site

**Files:**
- Create: `src/feed/publish.rs`. `src/feed/mod.rs` gains `pub mod publish; pub use publish::{commit_and_publish, FeedBatch, FramesPayload};`.
- Modify: `src/routes/frames.rs`:
  - split `FrameView` into `FrameEvent` + `own`;
  - add `frame_events_tx`;
  - `publish_pending_of_tx` returns the published uuids;
  - wire `announce`, `new_version`, `patch_frame`, `approve`, `reject`.
- Modify: `src/collab_auth.rs:234-241`. `bump_membership_version_tx` returns the new `projects.version` (`Result<i64, sqlx::Error>`).
- Modify the bump sites in `src/routes/{thresholds,dictionary,projects,members,join_requests,invites,operator,holders}.rs` (table in Step 4).
- Test: `tests/feed_publish.rs` (new), `tests/common/mod.rs` (`FeedApp`, `app_with_feed`, `next_wire`).

**Interfaces:**
- Consumes: `FeedHub::{offer_project, offer_holders, account_event, close_device, close_account}` (Task 3), `HolderWrite::finish` (Task 1).
- Produces:
  ```rust
  // feed::publish
  #[derive(Debug, Clone, Default, PartialEq)] pub struct FramesPayload { pub rows: Vec<(Uuid, serde_json::Value)>, pub more: bool }
  #[derive(Debug, Default)] pub struct FeedBatch { /* private */ }
  impl FeedBatch {
      pub fn bump(&mut self, project_id: Uuid, version: i64, kind: Kind);   // first call: prev = version - 1
      pub fn frames(&mut self, project_id: Uuid, frames: FramesPayload);    // after bump()
      pub fn holders(&mut self, bump: Option<HolderBump>);
      pub fn account(&mut self, account_id: Uuid, kind: AccountKind, project_id: Uuid);
      pub fn close_device(&mut self, device_id: Uuid);
      pub fn close_account(&mut self, account_id: Uuid);
      pub fn project_ids(&self) -> Vec<Uuid>;
  }
  impl FeedHub { pub fn publish(&self, batch: FeedBatch); }
  pub async fn commit_and_publish(state: &AppState, tx: sqlx::Transaction<'static, sqlx::Postgres>, batch: FeedBatch) -> Result<(), ApiError>;
  // routes::frames
  #[derive(Serialize)] pub struct FrameEvent { /* every FrameView field except `own` */ }
  #[derive(Serialize)] pub struct FrameView { #[serde(flatten)] pub event: FrameEvent, pub own: bool }
  impl FrameRow { pub fn into_event(self) -> FrameEvent; pub fn into_view(self, viewer: Uuid) -> FrameView }
  pub(crate) async fn frame_events_tx(conn: &mut PgConnection, project_id: Uuid, uuids: &[Uuid]) -> Result<FramesPayload, ApiError>;
  pub(crate) async fn publish_pending_of_tx(conn, project_id, publisher, decided_by, manifest_version) -> Result<Vec<Uuid>, sqlx::Error>;
  // changed signatures (each gains `feed: &mut FeedBatch` as its LAST parameter)
  join_requests::join_member_tx, join_requests::approve_join_core, members::remove_member_core, members::delete_member_row, projects::create_project_core
  collab_auth::bump_membership_version_tx(conn, project_id) -> Result<i64, sqlx::Error>
  ```
- Test helper (`tests/common/mod.rs`):
  ```rust
  pub struct FeedApp { pub app: axum::Router, pub mailer: CaptureMailer, pub feed: athenaeum_hub::feed::FeedHub, pub clock: athenaeum_hub::feed::clock::ManualClock }
  impl FeedApp { pub fn advance(&self, by: std::time::Duration) }   // clock.advance + feed.tick
  pub fn app_with_feed(pool: PgPool) -> FeedApp;                      // warm-up 0, epoch "test-epoch", probe token "probe-secret"
  pub fn app_with_feed_cfg(pool: PgPool, cfg: athenaeum_hub::feed::FeedConfig) -> FeedApp;
  pub fn next_wire(rx: &mut tokio::sync::broadcast::Receiver<std::sync::Arc<athenaeum_hub::feed::wire::WireEvent>>) -> Option<(String, Value)>;
  ```

- [ ] **Step 1: Test helpers and failing tests**

Append to `tests/common/mod.rs`:

```rust
/// A router whose feed runs on a manual clock (plan ruling P24): tests
/// advance it and tick the hub explicitly.
pub struct FeedApp {
    pub app: axum::Router,
    pub mailer: CaptureMailer,
    pub feed: athenaeum_hub::feed::FeedHub,
    pub clock: athenaeum_hub::feed::clock::ManualClock,
}

impl FeedApp {
    pub fn advance(&self, by: std::time::Duration) {
        self.clock.advance(by);
        self.feed.tick();
    }
}

pub fn app_with_feed(pool: PgPool) -> FeedApp {
    let mut cfg = athenaeum_hub::feed::FeedConfig::default();
    cfg.presence.warmup = std::time::Duration::ZERO;
    app_with_feed_cfg(pool, cfg)
}

pub fn app_with_feed_cfg(pool: PgPool, cfg: athenaeum_hub::feed::FeedConfig) -> FeedApp {
    let clock = athenaeum_hub::feed::clock::ManualClock::new();
    let feed = athenaeum_hub::feed::FeedHub::new(Arc::new(clock.clone()), cfg);
    feed.set_epoch("test-epoch".into());
    let mailer = CaptureMailer::default();
    let state = AppState::new(pool, Arc::new(mailer.clone()))
        .with_feed(feed.clone())
        .with_events_probe_token(Some("probe-secret".into()));
    FeedApp { app: build_router(state), mailer, feed, clock }
}

pub fn next_wire(
    rx: &mut tokio::sync::broadcast::Receiver<Arc<athenaeum_hub::feed::wire::WireEvent>>,
) -> Option<(String, Value)> {
    rx.try_recv().ok().map(|e| (e.name.to_string(), serde_json::from_str(&e.data).unwrap()))
}
```

`with_events_probe_token` is added in Task 5. Until then, drop that line from `app_with_feed_cfg`, and Task 5 re-adds it.

Create `tests/feed_publish.rs`:

```rust
//! Every committed change reaches the feed, after the commit, with the right
//! cursor range and kinds (spec 2026-09-25 §4.3, §4.5; plan ruling P1).
mod common;

use std::time::Duration;

use athenaeum_hub::feed::Control;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const Q: Duration = Duration::from_millis(250);
const S: Duration = Duration::from_secs(1);

async fn version(pool: &PgPool, id: &str) -> i64 {
    sqlx::query_scalar("SELECT version FROM projects WHERE id = $1").bind(Uuid::parse_str(id).unwrap()).fetch_one(pool).await.unwrap()
}

#[sqlx::test]
async fn announce_publishes_inline_frames_then_the_publisher_claims(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (coord, _) = register_device(&fa.app, &fa.mailer, "coord@example.com", 1, "Desktop").await;
    let project = create_project_via(&fa.app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap().to_string();
    let mut rx = fa.feed.subscribe(Uuid::parse_str(&id).unwrap());
    let (status, body) = announce_frames(&fa.app, &coord, &id, 1, 3).await;
    assert_eq!(status, StatusCode::OK);
    let v = body["projectVersion"].as_i64().unwrap();
    fa.advance(Q);
    let (name, ev) = next_wire(&mut rx).unwrap();
    assert_eq!(name, "project");
    assert_eq!((ev["prev"].as_i64(), ev["version"].as_i64()), (Some(v - 1), Some(v)));
    assert_eq!(ev["kinds"], json!(["frames"]));
    assert_eq!(ev["more"], false);
    let frames = ev["frames"].as_array().unwrap();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0]["frameSeq"], 1);
    assert_eq!(frames[0]["fileName"], "c_L_0001.fits");
    assert!(frames[0].get("own").is_none() && frames[0].get("holderCount").is_none(), "FrameEvent = FrameView minus own (P12)");
    fa.advance(S - Q);
    let (name, ev) = next_wire(&mut rx).unwrap();
    assert_eq!(name, "holders");
    assert_eq!(ev, json!({"projectId": id, "prev": 0, "seq": 1, "deltas": [{"device": pubkey_b64(1), "add": [[1, 1], [2, 1]], "rm": []}]}));
}

#[sqlx::test]
async fn a_pending_announce_inlines_nothing_and_says_more(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (coord, _) = register_device(&fa.app, &fa.mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&fa.app, &fa.mailer, "anna@example.com", 2, "Laptop").await;
    let project = create_project_via(&fa.app, &coord, "P", true).await;
    let id = project["id"].as_str().unwrap().to_string();
    join_and_approve(&fa.app, &coord, &anna, &id, "Anna", "send").await;
    fa.advance(Q);
    let mut rx = fa.feed.subscribe(Uuid::parse_str(&id).unwrap());
    assert_eq!(announce_frames(&fa.app, &anna, &id, 1, 2).await.1["state"], "pending");
    fa.advance(Q);
    let (_, ev) = next_wire(&mut rx).unwrap();
    assert_eq!(ev["more"], true);
    assert!(ev.get("frames").is_none(), "a pending row is visible only to its publisher and moderators (P12)");
}

#[sqlx::test]
async fn two_bumps_in_one_transaction_are_one_contiguous_event(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (coord, _) = register_device(&fa.app, &fa.mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&fa.app, &fa.mailer, "anna@example.com", 2, "Laptop").await;
    let project = create_project_via(&fa.app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap().to_string();
    let anna_account = join_and_approve(&fa.app, &coord, &anna, &id, "Anna", "send").await;
    let before = version(&pool, &id).await;
    fa.advance(Q); // flush the join's own event first, or it merges into this one
    let mut rx = fa.feed.subscribe(Uuid::parse_str(&id).unwrap());
    let (status, _) = send(&fa.app, patch(&format!("/api/v1/projects/{id}/members/{anna_account}"),
        &json!({"dataRole": "send_receive", "govCaps": ["posts.write"]}), Some(&coord))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    fa.advance(Q);
    let (_, ev) = next_wire(&mut rx).unwrap();
    assert_eq!((ev["prev"].as_i64(), ev["version"].as_i64()), (Some(before), Some(before + 2)));
    assert_eq!(ev["kinds"], json!(["members"]));
    assert!(next_wire(&mut rx).is_none());
}

#[sqlx::test]
async fn every_document_kind_is_published(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (coord, _) = register_device(&fa.app, &fa.mailer, "coord@example.com", 1, "Desktop").await;
    let project = create_project_via(&fa.app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap().to_string();
    let mut rx = fa.feed.subscribe(Uuid::parse_str(&id).unwrap());
    let (_, dict) = send(&fa.app, get(&format!("/api/v1/projects/{id}/dictionary"), Some(&coord))).await;
    let entries = as_json(&dict)["current"]["entries"].clone();
    for (req, kind) in [
        (post(&format!("/api/v1/projects/{id}/thresholds"), &json!({"rules": [{"metricKey": "fwhm_arcsec", "op": "lte", "value": 3.0}]}), Some(&coord)), "thresholds"),
        (put(&format!("/api/v1/projects/{id}/dictionary"), &json!({"entries": entries}), Some(&coord)), "dictionary"),
        (put(&format!("/api/v1/projects/{id}/grid"), &json!({"scaleArcsec": 1.2, "widthPx": 100, "heightPx": 100, "crval1": 1.0, "crval2": 2.0, "rotationDeg": 0.0}), Some(&coord)), "grid"),
        (patch(&format!("/api/v1/projects/{id}"), &json!({"description": "edited"}), Some(&coord)), "meta"),
    ] {
        let (status, body) = send(&fa.app, req).await;
        assert!(status.is_success(), "{kind}: {status} {}", String::from_utf8_lossy(&body));
        fa.advance(Q);
        let (_, ev) = next_wire(&mut rx).unwrap();
        assert_eq!(ev["kinds"], json!([kind]));
    }
}

#[sqlx::test]
async fn membership_changes_reach_the_joining_accounts_streams(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (coord, _) = register_device(&fa.app, &fa.mailer, "coord@example.com", 1, "Desktop").await;
    let (bob, bob_device) = register_device(&fa.app, &fa.mailer, "bob@example.com", 3, "PC").await;
    let project = create_project_via(&fa.app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap().to_string();
    let pid = Uuid::parse_str(&id).unwrap();
    let bob_account: Uuid = sqlx::query_scalar("SELECT account_id FROM devices WHERE id = $1")
        .bind(Uuid::parse_str(&bob_device).unwrap()).fetch_one(&pool).await.unwrap();
    let mut handle = fa.feed.open_stream(Uuid::parse_str(&bob_device).unwrap(), bob_account, pubkey_b64(3), None, &[]);
    join_and_approve(&fa.app, &coord, &bob, &id, "Bob", "send_receive").await;
    assert_eq!(handle.mailbox.try_recv().unwrap(), Control::Joined(pid));
    let (status, _) = send(&fa.app, post(&format!("/api/v1/projects/{id}/leave"), &json!({}), Some(&bob))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(handle.mailbox.try_recv().unwrap(), Control::Left(pid));
}

#[sqlx::test]
async fn a_holder_report_publishes_its_delta(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (coord, _) = register_device(&fa.app, &fa.mailer, "coord@example.com", 1, "Desktop").await;
    let (bob, _) = register_device(&fa.app, &fa.mailer, "bob@example.com", 3, "PC").await;
    let project = create_project_via(&fa.app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap().to_string();
    join_and_approve(&fa.app, &coord, &bob, &id, "Bob", "send_receive").await;
    assert_eq!(announce_frames(&fa.app, &coord, &id, 1, 2).await.0, StatusCode::OK);
    fa.advance(S);
    let mut rx = fa.feed.subscribe(Uuid::parse_str(&id).unwrap());
    let f1 = frame_uuid(1);
    let (status, _) = report_holders(&fa.app, &bob, &id, 1, false, &[(&f1, 1)], &[], &[(&f1, 1)]).await;
    assert_eq!(status, StatusCode::OK);
    fa.advance(S);
    let (name, ev) = next_wire(&mut rx).unwrap();
    assert_eq!(name, "holders");
    assert_eq!(ev["deltas"], json!([{"device": pubkey_b64(3), "add": [[1, 1]], "rm": []}]));
}
```

Run: `DATABASE_URL=… cargo test --test feed_publish` → FAIL. It does not compile (`app_with_feed` needs Task 3's `with_feed`, present), then runtime failures: no events.

- [ ] **Step 2: `FeedBatch` and `commit_and_publish`**

`src/feed/publish.rs`:

```rust
//! Publish after commit (spec 2026-09-25 §4.5; plan ruling P1). A writer
//! collects what its transaction changed in a [`FeedBatch`] and hands the
//! transaction to [`commit_and_publish`], which commits AND publishes inside
//! one spawned task — a dropped request future can no longer commit without
//! publishing. The 60 s `versions` vector covers any residual gap.

use std::collections::BTreeMap;

use uuid::Uuid;

use crate::claims::HolderBump;
use crate::error::ApiError;
use crate::feed::hub::{FeedHub, ProjectBump, ProjectPayload};
use crate::feed::wire::{AccountKind, Kind, INLINE_FRAMES};
use crate::routes::AppState;

/// The inline frame rows of a `project` event (P12).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FramesPayload {
    pub rows: Vec<(Uuid, serde_json::Value)>,
    pub more: bool,
}

#[derive(Debug, Default)]
pub struct FeedBatch {
    projects: BTreeMap<Uuid, ProjectBump>,
    holders: Vec<HolderBump>,
    accounts: Vec<(Uuid, AccountKind, Uuid)>,
    close_devices: Vec<Uuid>,
    close_accounts: Vec<Uuid>,
}

impl FeedBatch {
    /// Record a `projects.version` bump. The counter moves by exactly 1 per
    /// bump under the project row lock, so the first bump of a project in
    /// this transaction starts the range at `version - 1`; later bumps
    /// extend it.
    pub fn bump(&mut self, project_id: Uuid, version: i64, kind: Kind) {
        let entry = self.projects.entry(project_id).or_insert_with(|| ProjectBump {
            project_id,
            prev: version - 1,
            version,
            payload: ProjectPayload::default(),
        });
        entry.version = entry.version.max(version);
        entry.payload.kinds.insert(kind);
    }

    pub fn frames(&mut self, project_id: Uuid, frames: FramesPayload) {
        let Some(bump) = self.projects.get_mut(&project_id) else {
            tracing::error!(project_id = %project_id, "frame rows attached without a version bump, dropped");
            return;
        };
        bump.payload.more |= frames.more;
        bump.payload.frames.extend(frames.rows);
        if bump.payload.more || bump.payload.frames.len() > INLINE_FRAMES {
            bump.payload.more = true;
            bump.payload.frames.clear();
        }
    }

    pub fn holders(&mut self, bump: Option<HolderBump>) {
        if let Some(b) = bump {
            self.holders.push(b);
        }
    }

    pub fn account(&mut self, account_id: Uuid, kind: AccountKind, project_id: Uuid) {
        self.accounts.push((account_id, kind, project_id));
    }

    pub fn close_device(&mut self, device_id: Uuid) {
        self.close_devices.push(device_id);
    }

    pub fn close_account(&mut self, account_id: Uuid) {
        self.close_accounts.push(account_id);
    }

    pub fn project_ids(&self) -> Vec<Uuid> {
        self.projects.keys().copied().collect()
    }
}

impl FeedHub {
    pub fn publish(&self, batch: FeedBatch) {
        let FeedBatch { projects, holders, accounts, close_devices, close_accounts } = batch;
        for (_, bump) in projects {
            tracing::debug!(project_id = %bump.project_id, prev = bump.prev, version = bump.version, "feed project bump");
            self.offer_project(bump);
        }
        for bump in holders {
            tracing::debug!(project_id = %bump.project_id, prev = bump.prev, holder_seq = bump.seq, "feed holder bump");
            self.offer_holders(bump);
        }
        for (account_id, kind, project_id) in accounts {
            self.account_event(account_id, kind, project_id);
        }
        for device_id in close_devices {
            self.close_device(device_id);
        }
        for account_id in close_accounts {
            self.close_account(account_id);
        }
    }
}

/// Commit `tx` and publish `batch`, both inside one spawned task (P1).
pub async fn commit_and_publish(
    state: &AppState,
    tx: sqlx::Transaction<'static, sqlx::Postgres>,
    batch: FeedBatch,
) -> Result<(), ApiError> {
    let feed = state.feed.clone();
    let versions = state.versions.clone(); // retired in Task 10
    let task = tokio::spawn(async move {
        tx.commit().await?;
        for project_id in batch.project_ids() {
            versions.invalidate(project_id);
        }
        feed.publish(batch);
        Ok::<(), sqlx::Error>(())
    });
    match task.await {
        Ok(result) => result.map_err(ApiError::from),
        Err(join) => Err(ApiError::Internal(anyhow::Error::new(join).context("commit-and-publish task failed"))),
    }
}
```

- [ ] **Step 3: `FrameEvent`, `frame_events_tx`, membership bump value**

`src/routes/frames.rs`: replace `FrameView` and `into_view` with:

```rust
/// One manifest row as the event channel inlines it (spec 2026-09-25 §4.3):
/// everything a viewer sees except `own`, which the client derives from
/// `publisherAccountId == hello.accountId` (plan ruling P12).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameEvent {
    pub frame_uuid: Uuid,
    pub frame_seq: i32,
    pub publisher_account_id: Uuid,
    pub publisher_display_name: String,
    pub file_name: String,
    pub content_version: i32,
    pub blake3: String,
    pub byte_size: i64,
    pub xxh3: String,
    pub filter_raw: String,
    pub filter_canonical: String,
    pub channel: String,
    pub exptime_sec: f64,
    pub date_obs: Option<DateTime<Utc>>,
    pub meta: Value,
    pub gate_version: i32,
    pub accepted: bool,
    pub accepted_reason: Option<String>,
    pub state: String,
    pub reject_reason: Option<String>,
    pub manifest_version: i64,
    pub created_at: DateTime<Utc>,
}

/// A manifest row: the [`FrameEvent`] plus the viewer-specific `own`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameView {
    #[serde(flatten)]
    pub event: FrameEvent,
    pub own: bool,
}

impl FrameRow {
    pub fn into_event(self) -> FrameEvent {
        FrameEvent {
            publisher_display_name: self.publisher_display_name.unwrap_or_else(|| FORMER_MEMBER.to_string()),
            frame_uuid: self.frame_uuid,
            frame_seq: self.frame_seq,
            publisher_account_id: self.publisher,
            file_name: self.file_name,
            content_version: self.content_version,
            blake3: self.blake3,
            byte_size: self.byte_size,
            xxh3: self.xxh3,
            filter_raw: self.filter_raw,
            filter_canonical: self.filter_canonical,
            channel: self.channel,
            exptime_sec: self.exptime_sec,
            date_obs: self.date_obs,
            meta: self.meta,
            gate_version: self.gate_version,
            accepted: self.accepted,
            accepted_reason: self.accepted_reason,
            state: self.state,
            reject_reason: self.reject_reason,
            manifest_version: self.manifest_version,
            created_at: self.created_at,
        }
    }

    pub fn into_view(self, viewer: Uuid) -> FrameView {
        let own = self.publisher == viewer;
        FrameView { event: self.into_event(), own }
    }
}

/// The inline rows of a `project` event: the changed frames' events when
/// there are at most `INLINE_FRAMES` and every one is published; otherwise
/// `more` (P12). Reads inside the writing transaction, after its writes.
pub(crate) async fn frame_events_tx(
    conn: &mut PgConnection,
    project_id: Uuid,
    uuids: &[Uuid],
) -> Result<crate::feed::FramesPayload, ApiError> {
    use crate::feed::{wire::INLINE_FRAMES, FramesPayload};
    if uuids.is_empty() {
        return Ok(FramesPayload::default());
    }
    if uuids.len() > INLINE_FRAMES {
        return Ok(FramesPayload { rows: Vec::new(), more: true });
    }
    let sql = frame_select_sql("AND f.frame_uuid = ANY($4)", "ORDER BY f.frame_seq");
    let rows = sqlx::query_as::<_, FrameRow>(&sql)
        .bind(project_id)
        .bind(Uuid::nil())
        .bind(true)
        .bind(uuids)
        .fetch_all(&mut *conn)
        .await?;
    if rows.iter().any(|r| r.state != "published") {
        return Ok(FramesPayload { rows: Vec::new(), more: true });
    }
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let uuid = row.frame_uuid;
        let value = serde_json::to_value(row.into_event())
            .map_err(|e| ApiError::Internal(anyhow::Error::new(e).context("serialize frame event")))?;
        out.push((uuid, value));
    }
    Ok(FramesPayload { rows: out, more: false })
}
```

`publish_pending_of_tx` (`:764-782`) becomes `-> Result<Vec<Uuid>, sqlx::Error>`. Its body: `sqlx::query_scalar("UPDATE project_frames SET … WHERE project_id = $1 AND publisher = $2 AND state = 'pending' RETURNING frame_uuid").bind(…).fetch_all(conn).await`. At its two callers, counts become `.len() as u64`.

`src/collab_auth.rs:234-241`:

```rust
pub async fn bump_membership_version_tx(conn: &mut PgConnection, project_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query(BUMP).bind(project_id).execute(&mut *conn).await?;
    crate::project_version::bump_project_version_tx(conn, project_id).await
}
```

Also update its doc: "Returns the new `projects.version`; the caller records it in its `FeedBatch` with kind `members`."

- [ ] **Step 4: Wire every bump site**

The pattern at every site:
- Replace `tx.commit().await?; state.versions.invalidate(ID);` with `crate::feed::commit_and_publish(&state, tx, feed).await?;`.
- Declare `let mut feed = crate::feed::FeedBatch::default();` right after `begin()`.
- Record every bump as it happens: `let v = bump_…(&mut tx, ID).await?; feed.bump(ID, v, Kind::…);`.

Here is `patch_frame` in full, as the model:

```rust
    let mut tx = state.db.begin().await?;
    let mut feed = FeedBatch::default();
    lock_project_row(&mut tx, id).await?;
    // … unchanged checks …
    let version = bump_project_version_tx(&mut tx, id).await?;
    feed.bump(id, version, Kind::Frames);
    // … unchanged UPDATEs and record_event_tx …
    feed.frames(id, frame_events_tx(&mut tx, id, &[frame_uuid]).await?);
    commit_and_publish(&state, tx, feed).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
```

(`use crate::feed::{commit_and_publish, wire::Kind, FeedBatch};` at the top of each touched module.)

| # | Site (file:line at `6127951`) | What the batch records |
| ---- | ---- | ---- |
| 1 | `frames.rs:294, 325-326` `announce` | `bump(id, version, Frames)`; `frames(id, frame_events_tx(&mut tx, id, &uuids))`; `holders(hw.finish(&mut tx).await?)` (replaces `let _holders`) |
| 2 | `frames.rs:624, 647-648` `new_version` | `bump(…, Frames)`; `frames(…, &[frame_uuid])`; `holders(hw.finish(…))` |
| 3 | `frames.rs:725, 753-754` `patch_frame` | `bump(…, Frames)`; `frames(…, &[frame_uuid])` |
| 4 | `frames.rs:826, 857-858` `approve` | `bump(…, Frames)`, plus `bump(…, Members)` when `trust`. Rows: the `publish_pending_of_tx` uuids when `trust`, else `[frame_uuid]` |
| 5 | `frames.rs:932, 950-951` `reject` | `bump(…, Frames)`; `frames(…, &[frame_uuid])` (rejected → `more`) |
| 6 | `thresholds.rs:129-131` | `bump(id, v, Thresholds)` |
| 7 | `dictionary.rs:288, 298-299` | `bump(id, v, Dictionary)` |
| 8 | `projects.rs:447-450` `create_project`, `projects.rs:269` `create_project_core(…, feed)` | `create_project_core` records `account(coordinator, Joined, project.id)`; no bump (a new project) |
| 9 | `projects.rs:1356-1358, 1381-1382` `update_project` | `bump(id, v, Meta)`, plus `Members` when the `bump_membership_version_tx` branch ran |
| 10 | `projects.rs:1442, 1452-1453` `put_grid` | `bump(id, v, Grid)` |
| 11 | `members.rs:259, 290, 303-304` `patch_member` | `bump(id, v, Members)` after each of the two bumps (one event, P1/P13 contiguous range) |
| 12 | `members.rs:327` `delete_member_row(…, feed)` via `remove_member` (`:442-443`) and `remove_member_core(…, feed)` | `bump(project, v, Members)`; `account(target, Left, project)` |
| 13 | `members.rs:500, 512-513` `leave_project` | `bump(id, v, Members)`; `account(auth.account_id, Left, id)` |
| 14 | `members.rs:605, 615-616` `handover` | `bump(id, v, Members)` |
| 15 | `members.rs:722, 735-736` `put_trust` | `bump(id, v, Members)`. When granting publishes pending frames, also `bump(id, v, Frames)` and `frames(id, frame_events_tx(…, &published))` |
| 16 | `join_requests.rs:78` `join_member_tx(…, feed)`; callers `:238-239` (open door), `approve_join_core(…, feed)` at `:1046-1047` | `bump(project, v, Members)`; `account(account_id, Joined, project)` |
| 17 | `invites.rs:552-553` `redeem_invite` | via `join_member_tx` (row 16) |
| 18 | `operator.rs:300, 312-314` `patch_project` | `bump(id, v, Meta)` when `status` changed |
| 19 | `operator.rs:356-358` `create_project` | via `create_project_core` (row 8) |
| 20 | `operator.rs:372-405` `delete_project` | Before the DELETE, `SELECT account_id FROM project_members WHERE project_id = $1`, then `account(a, Left, id)` for each (P20) |
| 21 | `operator.rs:546, 565-566` `appoint_coordinator` | `bump(id, v, Members)`; `account(target, Joined, id)` when the `INSERT … ON CONFLICT DO NOTHING` affected a row |
| 22 | `operator.rs:653-656` `decide_join` | via `approve_join_core` (row 16) |
| 23 | `operator.rs:764, 774-775` `remove_member` | via `remove_member_core` (row 12) |
| 24 | `holders.rs` `put_holders_self` (Task 2) | `holders(bump)` where the handler has `let bump = hw.finish(…)`. Replace `tx.commit()` with `commit_and_publish(&state, tx, feed)`, keeping `holder_seq` computed before the call |

- **Helpers.** Helpers that commit nothing (`join_member_tx`, `approve_join_core`, `delete_member_row`, `remove_member_core`, `create_project_core`) take `feed: &mut FeedBatch` as their last parameter. Their callers declare the batch.
- **Other callers.** A caller outside a transaction-owning handler (a test, the scheduler) passes `&mut FeedBatch::default()` and drops it. `rg -n "join_member_tx|approve_join_core|remove_member_core|create_project_core" src tests` lists them.
- **After this step,** `rg -n "versions.invalidate" src` prints only `src/feed/publish.rs`.

- [ ] **Step 5: Run the whole suite**

Run: `DATABASE_URL=… cargo test 2>&1 | tail -15 && cargo build --release 2>&1 | grep -c warning`
Expected: `feed_publish` passes, as does every existing binary (they use `app_with_capture` and never read events); `0` warnings.

- [ ] **Step 6: Commit**

```bash
git add src/feed src/routes src/collab_auth.rs tests/common/mod.rs tests/feed_publish.rs
git commit -m "feat(hub): publish after commit — FeedBatch + commit_and_publish at every bump site; FrameEvent inline rows; account joined/left" \
           -m "Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RYd9tanU4ehWLDXkPv159r"
```

---

### Task 5: The event stream — `GET /me/events`, the presence beat, the driver

**Files:**
- Create: `src/routes/events.rs` (`pub mod events;` in `src/routes/mod.rs`), `src/feed/driver.rs`, and `src/feed/epoch.rs` (basic read/ensure only; Task 9 extends it). `src/feed/mod.rs` gains `pub mod driver; pub mod epoch;`.
- Modify: `src/auth_mw.rs:65-97` (extract `device_identity` from `require_auth`), `src/routes/devices.rs:200-211` (extract `validate_relay_url`).
- Modify: `src/config.rs` (`events_probe_token` from `HUB_EVENTS_PROBE_TOKEN`), `src/routes/mod.rs` (`AppState.events_probe_token` + builder; three routes in the `public` group; `TraceLayer::make_span_with`), `src/lib.rs:33-100` (epoch, driver, graceful shutdown).
- Test: `tests/events.rs` (new), `tests/common/mod.rs` (`SseReader`, `beat`, `leave`, `settle`; re-add `.with_events_probe_token` in `app_with_feed_cfg`).

**Interfaces:**
- Consumes: `FeedHub` (Tasks 3–4), `claims::digest::{Digest, ZERO_HEX}` (Task 2).
- Produces:
  ```rust
  // routes::events
  pub const EVENTS_PATH: &str = "/api/v1/me/events";
  pub const KEEPALIVE: Duration = Duration::from_secs(20);
  pub const RETRY: Duration = Duration::from_millis(3000);
  pub async fn events(State<AppState>, HeaderMap) -> Result<Response, ApiError>;
  pub async fn beat(State<AppState>, Json<BeatBody>) -> Result<StatusCode, ApiError>;   // POST /me/presence
  pub async fn leave(State<AppState>, Json<LeaveBody>) -> Result<StatusCode, ApiError>; // DELETE /me/presence
  // feed::driver
  pub const TICK: Duration = Duration::from_millis(50);
  pub const HEADS_EVERY: Duration = Duration::from_secs(60);
  pub async fn read_heads(db: &PgPool) -> Result<HeadsMap, sqlx::Error>;
  pub async fn heads_tick(db: &PgPool, feed: &FeedHub) -> Result<HeadsMap, sqlx::Error>;
  pub fn spawn(state: AppState) -> tokio::task::JoinHandle<()>;
  // feed::epoch (basic)
  pub const EPOCH_KEY: &str = "epoch";
  pub async fn read_epoch(db: &PgPool) -> Result<Option<String>, sqlx::Error>;
  pub async fn ensure_epoch(db: &PgPool) -> Result<String, sqlx::Error>;
  // auth_mw
  pub(crate) async fn device_identity(state: &AppState, headers: &HeaderMap) -> Result<AuthDevice, ApiError>;
  // devices
  pub(crate) fn validate_relay_url(raw: &str) -> Result<(), ApiError>;
  // AppState
  pub events_probe_token: Option<Arc<str>>;  pub fn with_events_probe_token(self, token: Option<String>) -> Self;
  ```
- Wire: § Wire contract "Event stream" and "Presence beat", exactly.

- [ ] **Step 1: Test helpers**

Append to `tests/common/mod.rs`:

```rust
/// Reads a live SSE response body event by event (spec §4.1 framing).
pub struct SseReader {
    body: axum::body::BodyDataStream,
    buf: String,
}

impl SseReader {
    pub async fn open(app: &axum::Router, token: &str) -> (StatusCode, axum::http::HeaderMap, Option<SseReader>) {
        let resp = app.clone().oneshot(get("/api/v1/me/events", Some(token))).await.expect("router responded");
        let status = resp.status();
        let headers = resp.headers().clone();
        let reader = status.is_success().then(|| SseReader { body: resp.into_body().into_data_stream(), buf: String::new() });
        (status, headers, reader)
    }

    /// The next `(event name, data)`; `None` once the stream ended. Panics
    /// when no byte arrives within 5 s of real time.
    pub async fn next_event(&mut self) -> Option<(String, Value)> {
        loop {
            if let Some(end) = self.buf.find("\n\n") {
                let block: String = self.buf.drain(..end + 2).collect();
                let mut name = None;
                let mut data = String::new();
                for line in block.lines() {
                    if let Some(v) = line.strip_prefix("event: ") {
                        name = Some(v.to_string());
                    } else if let Some(v) = line.strip_prefix("data: ") {
                        data.push_str(v);
                    }
                }
                match name {
                    Some(n) => return Some((n, serde_json::from_str(&data).unwrap_or(Value::Null))),
                    None => continue, // a keepalive comment
                }
            }
            let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), tokio_stream::StreamExt::next(&mut self.body))
                .await
                .expect("no SSE bytes within 5 s");
            match chunk {
                Some(Ok(bytes)) => self.buf.push_str(&String::from_utf8_lossy(&bytes)),
                Some(Err(err)) => panic!("sse body error: {err}"),
                None => return None,
            }
        }
    }

    pub async fn next_named(&mut self, name: &str) -> Value {
        loop {
            let (n, v) = self.next_event().await.unwrap_or_else(|| panic!("stream ended before a `{name}` event"));
            if n == name {
                return v;
            }
        }
    }

    /// `true` when no complete event arrives within `ms` of real time.
    pub async fn quiet_for(&mut self, ms: u64) -> bool {
        if self.buf.contains("event: ") {
            return false;
        }
        match tokio::time::timeout(std::time::Duration::from_millis(ms), tokio_stream::StreamExt::next(&mut self.body)).await {
            Err(_) => true,
            Ok(Some(Ok(bytes))) => {
                self.buf.push_str(&String::from_utf8_lossy(&bytes));
                !self.buf.contains("event: ")
            }
            Ok(_) => false,
        }
    }
}

pub async fn beat(app: &axum::Router, session: &str, serving: Value, relay: Option<&str>) -> (StatusCode, Vec<u8>) {
    send(app, post("/api/v1/me/presence", &json!({"sessionId": session, "serving": serving, "relayUrl": relay}), None)).await
}

pub async fn leave(app: &axum::Router, session: &str) -> StatusCode {
    let req = Request::builder()
        .method("DELETE")
        .uri("/api/v1/me/presence")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&json!({"sessionId": session})).unwrap()))
        .unwrap();
    send(app, req).await.0
}

/// Let spawned stream pumps notice a dropped body (real time, not the feed clock).
pub async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}
```

- [ ] **Step 2: Write the failing integration tests**

`tests/events.rs`:

```rust
//! The event channel end to end over the router (spec 2026-09-25 §4, §12):
//! hello, deltas with versions, contiguous delivery, resync on a gap and on
//! lag, the 60 s state vector, beat timeouts, the grace, session
//! replacement, the warm-up replace, flap damping — presence timers on a
//! manual clock, database I/O real.
mod common;

use std::collections::BTreeSet;
use std::time::Duration;

use athenaeum_hub::feed::wire::Kind;
use athenaeum_hub::feed::{FeedConfig, ProjectBump, ProjectPayload};
use axum::http::StatusCode;
use common::*;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

const Q: Duration = Duration::from_millis(250);
const S: Duration = Duration::from_secs(1);

async fn presence_of(reader: &mut SseReader, device: &str) -> Value {
    loop {
        let ev = reader.next_named("presence").await;
        if let Some(c) = ev["changes"].as_array().unwrap().iter().find(|c| c["device"] == device) {
            return c.clone();
        }
    }
}

/// coord (seed 1) and anna (seed 2, send_receive) in project P.
async fn two_members(fa: &FeedApp) -> (String, String, String) {
    let (coord, _) = register_device(&fa.app, &fa.mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&fa.app, &fa.mailer, "anna@example.com", 2, "Laptop").await;
    let project = create_project_via(&fa.app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap().to_string();
    join_and_approve(&fa.app, &coord, &anna, &id, "Anna", "send_receive").await;
    fa.advance(S); // flush the setup's own events
    (coord, anna, id)
}

#[sqlx::test]
async fn hello_carries_session_epoch_heads_claims_and_presence(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (coord, coord_device) = register_device(&fa.app, &fa.mailer, "coord@example.com", 1, "Desktop").await;
    let project = create_project_via(&fa.app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap().to_string();
    assert_eq!(announce_frames(&fa.app, &coord, &id, 1, 3).await.0, StatusCode::OK);
    let (status, headers, reader) = SseReader::open(&fa.app, &coord).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers["content-type"].to_str().unwrap().starts_with("text/event-stream"));
    assert_eq!(headers["x-accel-buffering"], "no");
    let mut reader = reader.unwrap();
    let hello = reader.next_named("hello").await;
    assert_eq!(hello["sessionId"].as_str().unwrap().len(), 32);
    assert_eq!(hello["epoch"], "test-epoch");
    let account: Uuid = sqlx::query_scalar("SELECT account_id FROM devices WHERE id = $1")
        .bind(Uuid::parse_str(&coord_device).unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(hello["accountId"], account.to_string());
    let version: i64 = sqlx::query_scalar("SELECT version FROM projects WHERE id = $1").bind(Uuid::parse_str(&id).unwrap()).fetch_one(&pool).await.unwrap();
    let p = &hello["projects"][id.as_str()];
    let (count, digest) = digest_of(&[(&frame_uuid(1), 1), (&frame_uuid(2), 1)]);
    assert_eq!(p["version"], version);
    assert_eq!(p["holderSeq"], 1);
    assert_eq!(p["claimCount"], count);
    assert_eq!(p["claimDigest"], digest);
    assert_eq!(p["reportSeq"], 0);
    assert_eq!(p["presence"], json!([{"device": pubkey_b64(1), "serving": false, "relayUrl": null}]));
}

#[sqlx::test]
async fn project_and_holder_events_follow_a_commit(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (coord, _anna, id) = two_members(&fa).await;
    let (_, _, reader) = SseReader::open(&fa.app, &coord).await;
    let mut reader = reader.unwrap();
    let hello = reader.next_named("hello").await;
    let v0 = hello["projects"][id.as_str()]["version"].as_i64().unwrap();
    assert_eq!(announce_frames(&fa.app, &coord, &id, 1, 2).await.0, StatusCode::OK);
    fa.advance(Q);
    let ev = reader.next_named("project").await;
    assert_eq!((ev["prev"].as_i64(), ev["version"].as_i64()), (Some(v0), Some(v0 + 1)), "prev == the hello cursor (I3)");
    fa.advance(S - Q);
    let ev = reader.next_named("holders").await;
    assert_eq!(ev["deltas"][0]["add"], json!([[1, 1]]), "every holding carries its contentVersion (I4)");
}

#[sqlx::test]
async fn a_gap_is_buffered_two_seconds_then_resynced(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (coord, _anna, id) = two_members(&fa).await;
    let pid = Uuid::parse_str(&id).unwrap();
    let (_, _, reader) = SseReader::open(&fa.app, &coord).await;
    let mut reader = reader.unwrap();
    reader.next_named("hello").await;
    let (status, _) = send(&fa.app, put(&format!("/api/v1/projects/{id}/grid"),
        &json!({"scaleArcsec": 1.2, "widthPx": 100, "heightPx": 100, "crval1": 1.0, "crval2": 2.0, "rotationDeg": 0.0}), Some(&coord))).await;
    assert!(status.is_success());
    fa.advance(Q);
    let v = reader.next_named("project").await["version"].as_i64().unwrap();
    // A commit whose predecessor (v+1) never reaches the feed.
    fa.feed.offer_project(ProjectBump { project_id: pid, prev: v + 1, version: v + 2,
        payload: ProjectPayload { kinds: BTreeSet::from([Kind::Meta]), frames: vec![], more: false } });
    fa.advance(S);
    fa.advance(S);
    assert_eq!(reader.next_named("resync").await, json!({"projectId": id, "what": "project"}));
}

#[sqlx::test]
async fn a_lagged_stream_gets_resync_for_both_sides(pool: PgPool) {
    let mut cfg = FeedConfig::default();
    cfg.presence.warmup = Duration::ZERO;
    cfg.channel_capacity = 4;
    let fa = app_with_feed_cfg(pool.clone(), cfg);
    let (coord, _anna, id) = two_members(&fa).await;
    let pid = Uuid::parse_str(&id).unwrap();
    let (_, _, reader) = SseReader::open(&fa.app, &coord).await;
    let mut reader = reader.unwrap();
    reader.next_named("hello").await;
    for i in 0..40 {
        fa.feed.offer_project(ProjectBump { project_id: pid, prev: 1000 + i, version: 1001 + i,
            payload: ProjectPayload { kinds: BTreeSet::from([Kind::Meta]), frames: vec![], more: false } });
        fa.advance(Q); // 40 broadcasts into a 4-slot channel before the pump runs
    }
    assert_eq!(reader.next_named("resync").await["what"], "project");
    let (name, ev) = reader.next_event().await.unwrap();
    assert_eq!((name.as_str(), ev["what"].as_str()), ("resync", Some("holders")));
}

#[sqlx::test]
async fn the_versions_vector_repairs_lost_wake_ups(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (coord, _anna, id) = two_members(&fa).await;
    let (_, _, reader) = SseReader::open(&fa.app, &coord).await;
    let mut reader = reader.unwrap();
    let hello = reader.next_named("hello").await;
    let heads = athenaeum_hub::feed::driver::heads_tick(&pool, &fa.feed).await.unwrap();
    let ev = reader.next_named("versions").await;
    let p = &hello["projects"][id.as_str()];
    assert_eq!(ev[id.as_str()], json!([p["version"], p["holderSeq"]]));
    assert_eq!(heads[&Uuid::parse_str(&id).unwrap()], [p["version"].as_i64().unwrap(), p["holderSeq"].as_i64().unwrap()]);
}

#[sqlx::test]
async fn a_closed_stream_drops_out_after_the_ten_second_grace(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (coord, anna, _id) = two_members(&fa).await;
    let (_, _, rb) = SseReader::open(&fa.app, &anna).await;
    let mut rb = rb.unwrap();
    rb.next_named("hello").await;
    let (_, _, ra) = SseReader::open(&fa.app, &coord).await;
    let mut ra = ra.unwrap();
    ra.next_named("hello").await;
    fa.advance(S);
    assert_eq!(presence_of(&mut rb, &pubkey_b64(1)).await["connected"], true);
    drop(ra);
    settle().await;
    fa.advance(Duration::from_secs(9));
    assert!(rb.quiet_for(200).await, "inside the grace (spec §4.2)");
    fa.advance(S);
    fa.advance(S);
    assert_eq!(presence_of(&mut rb, &pubkey_b64(1)).await, json!({"device": pubkey_b64(1), "connected": false, "serving": false, "relayUrl": null}));
}

#[sqlx::test]
async fn forty_seconds_without_a_beat_drops_a_device_with_its_stream_open(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (coord, anna, _id) = two_members(&fa).await;
    let (_, _, rb) = SseReader::open(&fa.app, &anna).await;
    let mut rb = rb.unwrap();
    let hb = rb.next_named("hello").await;
    let (_, _, ra) = SseReader::open(&fa.app, &coord).await;
    let mut ra = ra.unwrap();
    ra.next_named("hello").await;
    fa.advance(S);
    presence_of(&mut rb, &pubkey_b64(1)).await;
    // Anna keeps beating; coord never does.
    for _ in 0..3 {
        fa.advance(Duration::from_secs(13));
        assert_eq!(beat(&fa.app, hb["sessionId"].as_str().unwrap(), json!({}), None).await.0, StatusCode::NO_CONTENT);
    }
    fa.advance(S); // 40 s since coord's stream opened
    fa.advance(S);
    assert_eq!(presence_of(&mut rb, &pubkey_b64(1)).await["connected"], false);
    // The expired session's stream is closed: it drains to its end (a
    // stream that stayed open would trip next_event's 5 s timeout).
    while let Some((name, _)) = ra.next_event().await {
        assert_ne!(name, "hello");
    }
}

#[sqlx::test]
async fn a_second_stream_replaces_the_first_and_its_session_is_gone(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (coord, _anna, _id) = two_members(&fa).await;
    let (_, _, r1) = SseReader::open(&fa.app, &coord).await;
    let mut r1 = r1.unwrap();
    let s1 = r1.next_named("hello").await["sessionId"].as_str().unwrap().to_string();
    let (_, _, r2) = SseReader::open(&fa.app, &coord).await;
    let mut r2 = r2.unwrap();
    let s2 = r2.next_named("hello").await["sessionId"].as_str().unwrap().to_string();
    settle().await;
    while let Some((name, _)) = r1.next_event().await {
        assert_ne!(name, "hello", "the older stream ends");
    }
    let (status, body) = beat(&fa.app, &s1, json!({}), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(as_json(&body)["error"], "session_gone");
    assert_eq!(beat(&fa.app, &s2, json!({}), Some("https://relay.example.com")).await.0, StatusCode::NO_CONTENT);
    let (status, body) = beat(&fa.app, &s2, json!({}), Some("http://relay.example.com")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&body).contains("https"));
}

#[sqlx::test]
async fn delete_presence_is_an_immediate_clean_exit(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (coord, anna, id) = two_members(&fa).await;
    let (_, _, rb) = SseReader::open(&fa.app, &anna).await;
    let mut rb = rb.unwrap();
    rb.next_named("hello").await;
    let (_, _, ra) = SseReader::open(&fa.app, &coord).await;
    let mut ra = ra.unwrap();
    let sa = ra.next_named("hello").await["sessionId"].as_str().unwrap().to_string();
    assert_eq!(beat(&fa.app, &sa, json!({ id.as_str(): true }), None).await.0, StatusCode::NO_CONTENT);
    fa.advance(S);
    assert_eq!(presence_of(&mut rb, &pubkey_b64(1)).await["serving"], true);
    assert_eq!(leave(&fa.app, &sa).await, StatusCode::NO_CONTENT);
    assert_eq!(leave(&fa.app, &sa).await, StatusCode::NO_CONTENT, "idempotent (P14)");
    fa.advance(S);
    assert_eq!(presence_of(&mut rb, &pubkey_b64(1)).await["connected"], false, "no 10 s grace for a clean exit");
    settle().await;
    while let Some((name, _)) = ra.next_event().await {
        assert_ne!(name, "hello", "the stream of a clean exit ends");
    }
}

#[sqlx::test]
async fn the_warm_up_ends_with_one_replace_event(pool: PgPool) {
    let fa = app_with_feed_cfg(pool.clone(), FeedConfig::default()); // 30 s warm-up
    let (coord, _anna, id) = two_members(&fa).await;
    let (_, _, ra) = SseReader::open(&fa.app, &coord).await;
    let mut ra = ra.unwrap();
    let hello = ra.next_named("hello").await;
    assert_eq!(hello["projects"][id.as_str()]["presence"], json!([]), "no presence during the warm-up");
    fa.advance(Duration::from_secs(29));
    let ev = ra.next_named("presence").await;
    assert_eq!(ev["replace"], true);
    assert_eq!(ev["changes"], json!([{"device": pubkey_b64(1), "connected": true, "serving": false, "relayUrl": null}]));
}

#[sqlx::test]
async fn flap_damping_delays_the_third_offline_broadcast(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (coord, anna, _id) = two_members(&fa).await;
    let (_, _, rb) = SseReader::open(&fa.app, &anna).await;
    let mut rb = rb.unwrap();
    rb.next_named("hello").await;
    let a = pubkey_b64(1);
    for cycle in 0..3 {
        let (_, _, ra) = SseReader::open(&fa.app, &coord).await;
        let mut ra = ra.unwrap();
        ra.next_named("hello").await;
        fa.advance(S);
        assert_eq!(presence_of(&mut rb, &a).await["connected"], true);
        drop(ra);
        settle().await;
        fa.advance(Duration::from_secs(10));
        fa.advance(S);
        if cycle < 2 {
            assert_eq!(presence_of(&mut rb, &a).await["connected"], false);
        }
    }
    fa.advance(Duration::from_secs(58));
    assert!(rb.quiet_for(200).await, "the third offline is delayed 60 s (spec §4.2)");
    fa.advance(S);
    fa.advance(S);
    assert_eq!(presence_of(&mut rb, &a).await["connected"], false);
}

#[sqlx::test]
async fn an_account_joining_a_project_subscribes_its_open_stream(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (coord, anna, _p) = two_members(&fa).await;
    let q = create_project_via(&fa.app, &coord, "Q", false).await;
    let qid = q["id"].as_str().unwrap().to_string();
    let (_, _, rb) = SseReader::open(&fa.app, &anna).await;
    let mut rb = rb.unwrap();
    rb.next_named("hello").await;
    join_and_approve(&fa.app, &coord, &anna, &qid, "Anna", "send").await;
    assert_eq!(rb.next_named("account").await, json!({"kind": "joined", "projectId": qid}));
    fa.advance(Q);
    assert_eq!(rb.next_named("project").await["projectId"], qid.as_str());
}

#[sqlx::test]
async fn probe_token_perseus_and_bad_tokens(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (_, _, probe) = SseReader::open(&fa.app, "probe-secret").await;
    assert_eq!(probe.unwrap().next_named("hello").await, json!({"probe": true, "epoch": "test-epoch"}));
    let (status, _) = send(&fa.app, get("/api/v1/me/events", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(&fa.app, get("/api/v1/me/events", Some("not-a-token"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (perseus, _) = register_device_with_capability(&fa.app, &fa.mailer, "p@example.com", 9, "Capture", "perseus").await;
    let (status, _, _) = SseReader::open(&fa.app, &perseus).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "P17");
    let (status, body) = beat(&fa.app, "zz", json!({}), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&body).contains("sessionId"));
}
```

Run: `DATABASE_URL=… cargo test --test events` → FAIL (404 on `/api/v1/me/events`).

- [ ] **Step 3: Refactors the route needs**

`src/auth_mw.rs`: move the body of `require_auth` into `device_identity` and make `require_auth` call it:

```rust
/// Resolve a device bearer token to its identity (401 otherwise) and stamp
/// `last_seen_at`. Shared by the `require_auth` middleware and the event
/// stream, which authenticates itself (it also accepts the probe token).
pub(crate) async fn device_identity(state: &AppState, headers: &axum::http::HeaderMap) -> Result<AuthDevice, ApiError> {
    let token = bearer_token(headers).ok_or(ApiError::Status(StatusCode::UNAUTHORIZED))?;
    let row = sqlx::query_as::<_, AuthRow>(
        "SELECT d.id, d.account_id, d.role FROM devices d \
         JOIN accounts a ON a.id = d.account_id \
         WHERE d.token_hash = $1 AND d.revoked_at IS NULL AND a.blocked_at IS NULL",
    )
    .bind(security::hash_token(&token))
    .fetch_optional(&state.db)
    .await?;
    let Some(device) = row else {
        return Err(ApiError::Status(StatusCode::UNAUTHORIZED));
    };
    stamp_device_last_seen(state, device.id).await;
    Ok(AuthDevice { device_id: device.id, account_id: device.account_id, role: device.role })
}

pub async fn require_auth(State(state): State<AppState>, mut req: Request, next: Next) -> Result<Response, ApiError> {
    let device = device_identity(&state, req.headers()).await?;
    req.extensions_mut().insert(device);
    Ok(next.run(req).await)
}
```

`src/routes/devices.rs`: move `:200-211` into the function below and call `validate_relay_url(raw)?` from `update_self_address`.

```rust
/// A home relay URL must parse, use https, and name a host (shared by
/// `PUT /devices/self/address` and the presence beat).
pub(crate) fn validate_relay_url(raw: &str) -> Result<(), ApiError> {
    let uri: Uri = raw.parse().map_err(|_| ApiError::bad_request("homeRelayUrl is not a valid URL"))?;
    if uri.scheme_str() != Some("https") {
        return Err(ApiError::bad_request("homeRelayUrl must use https"));
    }
    if uri.host().is_none() {
        return Err(ApiError::bad_request("homeRelayUrl must have a host"));
    }
    Ok(())
}
```

`src/config.rs`:
- Add the field `pub events_probe_token: Option<String>`, documented as "`HUB_EVENTS_PROBE_TOKEN` — optional; the bearer the post-deploy probe opens `/me/events` with (P17). Blank reads as unset. Never logged."
- Read it like `HUB_RELAY_AUTH_TOKEN` (trim, blank → `None`).
- The `Debug` impl prints `.field("events_probe_token", &self.events_probe_token.as_ref().map(|_| "<set>"))`.

`src/routes/mod.rs`:
- Add the field `pub events_probe_token: Option<Arc<str>>` (doc: "the deploy probe's bearer for `/me/events`; `None` disables it"), with `events_probe_token: None` in `new`, and the builder below.

```rust
    pub fn with_events_probe_token(mut self, token: Option<String>) -> Self {
        self.events_probe_token = token.map(Arc::from);
        self
    }
```

- [ ] **Step 4: `src/routes/events.rs`**

```rust
//! The event channel endpoints (spec 2026-09-25 §4.1–§4.3; the plan's
//! § Wire contract):
//! - `GET /api/v1/me/events` — one SSE stream per device;
//! - `POST`/`DELETE /api/v1/me/presence` — the session-authenticated beat
//!   and clean exit, mounted outside token auth, no database query.
//!
//! Streams hold no database connection: the handler authenticates, reads the
//! hello state, releases the connection and hands the stream to a spawned
//! pump.

use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};
use tokio_stream::{StreamExt, StreamMap};
use uuid::Uuid;

use crate::claims::digest::{Digest, ZERO_HEX};
use crate::error::ApiError;
use crate::feed::presence::SessionId;
use crate::feed::wire::{AccountEvent, AccountKind, HelloEvent, HelloProject, ResyncWhat, WireEvent};
use crate::feed::{Control, FeedHub, HeadsMap, StreamHandle};
use crate::routes::AppState;

pub const EVENTS_PATH: &str = "/api/v1/me/events";
pub const KEEPALIVE: Duration = Duration::from_secs(20);
pub const RETRY: Duration = Duration::from_millis(3000);
const PROBE_HOLD: Duration = Duration::from_secs(10);
const OUT_BUFFER: usize = 256;

fn sse_response(rx: mpsc::Receiver<Event>) -> Response {
    let stream = ReceiverStream::new(rx).map(Ok::<Event, Infallible>);
    (
        [(HeaderName::from_static("x-accel-buffering"), HeaderValue::from_static("no"))],
        Sse::new(stream).keep_alive(KeepAlive::new().interval(KEEPALIVE)),
    )
        .into_response()
}

fn to_event(wire: &WireEvent) -> Event {
    Event::default().event(wire.name).data(&*wire.data)
}

fn account_event(kind: AccountKind, project_id: Uuid) -> Event {
    to_event(&WireEvent::new("account", &AccountEvent { kind, project_id }))
}

#[derive(sqlx::FromRow)]
struct DeviceRow {
    pubkey: Vec<u8>,
    capability: String,
    relay_url: Option<String>,
}

#[derive(sqlx::FromRow)]
struct HeadRow {
    id: Uuid,
    version: i64,
    seq: i64,
    count: Option<i32>,
    digest: Option<Vec<u8>>,
    max_report_seq: Option<i64>,
}

/// `GET /api/v1/me/events` (spec §4.1). 401 without a valid device token,
/// 403 for a perseus device (P17). The configured probe token gets a probe
/// stream instead.
#[tracing::instrument(skip_all)]
pub async fn events(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    if probe_matches(&state, &headers) {
        return Ok(probe_stream(&state));
    }
    let device = crate::auth_mw::device_identity(&state, &headers).await?;
    let row = sqlx::query_as::<_, DeviceRow>(
        "SELECT pubkey, capability, endpoint_addr->>'homeRelayUrl' AS relay_url FROM devices WHERE id = $1",
    )
    .bind(device.device_id)
    .fetch_one(&state.db)
    .await?;
    if row.capability != "athenaeum" {
        return Err(ApiError::Status(StatusCode::FORBIDDEN));
    }
    let project_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT project_id FROM project_members WHERE account_id = $1 ORDER BY project_id")
            .bind(device.account_id)
            .fetch_all(&state.db)
            .await?;
    // Subscribe BEFORE reading the heads: nothing committed after them can be missed (I3).
    let handle = state.feed.open_stream(
        device.device_id,
        device.account_id,
        crate::security::encode_pubkey(&row.pubkey),
        row.relay_url,
        &project_ids,
    );
    let session = handle.session;
    let heads = match sqlx::query_as::<_, HeadRow>(
        "SELECT p.id, p.version, c.seq, g.count, g.digest, g.max_report_seq FROM projects p \
         JOIN project_holder_cursor c ON c.project_id = p.id \
         LEFT JOIN device_project_digest g ON g.project_id = p.id AND g.device_id = $2 \
         WHERE p.id = ANY($1)",
    )
    .bind(&project_ids)
    .bind(device.device_id)
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows,
        Err(err) => {
            state.feed.detach(session);
            return Err(err.into());
        }
    };
    let projects: BTreeMap<Uuid, HelloProject> = heads
        .into_iter()
        .map(|h| {
            let (claim_count, claim_digest) = match (h.count, h.digest) {
                (Some(count), Some(bytes)) => {
                    let d = Digest::from_row(count, &bytes);
                    (d.count, d.to_hex())
                }
                _ => (0, ZERO_HEX.to_string()),
            };
            let presence = state.feed.presence_for(h.id);
            (h.id, HelloProject { version: h.version, holder_seq: h.seq, claim_count, claim_digest, report_seq: h.max_report_seq.unwrap_or(0), presence })
        })
        .collect();
    let hello = HelloEvent { session_id: session.to_hex(), epoch: state.feed.epoch(), account_id: device.account_id, projects };
    let first = to_event(&WireEvent::new("hello", &hello)).retry(RETRY);
    tracing::info!(device_id = %device.device_id, session_id = %session, projects = project_ids.len(), "event stream opened");
    let (out_tx, out_rx) = mpsc::channel(OUT_BUFFER);
    tokio::spawn(pump(state.feed.clone(), handle, first, out_tx));
    Ok(sse_response(out_rx))
}

/// Forward the stream's broadcasts, mailbox and heads into its body until
/// the client goes away or the hub closes it. A lagged receiver never
/// replays: it emits `resync` for both sides (spec §4.5).
async fn pump(feed: FeedHub, handle: StreamHandle, hello: Event, out: mpsc::Sender<Event>) {
    let StreamHandle { session, mut mailbox, projects, mut heads } = handle;
    let mut subs: StreamMap<Uuid, BroadcastStream<Arc<WireEvent>>> = StreamMap::new();
    for (project_id, rx) in projects {
        subs.insert(project_id, BroadcastStream::new(rx));
    }
    let outcome: Result<&'static str, &'static str> = async {
        out.send(hello).await.map_err(|_| "client gone")?;
        loop {
            let event = tokio::select! {
                _ = out.closed() => return Err("client gone"),
                control = mailbox.recv() => match control {
                    None | Some(Control::Close) => return Ok("closed by the hub"),
                    Some(Control::Joined(project_id)) => {
                        subs.insert(project_id, BroadcastStream::new(feed.subscribe(project_id)));
                        account_event(AccountKind::Joined, project_id)
                    }
                    Some(Control::Left(project_id)) => {
                        subs.remove(&project_id);
                        account_event(AccountKind::Left, project_id)
                    }
                },
                Some((project_id, item)) = subs.next() => match item {
                    Ok(wire) => to_event(&wire),
                    Err(BroadcastStreamRecvError::Lagged(lagged)) => {
                        tracing::warn!(session_id = %session, project_id = %project_id, lagged, "event stream lagged, resync sent");
                        out.send(to_event(&WireEvent::resync(project_id, ResyncWhat::Project))).await.map_err(|_| "client gone")?;
                        to_event(&WireEvent::resync(project_id, ResyncWhat::Holders))
                    }
                },
                head = heads.recv() => match head {
                    Ok(map) => match versions_event(&map, &subs) {
                        Some(event) => event,
                        None => continue,
                    },
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return Ok("feed closed"),
                },
            };
            out.send(event).await.map_err(|_| "client gone")?;
        }
    }
    .await;
    let (Ok(reason) | Err(reason)) = outcome;
    tracing::info!(session_id = %session, reason, "event stream closed");
    feed.detach(session);
}

fn versions_event(heads: &HeadsMap, subs: &StreamMap<Uuid, BroadcastStream<Arc<WireEvent>>>) -> Option<Event> {
    let mine: BTreeMap<Uuid, [i64; 2]> = subs.keys().filter_map(|p| heads.get(p).map(|h| (*p, *h))).collect();
    (!mine.is_empty()).then(|| to_event(&WireEvent::new("versions", &mine)))
}

fn probe_matches(state: &AppState, headers: &HeaderMap) -> bool {
    match (state.events_probe_token.as_deref(), crate::auth_mw::bearer_token(headers)) {
        (Some(expected), Some(token)) => crate::security::constant_time_eq(expected.as_bytes(), token.as_bytes()),
        _ => false,
    }
}

/// The deploy probe (spec §5.4, P17): `hello {probe, epoch}` at once, then
/// closed after 10 s — no session, no presence.
fn probe_stream(state: &AppState) -> Response {
    let hello = Event::default()
        .event("hello")
        .data(serde_json::json!({"probe": true, "epoch": state.feed.epoch()}).to_string())
        .retry(RETRY);
    let (tx, rx) = mpsc::channel(1);
    tokio::spawn(async move {
        if tx.send(hello).await.is_ok() {
            tokio::time::sleep(PROBE_HOLD).await;
        }
    });
    tracing::debug!("event stream probe served");
    sse_response(rx)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeatBody {
    pub session_id: String,
    #[serde(default)]
    pub serving: HashMap<Uuid, bool>,
    #[serde(default)]
    pub relay_url: Option<String>,
}

fn parse_session(raw: &str) -> Result<SessionId, ApiError> {
    SessionId::parse(raw).ok_or_else(|| ApiError::bad_request("sessionId must be 32 lowercase hex chars"))
}

/// `POST /api/v1/me/presence` — the 15 s beat (spec §4.2), authenticated by
/// the session alone, in memory. 409 `session_gone` makes the device reopen
/// its stream at once.
#[tracing::instrument(skip_all, level = "debug")]
pub async fn beat(State(state): State<AppState>, Json(body): Json<BeatBody>) -> Result<StatusCode, ApiError> {
    let session = parse_session(&body.session_id)?;
    if let Some(url) = &body.relay_url {
        crate::routes::devices::validate_relay_url(url)?;
    }
    state
        .feed
        .beat(session, body.serving, body.relay_url)
        .map_err(|_| ApiError::Message(StatusCode::CONFLICT, "session_gone".into()))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaveBody {
    pub session_id: String,
}

/// `DELETE /api/v1/me/presence` — clean exit: offline at once, stream
/// closed; 204 for an unknown session too (P14).
#[tracing::instrument(skip_all)]
pub async fn leave(State(state): State<AppState>, Json(body): Json<LeaveBody>) -> Result<StatusCode, ApiError> {
    let session = parse_session(&body.session_id)?;
    let known = state.feed.leave(session);
    tracing::info!(session_id = %session, known, "presence left");
    Ok(StatusCode::NO_CONTENT)
}
```

- [ ] **Step 5: Driver, basic epoch, router, startup**

`src/feed/epoch.rs` (Task 9 extends it):

```rust
//! The feed epoch (spec 2026-09-25 §4.4): an opaque string in `hub_meta`
//! (`migrations/0021_hub_meta.sql`) that every cursor is paired with. A
//! client on a different epoch reloads every snapshot.

use sqlx::PgPool;

pub const EPOCH_KEY: &str = "epoch";

pub async fn read_epoch(db: &PgPool) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT value FROM hub_meta WHERE key = $1").bind(EPOCH_KEY).fetch_optional(db).await
}

/// The stored epoch, created (a fresh UUID) on first boot.
pub async fn ensure_epoch(db: &PgPool) -> Result<String, sqlx::Error> {
    sqlx::query("INSERT INTO hub_meta (key, value) VALUES ($1, gen_random_uuid()::text) ON CONFLICT (key) DO NOTHING")
        .bind(EPOCH_KEY)
        .execute(db)
        .await?;
    sqlx::query_scalar("SELECT value FROM hub_meta WHERE key = $1").bind(EPOCH_KEY).fetch_one(db).await
}
```

`src/feed/driver.rs`:

```rust
//! The feed driver: ticks `FeedHub` every 50 ms (every presence and
//! coalescer timer lives in `tick`, P24) and broadcasts the `versions` state
//! vector every 60 s from one read of every project's heads (spec §4.3, P21).

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::PgPool;
use uuid::Uuid;

use crate::feed::{FeedHub, HeadsMap};
use crate::routes::AppState;

pub const TICK: Duration = Duration::from_millis(50);
pub const HEADS_EVERY: Duration = Duration::from_secs(60);

pub async fn read_heads(db: &PgPool) -> Result<HeadsMap, sqlx::Error> {
    let rows: Vec<(Uuid, i64, i64)> =
        sqlx::query_as("SELECT p.id, p.version, c.seq FROM projects p JOIN project_holder_cursor c ON c.project_id = p.id")
            .fetch_all(db)
            .await?;
    Ok(rows.into_iter().map(|(id, version, seq)| (id, [version, seq])).collect())
}

/// Read every head and broadcast the `versions` vector; returns the heads.
pub async fn heads_tick(db: &PgPool, feed: &FeedHub) -> Result<HeadsMap, sqlx::Error> {
    let heads = read_heads(db).await?;
    feed.broadcast_heads(Arc::new(heads.clone()));
    Ok(heads)
}

pub fn spawn(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_heads = Instant::now();
        loop {
            interval.tick().await;
            let feed = state.feed.clone();
            if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| feed.tick())) {
                let reason = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                tracing::error!(error = %reason, "feed tick panicked");
            }
            if last_heads.elapsed() >= HEADS_EVERY {
                last_heads = Instant::now();
                if let Err(err) = heads_tick(&state.db, &state.feed).await {
                    tracing::error!(error = %err, "feed heads tick failed");
                }
            }
        }
    })
}
```

`src/routes/mod.rs`:
- In the `public` router add:

```rust
        .route(events::EVENTS_PATH, get(events::events))
        .route("/api/v1/me/presence", post(events::beat).delete(events::leave))
```

- Replace `.layer(TraceLayer::new_for_http())` with:

```rust
        .layer(TraceLayer::new_for_http().make_span_with(|req: &axum::http::Request<axum::body::Body>| {
            if req.uri().path() == events::EVENTS_PATH {
                // Streams live for hours: a debug-level span (spec §4.5).
                tracing::debug_span!("event_stream", method = %req.method(), path = %req.uri().path())
            } else {
                tower_http::trace::DefaultMakeSpan::new().make_span(req)
            }
        }))
```

  with `use tower_http::trace::{MakeSpan, TraceLayer};`.

`src/lib.rs::run`:
- After `db::run_migrations(&pool).await?;` add:

```rust
    let epoch = feed::epoch::ensure_epoch(&pool).await.context("failed to read the feed epoch")?;
```

- Chain `.with_events_probe_token(config.events_probe_token.clone())` onto the state builder. Log `tracing::info!("event stream probe enabled (HUB_EVENTS_PROBE_TOKEN set)")` when it is `Some`.
- After the state is built:

```rust
    state.feed.set_epoch(epoch.clone());
    tracing::info!(epoch = %epoch, "feed epoch");
    feed::driver::spawn(state.clone());
```

- Replace the `axum::serve(…)` call with:

```rust
    let feed = state.feed.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            // P23: SSE streams never end by themselves; close them so the drain completes.
            feed.shutdown();
        })
        .await
        .context("hub server error")?;
```

`tests/common/mod.rs`: re-add `.with_events_probe_token(Some("probe-secret".into()))` in `app_with_feed_cfg`.

- [ ] **Step 6: Run the whole suite**

Run: `DATABASE_URL=… cargo test 2>&1 | tail -15 && cargo build --release 2>&1 | grep -c warning`
Expected: `events` passes in full, the rest stay green; `0` warnings. Then a manual smoke against a local hub:

```bash
DATABASE_URL=postgres://hub:hub@localhost:5432/hub HUB_EVENTS_PROBE_TOKEN=probe cargo run &
sleep 3; timeout 2 curl -sN -H 'Authorization: Bearer probe' http://127.0.0.1:8080/api/v1/me/events | head -c 12; echo; kill %1
```

Expected output: `event: hello`. Then `kill %1` returns promptly, because graceful shutdown closes the streams.

- [ ] **Step 7: Commit**

```bash
git add src/routes/events.rs src/routes/mod.rs src/routes/devices.rs src/auth_mw.rs src/config.rs src/lib.rs src/feed tests/common/mod.rs tests/events.rs
git commit -m "feat(hub): event stream — GET /me/events (hello, keepalive 20 s, X-Accel-Buffering), session beat POST/DELETE /me/presence, 50 ms driver, 60 s versions vector, probe token, graceful stream shutdown" \
           -m "Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RYd9tanU4ehWLDXkPv159r"
```

---

### Task 6: Holders read side — snapshot, deltas since a cursor, tombstone prune and floor

**Files:**
- Modify: `src/error.rs:16-93`. Add `ApiError::Body(StatusCode, serde_json::Value)`: a typed JSON refusal, logged at `debug`.
- Modify: `src/routes/holders.rs`. Add `run_length`, `holders_snapshot`, `holder_deltas`, `prune_tombstones`.
- Modify: `src/routes/mod.rs`. Two routes in `account_protected`.
- Modify: `src/scheduler.rs:62-108`. `TickReport.tombstones_pruned`; the tick calls `prune_tombstones`.
- Test: `tests/holders_read.rs` (new).

**Interfaces:**
- Consumes: `FeedHub::{epoch, live_relay}` (Task 3); claim columns (Task 1).
- Produces:
  ```rust
  // error
  ApiError::Body(StatusCode, serde_json::Value)
  // routes::holders
  pub fn run_length(claims: &[(i32, i32)]) -> Vec<[i32; 3]>;           // sorted (frame_seq, cv) → [[start, len, cv]]
  pub async fn holders_snapshot(State<AppState>, Extension<AuthAccount>, Path<Uuid>) -> Result<Json<HoldersSnapshot>, ApiError>;
  pub async fn holder_deltas(State<AppState>, Extension<AuthAccount>, Path<Uuid>, Query<SinceQuery>) -> Result<Json<HolderDeltaPage>, ApiError>;
  pub const TOMBSTONE_RETENTION_DAYS: i64 = 7;
  pub const DELTA_PAGE: i64 = 5000;
  pub async fn prune_tombstones(db: &PgPool, now: DateTime<Utc>) -> Result<u64, sqlx::Error>;
  // scheduler
  pub struct TickReport { pub stale_marked: u64, pub digests_sent: usize, pub tombstones_pruned: u64 }
  ```
- Wire: § Wire contract "Holders — reads", exactly.

- [ ] **Step 1: Write the failing tests**

`tests/holders_read.rs`:

```rust
//! Holder snapshot and delta reads (spec 2026-09-25 §5.3, §6.1; I3; plan
//! rulings P5, P26, P27).
mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

async fn head(pool: &PgPool, id: &str) -> (i64, i64) {
    sqlx::query_as("SELECT seq, floor FROM project_holder_cursor WHERE project_id = $1")
        .bind(Uuid::parse_str(id).unwrap()).fetch_one(pool).await.unwrap()
}

/// coord (seed 1) announces frames 1..=3; bob (seed 3, send_receive) claims 1 and 2.
async fn setup(fa: &FeedApp) -> (String, String, String) {
    let (coord, _) = register_device(&fa.app, &fa.mailer, "coord@example.com", 1, "Desktop").await;
    let (bob, _) = register_device(&fa.app, &fa.mailer, "bob@example.com", 3, "PC").await;
    let project = create_project_via(&fa.app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap().to_string();
    join_and_approve(&fa.app, &coord, &bob, &id, "Bob", "send_receive").await;
    assert_eq!(announce_frames(&fa.app, &coord, &id, 1, 4).await.0, StatusCode::OK);
    let (f1, f2) = (frame_uuid(1), frame_uuid(2));
    let (status, _) = report_holders(&fa.app, &bob, &id, 1, false, &[(&f1, 1), (&f2, 1)], &[], &[(&f1, 1), (&f2, 1)]).await;
    assert_eq!(status, StatusCode::OK);
    (coord, bob, id)
}

#[sqlx::test]
async fn the_snapshot_reads_frames_devices_and_run_length_claims(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (_coord, bob, id) = setup(&fa).await;
    let (status, body) = send(&fa.app, get(&format!("/api/v1/projects/{id}/holders/snapshot"), Some(&bob))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let snap = as_json(&body);
    let version: i64 = sqlx::query_scalar("SELECT version FROM projects WHERE id = $1").bind(Uuid::parse_str(&id).unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(snap["epoch"], "test-epoch");
    assert_eq!(snap["holderSeq"], head(&pool, &id).await.0);
    assert_eq!(snap["version"], version);
    assert_eq!(snap["frames"], json!([
        {"seq": 1, "uuid": frame_uuid(1), "contentVersion": 1},
        {"seq": 2, "uuid": frame_uuid(2), "contentVersion": 1},
        {"seq": 3, "uuid": frame_uuid(3), "contentVersion": 1},
    ]));
    assert_eq!(snap["devices"], json!([
        {"device": pubkey_b64(1), "displayName": "Coord", "relayUrl": null, "claims": [[1, 3, 1]]},
        {"device": pubkey_b64(3), "displayName": "Bob", "relayUrl": null, "claims": [[1, 2, 1]]},
    ]));
}

#[sqlx::test]
async fn pending_frames_are_hidden_but_their_claims_are_listed_by_seq(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (coord, _) = register_device(&fa.app, &fa.mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&fa.app, &fa.mailer, "anna@example.com", 2, "Laptop").await;
    let (bob, _) = register_device(&fa.app, &fa.mailer, "bob@example.com", 3, "PC").await;
    let project = create_project_via(&fa.app, &coord, "P", true).await;
    let id = project["id"].as_str().unwrap().to_string();
    join_and_approve(&fa.app, &coord, &anna, &id, "Anna", "send").await;
    join_and_approve(&fa.app, &coord, &bob, &id, "Bob", "send_receive").await;
    assert_eq!(announce_frames(&fa.app, &anna, &id, 1, 2).await.1["state"], "pending");
    let (_, body) = send(&fa.app, get(&format!("/api/v1/projects/{id}/holders/snapshot"), Some(&bob))).await;
    let snap = as_json(&body);
    assert_eq!(snap["frames"], json!([]), "bob cannot see Anna's pending frame");
    let anna_dev = snap["devices"].as_array().unwrap().iter().find(|d| d["device"] == pubkey_b64(2)).unwrap().clone();
    assert_eq!(anna_dev["claims"], json!([[1, 1, 1]]), "claims are listed by frameSeq regardless (P5)");
}

#[sqlx::test]
async fn deltas_since_a_cursor_page_through_current_claim_state(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (_coord, bob, id) = setup(&fa).await;
    let (seq_after_add, _) = head(&pool, &id).await;
    let before_add = seq_after_add - 1;
    let f1 = frame_uuid(1);
    let f2 = frame_uuid(2);
    report_holders(&fa.app, &bob, &id, 2, false, &[], &[&f1], &[(&f2, 1)]).await;
    let (status, body) = send(&fa.app, get(&format!("/api/v1/projects/{id}/holders?since={before_add}"), Some(&bob))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let page = as_json(&body);
    assert_eq!(page["holderSeq"], seq_after_add + 1);
    assert_eq!(page["deltas"], json!([{"device": pubkey_b64(3), "add": [[2, 1]], "rm": [1]}]), "each claim once, in its current state (P26)");
    assert_eq!(page["hasMore"], false);
    let (_, body) = send(&fa.app, get(&format!("/api/v1/projects/{id}/holders?since={seq_after_add}"), Some(&bob))).await;
    assert_eq!(as_json(&body)["deltas"], json!([{"device": pubkey_b64(3), "add": [], "rm": [1]}]));
    // Paging: one row per page.
    let (_, body) = send(&fa.app, get(&format!("/api/v1/projects/{id}/holders?since={before_add}&limit=1"), Some(&bob))).await;
    let p1 = as_json(&body);
    assert_eq!(p1["hasMore"], true);
    let next = &p1["next"];
    assert_eq!(next["since"], before_add);
    let after = next["after"].as_str().unwrap();
    let (_, body) = send(&fa.app, get(&format!("/api/v1/projects/{id}/holders?since={before_add}&after={after}&limit=1"), Some(&bob))).await;
    let p2 = as_json(&body);
    assert_eq!(p2["hasMore"], false);
    let mut seen: Vec<serde_json::Value> = p1["deltas"].as_array().unwrap().clone();
    seen.extend(p2["deltas"].as_array().unwrap().clone());
    assert_eq!(seen.len(), 2, "two rows over two pages");
}

#[sqlx::test]
async fn below_the_floor_ahead_of_the_head_or_on_another_epoch_is_410(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (_coord, bob, id) = setup(&fa).await;
    let f1 = frame_uuid(1);
    let f2 = frame_uuid(2);
    report_holders(&fa.app, &bob, &id, 2, false, &[], &[&f1], &[(&f2, 1)]).await;
    let (tombstone_seq,): (i64,) = sqlx::query_as("SELECT changed_seq FROM frame_holders WHERE removed AND frame_uuid = $1")
        .bind(Uuid::parse_str(&f1).unwrap()).fetch_one(&pool).await.unwrap();
    sqlx::query("UPDATE frame_holders SET removed_at = now() - interval '8 days' WHERE removed").execute(&pool).await.unwrap();
    let report = athenaeum_hub::scheduler::tick(&pool, Arc::new(CaptureMailer::default()), None, chrono::Utc::now()).await.unwrap();
    assert_eq!(report.tombstones_pruned, 1);
    let (seq, floor) = head(&pool, &id).await;
    assert_eq!(floor, tombstone_seq, "the prune raised the floor (spec §5.1)");
    for (query, error) in [
        (format!("since={}", floor - 1), "holders_below_floor"),
        (format!("since={}", seq + 5), "holders_cursor_ahead"),
        (format!("since={floor}&epoch=another"), "epoch_changed"),
    ] {
        let (status, body) = send(&fa.app, get(&format!("/api/v1/projects/{id}/holders?{query}"), Some(&bob))).await;
        assert_eq!(status, StatusCode::GONE, "{query}");
        assert_eq!(as_json(&body)["error"], error);
    }
    let (status, _) = send(&fa.app, get(&format!("/api/v1/projects/{id}/holders?since={floor}&epoch=test-epoch"), Some(&bob))).await;
    assert_eq!(status, StatusCode::OK);
}
```

Also add to the `#[cfg(test)]` module of `src/routes/holders.rs`:

```rust
    #[test]
    fn run_length_matches_the_wire_example() {
        assert_eq!(run_length(&[(1, 1), (2, 1), (3, 1), (5, 2), (6, 1)]), vec![[1, 3, 1], [5, 1, 2], [6, 1, 1]]);
        assert!(run_length(&[]).is_empty());
    }
```

Run: `DATABASE_URL=… cargo test --test holders_read` → FAIL (404 or a missing field).

- [ ] **Step 2: `ApiError::Body`**

`src/error.rs`:
- Add the variant below.
- `status()`: `ApiError::Body(s, _) => *s`.
- The log `match`: `ApiError::Body(_, body) => tracing::debug!(status = status.as_u16(), reason = %body, "request rejected")`.
- The response `match`: `ApiError::Body(_, body) => (status, Json(body)).into_response()`.

```rust
    /// Return this status with this exact JSON body — a typed refusal the
    /// client acts on (`version_conflict` with the current version,
    /// `holders_below_floor` with the floor). Never carries a secret.
    Body(StatusCode, serde_json::Value),
```

- [ ] **Step 3: Snapshot, deltas, prune**

Append to `src/routes/holders.rs`:

```rust
// ---- reads (spec 2026-09-25 §5.3, §6.1) ------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotFrame {
    pub seq: i32,
    pub uuid: Uuid,
    pub content_version: i32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotDevice {
    pub device: String,
    pub display_name: String,
    pub relay_url: Option<String>,
    pub claims: Vec<[i32; 3]>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldersSnapshot {
    pub epoch: String,
    pub holder_seq: i64,
    pub version: i64,
    pub frames: Vec<SnapshotFrame>,
    pub devices: Vec<SnapshotDevice>,
}

/// Run-length encode `(frame_seq, content_version)` claims sorted by
/// `frame_seq` into `[startSeq, runLength, contentVersion]` runs (§ Wire
/// contract).
pub fn run_length(claims: &[(i32, i32)]) -> Vec<[i32; 3]> {
    let mut out: Vec<[i32; 3]> = Vec::new();
    for &(seq, cv) in claims {
        match out.last_mut() {
            Some(run) if run[0] + run[1] == seq && run[2] == cv => run[1] += 1,
            _ => out.push([seq, 1, cv]),
        }
    }
    out
}

async fn repeatable_read(state: &AppState) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, sqlx::Error> {
    let mut tx = state.db.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY").execute(&mut *tx).await?;
    Ok(tx)
}

#[derive(sqlx::FromRow)]
struct SnapshotDeviceRow {
    id: Uuid,
    pubkey: Vec<u8>,
    display_name: String,
    relay_url: Option<String>,
}

/// `GET /api/v1/projects/{id}/holders/snapshot` — one REPEATABLE READ read
/// of the project's holder map with the cursors it reflects (I3; P5, P27).
#[tracing::instrument(skip_all)]
pub async fn holders_snapshot(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthAccount>,
    Path(id): Path<Uuid>,
) -> Result<Json<HoldersSnapshot>, ApiError> {
    let member = require_member(&state.db, id, auth.account_id).await?;
    let moderator = member.has_cap("data.moderate");
    let mut tx = repeatable_read(&state).await?;
    let (version,): (i64,) = sqlx::query_as("SELECT version FROM projects WHERE id = $1").bind(id).fetch_one(&mut *tx).await?;
    let (holder_seq,): (i64,) = sqlx::query_as("SELECT seq FROM project_holder_cursor WHERE project_id = $1").bind(id).fetch_one(&mut *tx).await?;
    let frames: Vec<(i32, Uuid, i32)> = sqlx::query_as(
        "SELECT frame_seq, frame_uuid, content_version FROM project_frames \
         WHERE project_id = $1 AND (state = 'published' OR publisher = $2 OR $3) ORDER BY frame_seq",
    )
    .bind(id)
    .bind(auth.account_id)
    .bind(moderator)
    .fetch_all(&mut *tx)
    .await?;
    let devices = sqlx::query_as::<_, SnapshotDeviceRow>(
        "SELECT d.id, d.pubkey, pm.display_name, d.endpoint_addr->>'homeRelayUrl' AS relay_url FROM project_members pm \
         JOIN devices d ON d.account_id = pm.account_id AND d.revoked_at IS NULL AND d.capability = 'athenaeum' \
         WHERE pm.project_id = $1",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let claims: Vec<(Uuid, i32, i32)> = sqlx::query_as(
        "SELECT h.device_id, f.frame_seq, h.content_version FROM frame_holders h \
         JOIN project_frames f ON f.project_id = h.project_id AND f.frame_uuid = h.frame_uuid \
         WHERE h.project_id = $1 AND NOT h.removed ORDER BY h.device_id, f.frame_seq",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut by_device: std::collections::HashMap<Uuid, Vec<(i32, i32)>> = std::collections::HashMap::new();
    for (device_id, seq, cv) in claims {
        by_device.entry(device_id).or_default().push((seq, cv));
    }
    let mut out: Vec<SnapshotDevice> = devices
        .into_iter()
        .map(|d| SnapshotDevice {
            device: crate::security::encode_pubkey(&d.pubkey),
            display_name: d.display_name,
            relay_url: state.feed.live_relay(d.id).unwrap_or(d.relay_url),
            claims: run_length(by_device.get(&d.id).map(Vec::as_slice).unwrap_or(&[])),
        })
        .collect();
    out.sort_by(|a, b| a.device.cmp(&b.device));
    Ok(Json(HoldersSnapshot {
        epoch: state.feed.epoch(),
        holder_seq,
        version,
        frames: frames.into_iter().map(|(seq, uuid, content_version)| SnapshotFrame { seq, uuid, content_version }).collect(),
        devices: out,
    }))
}

pub const DELTA_PAGE: i64 = 5000;

#[derive(Deserialize)]
pub struct SinceQuery {
    pub since: i64,
    pub after: Option<String>,
    pub limit: Option<i64>,
    pub epoch: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeltaCursor {
    pub since: i64,
    pub after: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HolderDeltaPage {
    pub epoch: String,
    pub holder_seq: i64,
    pub floor: i64,
    pub deltas: Vec<crate::feed::wire::HolderDeltaWire>,
    pub has_more: bool,
    pub next: Option<DeltaCursor>,
}

/// The opaque keyset cursor: `"{changed_seq}:{device_uuid}:{frame_seq}"`.
fn parse_after(raw: &str) -> Option<(i64, Uuid, i32)> {
    let mut parts = raw.splitn(3, ':');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

#[derive(sqlx::FromRow)]
struct DeltaRow {
    changed_seq: i64,
    device_id: Uuid,
    pubkey: Vec<u8>,
    frame_seq: i32,
    content_version: i32,
    removed: bool,
}

/// `GET /api/v1/projects/{id}/holders?since=S[&after][&limit][&epoch]` —
/// every claim changed after `S`, in its current state, paged (P26). `410`
/// below the floor, ahead of the head, or on another epoch: reload the
/// snapshot (spec §5.3, C21).
#[tracing::instrument(skip_all, level = "debug")]
pub async fn holder_deltas(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthAccount>,
    Path(id): Path<Uuid>,
    Query(q): Query<SinceQuery>,
) -> Result<Json<HolderDeltaPage>, ApiError> {
    require_member(&state.db, id, auth.account_id).await?;
    let epoch = state.feed.epoch();
    if q.epoch.as_deref().is_some_and(|e| e != epoch) {
        return Err(ApiError::Body(StatusCode::GONE, serde_json::json!({"error": "epoch_changed", "epoch": epoch})));
    }
    let limit = q.limit.unwrap_or(DELTA_PAGE).clamp(1, DELTA_PAGE);
    let after = match q.after.as_deref() {
        None => None,
        Some(raw) => Some(parse_after(raw).ok_or_else(|| ApiError::bad_request("after is not a cursor this hub issued"))?),
    };
    let mut tx = repeatable_read(&state).await?;
    let (holder_seq, floor): (i64, i64) =
        sqlx::query_as("SELECT seq, floor FROM project_holder_cursor WHERE project_id = $1").bind(id).fetch_one(&mut *tx).await?;
    if q.since < floor || q.since > holder_seq {
        let error = if q.since < floor { "holders_below_floor" } else { "holders_cursor_ahead" };
        return Err(ApiError::Body(StatusCode::GONE, serde_json::json!({"error": error, "floor": floor, "holderSeq": holder_seq})));
    }
    let rows = sqlx::query_as::<_, DeltaRow>(
        "SELECT h.changed_seq, h.device_id, d.pubkey, f.frame_seq, h.content_version, h.removed FROM frame_holders h \
         JOIN devices d ON d.id = h.device_id \
         JOIN project_frames f ON f.project_id = h.project_id AND f.frame_uuid = h.frame_uuid \
         WHERE h.project_id = $1 AND h.changed_seq > $2 \
           AND ($3::bigint IS NULL OR (h.changed_seq, h.device_id, f.frame_seq) > ($3, $4, $5)) \
         ORDER BY h.changed_seq, h.device_id, f.frame_seq LIMIT $6",
    )
    .bind(id)
    .bind(q.since)
    .bind(after.map(|a| a.0))
    .bind(after.map(|a| a.1))
    .bind(after.map(|a| a.2))
    .bind(limit + 1)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let has_more = rows.len() as i64 > limit;
    let page: Vec<DeltaRow> = rows.into_iter().take(limit as usize).collect();
    let next = if has_more {
        page.last().map(|r| DeltaCursor { since: q.since, after: format!("{}:{}:{}", r.changed_seq, r.device_id, r.frame_seq) })
    } else {
        None
    };
    let mut by_device: std::collections::BTreeMap<String, crate::claims::DeviceDelta> = std::collections::BTreeMap::new();
    for r in page {
        let delta = by_device.entry(crate::security::encode_pubkey(&r.pubkey)).or_default();
        if r.removed {
            delta.rm(r.frame_seq);
        } else {
            delta.add(r.frame_seq, r.content_version);
        }
    }
    let deltas = by_device
        .into_iter()
        .map(|(device, d)| crate::feed::wire::HolderDeltaWire { device, add: d.add.into_iter().collect(), rm: d.rm.into_iter().collect() })
        .collect();
    Ok(Json(HolderDeltaPage { epoch, holder_seq, floor, deltas, has_more, next }))
}

pub const TOMBSTONE_RETENTION_DAYS: i64 = 7;

/// Delete tombstones older than 7 days; each project's floor rises to the
/// highest `changed_seq` pruned there (spec §5.1). Hourly, from the
/// scheduler. Locks only the cursor row per project, like any holder writer.
pub async fn prune_tombstones(db: &PgPool, now: DateTime<Utc>) -> Result<u64, sqlx::Error> {
    let cutoff = now - chrono::Duration::days(TOMBSTONE_RETENTION_DAYS);
    let projects: Vec<Uuid> =
        sqlx::query_scalar("SELECT DISTINCT project_id FROM frame_holders WHERE removed AND removed_at < $1")
            .bind(cutoff)
            .fetch_all(db)
            .await?;
    let mut total = 0u64;
    for project_id in projects {
        let mut tx = db.begin().await?;
        sqlx::query("SELECT 1 FROM project_holder_cursor WHERE project_id = $1 FOR UPDATE").bind(project_id).execute(&mut *tx).await?;
        let (pruned, max_seq): (i64, Option<i64>) = sqlx::query_as(
            "WITH gone AS (DELETE FROM frame_holders WHERE project_id = $1 AND removed AND removed_at < $2 RETURNING changed_seq) \
             SELECT count(*), max(changed_seq) FROM gone",
        )
        .bind(project_id)
        .bind(cutoff)
        .fetch_one(&mut *tx)
        .await?;
        if let Some(max_seq) = max_seq {
            sqlx::query("UPDATE project_holder_cursor SET floor = GREATEST(floor, $2) WHERE project_id = $1")
                .bind(project_id)
                .bind(max_seq)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        if pruned > 0 {
            tracing::info!(project_id = %project_id, pruned, "holder tombstones pruned");
        }
        total += pruned as u64;
    }
    Ok(total)
}
```

The imports of `holders.rs` gain `axum::extract::Query`, `chrono::{DateTime, Utc}` and `sqlx::PgPool`.

`src/routes/mod.rs` (`account_protected`):

```rust
        .route("/api/v1/projects/{id}/holders/snapshot", get(holders::holders_snapshot))
        .route("/api/v1/projects/{id}/holders", get(holders::holder_deltas))
```

`src/scheduler.rs`:
- `TickReport` gains `pub tombstones_pruned: u64`.
- In `tick`, after the stale sweep, add `let tombstones_pruned = crate::routes::holders::prune_tombstones(db, now).await?;`.
- Include `tombstones_pruned` in the report, in the `info!`/`debug!` condition and in their fields.
- Extend the module doc with a third numbered item: "3. `holders::prune_tombstones` — holder tombstones older than 7 days are deleted and each project's holder floor rises (spec 2026-09-25 §5.1)."

- [ ] **Step 4: Run and commit**

Run: `DATABASE_URL=… cargo test 2>&1 | tail -10 && cargo build --release 2>&1 | grep -c warning` → green; `0`.

```bash
git add src/error.rs src/routes/holders.rs src/routes/mod.rs src/scheduler.rs tests/holders_read.rs
git commit -m "feat(hub): holder snapshot (REPEATABLE READ, run-length claims) and deltas since a cursor (paged, 410 below the floor); hourly tombstone prune raises the floor" \
           -m "Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RYd9tanU4ehWLDXkPv159r"
```

---

### Task 7: Versions by compare-and-set — single route and the 500-frame batch

**Files:**
- Modify: `src/routes/frames.rs:566-652` (`NewVersion.expected_version`, CAS, shared `validate_hashes`). Add `versions_batch`.
- Modify: `src/routes/mod.rs` (`POST /api/v1/projects/{id}/frames/versions`). axum 0.8's router prefers the static segment `versions` over `{frame_uuid}`. `PATCH …/frames/versions` then answers 405, and no client sends it. Task 8 adds `versions_batch` to the lock-order writer list in `frames.rs:1-29`.
- Test:
  - `tests/versions_cas.rs` (new).
  - Add `"expectedVersion": 1` (or the right current version) to every existing `…/version` call: `tests/claims.rs::new_version_and_reject_keep_holder_rows`, `tests/frames.rs` (two calls), `tests/holders.rs::holder_report_and_read_are_pinned…`.

**Interfaces:**
- Consumes: `store::hub_claims_tx` (Task 2), `commit_and_publish`, `frame_events_tx` (Task 4), `ApiError::Body` (Task 6), `compat::outdated` (Task 2).
- Produces:
  ```rust
  pub const MAX_VERSIONS: usize = 500;
  #[derive(Deserialize)] #[serde(rename_all = "camelCase")] pub struct VersionIn { pub uuid: Uuid, pub expected_version: i32, pub blake3: String, pub byte_size: i64, pub xxh3: String }
  #[derive(Serialize)] #[serde(rename_all = "camelCase")] pub struct VersionResult { pub uuid: Uuid, pub status: &'static str, pub content_version: i32 }
  pub async fn versions_batch(State<AppState>, Extension<AuthAccount>, Path<Uuid>, Json<VersionsBatch>) -> Result<Json<VersionsResponse>, ApiError>;
  ```
- Wire: § Wire contract "Frames — versions".

- [ ] **Step 1: Write the failing tests**

`tests/versions_cas.rs`:

```rust
//! Versions advance by compare-and-set (spec 2026-09-25 §5.2, I10, C9); a
//! batch of up to 500 is one transaction (spec §12 load check).
mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

fn version_body(expected: i32, n: u32) -> serde_json::Value {
    json!({"expectedVersion": expected, "blake3": format!("{:064x}", 1000 + n), "byteSize": 1, "xxh3": format!("{:016x}", 1000 + n)})
}

#[sqlx::test]
async fn single_version_is_compare_and_set(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap().to_string();
    assert_eq!(announce_frames(&app, &coord, &id, 1, 2).await.0, StatusCode::OK);
    let uuid = frame_uuid(1);
    let url = format!("/api/v1/projects/{id}/frames/{uuid}/version");
    let (status, body) = send(&app, post(&url, &version_body(1, 1), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(as_json(&body)["contentVersion"], 2);
    let (status, body) = send(&app, post(&url, &version_body(1, 2), Some(&coord))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(as_json(&body), json!({"error": "version_conflict", "contentVersion": 2}));
    let (status, body) = send(&app, post(&url, &json!({"blake3": format!("{:064x}", 9), "byteSize": 1, "xxh3": format!("{:016x}", 9)}), Some(&coord))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(as_json(&body)["error"], "collab_api_outdated", "a wave-2 body (P25)");
}

#[sqlx::test]
async fn a_batch_reports_per_frame_results_in_one_transaction(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&app, &mailer, "anna@example.com", 2, "Laptop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap().to_string();
    let pid = Uuid::parse_str(&id).unwrap();
    join_and_approve(&app, &coord, &anna, &id, "Anna", "send").await;
    assert_eq!(announce_frames(&app, &coord, &id, 1, 501).await.0, StatusCode::OK);
    assert_eq!(announce_frames(&app, &anna, &id, 900, 901).await.0, StatusCode::OK);
    let (v0, c0): (i64, i64) = sqlx::query_as("SELECT p.version, c.seq FROM projects p JOIN project_holder_cursor c ON c.project_id = p.id WHERE p.id = $1")
        .bind(pid).fetch_one(&pool).await.unwrap();
    let mut versions: Vec<serde_json::Value> = (1..=497).map(|n| { let mut b = version_body(1, n); b["uuid"] = json!(frame_uuid(n)); b }).collect();
    versions.push({ let mut b = version_body(7, 498); b["uuid"] = json!(frame_uuid(498)); b });            // conflict
    versions.push({ let mut b = version_body(1, 499); b["uuid"] = json!(frame_uuid(777)); b });            // not found
    versions.push({ let mut b = version_body(1, 500); b["uuid"] = json!(frame_uuid(900)); b });            // anna's: forbidden
    let (status, body) = send(&app, post(&format!("/api/v1/projects/{id}/frames/versions"), &json!({"versions": versions}), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let resp = as_json(&body);
    let results = resp["results"].as_array().unwrap();
    assert_eq!(results.len(), 500);
    assert_eq!(results[0], json!({"uuid": frame_uuid(1), "status": "ok", "contentVersion": 2}));
    assert_eq!(results[497], json!({"uuid": frame_uuid(498), "status": "conflict", "contentVersion": 1}));
    assert_eq!(results[498], json!({"uuid": frame_uuid(777), "status": "not_found", "contentVersion": 0}));
    assert_eq!(results[499], json!({"uuid": frame_uuid(900), "status": "forbidden", "contentVersion": 0}));
    let (v1, c1): (i64, i64) = sqlx::query_as("SELECT p.version, c.seq FROM projects p JOIN project_holder_cursor c ON c.project_id = p.id WHERE p.id = $1")
        .bind(pid).fetch_one(&pool).await.unwrap();
    assert_eq!((v1, c1), (v0 + 1, c0 + 1), "one transaction: one version bump, one holder-cursor bump");
    assert_eq!(resp["projectVersion"], v1);
    let distinct: i64 = sqlx::query_scalar("SELECT count(DISTINCT manifest_version) FROM project_frames WHERE project_id = $1 AND content_version = 2")
        .bind(pid).fetch_one(&pool).await.unwrap();
    assert_eq!(distinct, 1);
    let old_claims: i64 = sqlx::query_scalar("SELECT count(*) FROM frame_holders WHERE project_id = $1 AND content_version = 2 AND NOT removed")
        .bind(pid).fetch_one(&pool).await.unwrap();
    assert_eq!(old_claims, 497, "the hub-written claims moved to v2 (I4)");
    // Validation is whole-batch.
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/frames/versions"), &json!({"versions": []}), Some(&coord))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
```

Run: `DATABASE_URL=… cargo test --test versions_cas` → FAIL.

- [ ] **Step 2: Implement**

`src/routes/frames.rs`:
- Factor the three hash/size checks of `new_version` (`:591-599`) into `fn validate_hashes(blake3: &str, xxh3: &str, byte_size: i64) -> Result<(), ApiError>`, reusing the same messages.
- `NewVersion` gains `pub expected_version: Option<i32>`.
- In `new_version`, first thing after the device-token check: `let Some(expected) = body.expected_version else { return Err(crate::routes::compat::outdated()); };`.
- After the `status != "active"` check:

```rust
    if current != expected {
        return Err(ApiError::Body(
            axum::http::StatusCode::CONFLICT,
            serde_json::json!({"error": "version_conflict", "contentVersion": current}),
        ));
    }
```

Add the batch route:

```rust
pub const MAX_VERSIONS: usize = 500;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionIn {
    pub uuid: Uuid,
    pub expected_version: i32,
    pub blake3: String,
    pub byte_size: i64,
    pub xxh3: String,
}

#[derive(Deserialize)]
pub struct VersionsBatch {
    pub versions: Vec<VersionIn>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionResult {
    pub uuid: Uuid,
    pub status: &'static str,
    pub content_version: i32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionsResponse {
    pub project_version: i64,
    pub results: Vec<VersionResult>,
}

/// `POST /api/v1/projects/{id}/frames/versions` — up to 500 re-versions in
/// one transaction, each compare-and-set on its own `expectedVersion`
/// (spec 2026-09-25 §5.3, I10). Per-frame results; the project version
/// moves once, only when at least one frame was versioned. Lock order:
/// projects → cursor → frame rows (module doc).
#[tracing::instrument(skip_all)]
pub async fn versions_batch(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthAccount>,
    Path(id): Path<Uuid>,
    Json(body): Json<VersionsBatch>,
) -> Result<Json<VersionsResponse>, ApiError> {
    let Some(device_id) = auth.device_id else {
        return Err(ApiError::bad_request("a device token is required"));
    };
    require_member(&state.db, id, auth.account_id).await?;
    if body.versions.is_empty() || body.versions.len() > MAX_VERSIONS {
        return Err(ApiError::bad_request(format!("versions must contain 1..={MAX_VERSIONS} items")));
    }
    let mut seen = std::collections::HashSet::with_capacity(body.versions.len());
    for v in &body.versions {
        validate_hashes(&v.blake3, &v.xxh3, v.byte_size)?;
        if !seen.insert(v.uuid) {
            return Err(ApiError::bad_request(format!("duplicate uuid in batch: {}", v.uuid)));
        }
    }
    let mut tx = state.db.begin().await?;
    let mut feed = FeedBatch::default();
    let row: Option<(String, i64)> = sqlx::query_as("SELECT status, version FROM projects WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some((status, current_version)) = row else {
        return Err(ApiError::not_found("project not found"));
    };
    if status != "active" {
        return Err(ApiError::conflict("project is closed"));
    }
    let mut hw = crate::claims::HolderWrite::lock(&mut tx, id).await?;
    let uuids: Vec<Uuid> = body.versions.iter().map(|v| v.uuid).collect();
    let rows: Vec<(Uuid, Uuid, i32)> = sqlx::query_as(
        "SELECT frame_uuid, publisher, content_version FROM project_frames \
         WHERE project_id = $1 AND frame_uuid = ANY($2) ORDER BY frame_uuid FOR UPDATE",
    )
    .bind(id)
    .bind(&uuids)
    .fetch_all(&mut *tx)
    .await?;
    let current: std::collections::HashMap<Uuid, (Uuid, i32)> = rows.into_iter().map(|(u, p, cv)| (u, (p, cv))).collect();
    let mut results = Vec::with_capacity(body.versions.len());
    let mut ok: Vec<(&VersionIn, i32)> = Vec::new();
    for v in &body.versions {
        let (status, content_version) = match current.get(&v.uuid) {
            None => ("not_found", 0),
            Some((publisher, _)) if *publisher != auth.account_id => ("forbidden", 0),
            Some((_, cv)) if *cv != v.expected_version => ("conflict", *cv),
            Some((_, cv)) => {
                ok.push((v, cv + 1));
                ("ok", cv + 1)
            }
        };
        results.push(VersionResult { uuid: v.uuid, status, content_version });
    }
    let project_version = if ok.is_empty() {
        current_version
    } else {
        let version = bump_project_version_tx(&mut tx, id).await?;
        feed.bump(id, version, Kind::Frames);
        let u: Vec<Uuid> = ok.iter().map(|(v, _)| v.uuid).collect();
        let cv: Vec<i32> = ok.iter().map(|(_, n)| *n).collect();
        let b3: Vec<&str> = ok.iter().map(|(v, _)| v.blake3.as_str()).collect();
        let sz: Vec<i64> = ok.iter().map(|(v, _)| v.byte_size).collect();
        let x3: Vec<&str> = ok.iter().map(|(v, _)| v.xxh3.as_str()).collect();
        sqlx::query(
            "UPDATE project_frames f SET content_version = n.cv, blake3 = n.b3, byte_size = n.sz, xxh3 = n.x3, manifest_version = $2 \
             FROM unnest($3::uuid[], $4::int[], $5::text[], $6::bigint[], $7::text[]) AS n(u, cv, b3, sz, x3) \
             WHERE f.project_id = $1 AND f.frame_uuid = n.u",
        )
        .bind(id).bind(version).bind(&u).bind(&cv).bind(&b3).bind(&sz).bind(&x3)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO project_frame_versions (project_id, frame_uuid, content_version, blake3, byte_size, xxh3) \
             SELECT $1, n.u, n.cv, n.b3, n.sz, n.x3 FROM unnest($2::uuid[], $3::int[], $4::text[], $5::bigint[], $6::text[]) AS n(u, cv, b3, sz, x3)",
        )
        .bind(id).bind(&u).bind(&cv).bind(&b3).bind(&sz).bind(&x3)
        .execute(&mut *tx)
        .await?;
        let claims: Vec<(Uuid, i32)> = ok.iter().map(|(v, n)| (v.uuid, *n)).collect();
        crate::claims::store::hub_claims_tx(&mut tx, &mut hw, device_id, &claims).await?;
        record_event_tx(&mut tx, id, "frames_versioned", Some(auth.account_id), None, serde_json::json!({ "count": ok.len() })).await?;
        feed.frames(id, frame_events_tx(&mut tx, id, &u).await?);
        version
    };
    feed.holders(hw.finish(&mut tx).await?);
    commit_and_publish(&state, tx, feed).await?;
    tracing::info!(project_id = %id, count = ok.len(), requested = body.versions.len(), "frames versioned");
    Ok(Json(VersionsResponse { project_version, results }))
}
```

`src/routes/mod.rs`: `.route("/api/v1/projects/{id}/frames/versions", post(frames::versions_batch))`.

- [ ] **Step 3: Run and commit**

Run: `DATABASE_URL=… cargo test 2>&1 | tail -10 && cargo build --release 2>&1 | grep -c warning` → green; `0`.

```bash
git add src/routes/frames.rs src/routes/mod.rs tests/versions_cas.rs tests/claims.rs tests/frames.rs tests/holders.rs
git commit -m "feat(hub): versions by compare-and-set — expectedVersion on /version (409 version_conflict), 500-frame batch in one transaction with per-frame results" \
           -m "Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RYd9tanU4ehWLDXkPv159r"
```

---

### Task 8: Revocation, retirement and membership effects (I11)

**Files:**
- Modify: `src/routes/devices.rs:95-117` (`RevokeBody`, `device_out_of_service_tx`, `revoke_device`).
- Modify: `src/routes/operator.rs:906-932` (`revoke_device` through the same core), `src/routes/operator.rs:845-886` (`set_blocked` closes the account's streams).
- Modify: `src/routes/auth.rs:434-461` (`verify_otp`: device add bumps `members`).
- Modify: `src/routes/members.rs` (`remove_member`, `leave_project`, `remove_member_core` tombstone the account's claims under projects → cursor → members), `src/claims/store.rs` (`tombstone_account_tx`).
- Modify: the lock-order doc in `frames.rs:1-29` (add the device-lifecycle and leave writers), and `src/routes/snapshots.rs:6-9` (the doc now says device add/revoke bump `projects.version` with kind `members`, not `membershipVersion`).
- Test: `tests/revocation.rs` (new). `tests/common/mod.rs` gains `app_with_feed_and_operators`.

**Interfaces:**
- Consumes: `store::tombstone_device_tx` (Task 2), `FeedBatch::{bump, holders, close_device, close_account, account}` (Task 4), `SseReader` (Task 5).
- Produces:
  ```rust
  // routes::devices
  #[derive(Deserialize, Default)] #[serde(rename_all = "camelCase")] pub struct RevokeBody { #[serde(default)] pub retire: bool }
  pub(crate) async fn device_out_of_service_tx(conn: &mut PgConnection, device_id: Uuid, account_id: Uuid, athenaeum: bool, feed: &mut FeedBatch) -> Result<(), ApiError>;
  // claims::store
  pub async fn tombstone_account_tx(conn: &mut PgConnection, hw: &mut HolderWrite, account_id: Uuid) -> Result<usize, sqlx::Error>;
  // tests/common
  pub fn app_with_feed_and_operators(pool: PgPool, emails: &[&str]) -> FeedApp;
  ```
- Wire: § Wire contract "Changed shapes elsewhere" (revoke body, verify bump).

- [ ] **Step 1: Write the failing tests**

Append to `tests/common/mod.rs`:

```rust
/// `app_with_feed` plus the operator allowlist.
pub fn app_with_feed_and_operators(pool: PgPool, emails: &[&str]) -> FeedApp {
    let mut cfg = athenaeum_hub::feed::FeedConfig::default();
    cfg.presence.warmup = std::time::Duration::ZERO;
    let clock = athenaeum_hub::feed::clock::ManualClock::new();
    let feed = athenaeum_hub::feed::FeedHub::new(Arc::new(clock.clone()), cfg);
    feed.set_epoch("test-epoch".into());
    let mailer = CaptureMailer::default();
    let state = AppState::new(pool, Arc::new(mailer.clone()))
        .with_feed(feed.clone())
        .with_operator_emails(emails.iter().map(|s| s.to_string()).collect());
    FeedApp { app: build_router(state), mailer, feed, clock }
}
```

`tests/revocation.rs`:

```rust
//! Revocation, retirement and membership changes reach every provider
//! (spec 2026-09-25 §5.2, I4, I11, C12, C13; plan rulings P18–P20, P32).
mod common;

use std::time::Duration;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const Q: Duration = Duration::from_millis(250);
const S: Duration = Duration::from_secs(1);

async fn live_claims(pool: &PgPool, device: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM frame_holders WHERE device_id = $1 AND NOT removed")
        .bind(Uuid::parse_str(device).unwrap()).fetch_one(pool).await.unwrap()
}

#[sqlx::test]
async fn revoke_with_retire_tombstones_claims_bumps_members_everywhere_and_closes_the_stream(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (coord, _) = register_device(&fa.app, &fa.mailer, "coord@example.com", 1, "Desktop").await;
    let (anna1, anna1_id) = register_device(&fa.app, &fa.mailer, "anna@example.com", 2, "Old laptop").await;
    let p = create_project_via(&fa.app, &coord, "P", false).await["id"].as_str().unwrap().to_string();
    let q = create_project_via(&fa.app, &coord, "Q", false).await["id"].as_str().unwrap().to_string();
    for id in [&p, &q] {
        join_and_approve(&fa.app, &coord, &anna1, id, "Anna", "send_receive").await;
    }
    assert_eq!(announce_frames(&fa.app, &coord, &p, 1, 3).await.0, StatusCode::OK);
    assert_eq!(announce_frames(&fa.app, &coord, &q, 10, 11).await.0, StatusCode::OK);
    let (a, b, c) = (frame_uuid(1), frame_uuid(2), frame_uuid(10));
    report_holders(&fa.app, &anna1, &p, 1, false, &[(&a, 1), (&b, 1)], &[], &[(&a, 1), (&b, 1)]).await;
    report_holders(&fa.app, &anna1, &q, 2, false, &[(&c, 1)], &[], &[(&c, 1)]).await;
    let (anna2, _) = register_device(&fa.app, &fa.mailer, "anna@example.com", 3, "New laptop").await;
    fa.advance(S);
    let (_, _, stream) = SseReader::open(&fa.app, &anna1).await;
    let mut stream = stream.unwrap();
    stream.next_named("hello").await;
    let mut rx_p = fa.feed.subscribe(Uuid::parse_str(&p).unwrap());
    let mut rx_q = fa.feed.subscribe(Uuid::parse_str(&q).unwrap());

    let (status, _) = send(&fa.app, post(&format!("/api/v1/devices/{anna1_id}/revoke"), &json!({"retire": true}), Some(&anna2))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(live_claims(&pool, &anna1_id).await, 0, "every claim tombstoned in every project (I4)");
    fa.advance(Q);
    for rx in [&mut rx_p, &mut rx_q] {
        let (name, ev) = next_wire(rx).unwrap();
        assert_eq!((name.as_str(), ev["kinds"].clone()), ("project", json!(["members"])), "I11: members bump in every project of the account");
    }
    fa.advance(S - Q);
    let (_, ev) = next_wire(&mut rx_p).unwrap();
    assert_eq!(ev["deltas"], json!([{"device": pubkey_b64(2), "add": [], "rm": [1, 2]}]));
    settle().await;
    while let Some((name, _)) = stream.next_event().await {
        assert_ne!(name, "hello", "the revoked device's stream is closed");
    }
    let (status, _) = send(&fa.app, get("/api/v1/devices", Some(&anna1))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn adding_a_device_bumps_members_in_every_project_of_the_account(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (coord, _) = register_device(&fa.app, &fa.mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, _) = register_device(&fa.app, &fa.mailer, "anna@example.com", 2, "Laptop").await;
    let p = create_project_via(&fa.app, &coord, "P", false).await["id"].as_str().unwrap().to_string();
    join_and_approve(&fa.app, &coord, &anna, &p, "Anna", "send").await;
    fa.advance(Q);
    let mut rx = fa.feed.subscribe(Uuid::parse_str(&p).unwrap());
    register_device(&fa.app, &fa.mailer, "anna@example.com", 3, "Second").await;
    fa.advance(Q);
    assert_eq!(next_wire(&mut rx).unwrap().1["kinds"], json!(["members"]));
    // A perseus device changes no snapshot and bumps nothing (P18).
    register_device_with_capability(&fa.app, &fa.mailer, "anna@example.com", 4, "Capture", "perseus").await;
    fa.advance(Q);
    assert!(next_wire(&mut rx).is_none());
}

#[sqlx::test]
async fn leaving_a_project_ends_the_members_claims_there(pool: PgPool) {
    let fa = app_with_feed(pool.clone());
    let (coord, _) = register_device(&fa.app, &fa.mailer, "coord@example.com", 1, "Desktop").await;
    let (bob, bob_id) = register_device(&fa.app, &fa.mailer, "bob@example.com", 3, "PC").await;
    let p = create_project_via(&fa.app, &coord, "P", false).await["id"].as_str().unwrap().to_string();
    join_and_approve(&fa.app, &coord, &bob, &p, "Bob", "send_receive").await;
    assert_eq!(announce_frames(&fa.app, &coord, &p, 1, 2).await.0, StatusCode::OK);
    let f1 = frame_uuid(1);
    report_holders(&fa.app, &bob, &p, 1, false, &[(&f1, 1)], &[], &[(&f1, 1)]).await;
    assert_eq!(live_claims(&pool, &bob_id).await, 1);
    let (status, _) = send(&fa.app, post(&format!("/api/v1/projects/{p}/leave"), &json!({}), Some(&bob))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(live_claims(&pool, &bob_id).await, 0, "I4: a holding ends when the member leaves (P19)");
}

#[sqlx::test]
async fn operator_block_closes_streams_and_operator_revoke_has_the_same_effects(pool: PgPool) {
    let fa = app_with_feed_and_operators(pool.clone(), &["op@example.com"]);
    let (op, _) = register_device(&fa.app, &fa.mailer, "op@example.com", 9, "Ops").await;
    let (coord, _) = register_device(&fa.app, &fa.mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, anna_id) = register_device(&fa.app, &fa.mailer, "anna@example.com", 2, "Laptop").await;
    let p = create_project_via(&fa.app, &coord, "P", false).await["id"].as_str().unwrap().to_string();
    let anna_account = join_and_approve(&fa.app, &coord, &anna, &p, "Anna", "send_receive").await;
    assert_eq!(announce_frames(&fa.app, &coord, &p, 1, 2).await.0, StatusCode::OK);
    let f1 = frame_uuid(1);
    report_holders(&fa.app, &anna, &p, 1, false, &[(&f1, 1)], &[], &[(&f1, 1)]).await;
    let (_, _, stream) = SseReader::open(&fa.app, &anna).await;
    let mut stream = stream.unwrap();
    stream.next_named("hello").await;
    let (status, _) = send(&fa.app, post(&format!("/api/v1/operator/accounts/{anna_account}/block"), &json!({"note": "spam"}), Some(&op))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    settle().await;
    while let Some((name, _)) = stream.next_event().await {
        assert_ne!(name, "hello", "a blocked account's streams close (P20)");
    }
    let (status, _) = send(&fa.app, post(&format!("/api/v1/operator/devices/{anna_id}/revoke"), &json!({}), Some(&op))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(live_claims(&pool, &anna_id).await, 0);
}
```

Run: `DATABASE_URL=… cargo test --test revocation` → FAIL (claims stay, no events).

- [ ] **Step 2: Implement**

`src/claims/store.rs`:

```rust
/// Tombstone the claims of every device of `account_id` in the locked
/// project (the member left or was removed — I4, P19).
pub async fn tombstone_account_tx(
    conn: &mut PgConnection,
    hw: &mut HolderWrite,
    account_id: Uuid,
) -> Result<usize, sqlx::Error> {
    let devices: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT h.device_id FROM frame_holders h JOIN devices d ON d.id = h.device_id \
         WHERE h.project_id = $1 AND d.account_id = $2 AND NOT h.removed",
    )
    .bind(hw.project_id())
    .bind(account_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut total = 0;
    for device_id in devices {
        total += tombstone_device_tx(conn, hw, device_id).await?;
    }
    Ok(total)
}
```

`src/routes/devices.rs`:

```rust
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RevokeBody {
    /// L9: "this device replaces <device>". Same effects as a revoke; the
    /// flag is recorded in the log (P18).
    #[serde(default)]
    pub retire: bool,
}

/// I11 (spec 2026-09-25 §5.2, P32). A device leaves service (revoke or
/// retire). In one transaction:
/// - its claims are tombstoned in every project where it holds any, plus
///   every project of its account;
/// - every project of the account gets `projects.version` bumped with kind
///   `members` (an athenaeum device only: perseus never enters the snapshot);
/// - the holder cursors move.
///
/// After commit the device's stream is closed. Lock order: the caller already
/// updated the `devices` row; then every `projects` row in id order, then
/// every cursor row in id order.
pub(crate) async fn device_out_of_service_tx(
    conn: &mut PgConnection,
    device_id: Uuid,
    account_id: Uuid,
    athenaeum: bool,
    feed: &mut crate::feed::FeedBatch,
) -> Result<(), ApiError> {
    let member_of: Vec<Uuid> = sqlx::query_scalar("SELECT project_id FROM project_members WHERE account_id = $1")
        .bind(account_id)
        .fetch_all(&mut *conn)
        .await?;
    let projects: Vec<Uuid> = sqlx::query_scalar(
        "SELECT project_id FROM project_members WHERE account_id = $1 \
         UNION SELECT project_id FROM frame_holders WHERE device_id = $2 AND NOT removed ORDER BY 1",
    )
    .bind(account_id)
    .bind(device_id)
    .fetch_all(&mut *conn)
    .await?;
    sqlx::query("SELECT id FROM projects WHERE id = ANY($1) ORDER BY id FOR UPDATE")
        .bind(&projects)
        .execute(&mut *conn)
        .await?;
    let mut writes = Vec::with_capacity(projects.len());
    for project_id in &projects {
        writes.push(crate::claims::HolderWrite::lock(&mut *conn, *project_id).await?);
    }
    for mut hw in writes {
        let project_id = hw.project_id();
        crate::claims::store::tombstone_device_tx(&mut *conn, &mut hw, device_id).await?;
        if athenaeum && member_of.contains(&project_id) {
            let version = crate::project_version::bump_project_version_tx(&mut *conn, project_id).await?;
            feed.bump(project_id, version, crate::feed::wire::Kind::Members);
        }
        feed.holders(hw.finish(&mut *conn).await?);
    }
    feed.close_device(device_id);
    Ok(())
}
```

Replace `revoke_device` (`:95-117`):

```rust
/// Revoke (or, with `{retire: true}`, retire — L9) a device of the caller's
/// account. Idempotent; 404 if the id isn't the caller's. The I11 effects
/// run in the same transaction ([`device_out_of_service_tx`]).
#[tracing::instrument(skip_all)]
pub async fn revoke_device(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthDevice>,
    Path(id): Path<Uuid>,
    body: Option<Json<RevokeBody>>,
) -> Result<StatusCode, ApiError> {
    let retire = body.map(|Json(b)| b.retire).unwrap_or(false);
    let mut tx = state.db.begin().await?;
    let mut feed = crate::feed::FeedBatch::default();
    let capability: Option<String> =
        sqlx::query_scalar("UPDATE devices SET revoked_at = now() WHERE id = $1 AND account_id = $2 RETURNING capability")
            .bind(id)
            .bind(auth.account_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(capability) = capability else {
        return Err(ApiError::not_found("device not found"));
    };
    device_out_of_service_tx(&mut tx, id, auth.account_id, capability == "athenaeum", &mut feed).await?;
    crate::feed::commit_and_publish(&state, tx, feed).await?;
    tracing::info!(account_id = %auth.account_id, device_id = %id, retire, "device revoked");
    Ok(StatusCode::NO_CONTENT)
}
```

`src/routes/operator.rs::revoke_device` (`:906-932`):
- Change the `UPDATE` to `… WHERE id = $1 AND revoked_at IS NULL RETURNING account_id, capability` (`fetch_optional`, 404 on `None`, as today).
- Declare `let mut feed = FeedBatch::default();` and call `crate::routes::devices::device_out_of_service_tx(&mut tx, id, account_id, capability == "athenaeum", &mut feed).await?;` before the audit row.
- Replace `tx.commit()` with `commit_and_publish(&state, tx, feed)`.

`src/routes/operator.rs::set_blocked`:
- Declare `let mut feed = FeedBatch::default();`.
- When `blocked`, call `feed.close_account(target);` (P20).
- Replace `tx.commit().await?` with `crate::feed::commit_and_publish(state, tx, feed).await?`.

`src/routes/auth.rs::verify_otp`, between the upsert (`:434-459`) and the commit:

```rust
    // I11: a device added (or un-revoked) changes who may connect — every
    // project of the account gets a `members` bump (athenaeum devices only;
    // perseus never enters the signed snapshot, P18). Lock order: this
    // device row first (above), then every `projects` row in id order.
    let mut feed = crate::feed::FeedBatch::default();
    if capability == "athenaeum" {
        let projects: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM projects WHERE id IN (SELECT project_id FROM project_members WHERE account_id = $1) ORDER BY id FOR UPDATE",
        )
        .bind(account_id)
        .fetch_all(&mut *tx)
        .await?;
        for project_id in projects {
            let version = crate::project_version::bump_project_version_tx(&mut tx, project_id).await?;
            feed.bump(project_id, version, crate::feed::wire::Kind::Members);
        }
    }
    crate::feed::commit_and_publish(&state, tx, feed).await?;
```

(This replaces `tx.commit().await?;` at `:461`.)

`src/routes/members.rs`:
- In `remove_member`, `leave_project` and `remove_member_core`, directly after `lock_project_row(…)`, add:

```rust
    // Lock order (frames.rs module doc): projects → holder cursor → members.
    let mut hw = crate::claims::HolderWrite::lock(&mut *tx, project_id).await?;
```

- After the membership row is retired, add:

```rust
    // I4 (P19): the leaving member's holdings end here.
    crate::claims::store::tombstone_account_tx(&mut *tx, &mut hw, target_account_id).await?;
    feed.holders(hw.finish(&mut *tx).await?);
```

(Use `id`/`auth.account_id` for the names in `leave_project`.)

`src/routes/frames.rs:1-29`: add these lines to "Writers and their locks":

```rust
//! - `members::remove_member`/`remove_member_core`/`leave_project`:
//!   projects → cursor → members (the leaving member's claims end, I4).
//! - `frames::versions_batch`: projects → cursor → frames.
//! - `devices::revoke_device`, `operator::revoke_device`, `auth::verify_otp`:
//!   the `devices` row first, then every `projects` row of the account in id
//!   order, then every cursor row in id order (I11).
```

`src/routes/snapshots.rs:6-9`: replace the version sentence with: "`membershipVersion` orders membership changes only. Device add/revoke/retire change the content and bump `projects.version` with kind `members` (collab v3 wave 3, I11), so the event channel reaches every provider. Clients apply every verified snapshot by content."

- [ ] **Step 3: Run and commit**

Run: `DATABASE_URL=… cargo test 2>&1 | tail -10 && cargo build --release 2>&1 | grep -c warning` → green; `0`. `tests/device_registry.rs` and `tests/membership_snapshot.rs` stay green: revoke still answers 204, and `membershipVersion` does not move (P18).

```bash
git add src/routes/devices.rs src/routes/operator.rs src/routes/auth.rs src/routes/members.rs src/routes/frames.rs src/routes/snapshots.rs \
        src/claims/store.rs tests/common/mod.rs tests/revocation.rs
git commit -m "feat(hub): I11 — revoke/retire tombstone claims, bump members in every project of the account and close the stream; device add bumps members; leaving ends claims; block closes streams" \
           -m "Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RYd9tanU4ehWLDXkPv159r"
```

---

### Task 9: The epoch survives a restore — high-water marks, startup rotation, `rotate-epoch`, pool 20

**Files:**
- Modify: `src/feed/epoch.rs`. Add `Marks`, `Rotation`, `decide`, `read_marks`, `write_marks`, `rotate`, `startup`; delete `ensure_epoch`.
- Modify: `src/feed/driver.rs` (`spawn(state, state_dir)` writes the marks every 60 s), `src/config.rs` (`state_dir` from `STATE_DIRECTORY`), `src/lib.rs` (startup rotation; `rotate_epoch_command`; logging init moves to `main`), `src/main.rs` (subcommand), `src/db.rs:10` (pool 20).
- Test: unit tests in `src/feed/epoch.rs`; `tests/epoch.rs` (new).

**Interfaces:**
- Consumes: `driver::read_heads` (Task 5), `FeedHub::{epoch, set_epoch}` (Task 3).
- Produces:
  ```rust
  pub const MARKS_FILE: &str = "feed-marks.json";
  #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] #[serde(rename_all = "camelCase")]
  pub struct Marks { pub epoch: String, pub projects: BTreeMap<Uuid, [i64; 2]>, pub written_at: DateTime<Utc> }
  #[derive(Debug, Clone, PartialEq)] pub enum Rotation { Created, MarksUnreadable, EpochMismatch { file: String, db: String }, HeadBelowMark { project_id: Uuid, head: [i64; 2], mark: [i64; 2] } }
  impl Rotation { pub fn reason(&self) -> &'static str }
  pub fn decide(db_epoch: Option<&str>, marks: Result<Option<Marks>, String>, heads: &HeadsMap) -> Option<Rotation>;
  pub fn read_marks(dir: &Path) -> Result<Option<Marks>, String>;
  pub fn write_marks(dir: &Path, marks: &Marks) -> std::io::Result<()>;
  pub async fn rotate(db: &PgPool) -> Result<String, sqlx::Error>;
  pub async fn startup(db: &PgPool, state_dir: Option<&Path>) -> anyhow::Result<String>;
  // lib
  pub async fn rotate_epoch_command() -> anyhow::Result<()>;
  // config
  pub state_dir: Option<PathBuf>;   // STATE_DIRECTORY (first entry of a ':'-separated list)
  // db
  pub const POOL_SIZE: u32 = 20;
  ```

- [ ] **Step 1: Write the failing tests**

In `src/feed/epoch.rs` (unit, pure):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn marks(epoch: &str, projects: &[(u128, [i64; 2])]) -> Marks {
        Marks { epoch: epoch.into(), projects: projects.iter().map(|(p, m)| (Uuid::from_u128(*p), *m)).collect(), written_at: Utc::now() }
    }
    fn heads(entries: &[(u128, [i64; 2])]) -> HeadsMap {
        entries.iter().map(|(p, h)| (Uuid::from_u128(*p), *h)).collect()
    }

    #[test]
    fn rotation_rules() {
        let h = heads(&[(1, [10, 5]), (2, [3, 0])]);
        assert_eq!(decide(None, Ok(None), &h), Some(Rotation::Created));
        assert_eq!(decide(Some("e"), Ok(None), &h), None, "no marks yet (first boot with a state dir)");
        assert_eq!(decide(Some("e"), Err("bad json".into()), &h), Some(Rotation::MarksUnreadable));
        assert_eq!(decide(Some("e"), Ok(Some(marks("other", &[]))), &h),
            Some(Rotation::EpochMismatch { file: "other".into(), db: "e".into() }));
        assert_eq!(decide(Some("e"), Ok(Some(marks("e", &[(1, [10, 5]), (2, [3, 0])]))), &h), None, "heads at their marks");
        assert_eq!(decide(Some("e"), Ok(Some(marks("e", &[(1, [11, 5])]))), &h),
            Some(Rotation::HeadBelowMark { project_id: Uuid::from_u128(1), head: [10, 5], mark: [11, 5] }), "version behind");
        assert_eq!(decide(Some("e"), Ok(Some(marks("e", &[(1, [10, 6])]))), &h).map(|r| r.reason()), Some("head_below_mark"), "holder seq behind");
        assert_eq!(decide(Some("e"), Ok(Some(marks("e", &[(9, [99, 99])]))), &h), None, "a deleted project is legitimate");
    }

    #[test]
    fn marks_round_trip_atomically() {
        let dir = std::env::temp_dir().join(format!("hub-marks-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_marks(&dir), Ok(None));
        let m = marks("e", &[(1, [2, 3])]);
        write_marks(&dir, &m).unwrap();
        assert_eq!(read_marks(&dir), Ok(Some(m)));
        assert!(!dir.join(format!("{MARKS_FILE}.tmp")).exists());
        std::fs::write(dir.join(MARKS_FILE), b"{not json").unwrap();
        assert!(read_marks(&dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
```

`tests/epoch.rs`:

```rust
//! The epoch survives a restore (spec 2026-09-25 §4.4, C16; plan ruling P22).
mod common;

use athenaeum_hub::feed::epoch::{read_marks, startup, write_marks, Marks};
use axum::http::StatusCode;
use common::*;
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test]
async fn startup_rotates_only_when_the_database_is_behind_its_marks(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool.clone());
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap().to_string();
    assert_eq!(announce_frames(&app, &coord, &id, 1, 3).await.0, StatusCode::OK);
    let dir = std::env::temp_dir().join(format!("hub-epoch-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();

    let first = startup(&pool, Some(&dir)).await.unwrap();
    let marks = read_marks(&dir).unwrap().expect("startup writes fresh marks");
    assert_eq!(marks.epoch, first);
    assert_eq!(startup(&pool, Some(&dir)).await.unwrap(), first, "nothing restored, nothing rotated");

    // A restore from an older dump: the database head falls below its mark.
    sqlx::query("UPDATE projects SET version = version - 1 WHERE id = $1").bind(Uuid::parse_str(&id).unwrap()).execute(&pool).await.unwrap();
    let rotated = startup(&pool, Some(&dir)).await.unwrap();
    assert_ne!(rotated, first);
    assert_eq!(read_marks(&dir).unwrap().unwrap().epoch, rotated);

    // A marks file from another epoch (the database was swapped under it).
    write_marks(&dir, &Marks { epoch: "someone-else".into(), projects: Default::default(), written_at: chrono::Utc::now() }).unwrap();
    assert_ne!(startup(&pool, Some(&dir)).await.unwrap(), rotated);

    // Without a state directory the marks are off; the epoch persists.
    let current: String = sqlx::query_scalar("SELECT value FROM hub_meta WHERE key = 'epoch'").fetch_one(&pool).await.unwrap();
    assert_eq!(startup(&pool, None).await.unwrap(), current);
    std::fs::remove_dir_all(&dir).unwrap();
}
```

Run: `DATABASE_URL=… cargo test --test epoch && cargo test --lib feed::epoch` → FAIL (items missing).

- [ ] **Step 2: Implement**

Replace `src/feed/epoch.rs` with:

```rust
//! The feed epoch (spec 2026-09-25 §4.4; plan ruling P22): an opaque string
//! in `hub_meta` (`migrations/0021_hub_meta.sql`) that every cursor is
//! paired with. Every 60 s the driver writes `$STATE_DIRECTORY/feed-marks.json`
//! with the epoch and every project's `(version, holderSeq)`. Startup rotates
//! the epoch when:
//! - the database has no epoch;
//! - the marks file is unreadable;
//! - the file's epoch differs from the database's;
//! - any project's head is below its mark (a restore — C16).
//!
//! A project missing from the database is ignored: deletion is legitimate.
//! `athenaeum-hub rotate-epoch` rotates explicitly (the restore runbook).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::feed::HeadsMap;

pub const EPOCH_KEY: &str = "epoch";
pub const MARKS_FILE: &str = "feed-marks.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Marks {
    pub epoch: String,
    pub projects: BTreeMap<Uuid, [i64; 2]>,
    pub written_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Rotation {
    Created,
    MarksUnreadable,
    EpochMismatch { file: String, db: String },
    HeadBelowMark { project_id: Uuid, head: [i64; 2], mark: [i64; 2] },
}

impl Rotation {
    pub fn reason(&self) -> &'static str {
        match self {
            Rotation::Created => "created",
            Rotation::MarksUnreadable => "marks_unreadable",
            Rotation::EpochMismatch { .. } => "epoch_mismatch",
            Rotation::HeadBelowMark { .. } => "head_below_mark",
        }
    }
}

pub fn decide(db_epoch: Option<&str>, marks: Result<Option<Marks>, String>, heads: &HeadsMap) -> Option<Rotation> {
    let Some(db) = db_epoch else { return Some(Rotation::Created) };
    let marks = match marks {
        Err(_) => return Some(Rotation::MarksUnreadable),
        Ok(None) => return None,
        Ok(Some(m)) => m,
    };
    if marks.epoch != db {
        return Some(Rotation::EpochMismatch { file: marks.epoch, db: db.to_string() });
    }
    for (project_id, mark) in &marks.projects {
        if let Some(head) = heads.get(project_id) {
            if head[0] < mark[0] || head[1] < mark[1] {
                return Some(Rotation::HeadBelowMark { project_id: *project_id, head: *head, mark: *mark });
            }
        }
    }
    None
}

pub fn read_marks(dir: &Path) -> Result<Option<Marks>, String> {
    match std::fs::read(dir.join(MARKS_FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Write the marks atomically: a synced `.tmp` renamed over the file.
pub fn write_marks(dir: &Path, marks: &Marks) -> std::io::Result<()> {
    let tmp = dir.join(format!("{MARKS_FILE}.tmp"));
    let bytes = serde_json::to_vec_pretty(marks).map_err(std::io::Error::other)?;
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, dir.join(MARKS_FILE))
}

pub async fn read_epoch(db: &PgPool) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT value FROM hub_meta WHERE key = $1").bind(EPOCH_KEY).fetch_optional(db).await
}

pub async fn rotate(db: &PgPool) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO hub_meta (key, value) VALUES ($1, gen_random_uuid()::text) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now() RETURNING value",
    )
    .bind(EPOCH_KEY)
    .fetch_one(db)
    .await
}

/// The epoch this process serves: rotated when [`decide`] says so; fresh
/// marks written at once when a state directory is configured.
pub async fn startup(db: &PgPool, state_dir: Option<&Path>) -> anyhow::Result<String> {
    let heads = crate::feed::driver::read_heads(db).await.context("read project heads")?;
    let db_epoch = read_epoch(db).await.context("read the feed epoch")?;
    let marks = match state_dir {
        Some(dir) => read_marks(dir),
        None => {
            tracing::warn!("STATE_DIRECTORY unset — restore detection by high-water marks is disabled");
            Ok(None)
        }
    };
    let rotation = decide(db_epoch.as_deref(), marks, &heads);
    let epoch = match (rotation, db_epoch) {
        (None, Some(current)) => current,
        (rotation, _) => {
            let fresh = rotate(db).await.context("rotate the feed epoch")?;
            let reason = rotation.as_ref().map_or("created", Rotation::reason);
            if matches!(rotation, None | Some(Rotation::Created)) {
                tracing::info!(epoch = %fresh, reason, "feed epoch created");
            } else {
                tracing::warn!(epoch = %fresh, reason, detail = ?rotation, "feed epoch rotated — every client reloads its snapshots");
            }
            fresh
        }
    };
    if let Some(dir) = state_dir {
        write_marks(dir, &Marks { epoch: epoch.clone(), projects: heads, written_at: Utc::now() })
            .with_context(|| format!("write {}", dir.join(MARKS_FILE).display()))?;
    }
    Ok(epoch)
}
```

`src/feed/driver.rs`:
- `spawn` becomes `pub fn spawn(state: AppState, state_dir: Option<std::path::PathBuf>)`.
- In the heads branch, `Ok(heads) =>` also writes the marks when `state_dir` is `Some`:

```rust
                match heads_tick(&state.db, &state.feed).await {
                    Ok(heads) => {
                        if let Some(dir) = &state_dir {
                            let marks = crate::feed::epoch::Marks { epoch: state.feed.epoch(), projects: heads, written_at: chrono::Utc::now() };
                            if let Err(err) = crate::feed::epoch::write_marks(dir, &marks) {
                                tracing::error!(error = %err, path = %dir.display(), "feed marks write failed");
                            }
                        }
                    }
                    Err(err) => tracing::error!(error = %err, "feed heads tick failed"),
                }
```

`src/config.rs`:
- Add the field `pub state_dir: Option<std::path::PathBuf>` with this doc: "`STATE_DIRECTORY` (set by systemd's `StateDirectory=`): where the feed's high-water marks live. Unset disables restore detection by marks (P22)."
- Read it as: `std::env::var("STATE_DIRECTORY").ok().and_then(|v| v.split(':').next().map(str::trim).filter(|s| !s.is_empty()).map(std::path::PathBuf::from))`.
- Add it to the `Debug` impl.

`src/db.rs`: `pub const POOL_SIZE: u32 = 20;` with the doc "Streams hold no connection; the headroom is for reconnect storms (spec 2026-09-25 §5.4)". `.max_connections(POOL_SIZE)`.

`src/lib.rs`:
- Delete `logging::init();` from `run()`. `main` calls it.
- Replace the `ensure_epoch` line with `let epoch = feed::epoch::startup(&pool, config.state_dir.as_deref()).await?;`.
- `feed::driver::spawn(state.clone(), config.state_dir.clone());`.
- Add:

```rust
/// `athenaeum-hub rotate-epoch` — the restore runbook step (spec 2026-09-25
/// §4.4): run with the hub STOPPED, after restoring the database. Every
/// client reloads its snapshots and re-announces its frames on next connect.
pub async fn rotate_epoch_command() -> anyhow::Result<()> {
    let config = config::Config::from_env().context("invalid hub configuration")?;
    let pool = db::connect(&config.database_url).await?;
    db::run_migrations(&pool).await?;
    let epoch = feed::epoch::rotate(&pool).await.context("rotate the feed epoch")?;
    tracing::warn!(epoch = %epoch, reason = "explicit", "feed epoch rotated — every client reloads its snapshots");
    if let Some(dir) = &config.state_dir {
        let heads = feed::driver::read_heads(&pool).await?;
        feed::epoch::write_marks(dir, &feed::epoch::Marks { epoch, projects: heads, written_at: chrono::Utc::now() })
            .with_context(|| format!("write marks into {}", dir.display()))?;
    }
    Ok(())
}
```

`src/main.rs`:

```rust
//! Binary entry point — a thin shim over the library: `athenaeum-hub` serves;
//! `athenaeum-hub rotate-epoch` rotates the feed epoch (restore runbook).

#[tokio::main]
async fn main() {
    athenaeum_hub::logging::init();
    let result = match std::env::args().nth(1).as_deref() {
        None => athenaeum_hub::run().await,
        Some("rotate-epoch") => athenaeum_hub::rotate_epoch_command().await,
        Some(other) => Err(anyhow::anyhow!("unknown command {other:?}; usage: athenaeum-hub [rotate-epoch]")),
    };
    if let Err(err) = result {
        // `{err:#}` unfolds the full anyhow context chain.
        tracing::error!(error = %format!("{err:#}"), "hub exited with error");
        std::process::exit(1);
    }
}
```

(`anyhow` is already a dependency.)

- [ ] **Step 3: Run and commit**

Run: `DATABASE_URL=… cargo test 2>&1 | tail -10 && cargo build --release 2>&1 | grep -c warning` → green; `0`. Smoke:

```bash
mkdir -p /tmp/hub-state && STATE_DIRECTORY=/tmp/hub-state DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo run -- rotate-epoch && cat /tmp/hub-state/feed-marks.json
```

Expected: one `feed epoch rotated` warn line, then the JSON marks.

```bash
git add src/feed/epoch.rs src/feed/driver.rs src/config.rs src/db.rs src/lib.rs src/main.rs tests/epoch.rs
git commit -m "feat(hub): the feed epoch survives a restore — 60 s high-water marks in STATE_DIRECTORY, startup rotation, rotate-epoch subcommand; pool 20" \
           -m "Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RYd9tanU4ehWLDXkPv159r"
```

---

### Task 10: Retire the poll-era routes and bump the API

**Files:**
- Modify: `src/routes/mod.rs:161-164, 276-279`. Both routes point to `compat::retired`. Drop `pub mod versions;` and the `versions` field and builder.
- Delete: `src/routes/versions.rs`, `tests/versions.rs`.
- Modify: `src/project_version.rs`. Remove `VersionCache`, `Entry`, `MAX_AGE` and their tests; the module keeps `bump_project_version_tx`. New doc: "`projects.version` — the per-project cursor the event channel's `project` events carry."
- Modify: `src/feed/publish.rs` (`commit_and_publish` no longer invalidates a cache), `src/routes/holders.rs` (delete `frame_holders` and `HolderView`), `src/collab_auth.rs:224-226` (doc: no cache to invalidate).
- Test:
  - `tests/compat.rs` covers the four wave-2 shapes.
  - `tests/common/mod.rs` gains `current_holder_count`.
  - Replace every per-frame holders GET in `tests/holders.rs` and `tests/frames.rs` with it: `rg -n 'frames/\{[^}]*\}/holders' tests` lists them.

**Interfaces:**
- Produces: `pub async fn current_holder_count(app: &axum::Router, token: &str, project_id: &str, uuid: &str) -> usize` (tests only). It counts the devices whose run-length claims cover the frame's `seq` at its current `contentVersion` in `GET …/holders/snapshot`.
- Wire: § Wire contract "Retired routes".

- [ ] **Step 1: Confirm nothing deployed calls the retired routes (spec §5.3)**

```bash
curl -s -o /dev/null -w '%{http_code}\n' https://test-hub.artfrom.space/api/v1/me/project-versions
curl -s -o /dev/null -w '%{http_code}\n' https://test-hub.artfrom.space/api/v1/projects/00000000-0000-0000-0000-000000000000/frames/00000000-0000-0000-0000-000000000000/holders
git -C /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum tag --contains 4d3f963c
```

Expected:
- Neither curl prints `401`. A 401 would mean the route exists behind auth. The test hub runs the collab-v2 build (migrations ≤ 21, deployed 2026-09-21), where neither route exists, so the answer is the SPA fallback or 404.
- The tag list is empty: the only caller, the app's hub client since `4d3f963c` (wave 2), is in no release.

If either curl prints `401`, stop and report it to the controller; do not retire. Otherwise record the three outputs in the commit message body.

- [ ] **Step 2: Failing tests**

`tests/compat.rs`: add a second test:

```rust
#[sqlx::test]
async fn wave_two_routes_and_shapes_answer_409_outdated(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    let zero = "00000000-0000-0000-0000-000000000000";
    for req in [
        get("/api/v1/me/project-versions", Some(&coord)),
        get(&format!("/api/v1/projects/{id}/frames/{zero}/holders"), Some(&coord)),
        put(&format!("/api/v1/projects/{id}/holders/self"), &json!({"full": false, "add": [], "remove": []}), Some(&coord)),
        post(&format!("/api/v1/projects/{id}/frames/{zero}/version"), &json!({"blake3": "0".repeat(64), "byteSize": 1, "xxh3": "0".repeat(16)}), Some(&coord)),
    ] {
        let (status, body) = send(&app, req).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(as_json(&body)["error"], "collab_api_outdated");
    }
}
```

Append to `tests/common/mod.rs`:

```rust
/// How many devices claim frame `uuid` on its CURRENT content version,
/// read from the holder snapshot (the per-frame holders route is retired).
pub async fn current_holder_count(app: &axum::Router, token: &str, project_id: &str, uuid: &str) -> usize {
    let (status, body) = send(app, get(&format!("/api/v1/projects/{project_id}/holders/snapshot"), Some(token))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let snap = as_json(&body);
    let frame = snap["frames"].as_array().unwrap().iter().find(|f| f["uuid"] == uuid).expect("frame visible to the caller");
    let (seq, cv) = (frame["seq"].as_i64().unwrap(), frame["contentVersion"].as_i64().unwrap());
    snap["devices"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| {
            d["claims"].as_array().unwrap().iter().any(|run| {
                let (start, len, v) = (run[0].as_i64().unwrap(), run[1].as_i64().unwrap(), run[2].as_i64().unwrap());
                start <= seq && seq < start + len && v == cv
            })
        })
        .count()
}
```

Replace every per-frame GET in the listed tests with `current_holder_count(&app, TOKEN, ID, UUID).await` and keep the expected numbers. The count keeps the old GET's rule: current version, non-revoked devices, current members. The "relay URL only, never direct addresses" assertion in `holder_delta_add_remove_and_full_resync` moves onto the snapshot: `assert!(snap["devices"][0].get("directAddrs").is_none())`.

Run: `DATABASE_URL=… cargo test --test compat` → FAIL: `project-versions` still answers 200 and the per-frame GET still answers 200.

- [ ] **Step 3: Implement**

In `src/routes/mod.rs`:
- `.route("/api/v1/me/project-versions", get(compat::retired))`;
- `.route("/api/v1/projects/{id}/frames/{frame_uuid}/holders", get(compat::retired))`;
- remove `pub mod versions;`, `AppState.versions` and its initialisation.

Delete `src/routes/versions.rs` and `tests/versions.rs`. Its assertions ("versions move on every device-visible change") are covered by `tests/feed_publish.rs::every_document_kind_is_published` and `two_bumps_in_one_transaction…`.

In `src/project_version.rs`, delete `VersionCache`, `Entry`, `MAX_AGE` and the tests module, and rewrite the module doc.

In `src/feed/publish.rs::commit_and_publish`, drop the `versions` clone and loop.

In `src/routes/holders.rs`, delete `HolderView` and `frame_holders`.

`rg -n "versions.invalidate|VersionCache|frame_holders\(" src tests` → no hits.

- [ ] **Step 4: Run and commit**

Run: `DATABASE_URL=… cargo test 2>&1 | tail -10 && cargo build --release 2>&1 | grep -c warning` → green; `0`.

```bash
git add -A src tests
git commit -m "feat(hub)!: retire GET /me/project-versions and the per-frame holders route (409 collab_api_outdated); wave-2 bodies are outdated too" \
           -m "Test-hub check: <paste the three outputs of Step 1>" \
           -m "Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RYd9tanU4ehWLDXkPv159r"
```

---

### Task 11: Portal coverage on valid claims and live presence

**Files:**
- Modify: `src/routes/projects.rs:640-656` (`CoverageView` doc), `:945-963` (`online_holders` from `FeedHub::connected_devices`), `:18` (drop the `HOLDER_ONLINE_SQL` import).
- Modify: `src/routes/holders.rs:22-24` (delete `HOLDER_ONLINE_SQL`).
- Modify: `portal/src/types.ts:51-54, 110-122` (doc comments). The portal reads nothing else that changed: it never read `holderCount`, the holders routes or `/me/project-versions` (`rg -n "holderCount|/holders|project-versions" portal/src` → only these comments).
- Test: `tests/coverage.rs:14-145` (`app_with_feed`; `onlineHolders` 0 without streams, 2 with both holders connected).

**Interfaces:**
- Consumes: `FeedHub::connected_devices` (Task 3), `claims::VALID_CLAIM_SQL` (Task 1).
- Wire: `CoverageView` keeps its shape: `onlineHolders` now means holders connected per the hub's live presence (spec §5.2: "a server-side count computed from valid claims").

- [ ] **Step 1: Failing test**

In `tests/coverage.rs`:
- Switch the test to `let fa = app_with_feed(pool.clone()); let (app, mailer) = (&fa.app, &fa.mailer);` (adjust the `&app` borrows).
- Replace the `onlineHolders` assertion block (`:141-145`) with:

```rust
    // Holders count as online only while their device is connected (spec
    // 2026-09-25 §4.2): no stream open yet → 0.
    assert_eq!(coverage["onlineHolders"], 0);
    let (_, _, rc) = SseReader::open(app, &coord).await;
    let mut rc = rc.unwrap();
    rc.next_named("hello").await;
    let (_, _, ra) = SseReader::open(app, &anna).await;
    let mut ra = ra.unwrap();
    ra.next_named("hello").await;
    let (_, body) = send(app, get(&format!("/api/v1/projects/{id}"), None)).await;
    assert_eq!(as_json(&body)["coverage"]["onlineHolders"], 2, "project-wide distinct connected holders");
```

Run: `DATABASE_URL=… cargo test --test coverage` → FAIL: the first read is 2, via `last_seen_at`.

- [ ] **Step 2: Implement**

`src/routes/projects.rs`: replace the `online_holders` query with:

```rust
    // `onlineHolders` (spec 2026-09-25 §5.2): DISTINCT devices holding a
    // valid claim on a published, accepted frame that are connected right
    // now per the hub's live presence (§4.2) — `last_seen_at` is no longer a
    // liveness signal.
    let holding: Vec<Uuid> = sqlx::query_scalar(&format!(
        "SELECT DISTINCT h.device_id FROM frame_holders h \
         JOIN devices d ON d.id = h.device_id \
         JOIN project_members pm ON pm.project_id = h.project_id AND pm.account_id = d.account_id \
         JOIN project_frames f ON f.project_id = h.project_id AND f.frame_uuid = h.frame_uuid \
         WHERE f.project_id = $1 AND f.state = 'published' AND f.accepted AND {}",
        crate::claims::VALID_CLAIM_SQL
    ))
    .bind(project.id)
    .fetch_all(&state.db)
    .await?;
    let connected = state.feed.connected_devices();
    let online_holders = holding.iter().filter(|d| connected.contains(d)).count() as i64;
```

The `CoverageView` doc: "`singleHolderFrames`/`wellReplicatedFrames` count published, accepted frames by their valid claims (live, current version, non-revoked device of a current member); `onlineHolders` counts distinct such holders connected right now."

`portal/src/types.ts`:
- The `version` doc becomes "Bumped by every device-visible write — the cursor the hub's event channel carries in its `project` events (collab v3 wave 3)."
- The `CoverageView` doc uses the same wording as the Rust doc above.

- [ ] **Step 3: Run and commit**

```bash
DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test 2>&1 | tail -5
cd portal && npx vitest run 2>&1 | tail -3 && npx tsc -b && cd ..
git add src/routes/projects.rs src/routes/holders.rs portal/src/types.ts tests/coverage.rs
git commit -m "feat(hub,portal): coverage from valid claims; onlineHolders from live presence, not last_seen_at" \
           -m "Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RYd9tanU4ehWLDXkPv159r"
```

---

### Task 12: astronet — state directory, descriptors, the event-stream location, nginx workers, the probe

Repo: `/Volumes/BigMac/Users/astrobureau/Documents/astronet` (branch `main`).

This task changes files and checks them. It deploys nothing: a deploy happens only on the owner's word.

**Files:**
- Modify: `templates/athenaeum-hub.service.j2`, `templates/hub.env.j2`, `templates/nginx-projects.artfrom.space.conf.j2`, `deploy_athenaeum_hub.yml`.

**Interfaces:**
- Consumes (hub): `STATE_DIRECTORY` (Task 9), `HUB_EVENTS_PROBE_TOKEN` and the probe hello (Task 5), `LimitNOFILE` (spec §5.4).
- Produces:
  - the unit gets `StateDirectory={{ hub_instance_name }}` (`athenaeum-hub` for prod, `athenaeum-hub-test` for the test hub) and `LimitNOFILE=65536`;
  - hub.env gets `HUB_EVENTS_PROBE_TOKEN`;
  - the live vhost gets `location = /api/v1/me/events`;
  - nginx gets `worker_connections 4096` and `worker_rlimit_nofile 16384`;
  - post-deploy probes check the first byte of the stream within 2 s, directly and through nginx.

- [ ] **Step 1: Unit, env and fresh-host vhost template**

`templates/athenaeum-hub.service.j2`, after `RestartSec=5`:

```ini
# Collab v3 wave 3: the feed's high-water marks (feed-marks.json) live here.
# systemd creates /var/lib/{{ hub_instance_name }} for the service user and
# exports $STATE_DIRECTORY — the one writable path under ProtectSystem=strict.
StateDirectory={{ hub_instance_name }}
StateDirectoryMode=0700
# One SSE stream per signed-in device: the default soft limit of 1024
# descriptors would cap the hub near 1,000 streams.
LimitNOFILE=65536
```

`templates/hub.env.j2`, after `HUB_RELAY_AUTH_TOKEN`:

```
# Bearer the post-deploy probe opens /api/v1/me/events with (collab v3 wave 3).
# Generated once and preserved on re-run, like the relay token.
HUB_EVENTS_PROBE_TOKEN={{ hub_events_probe_token }}
```

`templates/nginx-projects.artfrom.space.conf.j2`, before `location /api/v1/ {` (fresh hosts only; live hosts get the converge in Step 2):

```nginx
    # The hub event stream (collab v3 wave 3): one long-lived SSE response per
    # device. No buffering, an hour of read timeout, HTTP/1.1 upstream with the
    # hop-by-hop Connection header cleared.
    location = /api/v1/me/events {
        proxy_pass         http://{{ hub_bind_addr }}/api/v1/me/events;
        proxy_http_version 1.1;
        proxy_set_header   Connection        "";
        proxy_set_header   Host              $host;
        proxy_set_header   X-Real-IP         $remote_addr;
        proxy_set_header   X-Forwarded-For   $proxy_add_x_forwarded_for;
        proxy_set_header   X-Forwarded-Proto $scheme;
        proxy_buffering    off;
        proxy_cache        off;
        proxy_read_timeout 1h;
    }
```

- [ ] **Step 2: The playbook**

`deploy_athenaeum_hub.yml`:

1. After "Read back existing relay auth token" add:

```yaml
    - name: Read back existing event-stream probe token (empty on first run)
      ansible.builtin.shell: |
        set -o pipefail
        if [ -f {{ hub_config_dir }}/hub.env ]; then
          sed -n 's/^HUB_EVENTS_PROBE_TOKEN=\(.*\)/\1/p' {{ hub_config_dir }}/hub.env
        fi
      args:
        executable: /bin/bash
      register: existing_probe_token
      changed_when: false
      no_log: true
```

2. In "Resolve hub secrets" add:

```yaml
        hub_events_probe_token: >-
          {{ existing_probe_token.stdout | trim
             if (existing_probe_token.stdout | trim | length > 0)
             else lookup('password', '/dev/null length=43 chars=ascii_letters,digits') }}
```

3. After "Ensure the live vhost proxies / to the hub (portal SPA)" add the converge. The vhost is write-once: certbot owns it, so a template edit never reaches a provisioned host. This follows the portal-proxy precedent above it:

```yaml
    - name: Ensure the live vhost streams /api/v1/me/events unbuffered (collab v3 wave 3)
      ansible.builtin.blockinfile:
        path: /etc/nginx/sites-available/{{ hub_domain }}
        marker: "    # {mark} ANSIBLE MANAGED: hub event stream"
        insertbefore: '^\s*location /api/v1/ \{'
        block: |2
              location = /api/v1/me/events {
                  proxy_pass         http://{{ hub_bind_addr }}/api/v1/me/events;
                  proxy_http_version 1.1;
                  proxy_set_header   Connection        "";
                  proxy_set_header   Host              $host;
                  proxy_set_header   X-Real-IP         $remote_addr;
                  proxy_set_header   X-Forwarded-For   $proxy_add_x_forwarded_for;
                  proxy_set_header   X-Forwarded-Proto $scheme;
                  proxy_buffering    off;
                  proxy_cache        off;
                  proxy_read_timeout 1h;
              }
      when: "'location = /api/v1/me/events' not in (hub_vhost_live.content | b64decode)"
      register: hub_vhost_events
```

4. Before "Validate nginx config" add:

```yaml
    # One proxied SSE stream holds two connections (client + upstream). Sized
    # once the core count is known (spec 2026-09-25 §5.4). NOTE: nginx.conf is
    # host-wide — this also applies to the production hub and every other site
    # on this VPS.
    - name: Read the host's core count
      ansible.builtin.command: nproc
      register: hub_nproc
      changed_when: false

    - name: Assert nginx runs one worker per core
      ansible.builtin.command: "grep -Eq '^\\s*worker_processes\\s+(auto|{{ hub_nproc.stdout }});' /etc/nginx/nginx.conf"
      changed_when: false

    - name: Raise nginx worker_connections to 4096
      ansible.builtin.lineinfile:
        path: /etc/nginx/nginx.conf
        regexp: '^\s*worker_connections\s+\d+;'
        line: "\tworker_connections 4096;"
      register: hub_nginx_workers

    - name: Let each nginx worker open enough descriptors for 4096 proxied connections
      ansible.builtin.lineinfile:
        path: /etc/nginx/nginx.conf
        regexp: '^worker_rlimit_nofile\s+'
        line: "worker_rlimit_nofile 16384;"
        insertafter: '^worker_processes'
      register: hub_nginx_rlimit

    - name: Report the nginx worker sizing
      ansible.builtin.debug:
        msg: "nginx: {{ hub_nproc.stdout }} core(s) × 4096 connections (≈ {{ (hub_nproc.stdout | int) * 2048 }} proxied streams)"
```

5. Extend "Reload nginx when the vhost changed" to `when: hub_vhost.changed or hub_vhost_link.changed or hub_vhost_portal.changed or hub_vhost_events.changed or hub_nginx_workers.changed or hub_nginx_rlimit.changed`.

6. In `post_tasks`, after "Verify the portal shell through nginx (https)" add:

```yaml
    # The event stream must deliver its first byte within 2 s (spec §5.4): a
    # buffering proxy would hold it back. Probe token provisioned above.
    - name: Verify the event stream directly (first byte within 2 s)
      ansible.builtin.shell: |
        out=$(timeout 2 curl -sN -H "Authorization: Bearer ${PROBE}" http://{{ hub_bind_addr }}/api/v1/me/events | head -c 12)
        test "$out" = "event: hello"
      args:
        executable: /bin/bash
      environment:
        PROBE: "{{ hub_events_probe_token }}"
      changed_when: false
      no_log: true

    - name: Verify the event stream through nginx (https, first byte within 2 s)
      ansible.builtin.shell: |
        out=$(timeout 2 curl -sN -H "Authorization: Bearer ${PROBE}" https://{{ hub_domain }}/api/v1/me/events | head -c 12)
        test "$out" = "event: hello"
      args:
        executable: /bin/bash
      environment:
        PROBE: "{{ hub_events_probe_token }}"
      changed_when: false
      no_log: true
```

7. In the `Report` message, add the line `  events : https://{{ hub_domain }}/api/v1/me/events -> first byte < 2 s`. In the header comment's "What it does" list, add: `* Event stream (collab v3 wave 3): StateDirectory + LimitNOFILE in the unit, an unbuffered location = /api/v1/me/events converged into the write-once vhost, nginx worker_connections 4096, a 2 s first-byte probe.`

- [ ] **Step 3: Check the files. Syntax and task listing only; no host contact**

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/astronet
ansible-playbook deploy_athenaeum_hub.yml --syntax-check
ansible-playbook deploy_athenaeum_hub.yml -e hub_target=athenaeum_hub_test --syntax-check
ansible-playbook deploy_athenaeum_hub.yml --list-tasks | grep -Ei 'event stream|worker|probe token|core count'
command -v yamllint >/dev/null && yamllint -d relaxed deploy_athenaeum_hub.yml || true
```

Expected:
- both syntax checks print `playbook: deploy_athenaeum_hub.yml`;
- `--list-tasks` shows the eight new task names.

A `--check --diff` run against `athenaeum_hub_test` contacts the VPS through the owner's SSH agent. It belongs to the owner's deploy, not to this task.

- [ ] **Step 4: Commit (astronet)**

```bash
git add templates/athenaeum-hub.service.j2 templates/hub.env.j2 templates/nginx-projects.artfrom.space.conf.j2 deploy_athenaeum_hub.yml
git commit -m "athenaeum hub: event stream — StateDirectory + LimitNOFILE, unbuffered /api/v1/me/events converged into the write-once vhost, nginx worker_connections 4096, 2 s first-byte probe (collab v3 wave 3)" \
           -m "Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RYd9tanU4ehWLDXkPv159r"
```

---

### Task 13: The load check — 100 devices × 5,000 frames

**Files:**
- Create: `tests/load_check.rs` (`#[ignore]`, run explicitly).

**Interfaces:**
- Consumes:
  - `report_holders`, `digest_of`, `SseReader`, `beat`, `register_device`, `join_and_approve`, `announce_frames` (`tests/common`);
  - `driver::heads_tick` (Task 5), `POST …/frames/versions` (Task 7).
- Produces: printed measurements (setup, snapshot size and time, republish time, reconnect storm) and three assertions (spec §12).

- [ ] **Step 1: Write the harness**

`tests/load_check.rs`:

```rust
//! Load check (spec 2026-09-25 §12; plan ruling P30): 100 simulated devices
//! × 5,000 frames, in process against real Postgres through a 20-connection
//! pool. Run explicitly:
//!
//!   DATABASE_URL=postgres://hub:hub@localhost:5432/hub \
//!     cargo test --release --test load_check -- --ignored --nocapture
//!
//! Asserts:
//! - an idle hub (streams open, beats flowing, digest checks) makes zero
//!   holder writes;
//! - a 500-frame republish is one transaction;
//! - the reconnect storm after a restart finishes under 30 s with delta
//!   resume and no snapshot reads.
mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use athenaeum_hub::feed::clock::ManualClock;
use athenaeum_hub::feed::{FeedConfig, FeedHub};
use athenaeum_hub::routes::{build_router, AppState};
use axum::http::StatusCode;
use common::*;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

const DEVICES: u8 = 100;
const FRAMES: u32 = 5_000;

fn hub(pool: PgPool, mailer: &CaptureMailer) -> (axum::Router, FeedHub, ManualClock) {
    let clock = ManualClock::new();
    let mut cfg = FeedConfig::default();
    cfg.presence.warmup = Duration::ZERO;
    let feed = FeedHub::new(Arc::new(clock.clone()), cfg);
    feed.set_epoch("load-epoch".into());
    let state = AppState::new(pool, Arc::new(mailer.clone())).with_feed(feed.clone());
    (build_router(state), feed, clock)
}

async fn heads(pool: &PgPool, id: Uuid) -> (i64, i64) {
    sqlx::query_as("SELECT p.version, c.seq FROM projects p JOIN project_holder_cursor c ON c.project_id = p.id WHERE p.id = $1")
        .bind(id).fetch_one(pool).await.unwrap()
}

/// Rows written to `frame_holders` so far (Postgres flushes table stats at
/// most once a second per backend — wait it out, then read in a fresh tx).
async fn holder_writes(pool: &PgPool) -> i64 {
    tokio::time::sleep(Duration::from_millis(1500)).await;
    sqlx::query_scalar("SELECT n_tup_ins + n_tup_upd + n_tup_del FROM pg_stat_user_tables WHERE relname = 'frame_holders'")
        .fetch_one(pool).await.unwrap()
}

#[sqlx::test]
#[ignore = "load check (spec §12): run with --ignored --nocapture"]
async fn load_check_100_devices_5000_frames(test_pool: PgPool) {
    let pool = PgPoolOptions::new().max_connections(20).connect_with((*test_pool.connect_options()).clone()).await.unwrap();
    let mailer = CaptureMailer::default();
    let (app, feed, clock) = hub(pool.clone(), &mailer);

    let t = Instant::now();
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 200, "Coord").await;
    let project = create_project_via(&app, &coord, "Load", false).await;
    let id = project["id"].as_str().unwrap().to_string();
    let pid = Uuid::parse_str(&id).unwrap();
    let mut tokens = Vec::new();
    for n in 0..DEVICES {
        let (token, _) = register_device(&app, &mailer, &format!("d{n}@example.com"), n, "PC").await;
        join_and_approve(&app, &coord, &token, &id, &format!("D{n}"), "send_receive").await;
        tokens.push(token);
    }
    for start in (1..=FRAMES).step_by(500) {
        assert_eq!(announce_frames(&app, &coord, &id, start, start + 500).await.0, StatusCode::OK);
    }
    let all: Vec<(String, i32)> = (1..=FRAMES).map(|n| (frame_uuid(n), 1)).collect();
    let all_refs: Vec<(&str, i32)> = all.iter().map(|(u, v)| (u.as_str(), *v)).collect();
    let (count, digest) = digest_of(&all_refs);
    for token in &tokens {
        let (status, body) = report_holders(&app, token, &id, 1, true, &all_refs, &[], &all_refs).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["digestMatch"], true);
    }
    println!("load: setup (100 devices, 5,000 frames, 500,000 claims) in {:?}", t.elapsed());

    let t = Instant::now();
    let (status, body) = send(&app, get(&format!("/api/v1/projects/{id}/holders/snapshot"), Some(&tokens[0]))).await;
    assert_eq!(status, StatusCode::OK);
    println!("load: holder snapshot {} bytes in {:?}", body.len(), t.elapsed());

    // Idle: 100 open streams, four 15 s beat rounds, a digest check per device per round, one heads tick.
    let mut readers = Vec::new();
    let mut sessions = Vec::new();
    for token in &tokens {
        let (status, _, reader) = SseReader::open(&app, token).await;
        assert_eq!(status, StatusCode::OK);
        let mut reader = reader.unwrap();
        let hello = reader.next_named("hello").await;
        assert_eq!(hello["projects"][id.as_str()]["claimDigest"], digest);
        sessions.push(hello["sessionId"].as_str().unwrap().to_string());
        readers.push(reader);
    }
    let digest_check: Value = json!({"reportSeq": 1, "full": false, "add": [], "remove": [], "digest": digest, "count": count});
    let (_, seq_idle) = heads(&pool, pid).await;
    let writes_before = holder_writes(&pool).await;
    for _ in 0..4 {
        clock.advance(Duration::from_secs(15));
        feed.tick();
        for session in &sessions {
            assert_eq!(beat(&app, session, json!({ id.as_str(): true }), None).await.0, StatusCode::NO_CONTENT);
        }
        for token in &tokens {
            let (status, body) = send(&app, put(&format!("/api/v1/projects/{id}/holders/self"), &digest_check, Some(token))).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(as_json(&body)["digestMatch"], true);
        }
    }
    athenaeum_hub::feed::driver::heads_tick(&pool, &feed).await.unwrap();
    assert_eq!(holder_writes(&pool).await, writes_before, "an idle hub makes zero holder writes (spec §12)");
    assert_eq!(heads(&pool, pid).await.1, seq_idle);

    // A 500-frame republish is one transaction.
    let versions: Vec<Value> = (1..=500)
        .map(|n| json!({"uuid": frame_uuid(n), "expectedVersion": 1, "blake3": format!("{:064x}", 50_000 + n), "byteSize": 1, "xxh3": format!("{:016x}", 50_000 + n)}))
        .collect();
    let (v0, c0) = heads(&pool, pid).await;
    let t = Instant::now();
    let (status, _) = send(&app, post(&format!("/api/v1/projects/{id}/frames/versions"), &json!({"versions": versions}), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK);
    println!("load: 500-frame republish in {:?}", t.elapsed());
    assert_eq!(heads(&pool, pid).await, (v0 + 1, c0 + 1), "one transaction: one version bump, one holder-cursor bump");

    // Reconnect storm: a fresh FeedHub (everyone offline) on the same database.
    drop(readers);
    let (app2, _feed2, _clock2) = hub(pool.clone(), &mailer);
    let t = Instant::now();
    let mut tasks = Vec::new();
    for token in tokens.clone() {
        let (app2, id, digest) = (app2.clone(), id.clone(), digest.clone());
        tasks.push(tokio::spawn(async move {
            let (status, _, reader) = SseReader::open(&app2, &token).await;
            assert_eq!(status, StatusCode::OK);
            let mut reader = reader.unwrap();
            let hello = reader.next_named("hello").await;
            assert_eq!(hello["projects"][id.as_str()]["claimDigest"], digest, "digest match: no full report needed (C27)");
            let (status, _) = send(&app2, get(&format!("/api/v1/projects/{id}/holders?since={c0}"), Some(&token))).await;
            assert_eq!(status, StatusCode::OK, "delta resume, not a snapshot");
            reader
        }));
    }
    let mut kept = Vec::new();
    for task in tasks {
        kept.push(task.await.unwrap());
    }
    let storm = t.elapsed();
    println!("load: reconnect storm of {DEVICES} devices in {storm:?}");
    assert!(storm < Duration::from_secs(30), "reconnect storm {storm:?} ≥ 30 s (spec §12)");
}
```

- [ ] **Step 2: Run it**

Run: `DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test --release --test load_check -- --ignored --nocapture 2>&1 | grep -E '^load:|test result'`
Expected: four `load:` lines and `test result: ok`. If an assertion fails, it is a finding: diagnose it (superpowers:systematic-debugging), do not loosen the bound. The spec numbers are the gate.

- [ ] **Step 3: Commit, with the measurements in the message**

```bash
git add tests/load_check.rs
git commit -m "test(hub): load check — 100 devices × 5,000 frames: idle zero holder writes, 500-frame republish in one transaction, reconnect storm < 30 s" \
           -m "<paste the four load: lines>" \
           -m "Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RYd9tanU4ehWLDXkPv159r"
```

---

### Task 14: Docs, the migration replay, the whole-suite gate

**Files:**
- Modify (hub): `README.md`:
  - the route table (`:100-122`): add the new routes, mark the retired ones, drop `holderCount`;
  - a new section "Live exchange (collab v3 wave 3)" after "Frames, the manifest and holders" (`:571`);
  - "Scheduler" (`:758`): the tombstone prune;
  - "Environment variables" (`:943`): `HUB_EVENTS_PROBE_TOKEN`, `STATE_DIRECTORY`;
  - "Migrations" (`:978`): 0023;
  - "Deploy sketch" (`:1013`): the unit lines and the events location;
  - a runbook paragraph for a database restore.
- Modify (app repo): `docs/superpowers/open-items.md`. Add a new "Collab v3 wave 3 — hub live exchange" section under "Unverified by hand".

- [ ] **Step 1: README**

- **Route table.** One line per new or changed route, in the file's existing format:

```
GET    /api/v1/me/events                                      (device token, athenaeum) the event channel — SSE: hello, project, holders, presence, account, resync, versions (see "Live exchange")
POST   /api/v1/me/presence                                    (session id)    the 15 s beat {sessionId, serving, relayUrl} — 204; 409 session_gone
DELETE /api/v1/me/presence                                    (session id)    clean exit {sessionId} — 204
GET    /api/v1/projects/{id}/holders/snapshot                 (member)        the holder map in one REPEATABLE READ read — {epoch, holderSeq, version, frames, devices[claims as runs]}
GET    /api/v1/projects/{id}/holders?since=S                  (member)        claim changes after S, paged — 410 below the floor / ahead / other epoch
PUT    /api/v1/projects/{id}/holders/self                     (device token)  holder report {reportSeq, full, add[{uuid,contentVersion}], remove, digest, count} → {holderSeq, digestMatch, nextFlushMs, refused}
POST   /api/v1/projects/{id}/frames/versions                  (device token)  ≤ 500 compare-and-set re-versions in one transaction → per-frame results
POST   /api/v1/projects/{id}/frames/{frame_uuid}/version      (device token, publisher-only) compare-and-set on expectedVersion — 409 version_conflict {contentVersion}
POST   /api/v1/devices/{id}/revoke                            (device token)  optional {retire}; tombstones the device's claims, bumps members in every project of the account, closes its stream
```

- **Retired routes.** Delete the `GET /api/v1/me/project-versions` and per-frame holders lines. Add both routes to the retired list after the table, along with "a holder report without `reportSeq`" and "a version without `expectedVersion`", all answering `409 collab_api_outdated`.

- **"Live exchange (collab v3 wave 3)" section.** A condensed, hub-side version of the plan's § Wire contract, with a pointer to the spec and the plan for the full contract:
  - the SSE framing and the event list;
  - the beat and offline rules with their numbers;
  - the cursor rules for clients;
  - the claim model (report_seq, full, refused, the digest definition, plus the worked vector for `{(a,1),(b,1)}` → `d173abce7c5386289c657a8a697518d8`);
  - the lock order;
  - the epoch.

- **Runbook paragraph (restore).** "To restore the hub database from a dump:
  1. Stop the service.
  2. Restore.
  3. Run `sudo -u athenaeum-hub env $(cat /etc/athenaeum-hub/hub.env | xargs) STATE_DIRECTORY=/var/lib/athenaeum-hub /usr/local/bin/athenaeum-hub rotate-epoch`.
  4. Start the service.

  Even without step 3, startup rotates the epoch on its own when the database is behind the marks file. Every client then reloads its snapshots and re-announces its frames (spec §4.4). The hub has no automated database backups yet (backlog)."

- **Environment variables.** `HUB_EVENTS_PROBE_TOKEN` (optional; the deploy probe's bearer for `/me/events`) and `STATE_DIRECTORY` (set by systemd `StateDirectory=`; the marks file; unset disables restore detection by marks).

- [ ] **Step 2: The migration replay against a wave-1 database**

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-hub
docker compose exec -T postgres psql -U hub -c 'DROP DATABASE IF EXISTS replay_w3' -c 'CREATE DATABASE replay_w3 OWNER hub'
git worktree add /tmp/hub-6127951 6127951
(cd /tmp/hub-6127951 && HUB_DATABASE_URL=postgres://hub:hub@localhost:5432/replay_w3 HUB_BIND_ADDR=127.0.0.1:8091 timeout 60 cargo run 2>&1 | grep -m1 'migrations applied')
HUB_DATABASE_URL=postgres://hub:hub@localhost:5432/replay_w3 HUB_BIND_ADDR=127.0.0.1:8092 timeout 20 cargo run 2>&1 | grep -E 'migrations applied|feed epoch' | head -3
docker compose exec -T postgres psql -U hub -d replay_w3 -tAc "SELECT max(version) FROM _sqlx_migrations; SELECT count(*) FROM project_holder_cursor"
git worktree remove /tmp/hub-6127951
```

Expected:
- the wave-1 binary applies 0001–0022;
- the wave-3 binary logs `migrations applied` and `feed epoch created`;
- `_sqlx_migrations` reaches 23.

The claim conversion over real 0022 rows is pinned by `tests/claims.rs::migration_0023_numbers_frames_and_converts_holds`.

- [ ] **Step 3: The whole-suite gate**

```bash
DATABASE_URL=postgres://hub:hub@localhost:5432/hub cargo test 2>&1 | grep -E '^test result' | sort | uniq -c
cargo build --release 2>&1 | grep -c warning                                   # 0
rg -n 'println!|eprintln!' src                                                  # no hits
rg -in 'VersionCache|reported_at|HOLDER_FRESH_SQL|HOLDER_ONLINE_SQL|project-versions' src   # only compat/route-table mentions of project-versions
cd portal && npx vitest run 2>&1 | tail -3 && npx tsc -b && cd ..
```

Expected: every `test result` is `ok`, no warnings, and no stray symbols.

- [ ] **Step 4: open-items (app repo)**

Add this section to `docs/superpowers/open-items.md` under "## Unverified by hand", above "### Collab v3 wave 2":

```markdown
### Collab v3 wave 3 — hub live exchange (2026-09-25)

Hub branch `collab-v3-wave3`, merged to local main only; astronet commit on
`main`, not deployed. The hub now pushes (SSE `/api/v1/me/events`), keeps
presence live, holds durable device-owned claims with a digest, versions by
compare-and-set, and propagates revocation (spec
`specs/2026-09-25-collab-v3-live-exchange-design.md`, plan
`plans/2026-09-25-collab-v3-wave3-hub-live-exchange-plan.md`).

- **Owed — test-hub deploy (owner's word).** Run `ansible-playbook
  deploy_athenaeum_hub.yml -e hub_target=athenaeum_hub_test -e
  hub_artifact_ref=collab-v3-wave3 -e @~/.config/athenaeum-hub/smtp.yml`. It
  converges the unit (StateDirectory, LimitNOFILE), the events location in
  the write-once vhost, nginx worker sizing (HOST-WIDE, the production hub
  included) and runs the 2 s first-byte probe. Post-deploy:
  - `SELECT max(version) FROM _sqlx_migrations` → 23;
  - `ls /var/lib/athenaeum-hub-test/feed-marks.json` (appears within 60 s);
  - `journalctl -u athenaeum-hub-test | grep 'feed epoch'`.
- **Owed — the load check on the test hub (P30).** The in-process harness
  (`tests/load_check.rs`) passed locally with the numbers in its commit. A
  run against the deployed test hub needs 100 device tokens minted there.
- **Owed — the one-week soak** (spec §15), with at least three real devices
  on the test hub once app wave 3 lands: the hourly digest checks all match.
- **Expected, do not re-flag:**
  - The desktop app on local main (wave 2) gets `409 collab_api_outdated`
    from a wave-3 hub on `/me/project-versions`, per-frame holders, holder
    reports and versions, until app wave 3.
  - The released app never called these routes.
```

- [ ] **Step 5: Commit both repos**

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-hub
git add README.md
git commit -m "docs(hub): live exchange — event channel, presence beat, claims and digest, snapshot/deltas, CAS versions, epoch runbook; route table" \
           -m "Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RYd9tanU4ehWLDXkPv159r"
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum
git add docs/superpowers/open-items.md
git commit -m "docs: collab v3 wave 3 hub — deploy, load-check and soak owed; wave-2 app outdated against it" \
           -m "Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01RYd9tanU4ehWLDXkPv159r"
```

No push and no deploy without the owner's word. The hub branch is merged to local main after the final whole-branch review (superpowers:finishing-a-development-branch).

---

## Self-review

**1. Spec coverage**

| Spec | Requirement | Task |
| ---- | ---- | ---- |
| §3 I2 | counters in the writing tx, gapless, commit order | 1 (cursor + watermark test), 4 (contiguous `prev`) |
| §3 I3 | snapshot with its cursor; `prev`-checked deltas; epoch/floor reload | 5 (subscribe-then-read hello), 6 (REPEATABLE READ, 410s), 9 |
| §3 I4 | device-owned durable claims by version; hub writes the publisher's first; ends by report, revoke/retire, leaving | 1, 2, 8 |
| §3 I5 | presence ephemeral, add-only, never written into holdings | 3, 5 |
| §3 I10 | version bump is CAS | 7 |
| §3 I11 | members bump everywhere on who-may-connect changes; stream closed | 8 (revoke/retire/device add/leave), 4 (member add/remove) |
| §4.1 | SSE `/me/events`, `X-Accel-Buffering: no`, keepalive 20 s, `retry: 3000`, one stream per device | 5 |
| §4.2 | session + hello `sessionId`; beat 15 s outside token auth, in-memory; `409 session_gone`; DELETE; 10 s grace; 40 s silence; flap damping; 1 s per-project fan-out to members only; relay URL in `connected`; 30 s warm-up + replace; `last_seen_at` unchanged; serving facet | 3, 5 (P14–P17) |
| §4.3 | the seven events; inline ≤ 50 rows / `more`; contiguous delivery + 2 s gap → `resync`; 60 s `versions` | 3, 4, 5 |
| §4.4 | epoch in `hub_meta`; 60 s marks in the state dir; startup rotation; explicit rotation | 5 (basic), 9 |
| §4.5 | `FeedHub` (broadcast 512, mailbox, presence, coalescers); publish after commit in a spawned task; lag → resync; no DB connection held; debug span via `make_span_with`; single instance | 3, 4, 5 |
| §5.1 | `hub_meta.epoch`, `next_frame_seq`, `frame_seq` + ordinals for 0022 rows, claim columns, `reported_at` dropped, report_seq upsert rule, tombstones + 7-day prune + floor, `project_holder_cursor`, `device_project_digest`, lock order (report locks cursor only) | 1, 2, 6 |
| §5.2 | new version: no deletes + CAS; reject: no deletes; duplicate announce unchanged; revoke `retire` + I11; device add bumps members; `holder_count` gone; portal coverage from valid claims | 1, 7, 8, 11 |
| §5.3 | every endpoint row; retirement after the test-hub check; API bump → 409 | 2, 5, 6, 7, 8, 10 |
| §5.4 | StateDirectory, LimitNOFILE, marker-guarded converge for the events location, worker_connections, pool 20, post-deploy probe with a provisioned token | 12 (astronet), 9 (pool, `STATE_DIRECTORY`), 5 (probe token) |
| §6.2 | `nextFlushMs`, report ordering by `report_seq` | 2 |
| §6.3 | digest definition; match after hello and in every PUT; full on mismatch; tombstones unlisted | 2, 5 (hello fields, P11) |
| §12 hub | cursor under concurrency, report_seq, digest, full replace, CAS, tombstones + floor, epoch rotation, revocation, SSE integration over a clock abstraction, load check | 1, 2, 7, 6, 9, 8, 5, 13 |
| §14 hub/portal | migration incl. conversion; endpoints; FeedHub; revocation; portal; tests; test-hub deploy with astronet | 1–13; the deploy is owed (14) |

Out of the hub's scope, and in the app plan: §4.1 client transport, §4.6 client retries, §6.1 holder map, §7–§9, §11, the §12 app e2e.

**2. Placeholder scan.**
- Every code step carries the code.
- The tasks that edit many existing call sites (Task 2 Step 6, Task 4 Step 4, Task 10 Step 2) give the exact transformation and a site table with `file:line` at `6127951`, plus an `rg` command that proves completeness.
- The only fill-ins left are measurement outputs pasted into commit messages (Tasks 10 and 13). They are data produced by the step, not missing design.

**3. Type and name consistency with § Wire contract.**
- **hello.** `HelloEvent{session_id, epoch, account_id, projects}` and `HelloProject{version, holder_seq, claim_count, claim_digest, report_seq, presence}` serialise to exactly the contract's `hello`.
- **PUT holders/self.** `HoldersReport` / `HoldersReportResponse` field names match the contract.
- **Holder reads.** `HoldersSnapshot` / `SnapshotDevice.claims: Vec<[i32; 3]>` and `HolderDeltaPage{epoch, holder_seq, floor, deltas, has_more, next}` match.
- **Error strings.** The 410 strings (`holders_below_floor`, `holders_cursor_ahead`, `epoch_changed`), `session_gone`, `version_conflict` and `collab_api_outdated` are each produced in exactly one place.
- **`device`.** It is `security::encode_pubkey` (base64 standard) everywhere: `HolderWrite::finish`, snapshot, deltas, presence.
- **Cross-task names.**
  - `HolderWrite::{lock, seq, base_seq, record_add, record_rm, finish}` (Task 1) are used unchanged by Tasks 2, 6–8.
  - `FeedBatch::{bump, frames, holders, account, close_device, close_account}` (Task 4) are used by Tasks 7, 8.
  - `commit_and_publish(&state, tx, feed)` has one signature throughout.
  - `driver::heads_tick(&pool, &feed)` (Task 5) is used by Tasks 9, 13.
  - `FeedHub::{live_relay, connected_devices}` (Task 3) are used by Tasks 6, 11.
