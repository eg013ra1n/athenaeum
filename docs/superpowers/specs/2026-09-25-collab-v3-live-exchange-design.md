# Collaboration v3, wave 3 — live exchange

Status: design, agreed with the owner in dialogue on 2026-09-25 after the wave-2
review, revised the same day after an independent architecture review (§16).
It replaces the timing and presence model of the wave-2 exchange (the 15 s
version poll, the 20-minute replication pass, the 75-minute holder freshness,
per-frame holder lookups, the loss-guard pause) with a push-driven,
event-ordered model. Everything else in
`2026-09-23-collab-v3-per-frame-model-design.md` (the v3 spec) stands unless
§2 of this document overrides it. Amendment A4 in the v3 spec records the
override and the new wave numbering.

The design was checked against prior art in five directions (swarm protocols,
multi-device folder sync, our transport's own ecosystem, server push and
presence, replica-state collisions). The research notes live outside the repo
because they name third-party projects; this document states only the
conclusions.

## 1. Why

On 2026-09-25 the owner read the wave-2 report and rejected its timing model.
The measured behaviour of wave 2:

- **Seeder return.** A seeder that comes back online changes nothing anyone
  watches. Holder reports do not bump `projects.version`, and the hub has no
  push. Other devices notice only on their own 20-minute pass, through one
  `GET …/frames/{uuid}/holders` per frame.
- **Holder freshness.** A holder counts only while its last full report is
  younger than 75 minutes, so every device re-reports its whole set every 20
  minutes. That is about 1,700 row updates/s at 20 processors × 20 projects ×
  5,000 frames, doing nothing.
- **Online.** "Online" means `devices.last_seen_at` younger than 5 minutes,
  which is a side effect of the 15 s poll.
- **Pass cadence.** Disk truth, the loss guard and the full holder report all
  ride one 20-minute pass. A hub error ends the pass until the next timer. A
  failing project backs off 20 minutes, and "Sync now" does not bypass that
  back-off.
- **Receive priority.** Collab and personal transfers share one FIFO receive
  semaphore. A 200-frame collab batch can hold a lane while a personal
  transfer waits.
- **Irreversible decline.** "Stop keeping" is irreversible: no code path clears
  `locally_declined`.
- **Edited files are served.** iroh-blobs 0.103 serves a reference-imported
  file that was edited on disk without any check
  (`iroh-blobs/src/store/fs/bao_file.rs:591-598`). The receiver's verification
  rejects the bytes, but only after the edited replica has started serving
  them.
- **Pool timeouts.** The assignment engine's connection pool runs with the pool
  defaults, a 1 s connect timeout and a 5 s idle timeout
  (`sharing/iroh/assign.rs:480`). That is tight for cross-continent relay
  dials, and too short for a connection to serve as a health signal.
- **Landing deletes before it replaces.** `export_child` removes the target
  before exporting onto it (`sharing/iroh/blobs.rs:701`). On a version bump
  the old file is gone before the new one arrives, so a failure between the
  two steps loses the only local copy.
- **Revocation does not propagate.** Revoking a device only sets `revoked_at`
  (`athenaeum-hub/src/routes/devices.rs:106-111`). The membership snapshot
  changes without a version bump (`routes/snapshots.rs:6-9`), so other
  members' connect gates keep admitting the revoked node until something else
  refreshes them.

The target is swarm-like behaviour:

- a seeder is visible to everyone about a second after it connects;
- a vanished seeder drops out within a minute;
- a landed frame is servable to others within seconds;
- no hub request is made per frame;
- nothing on the critical path waits for a timer.

## 2. Owner rulings (2026-09-25)

Numbered L1–L13 so plans and reviews can cite them.

- **L1 — Personal transfers take priority over collab** on both the receive
  and the upload side, whether or not a byte-rate cap is set. A user often
  finishes pulling files they are about to share.
- **L2 — Presence is live.** A seeder appears as soon as it connects. A
  seeder is dropped by a health check measured in seconds, never by the age
  of a report. Holdings never expire with time.
- **L3 — Sync is event-driven in both directions.**
  - No per-frame holder lookups.
  - No periodic pass on the critical path.
  - A hub error retries that request with back-off; it never ends the work
    until a timer.
