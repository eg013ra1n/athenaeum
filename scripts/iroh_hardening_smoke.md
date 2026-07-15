# iroh Transport Hardening — Owner Smoke Checklist (2026-07-15)

Acceptance cases for the hardening cycle (`e3353d59..af72aa1c` on `0.5.0`; hub
branches `device-addresses` + `collab-portal`). Prereqs: `git pull` + rebuild
dev builds on every machine; hub deploys done (prod ← device-addresses,
test-hub ← collab-portal). Log lines referenced are `info!`-level defaults.

## A — Single node basics (one machine)

- [ ] A1 `cargo run -p athenaeum-core --example relay_check` (device key, app
      closed) → all four prod relays `OK … in ~3s`.
- [ ] A2 C1 canary: `ATHENAEUM_TEST_RELAY=https://test-relay.artfrom.space:8443
      cargo test -p athenaeum-core same_key_second_endpoint_evicts -- --ignored`
      → PASSES (second endpoint with the same key evicts the first, observed).
- [ ] A3 App + Perseus concurrently on one machine (their own data dirs) →
      logs show ONE `iroh endpoint relay configuration` per process and ZERO
      `same endpoint id` relay warnings during transfers.
- [ ] A4 Copied-key guard: point a second app copy at a COPY of a sync dir
      (same `device_key`) → it must fail at startup with
      "device key is in use by another process…".
- [ ] A5 Quit the app (or Ctrl-C `athenaeum-web`) →
      `shared iroh node shut down` in the log; relaunch works (lock released).

## B — Personal sync across NAT (two machines, production hub)

- [ ] B1 Send a batch A→B (different NATs) → transfer completes; sender log
      shows `connection path established … conn_type=relay` and ideally a
      `connection path changed` upgrade to `direct` (staying on relay is OK —
      completion is the gate).
- [ ] B2 Each side's log shows `home relay connected relay_url=…` with a
      sensible (nearest) relay of the four.
- [ ] B3 After hub deploy + app restart on both: device rows carry
      `endpointAddr` (ask the controller to verify via psql, or GET /devices);
      a fresh send dials the PEER's reported relay (visible in the dial-hint
      debug logs / just: cross-relay transfer completes).
- [ ] B4 Send to a powered-off machine → attempts climb, `last_error` visible
      (Perseus web page / logs); power it on → delivery completes via retries.

## C — Perseus (observatory)

- [ ] C1 Update to a 0.5.0 build; multi-target batch send (≥2 destinations)
      → both receive; ONE node id in the logs (not one per target).
- [ ] C2 Relay-map change on the hub → within ≤1h the log shows
      `relay map changed; node rebuild pending` then a rebuild while idle
      (or on restart, immediately).

## D — Collaboration (test-hub, cross-NAT via test-relay)

- [ ] D1 The deferred slice-4 live smoke: publish → moderation → download
      between two machines on test-hub — now across NATs (test-relay in play).
- [ ] D2 Holder lists show `relayUrl` and NEVER direct addresses
      (controller can verify with an authenticated curl).
- [ ] D3 Slice-5 payoff: project WBPP export of the downloaded contributions.

Expected failure signatures that would indicate a regression: any
`same endpoint id` warning during A3/B1/C1; a transfer that times out with
both peers online and relays reachable; a second process silently starting
on a copied key (A4 must be LOUD).
