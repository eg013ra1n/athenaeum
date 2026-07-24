# Transport Observability & Relay-Reporting Follow-ups

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Status (2026-07-25):** Tasks 1–5 implemented and committed (athenaeum `b2ff649d`, athenaeum-hub `050fc96`); all gates green (`cargo build --workspace --all-targets`, `cargo build -p perseus --no-default-features`, `cargo test -p athenaeum-core --lib` 1137 passed, `npx tsc --noEmit`). **Task 6 (owner verification) is open** — nothing here has been observed against a live two-device transfer yet.

**Goal:** Close the transport-layer findings left open by the 2026-07-24 transfers/iroh audit — make the iroh transport observable (which is why a fleet-wide NAT-traversal outage went unseen for months), stop the hub address reporter from publishing a degraded address, and fix the home-relay watcher's health surface.

**Context — what the audit already fixed (do NOT redo):**

- QUIC address discovery served on UDP 8443 while every client probes iroh's `DEFAULT_RELAY_QUIC_PORT` (7842) ⇒ no endpoint behind NAT ever learned its public address and two peers behind different NATs could never hole-punch. Fixed by `relay_quic_port: 7842` in the astronet inventory + redeploy of all five relays (astronet `a460c31`). Verified: all 5 relays hand back a public address.
- `examples/relay_check.rs` now GATES QAD (public-address assertion, non-zero exit, `--no-qad` opt-out) — athenaeum `cc6737d5`.

**NOT in this plan:** the first-contact transfer stall (own queued cycle — sender's connect gate refuses the receiver's blob dial-back before the authorized-set refresh lands; unrelated to QAD), and the pending owner smokes (transfers v2 U1–U8, Perseus UI v2, Windows).

**Tech stack:** Rust (`athenaeum-core`), React/TS for one settings row. No wire changes, no DB changes, no new commands.

## Global Constraints

- Branch: `0.5.0`. Commit as `eg013ra1n` — NEVER add a Claude co-author footer.
- Nothing in `crates/athenaeum-core/src/sharing/types.rs` / `proto.rs` changes — `Msg` postcard indices are FROZEN.
- Logging conventions: message = short stable phrase, all data in snake_case fields; new field names must come from the spec's field dictionary or be added to it. Zero `println!`.
- `cargo build -p perseus --no-default-features` must keep working — every task here touches code Perseus links.
- `iroh`'s `metrics` feature is a DEFAULT feature and Task 2 depends on it (`Endpoint::metrics` is `#[cfg(feature = "metrics")]`). If anyone ever sets `default-features = false` on the `iroh` dep, Task 2's module stops compiling — say so in a code comment rather than adding a local feature flag.
- Gates for every task: `cargo build --workspace --all-targets`, the task's own tests, plus `npx tsc --noEmit` for Task 1.

## Verified facts (checked against the tree on 2026-07-25 — read before coding)

