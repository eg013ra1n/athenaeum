# Collaboration v3, wave 3 — live exchange

Status: design, agreed with the owner in dialogue on 2026-09-25 after the wave-2
review. It replaces the timing and presence model of the wave-2 exchange (the
15 s version poll, the 20-minute replication pass, the 75-minute holder
freshness, per-frame holder lookups, the loss-guard pause) with a push-driven,
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
  semaphore. A 200-frame collab batch can hold a lane while a personal transfer
  waits.
- **Irreversible decline.** "Stop keeping" is irreversible: no code path clears
  `locally_declined`.
- **Edited files are served.** iroh-blobs 0.103 serves a reference-imported
  file that was edited on disk without any check
  (`iroh-blobs/src/store/fs/bao_file.rs:592`). The receiver's verification
  rejects the bytes, but only after the edited replica has started serving
  them.
- **Pool timeouts.** Our iroh connection pool runs with a 1 s connect timeout
  and a 5 s idle timeout (`sharing/iroh/assign.rs:480`,
  `sharing/iroh/node.rs:5799`). That is tight for cross-continent relay dials,
  and too short for a connection to serve as a health signal.

The target is swarm-like behaviour:

- a seeder is visible to everyone about a second after it connects;
- a vanished seeder drops out within a minute;
- a landed frame is servable to others within seconds;
- no hub request is made per frame;
- nothing on the critical path waits for a timer.

## 2. Owner rulings (2026-09-25)

Numbered L1–L13 so plans and reviews can cite them.

- **L1 — Personal transfers take priority over collab** on both the receive
  and the upload side. A user often finishes pulling files they are about to
  share.
- **L2 — Presence is live.** A seeder appears as soon as it connects. A
  seeder is dropped by a health check measured in seconds, never by the age
  of a report. Holdings never expire with time.
- **L3 — Sync is event-driven in both directions.**
  - No per-frame holder lookups.
  - No periodic pass on the critical path.
  - A hub error retries that request with back-off; it never ends the work
    until a timer.
- **L4 — Deletion of a replica.** Deletions are collected over a 60 s settle
  window.
  - **Single deletions** (≤ 10 frames per window) are re-fetched
    automatically.
  - **Mass deletions** (> 10 frames per window) raise one non-blocking,
    reversible choice: "Re-fetch" or "Stop keeping". Everything else keeps
    running while the choice is open.
  - **Last-copy warning.** "Stop keeping" warns when a frame has fewer than 2
    other holders of its current version, offline holders included.
- **L5 — Edited replicas are quarantined.**
  - The moment a change is seen, the file stops serving.
  - It is listed under "Changed files" with **Re-fetch original** and
    **Delete**.
  - It is never overwritten or deleted silently.
- **L6 — "Stop keeping" is reversible.** A "Not kept" list offers
  **Keep again** per frame and for all.
- **L7 — Superseded versions are not served.**
  - Once v2 is current, nobody serves or fetches v1.
  - A replica keeps its v1 file on disk until v2 has landed over it (replace on
    land, never delete first).
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

- v3 spec §2 **R17** (mass loss pauses replication) → L4.
- v3 spec §2 **R19** (versioned polling) → §4.
- v3 spec §4.2 routes `GET /me/project-versions` and
  `GET /projects/{id}/frames/{uuid}/holders` → retired (§5.5).
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
  - **R24** (edited replica renamed aside at re-land) → L5, §9.4.
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
    manifest. This is the landing fence, already present as the wave-2
    conditional landing (`WHERE content_version AND blake3`).
- **I2 — One ordering authority per project.**
  - The hub feed orders everything, with cursor `(epoch, version)` for frames
    and metadata and `(epoch, holder_seq)` for holdings.
  - Both counters are assigned inside the writing transaction under the
    project row lock, so they are gapless and in commit order.
  - Nothing is ordered by a device clock.
- **I3 — Snapshots carry their cursor; deltas are idempotent.**
  - A snapshot is read in one transaction together with the cursor it
    reflects.
  - A delta applies only when its `prev` equals the client's cursor.
    Otherwise the client catches up over REST.
  - A client on a wrong epoch, or behind the retention floor, reloads the
    snapshot.
- **I4 — Holdings are durable facts keyed by version.**
  - A holding `(device, frame, content_version)` is written only by that
    device. The publisher's first holding is the exception: the hub writes it
    in the same transaction as the announce or the version.
  - Holdings are ordered by the device's own `report_seq` and never expire.
  - A holding ends only by the device's report, a device retirement (L9), a
    revocation, or the member leaving.