- **L4 — Deletion of a replica** is judged over a rolling 5-minute window,
  after each deletion has settled for 60 s.
  - **Single deletions** (≤ 10 frames in the window) are re-fetched
    automatically.
  - **Mass deletions** (> 10 frames in the window) raise one non-blocking,
    reversible choice: "Re-fetch" or "Stop keeping". A frame the user deletes
    a second time within 24 h joins that choice whatever the count, so the app
    never fights a user who deletes one file at a time. Everything else keeps
    running while the choice is open.
  - **Last-copy warning.** "Stop keeping" warns when a frame has fewer than 2
    other holders of its current version, offline holders included.
  - **Lost frame.** A deletion that leaves no holder of the current version
    anywhere raises a notification at once ("lost everywhere — restore it from
    the Trash"), because an automatic re-fetch has nowhere to fetch from.
- **L5 — Edited replicas are quarantined.**
  - The moment a change is confirmed, the file stops serving.
  - It is listed under "Changed files" with **Re-fetch original** and
    **Delete**.
  - It is never overwritten or deleted silently, including when the publisher
    releases a new version.
- **L6 — "Stop keeping" is reversible.** A "Not kept" list offers
  **Keep again** per frame and for all.
- **L7 — Superseded versions are not served.**
  - Once v2 is current, nobody serves or fetches v1.
  - A replica keeps its v1 file on disk until v2 has replaced it atomically
    (§7.5).
  - While only the offline publisher holds v2, the frame shows "v2 waiting for
    the publisher".
- **L8 — A new version of an accepted frame is not re-moderated.** This is
  unchanged: trust is per publisher, and the coordinator can exclude.
- **L9 — Device reinstall is an explicit step.** On sign-in, "This device
  replaces <device>" retires the old device and its holdings. The new device
  re-adopts the files in the Collaboration folder by hash, with no transfer.
- **L10 — "Sync now"** reconnects the event channel and reconciles at once, and
  clears every back-off.
- **L11 — Stream limits.** Two limits sit beside the existing byte-rate cap
  (Settings → Transfers, carried over from the wave-3 backlog):
  - the maximum number of simultaneous collab upload streams this device
    serves;
  - the maximum number of simultaneous receive streams one collab fetch opens.
- **L12 — The hub is the only authority for presence and change propagation.**
  - No peer gossip, synced documents or content-discovery tracker in this
    wave. The candidates were checked and are either incompatible with our
    pinned versions or unauthenticated and not durable.
  - A peer-to-peer "hub unreachable" mode is a documented future option, not a
    requirement.
- **L13 — Analysis before patches.** The wave-2 pass, loss guard and poll are
  replaced, not tuned. This follows the standing rule to stop patching and
  re-audit the model.

### 2.1 What this overrides

- v3 spec §2 **R17** (mass loss pauses replication; the guard thresholds live
  in Settings → Transfers) → L4. The guard settings `collab.loss_guard_fraction`
  and `collab.loss_guard_bytes` are removed. The deletion thresholds are fixed:
  10 frames, a 5-minute window, a 60 s settle.
- v3 spec §2 **R19** (versioned polling; retry cadence in minutes) → §4. Retries
  are in seconds (§4.6).
- v3 spec §4.2 routes `GET /me/project-versions` and
  `GET /projects/{id}/frames/{uuid}/holders` → retired (§5.3).
- v3 spec §4.2 `PUT …/holders/self` semantics ("full=true every 6 h") → §6.
- v3 spec §5.3 steps 3–6 (per-frame holder lookup, a permit per frame) → §7,
  §8.
- v3 spec §5.5 disk truth → §9.
- v3 spec amendment **A3** (full report every 20 minutes) → removed (§6).
- v3 spec §13 "hub push over SSE" → in scope here.
- Wave-2 plan rulings:
  - **R2/R17** (a receive permit per 200-frame batch) → §8.
  - **R13** (20-minute project back-off) → §4.6.
  - **R15/R18** (poll loop, 20-minute maintenance loop) → §4, §9.
  - **R16** (holder lookup per providerless frame) → §7.
  - **R19** (between-batch re-check) → §7.4.
  - **R24** (edited replica renamed aside at re-land; version bumps land over
    it) → L5, §9.4: nothing lands over a quarantined file.
  - **R25** (a lookup error ends the pass) → §4.6.
  - **R33** (an irreversible decline survives versions) → L6: a decline still
    survives versions, but it is reversible.

## 3. Invariants

Every component and every plan task is checked against these. A task that
cannot state how it keeps them is not ready.

- **I1 — Pinned targets.**
  - Every fetch, serve and landing names `(project, frame, content_version,
    blake3)`.
  - Bytes are verified against `blake3`.
  - A landing commits only if that version is still current in the local
    manifest. This is the landing fence: a DB conditional, as in the wave-2
    conditional landing (`WHERE content_version AND blake3`). It stays a DB
    conditional because the scanner and publish also write the local frame
    table.
- **I2 — One ordering authority per project.**
  - The hub feed orders everything, with cursor `(epoch, version)` for frames
    and metadata and `(epoch, holder_seq)` for holdings.
  - Both counters are assigned inside the writing transaction, under the lock
    order in §5.1, so each is gapless and in commit order.
  - Nothing is ordered by a device clock.
- **I3 — Snapshots carry their cursor; deltas are idempotent.**
  - A snapshot is read in one REPEATABLE READ transaction together with the
    cursor it reflects.
  - A delta applies only when its `prev` equals the client's cursor.
    Otherwise the client catches up over REST.
  - A client on a wrong epoch, or behind the retention floor, reloads the
    snapshot.
- **I4 — Holdings are durable facts keyed by version.**
  - A holding `(device, frame, content_version)` is written only by that
    device. The publisher's first holding is the exception: the hub writes it
    in the same transaction as the announce or the version.
  - Holdings are ordered by the device's own `report_seq` and never expire.
  - Every holding event and snapshot entry carries its `content_version`. A
    client derives "provider of the current version" by comparing it with the
    manifest; it never infers it from event order.
  - A holding ends only by the device's report, a device retirement or
    revocation (L9), or the member leaving.
- **I5 — Presence is ephemeral and comes from the hub.**
  - A frame is a candidate at a device iff the device holds the current
    version, is **connected**, is **serving** the project, and is a member.
  - Presence adds candidates. An open transfer connection is ended only by the
    connection itself closing or failing (§7.3), never by a presence event.
  - Presence is never written into holdings.
- **I6 — Versions advance by atomic replacement.**
  - `current` is the latest version (L8).
  - A replica moves forward only by landing the new version.
  - A superseded version is never served or fetched (L7).
- **I7 — The last copy is never lost by automation.**
  - No code path deletes a current-version replica on its own.
  - No code path removes a landed file before its replacement is complete on
    disk. Every replacement is a rename over the target.
  - A user action that would leave fewer than 2 other holders is warned with
    online and offline counts (L4).
- **I8 — Replication set.**
  - The set is published ∧ accepted, for members only.
  - Pending frames reach moderators only (unchanged).
  - Excluded frames stop serving and stop being fetched, but their files are
    not deleted.
- **I9 — The disk decides what is servable.**
  - A holding is served only while the file's `(size, mtime)` matches its
    verified record, checked on every serve request.
  - A mismatch is resolved by rehash: the same bytes re-record the stamp;
    different bytes quarantine.
  - Unmounted is not deleted; edited is quarantined; moved is re-adopted.
- **I10 — Content is separate from references.**
  - A blob is keyed by hash. A reference is `(project, frame, version, path)`.
  - A file is removed only by an explicit user action, and only when no
    reference needs it.
  - A version bump is compare-and-set on the expected version.
- **I11 — Membership is enforced at every provider.**
  - Any change to who may connect bumps the version of every affected project
    with kind `members`: member added or removed, device added, revoked or
    retired.
  - Every provider rebuilds its connect gate from the new snapshot and closes
    open collab connections of node ids it no longer admits.

## 4. The event channel (presence + push)

### 4.1 Transport

- **Channel.** `GET /api/v1/me/events` is a Server-Sent Events stream, one per
  signed-in device, carried over the existing HTTP stack on both ends.
- **Stream headers and keepalive.**
  - The hub sends `X-Accel-Buffering: no`.
  - It sends a keepalive comment every 20 s and a `retry: 3000` field.
- **Why SSE works behind our proxy.** The header and the keepalive make the
  stream work behind the current nginx vhost (HTTP/1.0 upstream, buffering on
  by default) without editing it. The vhost is write-once: the playbook
  deploys it with `force: false` because certbot rewrites it. The keepalive
  stays under nginx's default 60 s read timeout and under a 100 s proxy idle
  limit, should the DNS record ever become proxied.
- **Upstream traffic** is ordinary authenticated REST: holder deltas,
  announces, versions, and the presence beat below.
- **Why not WebSocket.** It was considered and rejected. It needs an Upgrade
  vhost change on the write-once file and a second client stack. Its one real
  advantage, dead-peer detection, is provided by the beat.
- **Long-poll fallback: deferred.** It is built only if a buffering proxy is
  ever observed in the field. The client logs, at `warn`, a stream that
  connects but never delivers a keepalive, so that case would be visible.
- **App-side client.** The app uses a dedicated streaming HTTP client: no total
  timeout, a 50 s read timeout (2.5 × keepalive), and reconnect with
  full-jitter exponential back-off (1 s base, 60 s cap). The hub client's
  whole-request 30 s timeout (`collab/hub_client.rs:325`) cannot carry a
  stream.

### 4.2 Presence

The hub's monotonic clock drives every timeout. No device timestamp orders
anything.

- **Facets.** A device's presence has three facets:
  - **connected** — the hub's judgement;
  - **serving** — self-reported, per project;
  - **reachable** — never asserted by the hub; a fetcher learns it by dialing.
- **Session.**
  - The stream opens with `hello`, which carries a 128-bit `sessionId`.
  - A second stream from the same device closes the older one.
  - A revoked or retired device's stream is closed by the hub.
- **Beat.** The device sends `POST /me/presence` every **15 s**, and
  immediately when its serving state or home relay changes.
  - Body: `{sessionId, serving: {projectId: bool}, relayUrl}`.
  - The route is mounted outside the token-auth group and authenticated in
    memory by `sessionId → device`, so it makes no database query.
  - An unknown or expired session answers `409 session_gone`, and the device
    reopens the stream at once.
  - A beat during the grace period below keeps the session alive.
- **Offline rules.**
  - **Clean exit.** On app quit or sign-out the device sends `DELETE
    /me/presence` and is offline at once. On system sleep it does the same
    where the platform gives the shell a sleep hook (best effort per
    platform); otherwise the 40 s rule covers it.
  - **Stream closes.** A 10 s grace period hides a reconnect (Wi-Fi roaming,
    proxy recycle). Past the grace the device is offline.
  - **Beat silence of 40 s.** The device is offline (two lost beats plus
    slack). This covers a crash, a pulled cable, or sleep without a hook.
- **Flap damping.** After 3 offline→online transitions within 5 minutes, the
  hub delays that device's offline broadcast by 60 s. The scheduler still
  reacts to its own connection results at once.
- **Fan-out.**
  - Presence changes are coalesced per project every 1 s and sent only to
    members of that project.
  - A device's presence is visible only in the projects it is a member of.
  - A `connected` change carries the device's current relay URL, so a device
    that changed home relay stays dialable without a snapshot.
- **Hub restart.** Presence lives in memory, so everyone starts offline.
  - For a 30 s warm-up the hub sends no presence diffs.
  - At the end of the warm-up it sends every open stream a full `presence`
    event per project with replace semantics, and from then on `hello` carries
    the full presence snapshot.
  - Presence only adds candidates (I5), so an empty presence after a hub
    restart cancels no transfer in flight.
- **`last_seen_at`** remains the throttled "last activity" stamp for profiles
  and the operator view. It is no longer a liveness signal.

**Serving** is false while any of these holds:

- the Collaboration storage is unavailable (§9.1);
- collab serving is off;
- the collab store is unmounted;
- the device is paused by the user.

A device that is connected but not serving is shown as "online · storage
unavailable". It is not a candidate provider.

### 4.3 Event schema

Each event is an SSE `event:` name with a JSON `data:` payload.

| Event | Data | When |
| ---- | ---- | ---- |
| `hello` | `{sessionId, epoch, projects: {id: {version, holderSeq, presence: [{device, serving, relayUrl}]}}}` | on connect |
| `project` | `{projectId, prev, version, kinds: [frames, meta, members, thresholds, dictionary, grid], frames?: [FrameEvent], more?: bool}` | every bump, coalesced ≤ 250 ms |
| `holders` | `{projectId, prev, seq, deltas: [{device, add: [[frameSeq, contentVersion]], rm: [frameSeq]}]}` | holder changes, coalesced 1 s |
| `presence` | `{projectId, replace?: bool, changes: [{device, connected, serving, relayUrl}]}` | coalesced 1 s; `replace` at warm-up end |
| `account` | `{kind: joined \| left, projectId}` | the caller's own membership changes |
| `resync` | `{projectId, what: project \| holders}` | a lagged receiver on the hub |
| `versions` | `{projectId: [version, holderSeq]}` | every 60 s (self-heal) |

**Frame rows.** A `FrameEvent` inlines at most 50 changed frame rows. A larger
change sets `more: true`, and the client pulls the manifest delta. Manifest
paging is cursored by the `version` read before page 1, and pages are ordered
by `manifest_version`.

**Contiguous delivery.** Each coalescer emits only contiguous cursor ranges. A
gap (two commits published out of order by their spawned tasks) is buffered for
up to 2 s before the hub emits `resync` instead.

**Self-heal.** The `versions` state vector repairs any lost wake-up: a
cancelled hub handler, a dropped event, or a lagged receiver.

**Why not `Last-Event-ID`.** A single header cannot carry about 20 per-project
cursors. `hello`, together with the client's stored cursors and the state
vector, does the same job without per-device queues on the hub.

### 4.4 Epoch

- **Where it lives.** The epoch is a row in the existing key/value `hub_meta`
  table (`migrations/0021_hub_meta.sql`).
- **High-water marks.** Every 60 s the hub writes to its state directory (§5.4)
  a file holding the epoch and every project's `(version, holderSeq)` high-water
  mark.
- **Startup check.** The hub rotates the epoch when any of these holds:
  - the file's epoch differs from the database's;
  - any project's database head is below its recorded mark (the database was
    restored from a backup, or moved to a host with an old dump).

  This catches a restore even after new writes have passed a client's old
  cursor.
