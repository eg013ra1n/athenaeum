# iroh Integration Audit — Athenaeum + Perseus — 2026-07-15

Three-track audit (transport layer / sync+collab flows / Perseus + cross-cutting) of the
iroh 1.0.2 integration on branch `0.5.0` at `a2a536b6`. Every iroh-semantics claim was
verified against the registry sources (`iroh-1.0.2`, `iroh-blobs-0.103.0`, `iroh-relay-1.0.2`).
Context: the 2026-07-12 field incident (relay "same endpoint id" kick + transfers timing out
behind NAT), the 2026-07-14 relay expansion (prod: relay1+relay2+relay-ru+relay-ams; test-hub:
test-relay), and the 2026-07-11 security audit (its fixes were confirmed intact, not re-audited).

## Verdict

**The iroh API usage is idiomatic and mostly correct** — ack-before-close sidesteps iroh's
immediate-close data-drop semantics; the connect gate covers both ALPNs on authenticated peer
ids; `online()` gating, keepalive/idle defaults, and the blobs GC/tag lifecycle are all handled
with accurate reasoning. **The architecture above it has one Critical and two High defects that
together reproduce the exact July-12 field signature**, none of which loopback tests can catch
(they run relay-disabled on distinct sockets).

## Critical

### C1 — Multiple same-key iroh endpoints self-collide on the relay

Production constructs up to three concurrent `IrohTransport`s from the SAME device key:
personal sender (`api/sync.rs:824`), collab sender (`api/collab_exchange.rs:424`), receiver
(`sync/receiver.rs:841`). Each is a full iroh `Endpoint` that homes on the relay under the
same node id — and a relay permits exactly one active connection per endpoint id
(`iroh-relay-1.0.2/src/server/clients.rs:84-92`, older connection is deactivated with
`SameEndpointIdConnected`). Inbound datagrams reach only whichever endpoint currently holds
the relay slot: an announce landing on a sender is silently delivery-acked then dropped
(`sync/engine.rs:990-994`); a blob request landing on the wrong endpoint can't find the
collection (stores are split: `blobs` / `blobs_out` / `blobs_collab`). The in-code premise
that "production role-gating keeps them mutually exclusive" (`api/sync.rs:817-823`) is stale —
the Phase-1 mesh model made every signed-in node send AND receive, and a collab holder
structurally runs receiver + collab sender concurrently. **This alone explains the July-12
incident** (relay kick at 20:56, generic sync timeouts at 21:39) without any copied DB.

**Fix direction:** ONE process-wide iroh endpoint per device key; multiplex the personal-sync,
collab, and blobs protocols over it (ALPN dispatch already exists), keeping the separate blob
stores. Never bind a second relay-enabled endpoint with the same secret.

## High

### H1 — Dial hints carry OUR relay map, not the peer's real address

Every production dial path synthesizes the peer's `EndpointAddr` as a bare node id + *our own*
resolved relay URLs (`sync/pairing.rs:178` via `api/sync.rs:841`, `collab_exchange.rs:438/:992`,
collab push-seed). Relays do not forward to each other, and iroh expects "zero or one home
relay" per endpoint addr (`iroh-1.0.2/src/address_lookup.rs:381`). With one relay this was
trivially correct; with four deployed, devices home by latency onto DIFFERENT relays, and
whether a 4-URL hint reaches a peer homed on the 3rd is UNVERIFIED behavior. Holder records
carry pubkey+display only — no address (`collab_exchange.rs:957-966`).

**Fix direction:** each device self-reports its live `endpoint_addr()` (home relay + direct
addrs — already available, `sharing/iroh/mod.rs:316`) to the hub device record / holder rows;
dial with the peer's real address. Until then, cross-region transfers between devices homed on
different relays are at risk — consider either fast-tracking this fix or temporarily trimming
the prod `relays` table back to relay1 (interim decision, owner call).

### H2 — Relay-map staleness is unbounded

`resolve_relays` runs once per transport build; nothing re-resolves, rebuilds, or invalidates
per-peer engines (`api/sync.rs:409-419`, `sync/receiver.rs:803`, `sync/sender.rs`). Perseus
resolves once per agent start with no timer (`perseus/account.rs:327`, only caller
`run.rs:445`). The 2026-07-14 relay additions are invisible to every running device until
restart — the fleet splits across relay sets. Retries reuse the address fixed at `add_peer`
time (`engine.rs:1159-1185`), so an address-caused stall walks all 5 attempts to `Failed`
without ever re-resolving.