- **I5 — Presence is ephemeral and comes from the hub.**
  - A frame is available from a device iff the device holds the current
    version, is **connected**, is **serving** the project, and is a member.
  - Presence is never written into holdings.
- **I6 — Versions advance by replace-on-land.**
  - `current` is the latest version (L8).
  - A replica moves forward only by landing the new version.
  - A superseded version is never served or fetched (L7).
- **I7 — The last copy is never lost by automation.**
  - No code path deletes a current-version replica on its own.
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
  - Unmounted is not deleted; edited is quarantined; moved is re-adopted.
- **I10 — Content is separate from references.**
  - A blob is keyed by hash. A reference is `(project, frame, version, path)`.
  - A file is removed only by an explicit user action, and only when no
    reference needs it.
  - Announce is idempotent on `(project, blake3)`.
  - A version bump is compare-and-set on the expected version.

## 4. The event channel (presence + push)

### 4.1 Transport

- **Channel.** `GET /api/v1/me/events` is a Server-Sent Events stream, one
  per signed-in device, carried over the existing HTTP stack on both ends.
- **Stream headers and keepalive.**
  - The hub sends `X-Accel-Buffering: no`.
  - It sends a keepalive comment every 20 s and a `retry: 3000` field.
- **Why SSE works behind our proxy.** The header and the keepalive make the
  stream work behind the current nginx vhost without editing it. The vhost is
  write-once: the playbook deploys it with `force: false` because certbot
  rewrites it. The keepalive stays under nginx's default 60 s read timeout and
  under a 100 s proxy idle limit, should the DNS record ever become proxied.
- **Upstream traffic** is ordinary authenticated REST: holder deltas,
  announces, versions, and the presence beat below.
- **Why not WebSocket.** It was considered and rejected. It needs an Upgrade
  vhost change on the write-once file and a second client stack. Its one real
  advantage, dead-peer detection, is provided by the beat.
- **Long-poll fallback.** `GET /me/events?poll=1&since=…` covers a network
  that buffers streams. The client switches to it when a stream shows no
  keepalive for 50 s twice in a row, and tries the stream again every 10
  minutes.
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
- **Session.** The stream opens with `hello`, which carries a 128-bit
  `sessionId`.
- **Beat.** The device sends `POST /me/presence` every **15 s**, and
  immediately when its serving state changes.
  - Body: `{sessionId, serving: {projectId: bool}}`.
  - It is authenticated in memory against the session. No database hit.
- **Offline rules.**
  - **Clean exit.** On app quit, sleep or sign-out the device sends `DELETE
    /me/presence` and is offline at once.
  - **Stream closes.** A 10 s grace period hides a reconnect (Wi-Fi roaming,
    proxy recycle). Past the grace the device is offline.
  - **Beat silence of 40 s.** The device is offline (two lost beats plus
    slack). This covers a crash, a pulled cable, or sleep without a clean
    exit.
- **Flap damping.** After 3 offline→online transitions within 5 minutes, the
  hub delays that device's offline broadcast by 60 s. The scheduler still
  reacts to its own dial results at once.
- **Fan-out.**
  - Presence changes are coalesced per project every 1 s and sent only to
    members of that project.
  - A device's presence is visible only in the projects it is a member of.
- **Hub restart.** Presence lives in memory, so everyone starts offline.
  Presence diffs are suppressed for a 30 s warm-up. After that, `hello`
  carries a full presence snapshot per project.
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
| `hello` | `{sessionId, epoch, projects: {id: {version, holderSeq, presence: [{device, serving}]}}}` | on connect |
| `project` | `{projectId, prev, version, kinds: [frames, meta, members, thresholds, dictionary, grid], frames?: [FrameEvent], more?: bool}` | every bump, coalesced ≤ 250 ms |
| `holders` | `{projectId, prev, seq, deltas: [{device, add: [frameSeq], rm: [frameSeq]}]}` | holder changes, coalesced 1 s |
| `presence` | `{projectId, changes: [{device, connected, serving}]}` | coalesced 1 s |
| `account` | `{kind: joined \| left \| revoked \| deviceRetired, projectId?, deviceId?}` | membership and devices |
| `resync` | `{projectId, what: project \| holders}` | a lagged receiver on the hub |
| `versions` | `{projectId: [version, holderSeq]}` | every 60 s (self-heal) |

A `FrameEvent` inlines at most 50 changed frame rows. A larger change sets
`more: true` and the client pulls the manifest delta.

The `versions` state vector repairs any lost wake-up: a cancelled hub handler,
a dropped event, or a lagged receiver.

