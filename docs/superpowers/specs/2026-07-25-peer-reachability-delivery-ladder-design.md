# D1 — Peer reachability and the delivery ladder

**Status:** design approved 2026-07-25 · **Audit:** `docs/superpowers/research/2026-07-25-delivery-model-audit.md` (findings F1, F2, F3, F7, F8) · **Successors:** D2 (receive-side terminal invariant), D3 (multi-source project distribution)

## 1. Problem

A transfer whose peer went away resumes on a timer derived from **our own failure count**, not from anything about the peer. Measured on the 2026-07-24 two-machine smoke: the receiver was back at 21:45:24; the sender noticed its ack timeout at 21:45:35 and armed the next attempt for 21:46:35 — a full minute of nothing while both sides were up and idle. On a later rung that wait is 30 minutes. On Perseus it is worse than cosmetic: source-file deletion is gated on `Confirmed`, so a parked transfer also holds the observatory's disk.

Three properties of the current model produce this:

- **No reachability notion.** The sender has no per-peer state. `kick_all()` exists but fires only when OUR OWN relay reconnects.
- **Absence counts as our failure.** `arm_retry` and the ack-timeout path climb a rung unconditionally; `ConnectClass` is computed and stored but never shapes the schedule.
- **The ladder is per package, the condition is per peer.** N packages to one absent peer wake and dial N times per window.

And one property makes frequent retries unaffordable today: **`role_serve` re-imports the whole package directory on every attempt** (`node.rs:1806` → `blobs::import_package_collection`, `add_path` per file), so an attempt against an absent peer re-hashes the payload before discovering nobody is there.

## 2. Decisions

| Decision | Choice |
| ---- | ---- |
| Primary signal | The **receiver announces its presence** to its authorized peers over the existing control channel |
| When | Strictly on two edges: receiver goes online at startup, and its own relay reconnects. **No periodic beacon.** |
| Fallback | A **flat** retry interval while the peer looks absent; escalation reserved for refusal and local faults |
| Peer state | In the engine's memory. No table, no migration |
| UI | Honest per-row text driven by the peer's failure class; no page restructuring |
| Retry cost | `serve` short-circuits when the package is already served in this process |

## 3. Architecture

Five components, each independently testable.

### 3.1 Presence beacon (receiver side)

A new control message, `Msg::Presence`, carrying **no payload** — the sender's identity is the authenticated `remote_id` of the connection it arrives on.

Sent to **every authorized account device**, not only peers with unfinished inbound rows: when the sender enqueued a package while we were down, we hold no row for it at all, so a row-scoped beacon would miss exactly the case it exists for.

"Account device" is the boundary, not "every peer we have ever spoken to": collab holders are cross-account, and announcing our uptime to another account's members leaks a presence signal nobody asked for. Personal-sync peers are our own devices, so the beacon tells them nothing they are not already entitled to know. Collab delivery keeps the flat ladder alone.

Fires on two edges only:

- the receiver reaches "online" during startup (after the transport is up and the authorized set is loaded);
- the node's `WakeHook` fires (own relay reconnect) — the same edge that already means "I am reachable again", today wired only to the sender half.

Delivery discipline: fire-and-forget, bounded concurrency (`PRESENCE_FANOUT_CONCURRENCY = 4`), short per-peer timeout (`PRESENCE_SEND_TIMEOUT = 5 s`, deliberately not the 30 s `CONTROL_SEND_TIMEOUT` — a beacon must never make startup wait on dead peers), failures logged at debug. **On a dedicated connection, never the pooled control connection** — see §4.

### 3.2 Presence handling (sender side)

The transport surfaces `TransportEvent::PresenceReceived { from }`. The host maps it to `kick_peer(from)` on **both** sender runtimes (personal sync and collab), mirroring what the existing wake hook does.

`SyncSenderRuntime::kick_peer` is **ensure-then-kick**: a beacon is useless if no engine exists for that peer, which happens when the beacon beats `resurrect_pending_senders`, or on a signed-in device whose cached allow-list is empty so resurrection filtered every peer out. It therefore builds the engine (`ensure_sender_engine`, idempotent) and then kicks.

Two gates before any of that, both evaluated from **local** state only:

1. the peer is in our authorized set — otherwise a stranger could make us allocate engines;
2. we hold at least one non-terminal `sync_outbound` row for that peer — otherwise there is nothing to resume.

