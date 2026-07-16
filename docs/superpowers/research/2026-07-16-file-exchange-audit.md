# File-Exchange Honest Audit — UX/UI, Exchange Technology, Testability

**Date:** 2026-07-16 · **HEAD:** `510d4454` on `0.5.0`
**Scope:** the exchange stack built across the last month's cycles — Stage I personal sync → Stage 1.5 hardening → mesh-model Phase 1 → Perseus agent → collab Stage II → iroh transport hardening → delivery-forever + transfer queue.
**Method:** three independent adversarial audits (UX/UI, distributed-systems architecture, test infrastructure), each grounded in code reads with file:line evidence, synthesized here. Full per-finding detail lives in the audit transcripts; this document keeps every finding's identity, severity, and fix direction.

## Verdict

**The core is sound; the edges are where users will bleed.** The engine/transport/state-machine work is genuinely strong — delivery-forever is real, ingest is transactional and replay-safe, ack peer-binding is enforced, the fan-out data-loss class is fixed with restart reconciliation, and the hardest invariants are pinned by tests. The three audits converge on the same shape of weakness: **diagnosability and lifecycle edges**. The flagship transfers screen doesn't show *why* a transfer stalled; the first-run path has no on-ramp and one silent data-location cliff; the wire protocol has no version signal so a future mismatch degrades into an undiagnosable backoff loop; cancelled/undeliverable payloads accumulate on disk forever with no purge lever; and the manual smoke burden grows each cycle because a handful of small automation knobs keep getting deferred.

Nothing found blocks the current push. Two items should land **before beta users touch this** (UX-1, UX-2); two architectural debts should land **before the next wire/lifecycle change** (TECH-1, TECH-2).

---

## A. UX/UI (10 findings)

| ID | Sev | Finding |
| ---- | ---- | ---- |
| UX-1 | **Blocker-for-beta** | Received files land in app-data when no incoming folder is set; the ONLY signal is a one-shot toast with a persisted dedupeKey (`useSyncStatus.ts:145-164`) — it fires once, ever. The "where did my files go" cliff. Fix: standing warning strip on `/transfers` + deep link to the folder designator; drop the localStorage dedupe for this condition. |
| UX-2 | Major | The torrent screen never renders `last_error` — it's plumbed into `TransferRow.lastError` and dropped (zero references in `src/components/transfers/`). A stalled row shows "stalled · attempt 3 · 0:47" with no reason, while Perseus's plain page DOES show it (`index.html:425-427`). Fix: render it (with a raw→plain map for the 3-4 common strings). |
| UX-3 | Major | Three surfaces, three "active" definitions; the sidebar ↓ is a **lifetime** received counter that reads as live downloads (`TransferIndicator.tsx:21-22`). Fix: one shared definition; relabel the lifetime stat. |
| UX-4 | Major | No first-run on-ramp: `/transfers` renders a barren table when signed out / no peers; Send lives only in File-Manager/Analysis toolbars; nothing links the setup steps. Fix: 3-variant empty state (sign in → add device → how to send). |
| UX-5 | Major | Vocabulary drift: node vs device vs peer vs target; fetching vs Downloading vs ingested across `/transfers`, ReceiveTab, Perseus. Fix: standardize on "device" + one receive-verb set. |
| UX-6 | Major | Collab downloads appear twice (global Active + ReceiveTab) with different words and no project label; "✓ delivered — safe to delete" + throughput exist in the slide-over history but are MISSING from the full-screen history it links to. Fix: port the helpers; add project chip to inbound rows. |
| UX-7 | Minor | Outbound bar shows fabricated stage percentages (2/8/50/95%) when bytes are absent; no ETA anywhere despite multi-GB payloads. Fix: indeterminate bar pre-transfer; ETA from EMA speed. |
| UX-8 | Minor | Rows not keyboard-operable; SendToNodeDialog lacks Esc/focus-trap (the panel has both). |
| UX-9 | Minor | Perseus page: 972-line flat wall of 8 equal sections, `window.prompt` bearer-token auth, dark-only hardcoded hex. Cheap wins: inline token field, `<details>` for rare sections, `prefers-color-scheme`. Full rewrite not warranted. |
| UX-10 | Polish | `formatBytes` duplicated ×4; two parallel History renderers (the root of UX-6 drift); EN-only strings (accepted non-goal). |