SSE `Last-Event-ID` is not used. A single header cannot carry about 20
per-project cursors, and `hello` together with the state vector does the same
job without per-device queues on the hub.

### 4.4 Epoch

The hub keeps a random `epoch` in a one-row `hub_meta` table, and a copy in
its data directory outside the database (`<hub data dir>/epoch`).

- **Startup check.** At startup the hub compares the two copies. A mismatch
  rotates the epoch and rewrites both: the database was restored from a
  backup, or it moved to a new host with an old dump.
- **Restore and reset.** The restore procedure and `migrate --reset` also
  rotate it explicitly.
- **Automatic detection.** A client whose cursor is ahead of the hub's head
  (`version` or `holderSeq` greater than what `hello` reports) treats that as
  an epoch change even if the rotation was forgotten.
- **On an epoch change** the client:
  - reloads every snapshot;
  - reconciles its holdings (§6.3);
  - re-offers its own frames that the hub no longer lists. The re-offer is
    idempotent on blake3 (I10). Moderation state lost with the restore comes
    back as `pending`, per the normal gate.

### 4.5 Hub implementation shape

- **`FeedHub` in `AppState`:**
  - a lazily created `broadcast::Sender` per project, capacity 512;
  - an `mpsc` per device for `account` events;
  - the in-memory presence registry;
  - the 250 ms and 1 s coalescers.
- **Publish after commit.** Every write handler publishes after `COMMIT`, next
  to the existing `state.versions.invalidate`. Commit and publish run inside a
  spawned task, so a dropped request future cannot commit without publishing.
  The 60 s state vector covers any residual gap.
- **Lag.** A lagged receiver (`RecvError::Lagged`) never replays; it emits
  `resync`.
- **Connections and logs.** Streams hold no database connection. They
  authenticate, read the `hello` state and release the connection. The stream
  route is instrumented at `debug` so hour-long spans do not flood the log.
- **No LISTEN/NOTIFY.** On a single hub instance the in-process broadcast is
  the relay. A second instance would add LISTEN/NOTIFY as a wake-up carrying
  `(project_id, version)` only, and move presence to a shared store. That is
  noted, not built.

### 4.6 Failure handling on the client

- **Per-request retry.** Every hub request retries with full-jitter exponential
  back-off (1 s → 60 s cap) on transport errors, 5xx and 429.
- **Auth and permission errors.** 401 goes to the existing re-authentication
  path. 403 marks that project or frame refused and logs it at `error`.
- **No pass-level abort.** A failure delays only the request that failed. The
  wave-2 behaviour, where a lookup error ended the pass (R25) and a failing
  project backed off 20 minutes (R13), is removed.
- **Hub down.** The stream reconnects with back-off. Meanwhile the scheduler
  keeps fetching from providers it already knows, using the cached holder map
  and presence, and reports holdings to a local outbox (§6.2). On reconnect,
  the `hello` and reconciliation bring everything current.
- **Sync now (L10)** drops the stream, reconnects at once, clears every
  back-off (requests, providers, projects), and runs reconciliation.

## 5. Hub model changes

### 5.1 Tables

- **`hub_meta(epoch uuid)`** — one row.
- **`project_frames.frame_seq int`** — a dense per-project ordinal, assigned at
  announce from a per-project counter and never reused. It indexes the holder
  bitmaps.
- **`frame_holders`** becomes device-owned claims:
  - Columns: `(project_id, frame_uuid, device_id, content_version,
    report_seq bigint, changed_seq bigint, removed bool)`.
  - `reported_at` and the freshness window are dropped.
  - An upsert applies only when the incoming `report_seq` is greater than the
    stored one (I4).
  - Removal sets `removed = true` with a new `changed_seq`. Tombstones are
    pruned after 7 days; the prune raises the project's holder floor.
- **`project_holder_cursor(project_id, seq bigint, floor bigint)`** — the
  gapless holder counter, bumped under the project row lock inside the report
  transaction.
- **`device_project_digest(project_id, device_id, count int, digest bytea)`**
  — updated in the same transaction as every claim change (§6.3).
- **`project_changes(project_id, version, kind)`** — one row per bump naming
  what moved, so a client refetches only the affected document.

### 5.2 Changes to existing behaviour

- **New version: no holder deletes.** `POST …/frames/{uuid}/version` stops
  deleting holder rows (`routes/frames.rs:631-637`). Older claims simply stop
  validating, because the read side joins `content_version = f.content_version`.