- `crates/athenaeum-core/src/logging/config.rs:16` `THIRD_PARTY_QUIET: [&str; 7] = ["iroh", "iroh_relay", "iroh_blobs", "net_report", "portmapper", "netwatch", "noq_udp"]`, appended as `,<t>=warn` BEFORE the user's module overrides — and `to_directives()` deliberately relies on `EnvFilter` last-directive-wins, so a module key mapping to the same targets overrides the baseline. `MODULE_TARGETS` (line 27) currently has 4 entries: scanner / solver / calibration / archive. UI mirror: `src/components/settings/LoggingSettings.tsx:18` `MODULES`.
- `iroh::Endpoint::metrics() -> &EndpointMetrics` (endpoint.rs:1608, `#[cfg(feature = "metrics")]`). `EndpointMetrics.socket: Arc<SocketMetrics>` carries `send_ipv4` / `send_ipv6` / `send_relay` (BYTES), `recv_data_ipv4` / `recv_data_ipv6` / `recv_data_relay` (BYTES), `paths_direct` / `paths_relay`, `holepunch_attempts`, `num_conns_direct` / `num_conns_opened`, `relay_home_change`. Read a counter with `Counter::get() -> u64` (inherent method — no `iroh_metrics` dep needed).
- `iroh::Endpoint::remote_info(EndpointId).await -> Option<RemoteInfo>` (endpoint.rs:1620) — snapshot of known transport addresses for ONE peer; `RemoteInfo::addrs() -> impl Iterator<Item = &TransportAddrInfo>`, `TransportAddrInfo::addr() -> &TransportAddr` (`is_relay()` / `is_ip()`), `TransportAddrInfo::usage() -> TransportAddrUsage` (`Active` vs not).
- `spawn_conn_path_diagnostics` (`sharing/iroh/mod.rs:1850`) instruments CONTROL connections only (node.rs:609 outgoing, mod.rs:1565/1785 incoming). The bulk transfer runs through `iroh-blobs`' own downloader/connection pool (`sharing/iroh/blobs.rs:336`, `store.downloader(endpoint)`), which never hands us a `Connection`.
- `sync::pairing::spawn_endpoint_address_reporter` (pairing.rs:292): 30 s poll (`ADDRESS_REPORT_INTERVAL`), publishes when `has_addr = relay.is_some() || !direct.is_empty()` AND the `(relay, direct)` tuple changed since the last SUCCESSFUL report. `endpoint_addr_report_parts` (pairing.rs:273) takes `addr.relay_urls().next()` — a BTreeSet iterator, so alphabetically-first, not "the connected one". Hub API is singular: `put_device_address(token, home_relay_url: Option<&str>, direct_addrs)` → `{"homeRelayUrl": …|null, "directAddrs": [...]}` (account/client.rs:249).
- Field evidence for the reporter bug (app logs, 2026-07-22): `22:30:28 reported … relay_url=https://relay2…` then `22:30:51 reported … relay_url=none` — a relay-less snapshot published over a good one during a home-relay switch.
- `spawn_home_relay_watcher` (`sharing/iroh/node.rs:2172`): streams `endpoint.home_relay_status()` (a `Vec<RelayStatus>` — one entry per home relay), keeps `last: HashMap<url, bool>`, and for every CHANGED entry writes the whole `RelayHealth` cell + logs. Two consequences, both real:
  1. the first status seen for a URL is treated as a transition, so a fresh start logs `WARN home relay disconnected` for a relay that was never connected (seen at every app start in the logs);
  2. the cell records the LAST transition, not the aggregate — a secondary relay dropping flips `transport_health()` to `direct_only` even while another home relay is connected. `Endpoint::online()` gets this right (`statuses.iter().any(is_connected)`, endpoint.rs:1359) — mirror that.
- `SharedIrohNode::transport_health()` (node.rs:1046) is the only reader of that cell; it feeds `api::sync::derive_transport_health` and the Settings → Sync status surface.

---

### Task 1: `transport` logging module in Settings

Today `iroh`, `iroh_relay`, `iroh_blobs`, `net_report`, `portmapper`, `netwatch`, `noq_udp` are pinned to `warn` with no UI way to raise them — a QAD outage that logged `probe timed out` at DEBUG was therefore invisible in every field log. Give the user one switch.

**Files:**
- Modify: `crates/athenaeum-core/src/logging/config.rs` (`MODULE_TARGETS` 4 → 5 entries)
- Modify: `src/components/settings/LoggingSettings.tsx` (`MODULES` entry)
- Test: `crates/athenaeum-core/src/logging/config.rs` `#[cfg(test)] mod tests`

**Approach:**
- Add `("transport", &["iroh", "iroh_relay", "iroh_blobs", "net_report", "portmapper", "netwatch", "noq_udp", "athenaeum_core::sharing::iroh"])`. Include our own transport module so one switch raises both sides of the seam; the third-party targets must be listed in full because the override wins by being LAST, not by prefix.
- UI label: `{ key: 'transport', label: 'Transport (iroh / relays)' }`. Add a one-line hint next to it that Debug here is very verbose (an evening Perseus run produced ~71k `iroh::socket::transports` span-close events — that is why the baseline exists).
- No type change: `LoggingConfig.modules` is a map, so no ts-rs regeneration.

**Tests:**
- `transport_module_overrides_the_third_party_quiet_baseline`: `to_directives()` for `{level: "info", modules: {transport: "debug"}}` contains `iroh=warn` BEFORE `iroh=debug` and the string parses as an `EnvFilter`.
- Extend the existing `default_config_is_info_plus_third_party_quiet` assertion set so the new key does not silently change the default output.

**Acceptance:** Settings → Logging offers "Transport (iroh / relays)"; selecting Debug and reproducing a relay problem yields `iroh::net_report` lines in the JSONL log without `ATHENAEUM_LOG`.

---

### Task 2: Transport telemetry — relay-vs-direct share and per-peer path

Two questions the app currently cannot answer: "did this transfer actually go direct?" and "what share of our bytes rides the relay?" The second one decides whether self-hosted relays stay cheap (it was asked during the audit and could not be answered). Both are now available from public iroh APIs; no per-connection instrumentation of `iroh-blobs` is needed.