**Journey summary:** first-ever send = sign-in on both (fine) → Send buried in other views → receive-side app-data cliff. Day-long stall = blind (no reason shown) with a fake 8% bar. Mid-fetch cancel = works, but no disposition summary and brief dual-surface lag for collab.

## B. Exchange technology (7 findings)

| ID | Sev | Finding |
| ---- | ---- | ---- |
| TECH-1 | **Major** | **No wire-version negotiation.** Postcard positional encoding; evolution discipline is a comment ("indices frozen", `proto.rs:92`); `ReceiptOutcome::Cancelled` already shipped knowing old peers can't decode acks carrying it. Worse: a decode failure `break`s the whole per-connection accept loop (`iroh/mod.rs:1190-1195`) with no error to the peer — a version mismatch presents as a permanently-stalled transfer backing off to 30m, diagnosable only from the RECEIVER's logs. The `athenaeum/sync/1` ALPN suffix already exists but has never been bumped. Fix (M): version byte in the `Msg` envelope + treat the ALPN suffix as load-bearing + a golden-byte wire test that fails on variant reorder. |
| TECH-2 | **Major** | **Payload disk is reclaimed ONLY on Confirmed** (exactly 3 call sites of `cleanup_package_payloads`, all Confirmed-paths). Cancelled (both kinds, by design — retryable), forever-pending against a dead peer, and one-dead-target-in-a-fan-out all keep full byte-for-byte staged copies indefinitely; there is NO purge command anywhere. Perseus worst case quantified: ~10GB/night capture with one dead subscriber → staged-copy leak approaches doubling nightly disk growth, unbounded. Plus: a `write_package` crash mid-copy leaves a dir with no `sync_outbound` row — invisible to every cleanup path. Fix (M): "purge local copy" command for Cancelled/stalled rows (both backends, surfaced in Transfers UI) + an orphan-dir sweep against `sync_outbound.package_ref`. |
| TECH-3 | Minor/Watch | The dev-ticket toggle (a real Settings switch) opens an unsigned receiver fully: no connect gate, allow-all authorizer, AND the dedup-handshake becomes a content-existence oracle (Offer/Want answers whether your catalog holds matching frames) — a blast radius the original F5 residual didn't include. Fix (S): UI warning on the toggle. |
| TECH-4 | Minor | Complexity growth: `api/sync.rs` 2,863 lines as the sole orchestration seam; `Pending` at 15 fields mixing retry/dedup/collab concerns; `ensure_sender_engine` at 7 params; two `SyncStore` impls kept parallel by hand (this already bit: the Perseus test-mock compile break). Fix (M, opportunistic): `SyncOrchestrator` extraction + `PendingCore`/collab split at the next feature. |
| TECH-5 | Watch | iroh `=1.0.2`/`=0.103.0` pins are load-bearing: per-blob progress semantics, the event-mask quirk, pre-handshake auth shape are all coded against 0.103 specifics. Upgrades = grep for `0.103`/`quirk` comments first, each is a regression target. |
| TECH-6 | Watch | Reconnect `kick_all` burst is bounded to one device's own backlog (the hourly peers refresh does NOT kick — verified), but a laptop waking with hundreds of pending rows × several peers is unsoaked. Fix (S): a soak test with a large pending backlog. |
| TECH-7 | Watch | Cancel-epilogue manifest-only fetch has no application-level size cap (control channel has 16MB `MAX_CONTROL_BYTES`; manifests don't). Fix (S): sanity cap in `read_manifest`. |

**Debt map:** (1) `api/sync.rs` orchestration seam; (2) payload lifecycle hard-coded to Confirmed in 3 places — the next terminal-adjacent state re-opens it; (3) wire evolution held by author discipline alone.

## C. Testability (13 findings)

| ID | Sev | Finding |
| ---- | ---- | ---- |
| TEST-9 | Minor / **cheapest fix in the audit** | `fan_out_delivers_to_every_target` flake root-caused: `#[tokio::test]` defaults to current_thread; 2 engines + 2 receivers + mutex loopback starve one OS thread under load (~2/12). Fix: `flavor = "multi_thread"` — one line. |
| TEST-8 | Watch | The 3 kick tests bound confirmation at 800-900ms on current_thread — structurally tighter than the already-flaky test. Same one-line fix. |
| TEST-1 | Major | No deterministic ack-failure test; `FaultPlan` has no fail/drop-ack knob (`ack_lost_then_duplicate_ack_confirms_once` actually tests duplicate delivery, not loss). Re-ledgered across cycles. Fix (S): `drop_ack_once` knob. |
| TEST-2 | Major | No delay-fetch knob → mid-transfer live state unobservable; e2e asserts recorded trails instead. Fix (M): `delay_per_chunk` in the loopback copy loop. |
| TEST-3 | Major | `cancel_incoming_package` live-control leg never exercised through the real command chain (no `SyncRuntime` test seating). Fix (M). |
| TEST-4 | Major (procedural) | The July-12 field-failure class (relay eviction) is proven once per cycle by an `#[ignore]`d owner-run canary. Fix (S, process): fold into the release checklist. |
| TEST-5 | Major | Zero frontend test infra; this month's duplicate-key + stale-cache bug (`7498de78`) is exactly the class a thin vitest over `useTransferQueue` would catch. Fix (S-M). |
| TEST-11/12 | Minor | Two missing log fields (`delay_ms`/`next_retry_at` on the backoff info line; a "rebuild complete" line in `try_rebuild`) — adding them converts two watch-it-live smoke steps into one-line `query_logs` checks. Fix (S). |
| TEST-6 | Watch | "e2e replay downgraded" is actually resolved-by-composition (unit test + distinct log line) — relabel, don't fix. |
| TEST-10 | Minor | A Perseus negative-assertion test has ~1s margin and fails toward FALSE PASS on slow machines. Fix (S). |
| TEST-13 | Minor | No multi-instance dev harness; every live scenario = manual N-instance setup. Fix (M): `scripts/dev-mesh.sh` (isolated DB/port per instance vs test-hub, prints tickets). |
| TEST-7 | Minor | Verify whether the real-iroh upload-progress test asserts mid-stream ticks or final only (5-minute read). |

**Smoke burden:** ~63 discrete checklist items + ~20 ledger pendings. ~9 are genuinely real-network-only (keep manual); **~11 are automatable via four small knobs** that keep being deferred: `drop_ack_once`, `delay_per_chunk`, `SyncRuntime` test seating, a JS test runner. Burden grows cumulatively because real-network items are carried forward and re-bundled each cycle instead of retired.

---

## Cross-cutting synthesis

1. **Diagnosability is the #1 gap, appearing in all three audits.** UX-2 (no reason on screen) + TECH-1 (version mismatch indistinguishable from dead peer) + TEST-11/12 (missing log fields) are the same disease: the system retries heroically but explains itself poorly. Delivery-forever *raises* the bar here — when nothing ever fails terminally, "why is it stalled" becomes the only question that matters.
2. **Lifecycle edges beat happy paths.** TECH-2 (payload leak) + UX-6 (safe-to-delete hidden) + UX-1 (app-data cliff) are all about what happens *around* a transfer, not during it.
3. **Small deferred knobs compound.** Four S/M test-infra investments would retire ~11 recurring smoke items and the known flake; deferring them each cycle is why the smoke list grows.

## Prioritized roadmap

**Wave 1 — before beta users (all S):**
1. UX-1 standing app-data warning + deep link.
2. UX-2 render `last_error` on stalled/failed rows (+ raw→plain map).
3. TEST-9/8 `multi_thread` flavor on the 4 timing-tight tests (kills the flake).
4. TEST-11/12 two log fields (cheapens every future smoke).
5. TECH-1-lite: golden-byte wire test + documented ALPN-bump rule (the M-sized envelope version can follow).

**Wave 2 — next maintenance window (S/M):**
6. TECH-2 purge command for Cancelled/stalled + orphan-dir sweep (do together).
7. UX-4 first-run empty-state on-ramp; UX-3 unify Active counts.
8. TEST-1/2 FaultPlan knobs (`drop_ack_once`, `delay_per_chunk`) + the tests they unblock.
9. UX-5/6 vocabulary pass + safe-to-delete/throughput in full-screen History + project chips.

**Wave 3 — opportunistic (M):**
10. vitest harness scoped to `useTransferQueue`; `scripts/dev-mesh.sh`.
11. TECH-4 `SyncOrchestrator`/`Pending` split — at the next sync feature, not before.
12. TECH-6 reconnect-burst soak; TECH-7 manifest cap; UX-7 ETA + honest bars; UX-8 keyboard; UX-9 Perseus cheap wins.

**Accepted / explicitly not doing:** bandwidth caps, pause/resume, queue prioritization, global speed aggregates (wrong product for them); i18n (solo-beta non-goal); Perseus page rewrite (operator tool, cheap wins only).