- **New version: compare-and-set.** The body carries `expectedVersion`. On a
  mismatch the hub answers 409 with the current version (I10).
- **Reject: no holder deletes.** Reject stops deleting holder rows
  (`routes/frames.rs:936`). Pending frames have no replica holders by
  construction.
- **Announce is idempotent on `(project, blake3)`.** A frame whose blake3 the
  project already lists returns the existing frame, and the caller becomes a
  holder of it. This covers two devices of one account publishing the same
  file (collision C8).
- **Retirement and revocation.** A device retirement (L9) or a revocation
  tombstones all of that device's claims in one transaction and emits
  `account` and `holders` events.
- **Manifest holder count.** `holder_count` leaves the manifest
  (`routes/frames.rs:461-475`). Clients derive redundancy from the holder map.
  The portal's coverage aggregate keeps a server-side count computed from valid
  claims.

### 5.3 Endpoints

| Route | Change |
| ---- | ---- |
| `GET /me/events` | new — SSE stream (§4) |
| `POST /me/presence`, `DELETE /me/presence` | new — beat and clean exit |
| `GET /projects/{id}/holders/snapshot` | new — `{epoch, holderSeq, frameSeqs: [{seq, uuid, contentVersion}], devices: [{device, displayName, relayUrl, bitmap}]}`; bitmaps are roaring-encoded over `frame_seq` for the current version |
| `GET /projects/{id}/holders?since=S` | new — paged holder deltas after `S`; `410` below the floor means "reload the snapshot" |
| `PUT /projects/{id}/holders/self` | changed — body `{reportSeq, add: [{uuid, contentVersion}], remove: [uuid], digest, count}`; answers `{holderSeq, digestMatch}` |
| `POST /projects/{id}/frames/versions` | new — batch re-version (≤ 500), one transaction, per-frame compare-and-set results |
| `GET /projects/{id}/changes?since=V` | new — the change kinds after `V` |
| `POST /devices/{id}/retire` | new — L9, the caller's own account's devices only |
| `GET /me/project-versions` | retired |
| `GET /projects/{id}/frames/{uuid}/holders` | retired |

No released app uses the retired routes. They exist only on the unreleased
wave-1/2 branches, which are merged locally and not deployed. The plan must
confirm this against the test hub before removing them. The v3 API version
bumps again, so an older wave-2 build gets `409 collab_api_outdated`.

### 5.4 Infrastructure (astronet)

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
- **Post-deploy probe.** It asserts the first SSE byte arrives within 2 s.

## 6. Holdings

### 6.1 Holder map on the client

- **Loading.** On connect and after any `resync`, the client loads
  `holders/snapshot` for each project it replicates or publishes into, then
  applies `holders` events.
- **Size.** 5,000 frames × 100 devices is about 62 KB of bitmaps
  uncompressed. A full-replica processor compresses to a few bytes as runs.
- **Version bumps.** A `project` event carrying a version bump clears that
  frame's column: no device holds the new version except the publisher,
  whose claim arrives in the same feed.
- **Providers.** A frame's providers are the devices whose bit is set, that
  are connected and serving, and that are not me (I5).

### 6.2 Reporting

- **Journal.** The device keeps a local holdings journal: `report_seq`
  increments on every change.
- **Outbox.** Every landing, loss, quarantine, decline, re-adoption and version
  swap appends to an outbox in the same local transaction as the state change
  (collision C24).
- **Flush.** The outbox flushes every 1 s, or immediately when it reaches 500
  entries, as one `PUT …/holders/self`.
- **Hub down.** The outbox holds the entries and flushes on reconnect.
- **Ordering.** Duplicated or reordered deliveries are harmless: the hub keeps
  the highest `report_seq` per `(device, frame)`.
- **Timing.** A landing is reported only after BLAKE3 verification and the
  atomic rename (I1, I9).

### 6.3 Reconciliation

- **Digest.** Each `(device, project)` claim set has an order-independent
  digest: the XOR of the first 16 bytes of `blake3(frame_uuid ‖
  content_version)` over its current-version, non-removed claims, plus a count.
  Both sides update it incrementally.
- **Check.** After `hello`, and in every `PUT …/holders/self` answer, the hub
  returns its `(count, digest)`.
- **Mismatch.** The device sends its full claim set in one `PUT` (about 40–100
  KB at 5,000 frames), and the hub replaces that device's claims for the
  project.
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

The scheduler is one task per app process: a deterministic event loop. Hub
events, disk events, fetch results and timers go in; fetch, cancel, land and
report commands come out. The core is a pure function over that state, so it
can be tested with seeded randomized event sequences (§12).