A beacon that passes both is debounced per peer (`PRESENCE_DEBOUNCE = 10 s`) so a flapping relay on the peer's side cannot turn into a burst of attempts.

**The kick touches only parked packages** (`next_action == Retry`). A package in `AwaitAck` with recent serve activity is a live pull; collapsing its deadline would fire a pointless re-announce mid-transfer.

### 3.3 Class-aware ladder

`retry_backoff` becomes a function of `(base, rung, class)`:

| Class | Meaning | Schedule |
| ---- | ---- | ---- |
| `no_route`, `timeout`, `relay_unreachable`, `not_started` | the peer looks absent | **flat `ack_timeout × 4` = 120 s at the default base**, rung not climbed |
| `refused` | the peer is up and declines us | current escalating ladder — this needs a human, not persistence |
| `other`, local faults | our side, or unknown | current escalating ladder, unchanged |

`not_started` belongs with the absent classes even though its doc reads "the local **or** remote endpoint is not started". Both readings want the same schedule: a remote endpoint that has not started is precisely an absent peer, and a local one is transient — our own start fires the wake hook, which releases the queue before the flat interval matters. Escalating on it would punish the commonest shape of "the other side's app is not running", and it is what the loopback tests produce.

The rung additionally resets to 0 on **any** successful contact with the peer — an ack, a serve tick, or a presence beacon — not only on a serve tick as today.

**An ack timeout carries no class.** A package that was announced successfully and then went unacknowledged (audit F7 — the receiver died mid-ingest, the bytes are already there) has no failed dial to classify, so it takes the escalating ladder. That is the right default: nothing observed says the peer is absent, and if its ingest is genuinely wedged, backing off is correct. What rescues it is the beacon, the moment the receiver restarts. Stated here because the alternative reading — "treat an unacked package as absent and retry flat forever" — would hammer a peer that is up and busy.

### 3.4 Per-peer absence and coalescing

Without this, "flat and frequent" is worse than the status quo: 50 queued packages would each attempt every 2 minutes.

The engine (already one per peer) keeps `PeerReachability { absent_since: Option<Instant>, last_class: Option<ConnectClass> }` in memory.

- An attempt failing with an **absent class** marks the peer absent (stamping `absent_since` only on the transition) and parks that package.
- While the peer is absent, exactly one package — the **head**, deterministically the lowest pending row id — carries a live `ABSENT_RETRY_INTERVAL` deadline. Every other pending package parks with no deadline; it waits for a signal, not a clock.
- The head is re-elected whenever it leaves `pending` (confirmed, cancelled, or terminalized by a local fault).
- **Any successful contact** (announce accepted, ack, serve tick, presence) clears absence and kicks every parked package.
- A **newly enqueued** package always makes one immediate attempt even while the peer is marked absent: the enqueue is user intent, the peer state may be stale, and the user deserves fast feedback. If it fails absent-class, it parks like the rest.

### 3.5 Idempotent serve

`role_serve` reuses the collection hash from the node's in-memory `served` map instead of re-importing, keyed on the pair `(tag, want fingerprint)` — the want subset is negotiated once and cached per package, so a differing subset must not silently reuse a full-package collection. The map is cleared by `release`, so a rebuilt payload (Perseus resend) re-imports correctly. Cross-restart the map is empty, so the first serve after a restart re-imports — correct, since the blob store's tags may have been swept.

## 4. Wire change and compatibility

`Msg::Presence` is **appended at the END** of the enum, per the frozen-index rule documented at `proto.rs:95`: postcard keys variants by declaration index, so appending is additive and needs no `SYNC_ALPN` bump. New golden bytes are pinned in `sharing::wire_golden_tests`.

**New receiver → old sender.** The old peer cannot decode the unknown variant, and its control accept loop `break`s on a decode failure (`mod.rs:1581`) — it tears down **that whole connection**, not just the stream. This is why the beacon rides a dedicated connection: the only casualty is the connection the beacon itself opened, while the peer's pooled channel for announces is untouched. Cost on an old peer: one `decode control message failed` warn per beacon.

**Old receiver → new sender.** No beacon ever arrives; the flat ladder is the whole mechanism. This is the reason §3.3 exists as more than a nicety.

## 5. Why not through the hub

