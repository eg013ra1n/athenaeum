# iroh Transport Hardening — Design — 2026-07-15

Remediates every finding of the 2026-07-15 iroh integration audit
(`docs/superpowers/research/2026-07-15-iroh-integration-audit.md`): C1 same-key endpoint
collision, H1 dial-hint relay mismatch, H2 unbounded relay-map staleness, I1–I4, and the six
minors. Owner decisions (2026-07-15): full audit scope in one cycle; hub carries device
addresses; prod keeps all 4 relays during the window (accepted risk — see §8); unified blob
store; prod hub is currently a single-tester environment, so hub changes may deploy to prod
freely during the cycle.

## 1. Shared iroh node (C1, minors #2/#4)

**One iroh endpoint per process per device key.** New `sharing/iroh/node.rs`:
`SharedIrohNode` — built once (sign-in / first transport need) from the device key + resolved
relay mode; owns the single `Endpoint`, the single `Router` with both ALPNs mounted once
(`athenaeum/sync/1` control + `iroh_blobs::ALPN`), and the single blob store instance (§2).
All current constructors (`api/sync.rs:824` personal sender, `api/collab_exchange.rs:424`
collab sender, `sync/receiver.rs:841` receiver, `perseus/run.rs:458` per-target transports)
become handle acquisitions on the process node. `IrohTransport` survives as the node's
internal implementation; the `SharingTransport` trait stays the consumer surface.

**Event demux.** The transport's single event stream becomes a registry-based demux owned by
the node:
- inbound `Announce`/`Request`/project messages → the registered receiver/serve consumer;
- acks → the owning engine keyed by **(peer, package id)** (one package goes to many
  destinations — Perseus multi-target — so package id alone is ambiguous);
- an event with no registered consumer logs `warn!(kind, peer, "inbound event with no
  consumer")` — never today's silent ack-and-drop (`engine.rs:990-994`).

**Control-connection pool.** Per-peer pooled control connection with idle close (bounded,
~60s) replacing connect-per-message; the existing ack-before-release semantics are preserved
verbatim (ack await is what makes close/reuse safe). Side effect: control connections now live
long enough to hole-punch, so control traffic can upgrade off the relay.

**Relay-health visibility.** The node subscribes to `home_relay_status`
(iroh `endpoint.rs:1384`) and logs transitions at `info!`/`warn!`; a relay eviction
(`SameEndpointIdConnected`) surfaces as `warn!` + a transport event instead of generic
timeouts.

## 2. Unified blob store (C1 continuation)

One `FsStore` at `<sync-dir>/blobs` for all roles — **a single in-process instance owned by
the node** (a second `FsStore::load` on the dir would fail on the redb lock). Tags are
role-namespaced: `recv/pkg/…`, `out/pkg/…`, `collab/pkg/…`. The sender startup sweep becomes
prefix-scoped (`out/` only) so it cannot wipe receiver or collab tags; the collab serve
reconstruction keeps its own `collab/` prefix. GC stays enabled once, node-level.
**No data migration:** all three current stores hold transient, reconstructable content
(crash-resume re-announces from source dirs; manifest-driven collab serve reconstruction;
received blobs finalize into landing roots). First start with the unified store deletes the
orphaned `blobs_out`/`blobs_collab` dirs after the node is up.

## 3. Device addresses via hub (H1, I2)

**Hub** (branch `device-addresses` from hub **main** — prod-deployable independently of the
unmerged collab lineage; the collab-holder surface below rides the collab branch):
- `devices.endpoint_addr` JSONB: `{ homeRelayUrl, directAddrs[], reportedAt }` (nullable).
- `PUT /api/v1/devices/self/address` (device bearer auth) — upsert own address.
- Personal-sync device resolution responses include the target's `endpoint_addr`.
- Collab lineage commit: holder lists gain **`homeRelayUrl` ONLY** — never direct addrs.
  **Privacy rule (binding): direct addresses flow only within one account; cross-account
  surfaces (project holders) carry the relay URL alone.** iroh establishes via the peer's
  relay and hole-punches from there; direct addrs in the hint are an optimization, not a
  requirement.

**App/Perseus:** report own `endpoint_addr()` on node start and on endpoint-addr change
(debounced; the 2026-07-14 path/relay watchers provide the signal). Dialers prefer the peer's
reported address; **fallback to the current our-relay-map hint when the report is absent OR
when dialing the reported address fails** (stale rows must never strand a transfer; also keeps
compatibility with un-upgraded beta.2 devices — no wire break, the report API is additive).
The personal-sync receiver's blob pull `add_peer`s the sender's reported address before
dialing (closes the 60s borrowed-path window, audit I2); fallback: current behavior.

