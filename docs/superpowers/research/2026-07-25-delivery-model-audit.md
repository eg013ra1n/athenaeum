# Delivery-model audit — personal sync, Perseus, collab

**Date:** 2026-07-25 · **Trigger:** owner smoke 2026-07-24 (two machines, real cross-NAT) produced two defects in one cycle; the standing rule is to re-audit the model rather than keep patching.

**Question this audit answers:** when does a transfer make progress, when does it wait, and *what tells it to stop waiting* — across the three surfaces that share the machinery.

**Method:** code read of `sync/engine.rs`, `sync/receiver.rs`, `sync/store.rs`, `api/sync.rs`, `api/collab_exchange.rs`, `crates/perseus/src/run.rs`, plus the pinned `iroh-blobs 0.103` downloader API; correlated against both machines' logs from the 2026-07-24 smoke. Line references are to the tree at commit `4b370eb4`.

---

## 1. The model as built

### 1.1 Sender engine (`sync/engine.rs`) — shared by the app AND Perseus

One `SyncEngine` per **peer**; Perseus spawns the same engine per target (`perseus/src/run.rs:802`). Everything below therefore applies to both hosts.

- **Row states** (`OutboundState`): `Queued → Announced → Transferring → Delivered → Confirmed`, plus `Failed` / `Cancelled`. `Delivered` = "uploaded, awaiting the receiver's confirmation" and is NOT terminal.
- **Per-package slot** (`pending: HashMap<i64, Pending>`) carries `deadline`, `rung`, `next_action` (`AwaitAck` | `Retry`), `last_attempt_class`.
- **The ladder**: `BACKOFF_MULTIPLIERS = [1, 2, 10, 30, 60]` × `ack_timeout` (30 s) ⇒ **30 s → 1 min → 5 min → 15 min → 30 min**, last rung held as the cap.
- **What climbs a rung**: an ack timeout (`engine.rs:2365`) and any failed build/serve/announce attempt (`arm_retry`, `engine.rs:1558`). Both increment unconditionally.
- **What resets it**: `note_serve_activity` — a serve tick from the peer sets `rung = 0` and pushes the deadline out (`engine.rs:1834`). This is what keeps a live multi-GB pull from tripping the 30 s ack timeout.
- **Terminality**: delivery-forever. The ONLY `Failed` write is `fail_package` (`engine.rs:2485`), reserved for local-fatal causes; a network condition never terminalizes a package — it parks at the 30-minute rung.
- **What wakes the worker**: `next_deadline()` (min over pending deadlines, else `IDLE_SLEEP` 1 h) and `Command::Kick(id)` / `Command::KickAll`. `KickAll` collapses every deadline to now.

### 1.2 Receiver (`sync/receiver.rs`)

- One long-lived `sync_inbound` row per `(peer, batch_uuid)`; attempts rotate the wire id and bump `generation`.
- **Startup reconcile** (`reconcile_stale_inbound`, `receiver.rs:685`): a non-terminal row whose receipts are complete is repaired to its honest terminal (Done / Cancelled); otherwise it becomes `Failed "interrupted by restart"`.
- **Terminal paths emit `sync-finished`** — cancel epilogue, replay, revoke, ingest done, unsafe-id reject… and, since `4b370eb4`, the failed fetch (that hole was smoke defect A).
- The receiver **never initiates**. It answers announces, acks, and declines. It has no channel to say "I am back" or "re-announce, please".

### 1.3 Perseus (`crates/perseus/`)

Send-only: `StandaloneSyncStore` (no catalog, no inbound side), the same engine per target, its own web UI over the same rows. Two Perseus-specific couplings matter here:

- **Source deletion is gated on `Confirmed`** (`web.rs::obligation_verdict`). A transfer parked on the ladder therefore also blocks disk cleanup at the observatory — the stall is not just cosmetic there.
- **Fan-out**: one payload dir → one `sync_outbound` row per target, each with its own engine and its own independent ladder.

### 1.4 Collab / project distribution (`api/collab_exchange.rs`)