**Files:**
- Add: `crates/athenaeum-core/src/sharing/iroh/telemetry.rs` (counter snapshot + delta + the peer-path classifier)
- Modify: `crates/athenaeum-core/src/sharing/iroh/node.rs` (module decl; sample at shutdown + a periodic tick; expose `peer_path_kind(peer)`)
- Modify: `crates/athenaeum-core/src/sharing/iroh/mod.rs` only if the classifier is shared with the legacy transport (prefer: it is not)
- Test: `crates/athenaeum-core/src/sharing/iroh/telemetry.rs` `#[cfg(test)]`

**Approach:**
- `TransportCounters { send_relay, send_direct, recv_relay, recv_direct, paths_direct, paths_relay, holepunch_attempts, relay_home_change, num_conns_direct, num_conns_opened }` with `fn snapshot(&Endpoint) -> Self` (sums ipv4+ipv6 into `*_direct`) and `fn delta(&self, earlier: &Self) -> Self`. Pure arithmetic — unit-testable without a network.
- Periodic sampler task on the node (reuse the existing spawn-and-abort-on-shutdown pattern of `spawn_home_relay_watcher`): every 5 min log `info!(…, "transport traffic")` with the DELTA since the previous tick, and SKIP the tick entirely when every byte counter delta is 0 (an idle app must not add a log line every 5 minutes). One final line at `shutdown`.
- Per-peer path: `pub async fn peer_path_kind(&self, peer: NodeId) -> &'static str` returning `direct` / `relay` / `mixed` / `unknown` from `endpoint.remote_info(id)` filtered on `TransportAddrUsage::Active`. Call it once at the end of a fetch (receiver side) and once on serve-complete (sender side) and include it in the existing terminal log line — do NOT add a new event.
- `relay_home_change` in the same struct answers the open ping-pong question (Task 6) with a number instead of log archaeology.
- Field names: `send_relay_bytes`, `send_direct_bytes`, `recv_relay_bytes`, `recv_direct_bytes`, `holepunch_attempts`, `relay_home_change`, `conn_path`. Add them to the field dictionary in `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` in this task (the CLAUDE.md rule: no inline-invented field names).
- Log-only in v1. No Tauri command, no axum route, no UI — so the two-backend rule does not apply. If the numbers later deserve a UI, that is its own task.

**Tests:**
- `delta_subtracts_monotonic_counters` and `delta_of_idle_sample_is_all_zero`.
- `classify_active_paths_*`: table test over synthetic `(is_relay, is_ip, active)` triples → `direct` / `relay` / `mixed` / `unknown` (classifier takes an iterator, so it is testable without an endpoint).
- Loopback integration check (relay disabled) asserting the sampler emits nothing while idle.

**Acceptance:** after one real transfer the log carries a `transport traffic` line whose `send_relay_bytes` / `send_direct_bytes` split can be read off directly, and the transfer's terminal line carries `conn_path=direct|relay`.

---

### Task 3: Honest home-relay reporting to the hub

The reporter publishes a relay-less address over a good one during a home-relay switch, and picks the alphabetically-first relay rather than the connected one. Same function, one fix.

**Files:**
- Modify: `crates/athenaeum-core/src/sync/pairing.rs` (`endpoint_addr_report_parts`, reporter loop)
- Modify: `crates/athenaeum-core/src/sharing/iroh/node.rs` only if a "current connected relay" accessor is needed (Task 4 makes `RelayHealth` aggregate, so `transport_health().relay_url` becomes trustworthy — depend on Task 4, do this one second)
- Test: `crates/athenaeum-core/src/sync/pairing.rs` `#[cfg(test)] mod tests`

**Approach:**
- Prefer the CONNECTED home relay: take the node's aggregate `RelayHealth` (post-Task-4) when it reports connected, else fall back to `addr().relay_urls().next()`. Keep the fallback — a device with the relay disabled must still report its direct addrs.
- Debounce the relay-less snapshot: extract a pure decision fn
  `fn should_publish(last: Option<&Report>, current: &Report, consecutive_relayless: u8) -> bool`
  that SKIPS a snapshot whose relay is `None` when the last successful report carried one, for at most 2 consecutive cycles (60 s), then publishes the truth. A genuinely relay-less device must not be able to pin a stale URL at the hub forever.
- Keep the existing "advance `last` only on success" behavior untouched.

**Tests:**
- `relayless_snapshot_after_a_good_one_is_skipped_then_published_after_two_cycles`
- `direct_addr_change_still_publishes_during_the_relay_gap`
- `first_report_with_no_relay_yet_still_publishes` (startup case — nothing to preserve)
- `connected_relay_wins_over_alphabetically_first`