## 4. Relay-map lifecycle (H2)

Both hosts re-resolve the relay map hourly and on sign-in (reusing `resolve_relays` + its
cache). On a changed map: the shared node is rebuilt **when idle** (no active transfers /
serves); a pending rebuild logs `info!` immediately and `warn!` if deferred beyond a max-defer
(6h) — then rebuilds at the next quiet moment regardless. Engine retries re-fetch the peer's
reported address and re-resolve relays before each re-attempt instead of reusing the address
frozen at `add_peer` time (`engine.rs:1159-1185`).

## 5. Lifecycle, identity, gates (I1, I3, I4)

- **Graceful shutdown (I1):** `SharingTransport` gains `async fn shutdown(&self)`. The node's
  shutdown awaits `Router::shutdown()` → store shutdown → bounded `endpoint.close()`. Wired
  into: receiver stop, web server shutdown, Tauri exit hook, Perseus `Agent::shutdown`
  (best-effort — a kill still drops, which iroh tolerates).
- **Device-key lock (I4):** process-lifetime cross-platform advisory lock (fd-lock/fs2 style;
  Windows ships too) on the `device_key` file, taken at node build. A second process with the
  same (copied) key fails loudly at startup with an actionable message instead of silently
  dueling on the relay.
- **Perseus relay gate parity (I3):** `[account].allow_default_relays` becomes effective only
  when signed out (mirror `api/sync.rs:396`). Operational consequence, accepted: a signed-in
  Perseus starting with hub unreachable AND no relay cache now fails loudly (agent `Failed`
  banner) instead of riding public relays.

## 6. Remaining minors

- **Early gating:** move connection authorization as early as iroh exposes the remote
  identity — `incoming_filter` if identity is available there, else `on_accepting`/
  `before_connect`; if no pre-handshake stage carries identity, KEEP the current
  handler-level gate and document why (conditional by iroh API reality, not a promise).
  The handler-level gate stays regardless (defense in depth).
- **Holder probing:** collab downloads probe each holder with a short control connect before
  committing to the 90s blob poll; per-holder failure recorded with a class
  (offline / refused / relay-unreachable) in the transfer history detail.
- **Perseus observability:** `OutboundRow` gains `last_error`; rendered on the web status
  page next to attempts.
- Drop the redundant double store-shutdown in the test-only path (`mod.rs:342-348`).

## 7. Testing

- Existing loopback suites pass with unchanged semantics (they now exercise the shared node).
- New unit/integration: event-demux routing (ack→right engine by (peer, package); announce→
  receiver; orphan event warns); prefix-scoped sweep leaves foreign tags; device-key lock
  (second locker fails); unified-store concurrent roles (send while serving while receiving,
  loopback).
- Hub: API tests for the address endpoint + resolve/holder inclusion + the privacy rule
  (holders never contain direct addrs).
- Live (owner-runnable): `relay_check --paths` mode printing reported-vs-actual home relay;
  an `#[ignore]`d two-endpoint real-relay test against test-relay that detects the
  same-endpoint-id eviction; the multi-PC dev smoke re-run after C1 lands is the cycle's
  real acceptance gate.
- Gates per task + slice: `cargo build --workspace`, `cargo test -p athenaeum-core`,
  `cargo test -p perseus`, `cargo build -p perseus --no-default-features`, `npx tsc
  --noEmit`; hub: its own suite.

## 8. Ops notes / accepted risks

- Prod keeps all 4 relays during the cycle (owner decision; single-tester environment).
  Known failure signature meanwhile: a device that restarts homes by latency (e.g.
  observatory → relay-ru) and becomes undialable by peers still hinting other relays —
  transfers time out generically. If observed: that is H1, not a new bug.
- Hub `device-addresses` may deploy to prod freely during the cycle (single-tester); the
  collab-holder surface reaches prod only with the collab merge.
- Rollout order: C1+§2 first (complete, testable), then H1/H2 stacking on the node, then
  §5/§6. Old and new app builds interoperate throughout (report API additive; dial fallback
  preserved).

## Out of scope

- Dark/store-and-forward relaying, parallel multi-source fetch (spec §12 carry-overs).
- Forcing relay selection / pinning per device.
- Any wire-protocol version bump (none needed — all changes are additive or client-side).