The hub already learns that a device came online — every device PUTs its endpoint address on startup. Using that as the signal was considered and rejected.

- **No push.** The hub is plain REST; devices poll (`list_devices`, hourly). Turning presence into a signal means polling often enough to matter, from every device, to learn something the peer can state in one message.
- **Blind exactly where we are strongest.** Two devices on one LAN reach each other with no hub involvement at all; a hub-derived signal would be silent there.
- **Indirect.** A published address proves a device booted, not that it is dialable now — the NAT mapping may already be gone.
- **Two sources of truth** for one fact, to be reconciled forever after.

Its one genuine advantage — it works with peers running older builds — is covered by the flat ladder, which we need regardless for the case neither side observes as a restart.

Worth recording honestly: this is **not** an argument that the design removes a hub dependency. In the cross-NAT case the relay asks the hub to admit *every* connection and fails closed when the hub is unreachable (`athenaeum-hub/docs/ops/relay.md` §0), so a relayed beacon needs a live hub just as much as a relayed announce does. The independence the beacon buys is real only on directly reachable paths — LAN, VPN, or an already-punched route.

## 6. Coverage

Every way a transfer can stall, and the single mechanism responsible for ending it:

| What went away | What resumes it |
| ---- | ---- |
| Receiver (app closed, machine slept) | its presence beacon on returning — **new** |
| Sender, with a restart | `resurrect_pending_senders` rebuilds engines from non-terminal rows (existing; uses the *cached* allow-list, so no hub needed) |
| Sender, without a restart (its network healed) | its own `WakeHook` → `kick_all` (existing) |
| Neither — the route between them broke | the flat `ABSENT_RETRY_INTERVAL` — **new** |
| Nothing — but the beacon found no engine | ensure-then-kick — **new** |
| Peer is up and refusing us | nothing, by design: it escalates and surfaces as an error a human must act on |

## 7. User-visible surface

A parked row reads "waiting for `<device>` — unreachable" instead of a countdown to an arbitrary retry. The head keeps its real countdown (`stalledUntil`); parked rows omit it, because they are waiting for a signal rather than a clock. Text derives from `ConnectClass`, which already reaches the UI as the `last_error` prefix. Perseus's web page inherits this for free — same process, same engines.

## 8. Out of scope

Deliberately excluded, with reasons: a Perseus-specific tuning profile (the observatory case is already covered by the home app's beacon; a knob without a demonstrated need); per-peer grouping in the Transfers page (a frontend cycle of its own); persisted peer state (memory suffices for what the UI states); the receive-side terminal invariant (**D2**); multi-source downloads (**D3**).

## 9. Testing

- **Wire**: golden bytes for `Msg::Presence`; existing variants' pins unchanged.
- **Ladder** (unit, table-driven): absent classes stay flat and do not climb; `refused` escalates; local faults escalate; rung resets on ack, on serve tick, and on presence.
- **Coalescing** (engine): three packages, peer absent ⇒ exactly one attempt per interval; a presence beacon ⇒ all three attempt; head re-election when the head terminalizes.
- **Non-disturbance**: presence during an `AwaitAck` package with recent serve activity issues no re-announce.
- **Gates**: presence from an unauthorized peer builds no engine and logs a warn; presence from an authorized peer with only terminal rows builds no engine.
- **Serve**: a second `serve` of the same `(tag, want)` performs no import; a differing want does import.
- **Beacon fan-out**: sent to every authorized peer, not only those with inbound rows; a dead peer does not delay the others (bounded concurrency, short timeout); a send failure is not fatal.
- **End-to-end (loopback)**: peer absent → package parks → beacon → delivered.

## 10. Risks

- **Old-peer noise**: one warn and one dropped incoming connection per beacon, bounded by the two edges. Accepted.
- **Beacon storm** from a peer whose relay flaps: bounded by the sender-side debounce.
- **A stale absent mark** (peer returned but nothing contacted it) resolves within one flat interval.
- **The flat interval (`ack_timeout × 4` = 120 s) is a judgement call**, chosen so an unattended overnight outage costs ~30 idle dials/hour per peer rather than 2 at the old cap; if field logs show that is too eager for Perseus fan-out, it is one multiplier. Expressing it as a multiple of the base — rather than an absolute duration — follows the convention `retry_backoff` already uses, so a test configured with a millisecond `ack_timeout` observes the flat path without waiting two minutes.