### 7.2 Choosing work

- **Order.** Frames go rarest first (fewest available providers), then oldest,
  with a random tie-break.
- **Frames without providers.** A frame with no available provider sleeps and
  costs zero requests. A `presence` or `holders` event that gives it a provider
  wakes it at once.
- **Work units.** A **work unit** is at most 8 frames or 1 GiB, whichever
  comes first. It is the unit of receive admission (§8). Inside a unit, frames
  are fetched through the existing assignment engine (`sharing/iroh/assign.rs`,
  hedging and eviction as today).
- **Live providers.** The engine's input changes from a provider list frozen
  per batch to a **live provider set**: a watch channel fed by holder and
  presence events. A provider that goes offline mid-unit is dropped, and one
  that appears is added.

### 7.3 Provider health

- **Keep connections open.** The scheduler keeps one QUIC connection open per
  provider it is actively pulling from. `Connection::closed()` evicts that
  provider at once. This is the live health check between fetcher and source;
  it complements hub presence (reachable ≠ connected).
- **Collab dial options.** Collab dials use their own options:
  - a 10 s connect timeout covering address lookup, relay and handshake;
  - a 10 s `max_idle_timeout` while pulling;
  - pooled connections that stay open at least 60 s after the last request.

  The personal-sync pool keeps its own settings. The 1 s connect and 5 s idle
  defaults are replaced for collab.
- **Failed dials.** A failed dial backs off per provider (1 s → 60 s) and never
  marks the holder gone at the hub.
- **Verification failure.** A `DecodeError` from verification evicts that
  provider for that hash. The fetcher logs it at `warn` with the device and
  frame. Reporting suspects to the hub is future work; the provider's own
  per-serve check (§9.3) is what stops the bad bytes at the source.

### 7.4 Reacting to changes mid-fetch

- **New version (I1, I6).** A `project` event that bumps a frame in flight
  cancels that frame's fetch. Partial bytes of the old hash are discarded, and
  the frame re-enters the need set at the new version.
- **Exclusion, lost project, revoked membership, a policy change that drops the
  frame, auto-replicate switched off.** Each one cancels the affected frames
  at once. This replaces the wave-2 between-batch re-check (R19).
- **Storage unavailable (§9.1).** All fetching for that store stops. No
  frame changes state.
- **Landing.** Landing keeps the wave-2 mechanics:
  - export from the collab store by rename;
  - the landing fence;
  - a same-version re-land is never written over a quarantined file (§9.4).

## 8. Priority and limits (L1, L11)

- **Receive gate with two classes.** `ReceiveGate` becomes a two-class
  admission gate, still sized by `sync.max_concurrent_receives`.
  - A waiting personal transfer is always admitted before any collab work unit.
  - A collab unit holds its permit only for one work unit (§7.2), so a
    personal transfer waits at most one unit — seconds to tens of seconds at
    typical rates.
  - The personal-sync side is unchanged: one permit per package.
- **Work units are resumable.** iroh-blobs keeps partial blobs, and a unit
  interrupted by cancellation resumes from the verified ranges it has.