- The hub already models a swarm: `announcement.holders: Vec<HolderWire>` with `relay_url` + `last_seen_at`, and `report_have` adds us after a successful ingest.
- The download is **sequential failover, one holder at a time** (`collab_exchange.rs:1005-1070`): attach dial hint → 5 s `probe_holder` → `request_project` (push model: the holder serves, we fetch) → wait up to the local-complete timeout → on failure, next holder.
- There is **no parallelism and no multi-source**: every blob fetch passes exactly one provider (`blobs.rs:347, 438, 552, 573` — `Shuffled::new(vec![provider])`).

### 1.5 What the transport layer already offers (unused)

- `SharedIrohNode::probe_holder(peer, has_relay_hint, timeout)` — a cheap control-ALPN reachability probe with a classified failure (`ProbeClass`). Used by collab only.
- `iroh-blobs 0.103` `DownloadRequest::new(request, providers, SplitStrategy)`: **`SplitStrategy::Split` splits one request across several providers** (true multi-source), and `ContentDiscovery` is a pluggable trait (`find_providers(hash) -> Stream<EndpointId>`). Our convenience call path pins `SplitStrategy::None` + a one-element provider list (`downloader.rs:414`).
- `Endpoint::remote_info(peer)` / `Endpoint::metrics()` — per-peer active-path snapshot and relay-vs-direct byte counters (wired for logging in `b2ff649d`, not consumed by any decision).

---

## 2. Signal inventory — the heart of the matter

| Signal | Exists? | Consumed by |
| ---- | ---- | ---- |
| Our own relay reconnects | yes (`WakeHook`) | `kick_all()` on every engine — **the only automatic kick** |
| User presses Send now | yes | `Command::Kick(id)` |
| App restart with non-terminal rows | yes (`resurrect_pending_senders`) | rebuild engines + re-announce |
| Peer's address (re)published to the hub | **yes — the peer does this on every start** | **nobody** |
| Peer refused an unknown-device connection | yes (`maybe_refresh_on_refusal`) | debounced authorized-set refresh (not a kick) |
| Periodic peer list from the hub | yes, **hourly** | names / capabilities / allow-list only — addresses ignored |
| Receiver has an interrupted transfer from peer X | yes (reconcile marks it) | **nobody** — the receiver tells no one |
| Peer reachable right now | computable (`probe_holder`) | collab only, never the sender ladder |

**The sender has no notion of peer reachability.** It waits out a timer whose length is derived from how many times it has already failed — not from anything about the peer.

---

## 3. Findings

Ordered by user-visible impact. Each is evidence-backed; none is a proposal yet.

**F1 — A returning peer waits out a blind timer.** (root of smoke defect B)
Measured 2026-07-24: receiver back online 21:45:24; sender noticed the ack timeout 21:45:35 and armed the retry for 21:46:35; the owner pressed Send now at 21:46:10, 25 s early. Nothing was broken — but nothing was listening either. On a later rung the same situation costs up to 30 minutes. *Impact:* every restart of either side; on Perseus it also holds the source files hostage (§1.3).

**F2 — The ladder counts the peer's absence as our failure.** `arm_retry`/ack-timeout climb the rung unconditionally, and `ConnectClass` (`no_route` / `relay_unreachable` / `refused` / `timeout` / `not_started`) is computed and stored but only ever used for log text, the journal detail, and the T8 stale-address refresh decision (`engine.rs:2406`). So "the peer's laptop is shut" escalates identically to "our disk is full", and an overnight-offline peer is guaranteed to be at the 30-minute cap by morning — exactly the Perseus-at-the-observatory case.

**F3 — The ladder is per package, the condition is per peer.** Each `Pending` slot owns its rung and deadline. Ten packages queued to one dead peer wake and dial ten times per window, each climbing its own rung, each writing its own `last_error`. There is no per-peer "this peer is down" state, so there is neither backpressure nor a single coherent thing to tell the user.

**F4 — The receiver never nudges.** It knows, at startup, exactly which peers it has interrupted transfers with (`reconcile_stale_inbound` enumerates them), and it holds an authenticated control channel to those peers. That is the most direct possible signal for F1 and it is unused.