- **Explicit rotation.** The restore runbook line and `migrate --reset` rotate
  it too. The hub has no database backups today; adding them is a backlog item
  outside this wave.
- **Client side.** A client whose stored cursor is ahead of the hub's head in
  `hello` treats that as an epoch change as well. On an epoch change the
  client:
  - reloads every snapshot;
  - reconciles its holdings (§6.3);
  - re-announces its own frames the hub no longer lists, through the normal
    announce path under their existing uuids. Moderation state lost with the
    restore comes back as `pending` per the normal gate.

### 4.5 Hub implementation shape

- **`FeedHub` in `AppState`:**
  - a lazily created `broadcast::Sender` per project, capacity 512;
  - an `mpsc` per device for `account` events and stream closure;
  - the in-memory presence and session registry;
  - the 250 ms and 1 s coalescers.
- **Publish after commit.** Every write handler publishes after `COMMIT`, next
  to the existing `state.versions.invalidate`. Commit and publish run inside a
  spawned task, so a dropped request future cannot commit without publishing.
  The 60 s state vector covers any residual gap.
- **Lag.** A lagged receiver (`RecvError::Lagged`) never replays; it emits
  `resync`.
- **Connections and logs.** Streams hold no database connection. They
  authenticate, read the `hello` state and release the connection. The stream
  route gets a `debug`-level span through the trace layer's `make_span_with`,
  so hour-long spans do not flood the log.
- **No LISTEN/NOTIFY.** On a single hub instance the in-process broadcast is
  the relay. A second instance would add LISTEN/NOTIFY as a wake-up carrying
  `(project_id, version)` only, and move presence to a shared store. That is
  noted, not built.

### 4.6 Failure handling on the client

- **Per-request retry.** Every hub request retries with full-jitter exponential
  back-off (1 s → 60 s cap) on transport errors, 5xx and 429. The hub client
  has no retry today (`collab/hub_client.rs`); this is new.
- **Auth and permission errors.** 401 goes to the existing re-authentication
  path. 403 marks that project or frame refused and logs it at `error`.
- **No pass-level abort.** A failure delays only the request that failed. The
  wave-2 behaviour, where a lookup error ended the pass (R25) and a failing
  project backed off 20 minutes (R13), is removed.
- **Hub down.** The stream reconnects with back-off. Meanwhile the scheduler
  keeps fetching from providers it already knows, using the cached holder map
  and presence, and reports holdings to a local outbox (§6.2). On reconnect,
  `hello`, the cursors and reconciliation bring everything current.