**Fix direction:** periodic relay re-resolution (reuse `resolve_relays` + cache) with
endpoint rebind or scheduled recycle, on both app and Perseus; re-resolve + re-`add_peer` on
retry.

## Important

- **I1 — No reachable graceful shutdown in production.** `IrohTransport::shutdown` consumes
  `self`; the `SharingTransport` trait has no shutdown; production transports are always
  `Arc`-wrapped → drop-only teardown aborts the router mid-poll, skipping
  `protocols.shutdown()` → `endpoint.close()` (`sharing/iroh/mod.rs:341`, iroh
  `protocol.rs:597-602`; iroh explicitly recommends awaiting shutdown). Peers see QUIC resets
  instead of clean closes; on fast restart the relay still holds the old registration —
  aggravates C1's kick behavior. Perseus same (`run.rs:882`). Fix: trait-level
  `async fn shutdown(&self)`, awaited `Router::shutdown` + `Store::shutdown` + bounded
  `endpoint.close()` on agent/receiver teardown.
- **I2 — Personal-sync blob pull dials by bare id with no lookup entry**
  (`sharing/iroh/blobs.rs:236-249` from `sync/receiver.rs:479/:633`) — relies on iroh's
  `RemoteStateActor` retaining paths from the just-closed announce connection (60s idle,
  `socket.rs:1306-1308`). A delayed/retried fetch fails with `NoAddress`. Fix: carry the
  sender's addr (or reuse the announce connection); the collab path already `add_peer`s.
- **I3 — Signed-in Perseus can ride public relays.** `[account].allow_default_relays` is
  honored even when signed in (`perseus/account.rs:381-385`); the app forbids it for signed-in
  nodes (`api/sync.rs:396`) after a documented production incident (mixed relay networks can't
  dial each other). Fix: mirror the app's gate.
- **I4 — Device-key isolation is by-path only** (`account/keys.rs`, `perseus/config.rs:624`) —
  nothing detects a copied key (backup restore / rsync'd app-data). Combined with C1's fix, a
  single-instance advisory lock on the key file + surfacing the relay's
  `SameEndpointIdConnected`/home-relay-status watcher would make duplicate identity loud
  instead of silent.

## Minor

- Connect gate fires post-handshake (`sharing/iroh/mod.rs:775/:953`) — bounded DoS surface;
  `incoming_filter`/`before_connect` would reject earlier (deliberate choice, documented).
- Per-message control connections never live long enough to hole-punch → control traffic
  always rides the relay; plus connection/task churn (efficiency only).
- Holder failures indistinguishable (offline vs NAT-blocked), 90s sequential each
  (`collab_exchange.rs:1001-1019`).
- Mis-delivered inbound events dropped silently (`engine.rs:990-994`) — warn instead;
  subscribe to `home_relay_status` (`iroh endpoint.rs:1384`).
- Perseus web page shows attempts but no per-package `last_error` (`sync/models.rs:118-127`).
- Test-only double store-shutdown after router shutdown (benign, `mod.rs:342-348`).

## Confirmed-correct highlights

`presets::Minimal` = crypto provider only (no discovery — deliberate, hint-driven design);
lookup-miss dials fail fast with `NoAddress`, never hang; ack-before-close makes per-message
connections lossless; keepalive 5s < all idle timeouts so serves can't idle-drop mid-transfer;
startup tag sweep is GC-safe (temp tags protect in-flight imports); ALPN set and gating
identical app↔Perseus by construction (shared transport, `iroh 1.0.2`/`iroh-blobs 0.103.0`/
`n0-future 0.3.2` single-versioned); headless build compiles out nothing protocol-relevant;
dev default-relay refusal is loud and surfaced (app side).

## Remediation priority (proposed)

1. **P1 — C1**: single shared endpoint per process (prerequisite for reliable NAT operation).
2. **P2 — H1**: propagate real peer `EndpointAddr`s via hub/holders (decide interim: trim prod
   relay map to relay1 vs fast-track).
3. **P3 — H2 + I1**: relay-map refresh + graceful shutdown (both hosts + Perseus).
4. **P4 — I2/I3/I4** and the minors, ridealong with the above.