**F5 — Terminal-path emission is a convention, not an invariant.** Defect A existed because one of six terminal paths forgot `sync-finished`; the UI's durable-list refresh hangs off that event, and a Failed row is excluded from `inbound_active`, so a missed emission makes a row vanish. Fixed for that path (`4b370eb4`), but nothing prevents the seventh path from repeating it.

**F6 — Collab wastes the swarm it already models.** Holders are known, probed and ranked one at a time; the download uses one provider and `SplitStrategy::None`. A project file held by four participants downloads at one participant's uplink, and a holder that dies mid-transfer costs the whole attempt (the next holder restarts the fetch — resumable from the partial blobs, but serially). Meanwhile the library ships `SplitStrategy::Split` and a `ContentDiscovery` seam that a hub-backed holder source fits exactly.

**F7 — "Delivered" has no timeout of its own.** A package uploaded but never confirmed rides the same ack ladder; correct, but combined with F2 it means a receiver that crashes during ingest pushes the sender to the cap even though the bytes are already there.

---

**F8 — A retry attempt re-hashes the whole payload before it learns the peer is absent.** `attempt()` calls `serve()` on every try (`engine.rs:1193`), and `role_serve` has no short-circuit (`node.rs:1806`): it re-imports the package directory through `add_path` per file each time. The dedup handshake, by contrast, runs once and is cached — so the expensive half repeats and the cheap half does not. Measured indirectly on the smoke: ~3 s for a 1.98 GB package on SSD (BLAKE3 is fast), which would be minutes on network or spinning storage. This finding constrains every other one: it is why "just retry more often" is not free, and why any fix must separate a cheap reachability check from a committed attempt.

## 4. What the model needs (framing, not yet design)

1. **A reachability notion for the sender** — per peer, not per package: is this peer dialable right now, and what changed when it became so.
2. **A ladder that distinguishes classes** — "peer absent" should be cheap and flat (retry often, never escalate to 30 min); "we are broken" should escalate; "peer refuses us" should stop and surface.
3. **A nudge from whoever learns first** — receiver on restart, hub on address change, or the sender's own probe. These are alternatives, not layers: pick one primary and one fallback.
4. **One per-peer status the UI can state** — "waiting for MiniMac (offline since 21:45)" instead of N rows each with a private countdown.
5. **A provider SET instead of a provider** everywhere a fetch is issued — the personal-sync case has exactly one, the collab case has many, and that is the only difference.

---

## 5. Proposed decomposition

| # | Spec | Covers | Depends on |
| ---- | ---- | ---- | ---- |
| **D1** | Peer reachability + delivery ladder | F1, F2, F3, F7 — the signal, the class-aware ladder, per-peer state, the UI status it feeds. Lands on app + Perseus at once (one engine). | — |
| **D2** | Receive-side terminal invariant | F5 — make "every terminal path emits" enforceable (single settle helper + a test that walks every terminal), plus the receiver-side nudge if D1 chooses it. | D1's choice of signal |
| **D3** | Multi-source project distribution | F6 — provider set, `SplitStrategy::Split`, a hub-backed `ContentDiscovery`, progress across sources, partial-holder failure. | D1's reachability notion (which holders are up) |

Sequence D1 → D2 → D3. D1 is the one that unblocks the owner's smoke; D3 is the only one that adds a capability rather than fixing behaviour, and it inherits D1's vocabulary for "is this peer up".

---

## 6. Open questions for the D1 design session

1. **Primary signal**: receiver-initiated nudge (new `Msg` variant — additive, wire-versioned) · hub address-change watch (no wire change, needs a faster poll for peers with pending work) · sender-side `probe_holder` loop (no wire, no hub, self-contained) — and which is the fallback.
2. **Ladder shape by class**: flat-and-frequent for `no_route`/`timeout` (e.g. fixed 60 s while the peer looks absent), escalating only for local-fatal; where `refused` stops and becomes user-visible.
3. **Per-peer state**: does it live in the engine (in-memory, per peer) or persist (a `sync_peer_state` row) so the UI can say "offline since" across restarts?
4. **Perseus parity**: the observatory case wants aggressive resume (the peer is offline most of the day, and disk cleanup depends on it). Same tuning as the app, or a config knob?