- **Sync now (L10)** drops the stream, reconnects at once, clears every
  back-off (requests, providers, projects), and runs reconciliation.

## 5. Hub model changes

### 5.1 Tables and lock order

- **`hub_meta`** — the existing key/value table gains the `epoch` key.
- **`projects.next_frame_seq int`** — the per-project frame ordinal counter.
  Announce already holds the project row lock.
- **`project_frames.frame_seq int`** — a dense per-project ordinal, assigned at
  announce and never reused. It indexes the holder bitsets. Existing rows from
  migration 0022 get ordinals in `(announced_at, frame_uuid)` order when the
  migration runs.
- **`frame_holders`** becomes device-owned claims:
  - Columns: `(project_id, frame_uuid, device_id, content_version,
    report_seq bigint, changed_seq bigint, removed bool)`.
  - `reported_at` and the freshness window are dropped. Existing rows migrate
    as claims with `report_seq = 0`.
  - An upsert applies only when the incoming `report_seq` is greater than the
    stored one (I4).
  - Removal sets `removed = true` with a new `changed_seq`. Tombstones are
    pruned after 7 days; the prune raises the project's holder floor.
- **`project_holder_cursor(project_id, seq bigint, floor bigint)`** — the
  gapless holder counter.
- **`device_project_digest(project_id, device_id, count int, digest bytea)`**
  — updated in the same transaction as every claim change (§6.3).

**Lock order.** `projects` → `project_holder_cursor(project_id)` →
`project_members` → frame rows. This extends the hub's existing lock-order doc
comment (`routes/frames.rs:4-29`).

- A holder report locks only the cursor row, never `projects`. A 1 s flush from
  a hundred devices then never contends with an announce for the project row.
- Announce, version, revocation and retirement write claims under both locks,
  in that order.

### 5.2 Changes to existing behaviour

- **New version: no holder deletes.** `POST …/frames/{uuid}/version` stops
  deleting holder rows (`routes/frames.rs:631-637`). Older claims simply stop
  validating, because they carry their own `content_version`.
- **New version: compare-and-set.** The body carries `expectedVersion`. On a
  mismatch the hub answers 409 with the current version (I10).
- **Reject: no holder deletes.** Reject stops deleting holder rows
  (`routes/frames.rs:936`). Pending frames have no replica holders by
  construction of the need set.
- **Duplicate announce.** Announce keeps the wave-2 rule: a duplicate uuid is
  "already announced" and the app adopts the hub's row (wave-2 R8,
  `routes/frames.rs:275-290`). That already covers two devices of one account
  (C8) and a restore that lost the row (C17).
- **Revocation and retirement (I11).** `POST /devices/{id}/revoke` gains a
  `retire` flag (L9) instead of a second device state. Revoke and retire
  both do this in one transaction:
  - tombstones all of the device's claims;
  - bumps `projects.version` with kind `members` for every project of the
    account;
  - bumps the holder cursor.

  After commit the hub closes the device's stream. Adding a device bumps
  `members` the same way.
- **Manifest holder count.** `holder_count` leaves the manifest
  (`routes/frames.rs:461-475`). Clients derive redundancy from the holder map.
  The portal's coverage aggregate keeps a server-side count computed from valid
  claims.

### 5.3 Endpoints

| Route | Change |
| ---- | ---- |
| `GET /me/events` | new — SSE stream (§4) |
| `POST /me/presence`, `DELETE /me/presence` | new — beat and clean exit, session-authenticated (§4.2) |
| `GET /projects/{id}/holders/snapshot` | new — one REPEATABLE READ read of `{epoch, holderSeq, version, frames: [{seq, uuid, contentVersion}], devices: [{device, displayName, relayUrl, claims}]}`. `claims` is a run-length list of `(frameSeq, contentVersion)` runs. `version` is the manifest version the snapshot was read against |
| `GET /projects/{id}/holders?since=S` | new — paged holder deltas after `S`, each with its `contentVersion`; `410` below the floor means "reload the snapshot" |
| `PUT /projects/{id}/holders/self` | changed — body `{reportSeq, full, add: [{uuid, contentVersion}], remove: [uuid], digest, count}`; answers `{holderSeq, digestMatch, nextFlushMs}` |
| `POST /projects/{id}/frames/versions` | new — batch re-version (≤ 500), one transaction, per-frame compare-and-set results |
| `POST /devices/{id}/revoke` | changed — `retire` flag and the I11 effects |
| `GET /me/project-versions` | retired |
| `GET /projects/{id}/frames/{uuid}/holders` | retired |

No released app uses the retired routes. They exist only on the unreleased
wave-1/2 branches, which are merged locally and not deployed. The plan must
confirm this against the test hub before removing them. The v3 API version
bumps again, so an older wave-2 build gets `409 collab_api_outdated`.

On a `resync` for a project, the client refetches the small project documents
(meta, members snapshot, thresholds, dictionary, grid) together with the
manifest delta. That is why no change-kind table or `GET /changes` endpoint is
needed.

### 5.4 Infrastructure (astronet)

- **State directory.** `StateDirectory=athenaeum-hub` (and the test-hub
  variant) in the systemd unit. The unit runs `ProtectSystem=strict` with no
  writable path today. The hub reads its state directory from
  `$STATE_DIRECTORY`.
- **File descriptors.** `LimitNOFILE=65536` in the hub unit. Today the default
  soft limit of 1024 descriptors caps the hub near 1,000 streams.
- **nginx converge task.** A marker-guarded `blockinfile` converge task (the
  only safe way to change the write-once vhost) adds
  `location = /api/v1/me/events` with:
  - `proxy_buffering off`;
  - `proxy_read_timeout 1h`;
  - `proxy_http_version 1.1`;
  - `proxy_set_header Connection ""`.

  The hub header already makes SSE work before this lands; the converge task
  hardens it.
- **nginx workers.** Raise `worker_connections` to 4096 once the host's core
  count is checked.
- **Database pool.** Raise it from 5 to 20 (`src/db.rs:10`) for reconnect
  storms. Streams hold no connections.
- **Post-deploy probe.** It opens the stream with a probe token provisioned
  for the deploy, and asserts that the first byte arrives within 2 s.

## 6. Holdings

### 6.1 Holder map on the client

- **Claims and currency.** The client keeps `claimed[device][frameSeq] =
  contentVersion`: a u32 map, about 2 MB at 100 devices × 5,000 frames.
- **Providers.** A frame's providers are the devices whose claimed version
  equals the manifest's current version, that are connected and serving, and
  that are not me (I4, I5). Nothing is cleared on a version event;
  availability is derived. So the order in which `project` and `holders`
  events arrive does not matter.
- **Persistence.** The map and its cursor persist across reconnects and app
  restarts. The client loads `holders/snapshot` only when:
  - it has no local map;
  - its epoch changed;
  - the hub answers `410` below the floor.

  Otherwise it resumes with `GET /holders?since=` from its cursor, then applies
  `holders` events. After a hub restart, a hundred devices therefore make a
  hundred small delta reads, not two thousand snapshot builds.

### 6.2 Reporting

- **Journal.** The device keeps a local holdings journal: `report_seq`
  increments on every change.