**Acceptance:** across an app run with home-relay switching, `reported endpoint address to hub` never logs `relay_url=none` after a relay has once been connected (unless the relay is genuinely gone for >1 min).

---

### Task 4: Home-relay watcher — aggregate health, no false startup WARN

`RelayHealth` is written from whatever entry changed LAST, so with more than one home relay it can report `direct_only` while another relay is connected; and the first status per URL counts as a transition, so every app start logs a WARN for a relay that was never connected.

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/iroh/node.rs` (`spawn_home_relay_watcher`, and the `RelayHealth` doc comment which currently describes the last-transition semantics)
- Test: `crates/athenaeum-core/src/sharing/iroh/node.rs` `#[cfg(test)] mod tests` (there is already a test-only health-cell overwrite hook — extend it rather than inventing a new seam)

**Approach:**
- Compute the cell from the WHOLE status vector on each stream item: `connected = statuses.iter().any(|s| s.is_connected())`, `url` = the connected one (first connected, stable), `last_error` = the error of the disconnected entry only when nothing is connected. This mirrors `Endpoint::online()` and keeps `transport_health()` honest.
- Logging stays per-URL transition-based (that granularity is genuinely useful), but the FIRST observation of a URL logs at `debug`, not `warn`, when it arrives disconnected. Only a true connected → disconnected flip is a `warn`.
- Extract the decision as a pure fn (`fn health_from(statuses: &[…]) -> RelayHealth` + `fn transition_level(prev: Option<bool>, now: bool) -> Level`) so both behaviours are unit-testable without a live relay.

**Tests:**
- `health_is_connected_when_any_relay_is_connected`
- `secondary_relay_drop_does_not_flip_health_to_direct_only`
- `first_disconnected_status_is_not_a_warning` / `real_drop_after_connect_is_a_warning`

**Acceptance:** a fresh start with relays configured logs no `WARN home relay disconnected` before the first connect; `get_sync_status` reports `relay_connected` whenever at least one home relay is up.

---

### Task 5: Ops doc matches the deployed relay contract (hub repo)

The doc a future relay would be built from still describes the pre-audit world, which is exactly how the port drift happened.

**Files:**
- Modify: `../athenaeum-hub/docs/ops/relay.md` (separate repo — commit there, same identity)

**Approach:**
- Record reality: relay HTTPS on **8443** (TCP 443 on those hosts is taken by other services — Cloudflare-fronted site on relay1, a TLS proxy on relay2/relay-ru, nothing on relay-ams), QAD on **UDP 7842**, `[tls] quic_bind_addr` explicit in `relay.toml`.
- Change QAD from "optional but recommended" to MANDATORY, and state WHY the port is not free: clients derive it from iroh's `DEFAULT_RELAY_QUIC_PORT`, the hub relay map carries URLs only, so a non-default port is undiscoverable and silently kills hole punching for every client.
- Note that the deployed config is rendered from astronet `templates/iroh-relay.toml.j2` (`relay_quic_port`), not hand-written, and that `enable_quic_addr_discovery` defaults to **false** in the relay binary.
- Add the verification step: `cargo run -p athenaeum-core --example relay_check -- --paths` from a NATed machine must print `qad OK` per relay and exit 0.

**Acceptance:** someone provisioning a new relay from this doc alone produces one that passes `relay_check`.

---

### Task 6 (owner, after 1–4): verification pass

Not code — the measurements that close the audit.

- [ ] Start the app, confirm `reported endpoint address to hub` now carries a PUBLIC direct address (this is the end-to-end proof the QAD fix reaches the hub, not just the probe).
- [ ] Read the `transport traffic` line's `relay_home_change` delta over ~30 min: is the ~27 s home-relay ping-pong (`relay-ams ↔ relay2`) gone now that QAD gives a real latency signal? If it persists, it becomes its own investigation with data attached.
- [ ] **Real cross-NAT smoke** — two devices on genuinely different networks (not LAN, not WireGuard): send a batch, confirm the terminal line says `conn_path=direct` and `send_relay_bytes` stays near zero. This scenario has never once passed on this codebase; every "direct" path in the historical logs was an RFC1918 address.

---

## Task order

1 → 4 → 3 → 2 → 5. Task 1 first (it is what makes the rest debuggable), Task 4 before Task 3 (Task 3 consumes the aggregate health), Task 2 after both so its terminal lines land on a correct health surface, Task 5 any time.