- **Upload side.**
  - The collab store's provider uses the throttle intercept (`EventMask {
    throttle: Intercept }`) to yield to personal-sync uploads when both are
    active and a byte-rate cap is set.
  - The per-device limit on simultaneous collab upload streams (L11) is
    enforced in the get-request intercept. Past the limit, a request is
    refused with the rate-limited error, and the fetcher moves to another
    provider.
- **Receive side.** The per-fetch limit on simultaneous collab receive streams
  (L11) caps the assignment engine's in-flight children for collab. Today that
  cap is a fixed `MAX_IN_FLIGHT = 32`.
- **Settings.** Both limits are Settings → Transfers keys with defaults. The
  defaults are chosen in the plan from a measurement on the test relay.

## 9. Local storage state

### 9.1 Storage availability

- **Marker.** The Collaboration root carries a marker, `.athenaeum/store-id`,
  holding a random id written when the root is designated and recorded in the
  app DB.
- **Checks.** The marker is checked before every write, delete, landing,
  classification of a missing file, and scan of the root, and whenever the
  file watcher reports the root itself.
- **Unavailable states:**
  - `path missing`;
  - `not a directory`;
  - `marker missing`;
  - `marker id mismatch` (another disk mounted at the same path);
  - `not writable`.
- **While unavailable:**
  - serving is false for every project on that store (§4.2);
  - the scheduler stops for that store;
  - no frame changes state and no holding is withdrawn;
  - the root is never recreated.
- **Coming back.** When the marker is back, serving resumes. A stat sweep
  (§9.2) confirms the files before the device reports anything new.
- **Missing folders inside the root.** A present marker with a missing
  project or publisher folder is not an unavailable store. It takes the
  deletion path (§9.4, L4).

### 9.2 Detecting changes

- **File watcher (fast path).** Events are aggregated for 10 s. A removal is
  concluded only after the 60 s settle window, so a move or rename becomes one
  event. Our own temp files and the blob store directory are ignored.
- **Stat sweep (authority).** The sweep checks `size:mtime` against the
  recorded values and rehashes (xxh3, then BLAKE3) only on a change. It runs:
  - hourly, jittered ±25 %, while the watcher is healthy;
  - every 5 minutes while the watcher is unavailable or the store is on a
    network volume.

  A canary file detects a dead watcher. The UI shows "changes are seen by
  periodic check only" on such stores.
- **Serve check (correctness gate, §9.3).** Every serve request re-checks
  `size:mtime`, so detection latency never lets a changed or missing file
  serve.
- **mtime tolerance.** 2 s, for FAT and SMB granularity.

The wave-2 20-minute disk truth, maintenance loop and loss guard are removed.

### 9.3 The serve check (provider side)

The collab store's provider intercepts every get request
(`EventMask { get: InterceptLog }`, iroh-blobs 0.103) and refuses it unless:

- the hash is the current version of a frame this device holds;
- the landed file's `(size, mtime)` matches the verified record;
- the storage is available;
- the upload stream limit (L11) is not exceeded.

**What a refusal does.**

- A refusal on a size or mtime mismatch triggers an immediate local check of
  that frame (§9.4), which quarantines or marks it missing and reports it.
- A refusal of a superseded hash (L7) is logged at `debug`.

This closes the gap measured in §1: an edited file can no longer serve a byte.

### 9.4 Per-frame local states (replica)

```text
Wanted ──fetch──▶ Fetching ──verified+renamed──▶ Held
Held ──file gone (settled)──▶ Missing
Missing ──single deletion (≤ 10 per window)──▶ Wanted            (L4, automatic)
Missing ──mass deletion (> 10 per window)────▶ AwaitingChoice    (one notification)
AwaitingChoice ──Re-fetch──▶ Wanted
AwaitingChoice ──Stop keeping──▶ NotKept                         (last-copy warning)
Held ──content changed──▶ Quarantined     (stops serving at once; file untouched)
Quarantined ──Re-fetch original──▶ Wanted (the edited file goes to the OS trash, else is deleted after confirmation)
Quarantined ──Delete──▶ NotKept           (the edited file is removed after confirmation)
NotKept ──Keep again──▶ Wanted                                   (L6)
Held ──moved inside the root──▶ Held      (re-adopted by hash, path updated, no transfer)
Held(vN) ──vN+1 current──▶ Wanted(vN+1)   (the vN file stays until vN+1 lands over it, L7)
any ──excluded / lost project / policy drop──▶ Idle   (file kept, not served, not fetched)
```

**Reporting per state.**

- Every transition that changes servability appends to the outbox (§6.2):
  - `Held` → add;
  - `Missing`, `Quarantined`, `NotKept`, `AwaitingChoice` → remove.
- An `AwaitingChoice` frame is withdrawn from the hub like any unservable
  frame. The file is gone, so it is not a copy anyone can count on. The
  last-copy warning shown with the choice counts the other devices' claims
  from the holder map, online and offline.

**Unknown files.** A file in the root that no row references is listed under
"Other files" (v3 R18) and is never deleted by the app. A hash that matches a
wanted frame is re-adopted instead of fetched.

**Own frames** keep v3 R17:

- missing → "Published · not on disk", not re-fetched, "Ask to exclude";
- changed → quarantined from serving, shown as "Published · file changed",
  with "Republish from source" (the publish flow regenerates, and bytes that
  differ become a new version).

### 9.5 Device reinstall and re-adoption (L9)

- **Sign-in on a machine with an existing Collaboration root.** When the
  marker's store id is known to the account, the app offers "This device
  replaces <device name>".
- **Confirming:**
  - retires the old device (`POST /devices/{id}/retire`) and tombstones its
    claims;
  - hashes the files under the root against the manifest;
  - adopts every match as `Held`, with zero transfer;
  - reports the adopted set.
- **Without confirmation**, the old device is proposed for retirement after 30
  days offline. It is never auto-retired.

## 10. Collisions and how the design answers them

The catalogue from the research, condensed. Each row names the rule that
answers it.

| # | Scenario | Rule |
| ---- | ---- | ---- |
| C1 | A version bump while peers are mid-download of the old version | I1 fence + cancel on the `project` event (§7.4) |
| C2 | Fetch plan read before a version bump | Hash comes from the manifest, never from a provider; fence at landing |
| C3 | The publisher goes offline right after versioning | L7: v1 files stay; "v2 waiting for the publisher" |
| C4 | Peers holding only the old version | Claims keyed by version; old claims are not providers (I4, I5) |
| C5 | A new version while moderation is on | L8: no re-moderation; exclusion is the tool |
| C6 | Reject after some peers fetched | Impossible: pending frames reach moderators only (I8) |
| C7 | Exclusion of an accepted frame | Stops serving and fetching, file kept (I8) |
| C8 | Two devices of one account publish the same file | Announce idempotent on blake3 (§5.2) |
| C9 | Two devices re-version the same frame | Compare-and-set with 409 (§5.2) |
| C10 | Identical bytes in two frames or projects | Blob ≠ reference; removal only by user action (I10) |
| C11 | A provider serving while its file is replaced | Temp file + rename; the serve check refuses the old hash (§9.3) |
| C12 | Membership revoked mid-transfer | `account` event: the connect gate refreshes the membership snapshot and closes that device's collab connections |
| C13 | A revoked device rejoins | Claims were tombstoned; it reconciles like any reconnect (§6.3) |
| C14 | Project settings change mid-replication | Same feed, same cursor; policy re-evaluated on the event |
| C15 | Commit-order gaps in cursors | Counters under the project row lock (I2) |
| C16 | Hub restored from backup | Epoch rotation + cursor-ahead detection (§4.4) |
| C17 | Frames published after the backup are missing | Publishers re-offer idempotently (§4.4) |
| C18 | "Versioned" and "holder reported" events race | Claims keyed by version; a version event clears the column (§6.1) |
| C19 | Add and remove reports reorder | `report_seq` per device (I4) |
| C20 | Duplicate deliveries | `prev`-checked deltas; idempotent upserts (I3) |
| C21 | Reconnect after the retention floor | `410` → snapshot (§5.3) |
| C22 | Gap between the snapshot and the stream | Snapshot carries its cursor (I3) |
| C23 | Clock skew | Only hub counters and device `report_seq` order anything (I2) |
| C24 | Crash between landing and recording | Local state + outbox in one transaction; re-adoption by hash on start |
| C25 | Presence flapping | 10 s grace, flap damping (§4.2) |
| C26 | Connected but not reachable | Per-provider dial health, separate from presence (§7.3) |
| C27 | 100 devices reconnect after a hub restart | Jittered reconnect; digest match costs nothing (§6.3) |
| C28 | One processor lands 5,000 frames | Holder deltas coalesced 1 s per project (§4.3) |
| C29 | Holding counted while storage is offline | `serving` facet (§4.2, §9.1) |
| C30 | Storage unmounted | Marker (§9.1) |
| C31 | The user deletes one replica | Settle, then automatic re-fetch (L4) |
| C32 | Mass deletion | One reversible choice (L4) |
| C33 | A file moved or renamed inside the root | Settle window + re-adoption by hash (§9.4) |
| C34 | A replica edited in place | The serve check + quarantine (L5, §9.3) |
| C35 | Device reinstall | L9 (§9.5) |
| C36 | "Stop keeping" by the last holder | Last-copy warning (L4, I7) |
| C37 | A network volume with no change notifications | 5-minute sweep; the serve check is the gate (§9.2) |

## 11. App changes (reuse audit)

The standing rule for an exchange plan: reuse the existing code, keep no extra
disk copies. The plan must carry the full KEEP / ADAPT / REMOVE audit, with
file:line, and the disk-copy ledger. At design level:

| Area | Decision |
| ---- | ---- |
| Collab blob store under `<Collab>/.athenaeum/blobs`, the collab ALPN, the swappable mount slot | KEEP |
| Publish, the recipe hash, staged own updates, adoption (R8), identical-bytes rule, auto-publish worker | KEEP. Announce becomes idempotent on blake3. Versions go through the batch endpoint with compare-and-set |
| Landing: export by rename, landing fence, `holds_frame_content` | KEEP |
| Assignment engine (`sharing/iroh/assign.rs`) | ADAPT: a live provider set, the collab in-flight limit, collab dial options |
| `ReceiveGate` | ADAPT: two classes, collab work units |
| Collab provider event consumer (`sharing/iroh/mod.rs` `CollabSlotBlobs`) | ADAPT: the get intercept (serve check, stream limit), the throttle intercept |
| Hub client | ADAPT: a streaming client for `/me/events`, retry with back-off on every call, the new endpoints |
| `poll_versions_once`, `tick_loop`, `run_collab_auto_sync_loop` and its three loops, `COLLAB_AUTO_SYNC_INTERVAL`, `POLL_BACKOFF` | REMOVE → the event channel + the scheduler task |
| `disk_truth` as a 20-minute walk, `run_maintenance`, the loss guard (`collab.loss_guard_*` settings, `resolve_collab_loss`, the paused state) | REMOVE → §9 (watcher, sweep, serve check, per-frame states) |
| `frame_holders` lookups per frame, `report_holders` full chunks | REMOVE → holder snapshot, deltas, outbox |
| `locally_declined` | ADAPT → the `NotKept` state with Keep again |
| `collab_foreign_files` | KEEP (unknown files) + a new quarantine list |
| Scanner reconcile branch for the Collaboration root | KEEP, fed by the same classification |

**Disk-copy ledger.** No new copies:

- A re-adopted file is renamed or kept in place.
- A quarantined file stays where it is until the user acts.
- "Re-fetch original" sends the edited file to the OS trash where available,
  so the user can still undo.
- The steady state stays 1×N on every device.

## 12. Testing

- **Deterministic scheduler core.**
  - It is a pure state machine: events in, commands out.
  - Seeded randomized tests generate interleavings of hub events, presence,
    disk events, fetch results and crashes, and assert I1–I10 after every step.
  - A failing seed is reproducible.
- **Hub.**
  - Unit tests for the cursor counters under concurrent writers (gapless, commit
    order), `report_seq` ordering, the digest update, compare-and-set,
    idempotent announce, tombstones and the floor, and epoch rotation.
  - An SSE integration test covering `hello`, deltas, `resync` on lag, the
    60 s state vector, beat timeouts, the grace period, and flap damping. It
    runs with a virtual clock.
- **App e2e on three instances** (contributor, processor, coordinator) across
  the test relay. It extends the wave-2 three-instance test and keeps its disk
  ledger. Each scenario asserts a latency bound:

  | Scenario | Bound |
  | ---- | ---- |
  | A publishes → B starts fetching | ≤ 2 s |
  | B lands a frame → C can fetch it from B | ≤ 3 s |
  | B quits cleanly → C stops dialing B | ≤ 2 s |
  | B is killed → C drops B | ≤ 50 s |
  | B restarts → B is a provider again | ≤ 3 s after connect, with zero re-reports when digests match |
  | v2 published mid-download of v1 | the v1 fetch is cancelled; v2 lands; no v1 served afterwards |
  | a replica is edited in place | the next serve is refused; quarantined within the watcher window |
  | a single delete | re-fetched after the settle window |
  | 15 deletes at once | one choice; nothing blocked |
  | storage unmounted | serving false; zero state changes; remount → resumes |
  | hub restart | reconnect; no duplicate or lost holdings |
  | epoch rotation | full resync; own frames re-offered idempotently |
  | revocation mid-transfer | the connection closes |
  | personal transfer during a collab fetch | admitted within one work unit |

- **Load check on the test hub.** 100 simulated devices, 5,000 frames: the idle
  hub makes zero holder writes; a 500-frame republish is one transaction; the
  reconnect storm after a restart finishes under 30 s.

## 13. Out of scope

- Serving partial ranges of a frame while it is still downloading. This would
  be a high-value swarm speed-up with many processors, noted for a later cycle.
- Publisher "send each frame out once first" seeding on weak uplinks.
- A peer-to-peer "hub unreachable" mode (L12).
- Reporting suspect providers to the hub.
- A second hub instance.

## 14. Wave and rollout

This is **wave 3** of collab v3. The v3 spec's former waves 3–6 become 4–7
(amendment A4).

- **Hub and portal:**
  - migration(s) for §5.1, the endpoints, `FeedHub`;
  - the portal keeps working on the coverage aggregate (holder counts from
    valid claims);
  - hub tests;
  - test-hub deploy with the astronet converge (§5.4).
- **App:** the scheduler, event client, outbox, receive gate, provider
  intercepts, local storage model, the UI parts:
  - a live status line: "live", "reconnecting in N s", "hub unreachable";
  - Sync now;
  - the Changed files, Not kept and Other files lists;
  - the mass-deletion choice;
  - the last-copy warning;
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