- **Outbox.** Every landing, loss, quarantine, decline, re-adoption and version
  swap appends to an outbox in the same local transaction as the state change
  (collision C24).
- **Flush.** The outbox flushes once `nextFlushMs` has passed (default 1 s,
  set by the hub), or immediately when it reaches 500 entries, as one `PUT
  …/holders/self`.
- **Hub down.** The outbox holds the entries and flushes on reconnect.
- **Ordering.** Duplicated or reordered deliveries are harmless: the hub keeps
  the highest `report_seq` per `(device, frame)`.
- **Timing.** A landing is reported only after BLAKE3 verification and the
  atomic replacement (I1, I9).

### 6.3 Reconciliation

- **Digest.** Each `(device, project)` claim set has an order-independent
  digest: the XOR of the first 16 bytes of `blake3(frame_uuid ‖
  content_version)` over the device's own non-removed claims, whatever the
  current version is, plus a count.
  - It depends only on the device's report stream, so both sides maintain it
    from identical inputs.
  - A hub version bump never changes it.
  - Claims are unique per `(device, frame)`, so the XOR never cancels a
    duplicate.
- **Check.** After `hello`, and in every `PUT …/holders/self` answer, the hub
  returns whether its `(count, digest)` matches the device's.
- **Mismatch.** The device sends a full claim set, "as of `reportSeq` R", in
  one `PUT` with `full: true` (about 40–100 KB at 5,000 frames). The hub
  tombstones every claim of that device in the project not listed there.
  Deltas with a sequence above R follow as usual.
- **Hourly check.** An hourly digest comparison runs off the critical path.
  There is no periodic full report.
- **Range-based set reconciliation** is the documented upgrade path if a
  project ever exceeds about 100,000 frames. At our scale a full set on
  mismatch costs the same and needs less code.

## 7. Scheduler (receive side)

### 7.1 State

Per replicated project, the scheduler keeps:

- **the need set:** published ∧ accepted ∧ replica ∧ current version not
  landed ∧ not declined ∧ not quarantined ∧ policy matches ∧ storage
  available;
- **the provider sets**, from the holder map and presence (§6.1);
- **per-provider dial health:** last error, back-off, open connection.

**Structure.** The scheduler is one task per app process: a deterministic core
plus an effect executor.

- The core is a pure function over that state. Hub events, disk events, fetch
  results and timers go in; fetch, cancel, land and report commands come out.
- The executor performs the commands and re-checks their preconditions against
  the database. The landing fence (I1) stays a DB conditional.
- Because the core is pure, it can be tested with seeded randomized event
  sequences (§12).

### 7.2 Choosing work

- **Order.** Frames go rarest first (fewest available providers), then in
  random order, so processors that start together spread over different
  frames instead of queueing on the publisher's first ones. A frame waiting
  more than 1 h jumps the queue, so nothing starves.
- **Frames without providers.** A frame with no available provider sleeps and
  costs zero requests. A `presence` or `holders` event that gives it a provider
  wakes it at once.
- **Work units.** A **work unit** is one frame (at most 256 MiB). Receive
  admission is per unit (§8). Frames are fetched through the existing
  assignment engine (`sharing/iroh/assign.rs`: hedging, eviction and resume
  via `local_for_request`/`execute_get_sink`, as today).
- **Live providers.** The engine's provider list is frozen per item today
  (`assign.rs:440, 890`). It becomes a **live provider set**: a watch channel
  fed by holder and presence events, so a provider that appears is added.
- **Reading errors.** The engine learns to read the provider's refusal through
  `GetError::iroh_error_code()` instead of treating every failure as
  `Failed|Stalled` (`assign.rs:483-506`):
  - `ERR_PERMISSION` → this provider does not serve this hash now. Evict it for
    the hash, with no back-off strike against the provider.
  - `ERR_LIMIT` → the provider is at its stream limit. Try another provider,
    and retry this one after a short delay.

### 7.3 Provider health

- **Keep connections open.** The scheduler keeps one QUIC connection open per
  provider it is actively pulling from.
  - `Connection::closed()` or a failed dial evicts that provider at once.
  - This is the live health check between fetcher and source; it complements
    hub presence (reachable ≠ connected).
- **A dedicated collab pool.** Collab gets its own small connection pool. It
  dials with `connect_with_opts` and its own transport config, because the
  shared pool dials with plain `connect` and exposes only idle and connect
  timeouts (`iroh-util` `connection_pool.rs:44-66, 185-190`). The pool uses:
  - a 10 s connect timeout covering address lookup, relay and handshake;
  - the default 30 s connection idle timeout, with the 5 s keep-alive and
    `closed()` giving prompt detection;
  - connections kept open at least 60 s after the last request.

  The personal-sync pool keeps its own settings.
- **Failed dials.** A failed dial backs off per provider (1 s → 60 s) and never
  marks the holder gone at the hub.
- **Verification failure.** A `DecodeError` from verification evicts that
  provider for that hash, logged at `warn` with the device and frame.
  Reporting suspects to the hub is future work; the provider's own per-serve
  check (§9.3) is what stops the bad bytes at the source.

### 7.4 Reacting to changes mid-fetch

- **New version (I1, I6).** A `project` event that bumps a frame in flight
  cancels that frame's fetch. Partial bytes of the old hash are discarded, and
  the frame re-enters the need set at the new version.
- **Exclusion, lost project, membership change, a policy change that drops the
  frame, auto-replicate switched off.** Each one cancels the affected frames
  at once. This replaces the wave-2 between-batch re-check (R19).
- **Storage unavailable (§9.1).** All fetching for that store stops, and
  in-flight fetches are cancelled. No frame changes state.
- **Provider leaves.** Only a closed connection or failed dial removes a
  provider from an in-flight fetch. A presence event does not (I5).

### 7.5 Landing

Landing keeps the wave-2 path (export from the collab store by rename, the
landing fence) with one change (ADAPT).

- **Export to a temporary file.** `export_child` exports to
  `<target>.athtmp` in the target's directory and then renames over the target.
  The rename is an atomic replace on every platform we ship.
- **No delete first.** The old `remove_file(target)` pre-step
  (`sharing/iroh/blobs.rs:701`) is removed. That upholds I7 and L7: v1 stays
  on disk until v2 is complete.
- **Quarantined targets.** Nothing lands over a quarantined file, whatever the
  version (§9.4).

## 8. Priority and limits (L1, L11)

- **Receive gate with two classes.** `ReceiveGate` (today a plain tokio
  semaphore, `sync/receiver.rs:258-308`) becomes a two-class admission gate,
  still sized by `sync.max_concurrent_receives`.
  - A waiting personal transfer is always admitted before any collab unit.
  - While a personal transfer waits, the gate signals the collab permit
    holders. Each finishes the frame in flight, releases its permit and
    re-queues.
  - So a personal transfer waits at most one frame. A cancelled collab fetch
    resumes from its verified ranges, because iroh-blobs keeps partial blobs.
  - The personal-sync side is unchanged: one permit per package.
- **Upload side.**
  - The endpoint's single upload pacer (`sharing/iroh/mod.rs:586-604,
    826-845`) becomes class-aware. While a personal-sync upload is active,
    collab throttle replies are delayed to a fixed low share of the link,
    whether or not a byte-rate cap is set.
  - The per-device limit on simultaneous collab upload streams (L11) is
    enforced in the get-request intercept. Past the limit, a request is
    refused with `ERR_LIMIT`, and the fetcher moves to another provider
    (§7.2).
- **Receive side.** The per-fetch limit on simultaneous collab receive streams
  (L11) caps the assignment engine's in-flight children for collab. Today that
  cap is a fixed `MAX_IN_FLIGHT = 32`.
- **Settings.** Both limits are Settings → Transfers keys with defaults. The
  defaults are chosen in the plan from a measurement on the test relay.

## 9. Local storage state

### 9.1 Storage availability

- **Marker.** The Collaboration root carries a marker, `.athenaeum/store-id`,
  holding a random store id and the id of the device that designated it. Both
  are recorded in the app DB.
- **Checks.** The marker is checked before every write, delete, landing,
  classification of a missing file, and scan of the root, and whenever the
  file watcher reports the root itself.
- **States:**
  - **unavailable** — `path missing`, `not a directory`, `marker missing`, or
    `marker id mismatch` (another disk mounted at the same path). Serving is
    false for every project on that store (§4.2), the scheduler stops, no
    frame changes state, no holding is withdrawn, and the root is never
    recreated.
  - **read-only** — `not writable`. Fetching stops, and serving continues,
    because a read-only remount must not take a full replica off the swarm.
- **Coming back.** When the marker is back, serving resumes. A stat sweep
  (§9.2) confirms the files before the device reports anything new.
- **Missing folders inside the root.** A present marker with a missing
  project or publisher folder is not an unavailable store. It takes the
  deletion path (§9.4, L4).
- **Another device's root.** A root whose marker names another device of the
  same account, one that is not retired, is refused as this device's
  Collaboration root. The case is two machines mounting one NAS folder: two
  blob stores on one directory would corrupt each other.

### 9.2 Detecting changes

All of this is new code. The core's `monitor/` polls by design and has no
file watcher (`monitor/mod.rs:9-11`).

- **File watcher (fast path).** Events are aggregated for 10 s. A removal is
  concluded only after the 60 s settle window, so a move or rename becomes one
  event. Our own `.athtmp` files and the blob store directory are ignored.
- **Stat sweep (authority).** The sweep checks `size:mtime`, with a 2 s mtime
  tolerance for FAT and SMB, and rehashes (xxh3, then BLAKE3) only on a change.
  - It covers every landed path, including own frames that live outside the
    root (v3 amendment A1).
  - It runs hourly, jittered ±25 %, while the watcher is healthy.
  - It runs every 5 minutes while the watcher is unavailable or the store is
    on a network volume.
  - A canary file detects a dead watcher. The UI shows "changes are seen by
    periodic check only" on such stores.
- **Serve check (correctness gate, §9.3).** Every serve request re-checks
  `size:mtime`, so detection latency never lets a changed or missing file
  serve.

The wave-2 20-minute disk truth, maintenance loop and loss guard are removed.

### 9.3 The serve check (provider side)

The collab store's provider intercepts every get request
(`EventMask { get: InterceptLog }`, iroh-blobs 0.103). A refusal reaches the
requester as a stream reset carrying `ERR_PERMISSION` or `ERR_LIMIT`
(`iroh-blobs/src/provider/events.rs:81-138`).

A request is served only if all of these hold:

- the hash is the current version of a frame this device holds;
- the landed file's `(size, mtime)` matches the verified record;
- the storage is available;
- the upload stream limit (L11) is not exceeded.

**What a refusal does.**

- A size or mtime mismatch refuses the request and triggers an immediate local
  check of that frame. The check rehashes the file:
  - the same bytes re-record the stamp and serving resumes (a `touch`, a backup
    tool, a DST shift);
  - different bytes quarantine the frame (§9.4);
  - a missing file marks it missing.
- A refusal of a superseded hash (L7) is logged at `debug`.

This closes the gap measured in §1: an edited file can no longer serve a byte.

The membership part of admission stays in the connect gate (I11). A BLAKE3
hash is a capability across projects: a member of project A who learns a hash
of project B could request it from a device that serves both. This is accepted.
Hashes are only published inside a project's manifest, to that project's
members.

### 9.4 Per-frame local states (replica)

```text
Wanted ──fetch──▶ Fetching ──verified+replaced──▶ Held
Fetching ──storage unavailable / cancelled──▶ Wanted     (resumes from verified ranges)
Held ──file gone (settled)──▶ Missing
Missing ──file back at its path before the fetch──▶ Held (re-checked by stat + hash)
Missing ──L4 single──▶ Wanted                            (automatic)
Missing ──L4 mass, or deleted twice in 24 h──▶ AwaitingChoice  (one notification)
AwaitingChoice ──Re-fetch──▶ Wanted
AwaitingChoice ──Stop keeping──▶ NotKept                 (last-copy warning)
AwaitingChoice ──new version current──▶ AwaitingChoice   (the choice now applies to the new version)
Held ──stamp drift, same bytes──▶ Held                   (stamp re-recorded)
Held ──content changed──▶ Quarantined                    (stops serving at once; file untouched)
Quarantined ──Re-fetch original──▶ Wanted                (the edited file goes to the OS trash, else is deleted after confirmation)
Quarantined ──Delete──▶ NotKept                          (the edited file is removed after confirmation)
Quarantined ──new version current──▶ Quarantined         (listed as "a new version is waiting"; nothing lands until the user chooses)
NotKept ──Keep again──▶ Wanted                           (L6)
NotKept ──the user puts the file back (hash matches the current version)──▶ Held (re-adopted)
Held ──moved inside the root──▶ Held                     (re-adopted by hash, path updated, no transfer)
Held(vN) ──vN+1 current──▶ Wanted(vN+1)                  (the vN file stays until vN+1 replaces it, L7)
Wanted / Held / AwaitingChoice ──excluded / lost project / policy drop──▶ Idle  (file kept, not served, not fetched)
Idle ──re-included──▶ Held or Wanted                     (stat + hash decide)
```

**Reporting per state.**

- Every transition that changes servability appends to the outbox (§6.2):
  - `Held` → add;
  - `Missing`, `Quarantined`, `NotKept`, `AwaitingChoice`, `Idle` → remove.
- An `AwaitingChoice` frame is withdrawn from the hub like any unservable
  frame. The file is gone, so it is not a copy anyone can count on.
- The last-copy warning counts the other devices' claims from the holder map,
  online and offline.

**Unknown files.** A file in the root that no row references is listed under
"Other files" (v3 R18) and is never deleted by the app. A hash that matches a
wanted frame is re-adopted instead of fetched.

**Own frames** keep v3 R17:

- missing → "Published · not on disk", not re-fetched, "Ask to exclude";
- changed → quarantined from serving (the same bytes re-record the stamp),
  shown as "Published · file changed", with "Republish from source" (the
  publish flow regenerates, and bytes that differ become a new version).

### 9.5 Device reinstall and re-adoption (L9)

- **The prompt.** At sign-in, when the Collaboration root's marker names a
  device of the same account, the app offers "This device replaces <device
  name>" in either case:
  - that device has been offline for more than 7 days;
  - the user picks the device from the account's device list.
- **Confirming:**
  - revokes the old device with `retire` (§5.2), which tombstones its claims
    and bumps `members`;
  - rewrites the marker with this device's id;
  - hashes the files under the root against the manifest;
  - adopts every match as `Held`, with zero transfer;
  - reports the adopted set.
- **Without confirmation.** The old device is proposed for retirement after 30
  days offline. It is never auto-retired.

## 10. Collisions and how the design answers them

The catalogue from the research, condensed. Each row names the rule that
answers it.

| # | Scenario | Rule |
| ---- | ---- | ---- |
| C1 | A version bump while peers are mid-download of the old version | I1 fence + cancel on the `project` event (§7.4) |
| C2 | Fetch plan read before a version bump | The hash comes from the manifest, never from a provider; fence at landing |
| C3 | The publisher goes offline right after versioning | L7: v1 files stay; "v2 waiting for the publisher" |
| C4 | Peers holding only the old version | Claims carry their version; providers are derived (I4, §6.1) |
| C5 | A new version while moderation is on | L8: no re-moderation; exclusion is the tool |
| C6 | Reject after some peers fetched | Cannot happen: the need set holds published frames only (I8) |
| C7 | Exclusion of an accepted frame | Stops serving and fetching; file kept (I8) |
| C8 | Two devices of one account publish the same frame | Duplicate uuid → adopt the hub row (§5.2) |
| C9 | Two devices re-version the same frame | Compare-and-set with 409 (§5.2) |
| C10 | Identical bytes in two frames or projects | Blob ≠ reference; removal only by user action (I10) |
| C11 | A provider serving while its file is replaced | Temp file + rename (§7.5); the serve check refuses the old hash (§9.3) |
| C12 | Membership or device revoked mid-transfer | `members` bump everywhere; gates rebuilt; connections closed; the device's stream closed (I11) |
| C13 | A revoked device rejoins | Claims were tombstoned; it reconciles like any reconnect (§6.3) |
| C14 | Project settings change mid-replication | Same feed, same cursor; policy re-evaluated on the event |
| C15 | Commit-order gaps in cursors | Counters under the lock order (§5.1, I2) |
| C16 | Hub restored from backup | Epoch + high-water marks + cursor-ahead detection (§4.4) |
| C17 | Frames published after the backup are missing | Publishers re-announce under the same uuids (§4.4) |
| C18 | "Versioned" and "holder reported" events race | Claims carry their version; availability is derived, never cleared (§6.1) |
| C19 | Add and remove reports reorder | `report_seq` per device (I4) |
| C20 | Duplicate deliveries | `prev`-checked deltas; idempotent upserts (I3) |
| C21 | Reconnect after the retention floor | `410` → snapshot (§5.3) |
| C22 | Gap between the snapshot and the stream | Snapshot carries its cursors (I3) |
| C23 | Clock skew | Only hub counters and device `report_seq` order anything (I2) |
| C24 | Crash between landing and recording | Local state + outbox in one transaction; re-adoption by hash on start |
| C25 | Presence flapping | 10 s grace, flap damping (§4.2) |
| C26 | Connected but not reachable | Per-provider dial health, separate from presence (§7.3) |
| C27 | 100 devices reconnect after a hub restart | Jittered reconnect; delta resume; digest match costs nothing (§6.1, §6.3) |
| C28 | One processor lands 5,000 frames | Holder deltas coalesced 1 s per project (§4.3) |
| C29 | Holding counted while storage is offline | `serving` facet (§4.2, §9.1) |
| C30 | Storage unmounted | Marker (§9.1) |
| C31 | The user deletes one replica | Settle, then automatic re-fetch (L4) |
| C32 | Mass deletion, or a slow deletion over many windows | One reversible choice; rolling window; second-deletion rule (L4) |
| C33 | A file moved or renamed inside the root | Settle window + re-adoption by hash (§9.4) |
| C34 | A replica edited in place | The serve check + rehash + quarantine (L5, §9.3) |
| C35 | Device reinstall | L9 (§9.5) |
| C36 | "Stop keeping" by the last holder | Last-copy warning; immediate "lost everywhere" notification (L4, I7) |
| C37 | A network volume with no change notifications | 5-minute sweep; the serve check is the gate (§9.2) |
| C38 | An edited replica when the publisher releases a new version | Stays quarantined; nothing lands until the user chooses (L5, §9.4) |
| C39 | Two machines of one account mount the same folder | Refused while the other device is not retired (§9.1) |
| C40 | A device changes home relay | The relay URL travels in the beat and in `presence` (§4.2) |
| C41 | Hub restart during active transfers | Presence only adds; transfers continue on their open connections (I5, §4.2) |

## 11. App changes (reuse audit)

The standing rule for an exchange plan: reuse the existing code, keep no extra
disk copies. The plan must carry the full KEEP / ADAPT / REMOVE / NEW audit,
with file:line, and the disk-copy ledger. At design level:

| Area | Decision |
| ---- | ---- |
| Collab blob store under `<Collab>/.athenaeum/blobs`, the collab ALPN, the swappable mount slot | KEEP |
| Publish, the recipe hash, staged own updates, adoption on "already announced" (R8), identical-bytes rule, auto-publish worker | KEEP. Versions go through the batch endpoint with compare-and-set |
| Landing (`export_child`, `landing_target`, landing fence) | ADAPT: temp file + rename, no delete first (§7.5); no landing over a quarantined file |
| Assignment engine (`sharing/iroh/assign.rs`) | ADAPT: live provider set, reading the refusal codes, the collab in-flight limit |
| Collab connection pool | NEW: dials with `connect_with_opts` (§7.3) |
| `ReceiveGate` | ADAPT: two classes, yield-on-demand, one-frame collab units |
| Upload pacer (`sharing/iroh/mod.rs`) | ADAPT: class-aware |
| Collab provider event consumer (`CollabSlotBlobs`) | ADAPT: the get intercept (serve check, stream limit) |
| Connect gate (`SharedConnectGate`, `collab/authz.rs`) | ADAPT: rebuild on a `members` event and close collab connections of dropped node ids |
| Hub client | ADAPT: retry with back-off on every call (none today), the new endpoints. NEW: the streaming client for `/me/events` |
| File watcher, settle window, canary, stat sweep with mtime tolerance | NEW (`monitor/` polls and is not reused) |
| `poll_versions_once`, `tick_loop`, `run_collab_auto_sync_loop` and its three loops, `COLLAB_AUTO_SYNC_INTERVAL`, `POLL_BACKOFF` | REMOVE → the event channel + the scheduler task |
| `disk_truth` as a 20-minute walk, `run_maintenance`, the loss guard (`collab.loss_guard_*` settings, `resolve_collab_loss`, the paused state) | REMOVE → §9 |
| Per-frame `frame_holders` lookups, `report_holders` full chunks | REMOVE → holder snapshot, deltas, outbox |
| `locally_declined` | ADAPT → the `NotKept` state with Keep again |
| `collab_foreign_files` | KEEP (unknown files), plus a NEW quarantine list |
| Scanner reconcile branch for the Collaboration root | KEEP, fed by the same classification |

New dependencies (checked for version and licence in the plan): a
file-watcher crate (the workspace already uses one in `crates/perseus`) and an
OS-trash crate. The holder claims use a plain run-length list, no bitmap
library.

**Disk-copy ledger.** No new copies:

- A re-adopted file is renamed or kept in place.
- A quarantined file stays where it is until the user acts.
- A landing's `.athtmp` is the file that becomes the target, so it is not a
  second copy.
- "Re-fetch original" sends the edited file to the OS trash where available,
  so the user can still undo.
- The steady state stays 1×N on every device.

## 12. Testing

- **Deterministic scheduler core.**
  - Seeded randomized tests generate interleavings of hub events (including
    `project` and `holders` in both orders), presence, disk events, fetch
    results and crashes.
  - They assert I1–I11 after every step.
  - A failing seed is reproducible.
- **Hub.**
  - Unit tests for the cursor counters under concurrent writers (gapless,
    commit order, lock order), `report_seq` ordering, the digest update and a
    full report replacing claims, compare-and-set, tombstones and the floor,
    epoch rotation from high-water marks, and revocation effects (I11).
  - An SSE integration test covering `hello`, deltas with versions, contiguous
    delivery, `resync` on lag, the 60 s state vector, beat timeouts, the grace
    period, session replacement, the warm-up `replace`, and flap damping. It
    runs over a clock abstraction for the presence timers; database I/O stays
    real.
- **App e2e on three instances** (contributor, processor, coordinator) across
  the test relay. It extends the wave-2 three-instance test and keeps its disk
  ledger.
  - "Starts fetching" means the first get request is issued.
  - Each scenario asserts a latency bound:

  | Scenario | Bound |
  | ---- | ---- |
  | A publishes → B starts fetching | ≤ 2 s |
  | B lands a frame → C, already connected to B, can fetch it | ≤ 3 s (≤ 5 s on a cold relay dial) |
  | B quits cleanly → C stops dialing B | ≤ 2 s |
  | B is killed | C drops B ≤ 50 s after its last beat (flap damping not engaged) |
  | B restarts | B is a provider again ≤ 3 s after connect; zero re-reports when digests match |
  | v2 published mid-download of v1 | the v1 fetch is cancelled; v2 replaces v1 atomically; v1 never served afterwards |
  | v2 landing interrupted (kill during export) | the v1 file is intact |
  | a replica is edited in place | the next serve is refused; quarantined; a later v2 does not land over it |
  | `touch` on a replica | serving resumes after the rehash |
  | a single delete | re-fetched after the settle window |
  | 15 deletes at once | one choice; nothing blocked |
  | storage unmounted | serving false; zero state changes; remount → resumes |
  | hub restart | reconnect; in-flight transfers continue; no duplicate or lost holdings |
  | epoch rotation | full resync; own frames re-announced under the same uuids |
  | device revoked mid-transfer | every provider closes its connection to that device |
  | personal transfer during a collab fetch | admitted after at most one frame |

- **Load check on the test hub.** 100 simulated devices, 5,000 frames:
  - the idle hub makes zero holder writes;
  - a 500-frame republish is one transaction;
  - the reconnect storm after a restart finishes under 30 s with delta resume.

## 13. Out of scope

- Serving partial ranges of a frame while it is still downloading. This would
  be a high-value swarm speed-up with many processors, noted for a later cycle.
- Publisher "send each frame out once first" seeding on weak uplinks.
- A peer-to-peer "hub unreachable" mode (L12).
- The long-poll fallback (§4.1).
- Reporting suspect providers to the hub.
- A second hub instance.
- Hub database backups (a backlog item; §4.4 only makes a restore safe).

## 14. Wave and rollout

This is **wave 3** of collab v3. The v3 spec's former waves 3–6 become 4–7
(amendment A4).

- **Hub and portal:**
  - migration(s) for §5.1, including ordinals and claim conversion for the
    rows migration 0022 created;
  - the endpoints, `FeedHub`, revocation effects;
  - the portal keeps working on the coverage aggregate (holder counts from
    valid claims);
  - hub tests;
  - test-hub deploy with the astronet changes (§5.4).
- **App:** the scheduler, event client, outbox, receive gate, upload pacer,
  collab pool, provider intercept, connect-gate rebuild, landing change, local
  storage model, and the UI parts:
  - a live status line: "live", "reconnecting in N s", "hub unreachable";
  - Sync now;
  - the Changed files, Not kept and Other files lists;
  - the mass-deletion choice;
  - the last-copy warning and the "lost everywhere" notification;
  - the device-replace prompt;
  - Settings → Transfers stream limits.
- **Both backends** (Tauri and Axum) get every new command, per the house rule.

Each part gets its own plan under `docs/superpowers/plans/`, reviewed per task
and per branch, merged to local main, and pushed and deployed only on the
owner's word. Production hub deploys remain a separate explicit procedure.

## 15. Acceptance

The e2e scenario list in §12 runs green on the test hub and the test relay,
within the latency bounds, plus the v3 spec §15 acceptance.

Two edits to that acceptance:

- "B deletes 15 at once" now expects one non-blocking choice, not a pause.
- "auto-publish announces within 15 s" becomes "B starts fetching within 2 s
  of the announce".

The load check (§12) passes. A one-week soak on the test hub with at least
three real devices shows no holder drift: the hourly digest checks all match.

## 16. Review record

An independent architecture review (2026-09-25) verified the backbone against
the pinned crates, the app, the hub and the deploy repo, and found no
component needing redesign. Every finding it raised was folded into this
revision:

- **Four critical findings:**
  - version-less holder deltas (→ I4, §6.1);
  - delete-before-rename landing (→ I7, §7.5);
  - a new version landing over a quarantined edit (→ L5, §9.4);
  - revocation never reaching other members' gates (→ I11, §5.2).
- **Sixteen important findings:**
  - lock order (§5.1);
  - a currency-free digest and a restored `full` flag (§6.3);
  - an epoch that survives a restore (§4.4, §5.4);
  - a dedicated collab pool (§7.3);
  - one-frame units with yield-on-demand (§8);
  - a class-aware pacer (§8);
  - random order after rarity (§7.2);
  - presence as an add-only signal and the warm-up replace (§4.2, I5);
  - delta resume instead of snapshot storms (§6.1);
  - rehash on stamp drift (§9.3);
  - deletion edge cases (L4);
  - a shared-folder guard (§9.1);
  - reuse-audit corrections (§11);
  - dropping blake3-idempotent announce (§5.2);
  - relay URL freshness (§4.2);
  - a session-authenticated beat (§4.2).
- **Minor findings:** folded in where they changed the text. That covers
  contiguous delivery, snapshot transactions, best-effort sleep, session edge
  cases, the read-only store, diagram gaps, own frames outside the root, the
  removed settings, precise test bounds, a DB-conditional fence, `nextFlushMs`,
  hash capability, and cutting long-poll and the change-kind table.
