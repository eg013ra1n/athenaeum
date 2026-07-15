# Sync Delivery-Forever + Transfer Queue Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Rust tasks → `rust-engineer` subagents; Task 15 (React page) → `frontend-dev` subagent.

**Goal:** Outbound sync packages retry forever (capped backoff + event kicks) instead of dying after 5 attempts; both sides can user-cancel; receivers refresh their authorized-peer list unattended; and a torrent-style `/transfers` screen (desktop) + Perseus web additions surface batches, per-file receive progress, and retry controls.

**Architecture:** All semantics changes live in `athenaeum-core/src/sync/` (engine scheduling, store columns, receiver inbound rows) and `sharing/` (a `FetchSink` progress callback into `fetch`, provider upload events, one additive wire value `ReceiptOutcome::Cancelled`). Tauri commands and Axum routes are thin mirrors added in the same task. Frontend is one new page reusing the existing `TransfersContext`.

**Tech Stack:** Rust (tokio, rusqlite, iroh-blobs =0.103.0), Tauri 2 + Axum mirrors, React/TS + Tailwind (tokens are already Nord), ts_rs-generated TS models.

**Spec:** `docs/superpowers/specs/2026-07-15-sync-delivery-queue-design.md` (all §-references below point there).

## Global Constraints

- Branch `0.5.0`. Commit as the user (`eg013ra1n` / `vilen.sharifov@gmail.com`), **no AI co-author lines**.
- **Two backends in sync**: every new/changed Tauri command in `crates/athenaeum-tauri/src/commands/sync.rs` gets its Axum mirror in `crates/athenaeum-web/src/routes/sync.rs` **in the same task**, registered in `lib.rs` `invoke_handler` + `routes/mod.rs` `build_router`.
- Command boundary: `#[tracing::instrument(skip_all, err)]` on every new command/route. Never swallow errors. Log style: `info!(package_id, "short phrase")` — data in snake_case fields, never interpolated.
- `anyhow::Result` inside core; `.map_err(|e| e.to_string())` at the Tauri boundary; `api_err` at the Axum boundary.
- **TS models are generated**: change the Rust `ts_rs::TS` structs, register new types in `crates/athenaeum-core/src/ts_export.rs`, regenerate with `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`. Never hand-edit `src/types/models.ts`.
- Wire protocol: the ONLY wire delta in this cycle is the additive `ReceiptOutcome::Cancelled` variant (Task 4). Announce/fetch/ack shapes stay byte-identical.
- Design tokens only (`bg-surface`, `text-content-muted`, `bg-accent`, `text-error`, …) — they are already the Nord palette (`tailwind.config.js:12-58`). No raw hex in components.
- Zero `println!`/`eprintln!` in production code.
- Gates per task: `cargo build --workspace` (or the touched crates), `cargo test -p athenaeum-core` (+ `-p perseus` when touched), `npx tsc --noEmit` when TS touched. `clippy -D warnings` is NOT a gate (pre-existing debt). `cargo fmt` only via `rustfmt <files>` (scoped).
- Line numbers cited below were read on 2026-07-15; re-locate by symbol name if drifted.

---

### Task 1: Backoff schedule + unlimited retries (engine core)

**Files:**
- Modify: `crates/athenaeum-core/src/sync/engine.rs` (constants :72-82, `SyncConfig` :84-100, `Pending` :121-163, run-loop select :584-638, `attempt` :748-877, `arm_retry` :1043-1047, `handle_timeouts` :1258-1317)
- Modify: `crates/athenaeum-core/src/sync/mod.rs:73-75` (re-exports)
- Modify: `crates/perseus/src/` — any use of `SyncConfig { max_attempts, .. }` / `MAX_ATTEMPTS` (grep; fix compile)
- Test: `crates/athenaeum-core/src/sync/engine_tests.rs` (rewrite :517 `failed_after_max_attempts_with_error_outcome_in_history`, :575 `first_attempt_peer_offline_retries_then_fails`; add backoff tests)

**Interfaces:**
- Consumes: existing `Pending`, `SyncStore::bump_attempts`.
- Produces: `pub fn retry_backoff(base: Duration, rung: u32) -> Duration`; `SyncConfig { ack_timeout: Duration }` (field `max_attempts` REMOVED, `MAX_ATTEMPTS` const removed); `Pending` fields `rung: u32`, `next_action: NextAction`; `enum NextAction { AwaitAck, Retry }`. Tasks 2/5 build on `rung`/`next_action`.

- [ ] **Step 1: Write failing unit tests** in `engine_tests.rs`:

```rust
#[test]
fn backoff_schedule_multiplies_base_and_caps() {
    use std::time::Duration;
    let base = Duration::from_secs(30);
    // 30s → 1m → 5m → 15m → 30m → 30m (cap), spec §2
    assert_eq!(retry_backoff(base, 0), Duration::from_secs(30));
    assert_eq!(retry_backoff(base, 1), Duration::from_secs(60));
    assert_eq!(retry_backoff(base, 2), Duration::from_secs(300));
    assert_eq!(retry_backoff(base, 3), Duration::from_secs(900));
    assert_eq!(retry_backoff(base, 4), Duration::from_secs(1800));
    assert_eq!(retry_backoff(base, 99), Duration::from_secs(1800));
}
```

Import `retry_backoff` alongside the existing `super::*`-style imports at the top of `engine_tests.rs`.

- [ ] **Step 2: Run** `cargo test -p athenaeum-core backoff_schedule -- --nocapture` — FAIL (`retry_backoff` not found).

- [ ] **Step 3: Implement the schedule + state machine.** In `engine.rs`:

```rust
/// Spec §2: capped exponential backoff, expressed as multiples of the base
/// rung (ack_timeout) so tests with short timeouts scale down naturally.
/// 30s → 1m → 5m → 15m → 30m with the default 30s base.
const BACKOFF_MULTIPLIERS: [u32; 5] = [1, 2, 10, 30, 60];

pub fn retry_backoff(base: Duration, rung: u32) -> Duration {
    let m = BACKOFF_MULTIPLIERS[(rung as usize).min(BACKOFF_MULTIPLIERS.len() - 1)];
    base * m
}

/// What the per-package deadline means when it fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NextAction {
    /// Announce succeeded; deadline = ack wait. Firing = ack timed out.
    AwaitAck,
    /// Waiting out a backoff window. Firing = attempt the announce now.
    Retry,
}
```

Changes, in order:
1. Delete `pub const MAX_ATTEMPTS: u32 = 5;` (engine.rs:75) and the `max_attempts` field + `Default` arm from `SyncConfig` (:84-100). Fix `sync/mod.rs:73-75` re-exports. Grep the workspace for `MAX_ATTEMPTS` / `max_attempts` (engine_tests.rs:405-412, :529-536 build `SyncConfig` literals; Perseus may too) and fix each.
2. `Pending` gains `rung: u32` (init 0) and `next_action: NextAction` (init `Retry` — a fresh enqueue's first deadline is an immediate attempt, matching today's flow).
3. `handle_timeouts` (:1258): delete the fuse (`if attempts >= self.config.max_attempts { self.fail_package(id)?; }`) and the immediate re-`attempt`. New body per due id:

```rust
for id in due {
    let Some(p) = self.pending.get_mut(&id) else { continue };
    match p.next_action {
        NextAction::AwaitAck => {
            // Ack timed out → record, climb one rung, wait it out. Never terminal.
            let _ = self.store.set_last_error(id, Some("no ack from peer within timeout"));
            p.rung = p.rung.saturating_add(1);
            p.next_action = NextAction::Retry;
            p.deadline = Instant::now() + retry_backoff(self.config.ack_timeout, p.rung);
            tracing::info!(package_id = id, rung = p.rung, "ack timeout, backing off");
        }
        NextAction::Retry => {
            if !refreshed { /* keep the existing T8 addr-refresh block (:1292-1303) here */ }
            if let Err(e) = self.attempt(id).await {
                tracing::warn!(package_id = id, error = %e, "attempt errored, backing off");
                self.arm_retry(id);
            }
        }
    }
}
```

4. `attempt` (:748): at entry, `let attempts = self.store.bump_attempts(id)?;` (attempts now means "announce attempts made"; delete the bump from `handle_timeouts`). On announce success (:868) set `p.next_action = NextAction::AwaitAck; p.deadline = Instant::now() + self.config.ack_timeout;`. The existing announce-failure path (:788-825) keeps calling `arm_retry`.
5. `arm_retry` (:1043) becomes the backoff arm:

```rust
fn arm_retry(&mut self, id: i64) {
    if let Some(p) = self.pending.get_mut(&id) {
        p.rung = p.rung.saturating_add(1);
        p.next_action = NextAction::Retry;
        p.deadline = Instant::now() + retry_backoff(self.config.ack_timeout, p.rung);
    }
}
```

6. `fail_package` (:1324) survives untouched but is now reachable ONLY from local-unrecoverable call sites (package dir missing / payload gone — the existing serve-error branch that detects a missing package dir). Verify by grepping `fail_package(` call sites: the `handle_timeouts` one is gone; any remaining caller must be a payload-missing branch. If a remaining caller is network-conditional, route it to `arm_retry` instead.

- [ ] **Step 4: Rewrite the two contract tests.**
  - `failed_after_max_attempts_with_error_outcome_in_history` (:517) → rename `timeouts_back_off_forever_without_failing`: same offline-receiver setup with `SyncConfig { ack_timeout: Duration::from_millis(50) }`; drive past 5+ deadline fires (use the existing `wait_until` helper on `store.non_terminal()` attempts count); assert the row is STILL non-terminal, `attempts >= 5`, `last_error` set, and NO `failed` history row exists.
  - `first_attempt_peer_offline_retries_then_fails` (:575) → rename `peer_offline_backs_off_and_stays_pending`: assert state remains `Queued`/`Announced` (never `Failed`) after several backoff windows.
  - Keep `first_attempt_peer_offline_then_online_completes` (:636) green — it is the §6 acceptance shape at unit scale.

- [ ] **Step 5: Run** `cargo test -p athenaeum-core sync` — all engine tests PASS. `cargo build --workspace` — compiles (Perseus fixed).

- [ ] **Step 6: Commit** `feat(sync): unlimited retries with capped exponential backoff, no terminal fail from timeouts`

---

### Task 2: `next_retry_at` persistence

**Files:**
- Modify: `crates/athenaeum-core/src/sync/store.rs` (`ensure_outbound_columns` :46-59, `OUTBOUND_COLS` :278, trait :165-208, both impls; row mapping)
- Modify: `crates/athenaeum-core/src/sync/models.rs` (`OutboundRow` :118-135)
- Modify: `crates/athenaeum-core/src/db/schema.rs` (sync block :1648-1671 — same guarded ALTER for the catalog DB)
- Modify: `crates/athenaeum-core/src/sync/engine.rs` (write-through on every deadline arm; restart)
- Test: `crates/athenaeum-core/src/sync/engine_tests.rs`, store tests in `store.rs`

**Interfaces:**
- Produces: `OutboundRow.next_retry_at: Option<String>` (RFC3339 UTC, `chrono::Utc::now()` formatted like `created_at` is); `SyncStore::set_next_retry_at(&self, id: i64, at: Option<&str>) -> Result<()>`. Tasks 9/14 read the field.

- [ ] **Step 1: Failing store test** (module test in `store.rs`, follow the existing store-test style):

```rust
#[test]
fn next_retry_at_roundtrips_and_clears() {
    let tmp = tempfile::tempdir().unwrap();
    let store = StandaloneSyncStore::open(tmp.path().join("s.db")).unwrap();
    let id = store.enqueue("/tmp/pkg", [7u8; 32]).unwrap();
    store.set_next_retry_at(id, Some("2026-07-15T12:00:00Z")).unwrap();
    let row = store.non_terminal().unwrap().pop().unwrap();
    assert_eq!(row.next_retry_at.as_deref(), Some("2026-07-15T12:00:00Z"));
    store.set_next_retry_at(id, None).unwrap();
    assert!(store.non_terminal().unwrap().pop().unwrap().next_retry_at.is_none());
}
```

- [ ] **Step 2: Run it** — FAIL (no method/field).

- [ ] **Step 3: Implement.** Extend `ensure_outbound_columns` to a loop over `[("last_error", "TEXT"), ("next_retry_at", "TEXT")]` using the same PRAGMA scan; add the same two-column guard to `db/schema.rs:1648-1671` (it uses `column_exists(conn, "sync_outbound", ...)`). Append `next_retry_at` to `OUTBOUND_COLS`, `OutboundRow`, both impls' row mappers, and implement `set_next_retry_at` in both stores (`UPDATE sync_outbound SET next_retry_at = ?1 WHERE id = ?2`).

- [ ] **Step 4: Engine write-through.** Wherever Task 1 sets a `Retry` deadline (`arm_retry`, the `AwaitAck→Retry` transition), also persist the wall-clock deadline: compute `chrono::Utc::now() + chrono::Duration::from_std(delay).unwrap_or_default()`, format it exactly like `created_at` is formatted in `store.rs::enqueue`, and `let _ = self.store.set_next_retry_at(id, Some(&stamp));` (best-effort — a failed persist must not break scheduling). On announce success (`AwaitAck` arm) and on every terminal transition, clear it (`set_next_retry_at(id, None)`). On engine startup re-announce (the existing pending re-load in `spawn_inner`/worker init), treat restart as a wake event (spec §2): `rung = 0`, `next_action = Retry`, `deadline = Instant::now()` — immediate attempt; persisted `next_retry_at` is informational for the UI, the restart attempt overwrites it.

- [ ] **Step 5: Engine test** — extend `timeouts_back_off_forever_without_failing`: after the first ack-timeout, `store.non_terminal()` row has `next_retry_at = Some(..)` parseable and in the future; after the receiver comes online and the package confirms, `next_retry_at` is `None`.

- [ ] **Step 6: Run** `cargo test -p athenaeum-core sync` — PASS. **Commit** `feat(sync): persist next_retry_at for retry countdowns and honest restarts`

---

### Task 3: `OutboundState::Cancelled`

**Files:**
- Modify: `crates/athenaeum-core/src/sync/models.rs` (enum :24-46, `as_str`/`from_db` :50-73, `is_terminal` :77-79)
- Modify: `crates/athenaeum-core/src/sync/store.rs` (`non_terminal` SQL :738 + catalog twin :936; `confirmed()` and any state-counting SQL — grep `'failed'` in store.rs)
- Modify: `crates/athenaeum-core/src/sync/engine.rs` (`cancel_package` :1363-1410)
- Modify: `crates/athenaeum-core/src/api/sync.rs` (`build_sender_status` :752-812 — `failed_total` counting)
- Test: `engine_tests.rs` (:711 `cancel_moves_to_failed_cancelled`)

**Interfaces:**
- Produces: `OutboundState::Cancelled` (DB text `"cancelled"`, terminal). Consumed by Tasks 4/8/9/14. TS regen happens in Task 14 (one regen for all model changes); this task only keeps `cargo build` green.

- [ ] **Step 1: Rewrite the cancel test first.** Rename :711 to `cancel_moves_to_cancelled_state`; assert `row.state == OutboundState::Cancelled` (load via a store query on all rows — add a tiny test-only helper if needed), history outcome stays `"cancelled"`, `sync-finished` outcome `"cancelled"`. Run — FAIL.

- [ ] **Step 2: Implement.** Add the variant (serde camelCase gives `"cancelled"` in TS); `as_str` → `"cancelled"`, `from_db` arm, `is_terminal` → `matches!(self, Confirmed | Failed | Cancelled)`. SQL exclusion sets: `state NOT IN ('confirmed', 'failed', 'cancelled')` in BOTH stores' `non_terminal`. `cancel_package`: `self.store.set_state(id, OutboundState::Cancelled)` (was `Failed` at :1390) and clear `next_retry_at`. In `build_sender_status`, count cancelled rows separately or fold into `failed_total` — add `cancelled_total: u32` to `SyncSenderStatus` (spec-consistent; register in Task 14 regen).

- [ ] **Step 3: Run** `cargo test -p athenaeum-core sync && cargo build --workspace` — PASS (grep Perseus for exhaustive `OutboundState` matches and fix: `to_sent_dto` web.rs:1490 uses `as_str`-style so likely fine; `api_retry` eligibility is Task 9).

- [ ] **Step 4: Commit** `feat(sync): first-class Cancelled outbound state`

---

### Task 4: `ReceiptOutcome::Cancelled` + sender handling of a cancelled ack

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/types.rs` (:42 enum)
- Modify: `crates/athenaeum-core/src/sync/store.rs` (`receipt_outcome_to_db` :515, `receipt_outcome_from_db` :526; verify `count_satisfied_receipts` :580)
- Modify: `crates/athenaeum-core/src/sync/engine.rs` (the `AckReceived` handler — the path that calls `store.confirm(id, receipts)` and emits at :1028)
- Modify: `crates/athenaeum-core/src/sync/ingest.rs` (receipt→history outcome mapping — grep `Rejected` mapping to reuse for `Cancelled` → `"cancelled"`)
- Test: `engine_tests.rs`, store module tests

**Interfaces:**
- Consumes: `OutboundState::Cancelled` (Task 3).
- Produces: `ReceiptOutcome::Cancelled` (DB text `"cancelled"`); sender contract: an ack whose receipts are non-empty and ALL `Cancelled` → outbound row `Cancelled`, `last_error = "cancelled by receiver"`, per-frame history outcome `"cancelled"`, `sync-finished` outcome `"cancelled"`. Task 12 (receiver cancel) produces such acks.

- [ ] **Step 1: Failing store test:** `receipt_outcome_cancelled_roundtrips_and_counts_satisfied` — insert a `Cancelled` receipt via `insert_receipt`, `load_receipts` returns it intact, `count_satisfied_receipts` counts it (the `outcome NOT LIKE 'rejected:%'` guard at :580 must treat `cancelled` as satisfied so the §4 replay fires — assert count == 1).

- [ ] **Step 2: Failing engine test:** `all_cancelled_ack_marks_cancelled_by_receiver` — loopback pair where the test receiver (adapt the `spawn_receiver` helper at engine_tests.rs:88-127) acks every frame with `ReceiptOutcome::Cancelled`; assert row state `Cancelled`, `last_error == Some("cancelled by receiver")`, history rows outcome `"cancelled"`, finished event outcome `"cancelled"`. Run both — FAIL.

- [ ] **Step 3: Implement.** Enum variant `Cancelled` (unit variant, wire-serialized — this is the cycle's single wire value, spec §5). `receipt_outcome_to_db` → `"cancelled"`; `from_db` arm. In the engine's `AckReceived` handling, before the confirm call:

```rust
let all_cancelled = !receipts.is_empty()
    && receipts.iter().all(|r| matches!(r.outcome, ReceiptOutcome::Cancelled));
if all_cancelled {
    self.store.set_state(id, OutboundState::Cancelled)?;
    let _ = self.store.set_last_error(id, Some("cancelled by receiver"));
    let _ = self.store.set_next_retry_at(id, None);
    // per-frame history via the existing receipts→history path with outcome "cancelled",
    // then the same terminal epilogue cancel_package uses (release blobs, cleanup_sink,
    // remove pending slot), and emit_finished(id, "cancelled", …).
    ...
    return Ok(());
}
```

Mixed receipts (some cancelled, some ingested/rejected) go through the normal confirm path; the cancelled frames' history outcome maps to `"cancelled"`.

- [ ] **Step 4: Run** `cargo test -p athenaeum-core sync` — PASS. **Commit** `feat(sync): ReceiptOutcome::Cancelled wire value; cancelled ack terminates outbound as cancelled-by-receiver`

---

### Task 5: Kick API (send-now)

**Files:**
- Modify: `crates/athenaeum-core/src/sync/engine.rs` (`Command` :102-115, run-loop select :584-638, handle :422-475)
- Modify: `crates/athenaeum-core/src/sync/sender.rs` (`SyncSenderRuntime`)
- Test: `engine_tests.rs`

**Interfaces:**
- Consumes: `rung`/`next_action` (Task 1).
- Produces: `SyncEngineHandle::kick(&self, id: i64) -> Result<()>`; `SyncEngineHandle::kick_all(&self) -> Result<()>`; `SyncSenderRuntime::kick_all(&self)` (fire-and-forget over every started engine). Tasks 6/8/9 call these.

- [ ] **Step 1: Failing test** `kick_fires_immediate_attempt_and_resets_backoff`: offline receiver, short `ack_timeout`, wait until `attempts >= 2` (backoff climbing); bring the receiver online; call `engine.kick(id).await` (or `kick_all`); assert confirmation arrives well before the next scheduled backoff window (e.g. within 2×`ack_timeout`), proving the deadline collapsed to "now".

- [ ] **Step 2: Implement.** `Command::Kick(i64)` + `Command::KickAll`; select arm:

```rust
Command::Kick(id) => {
    if let Some(p) = self.pending.get_mut(&id) {
        p.rung = 0;
        p.next_action = NextAction::Retry;
        p.deadline = Instant::now();
        let _ = self.store.set_next_retry_at(id, None);
        tracing::info!(package_id = id, "kick: immediate retry");
    }
}
Command::KickAll => { /* same reset for every pending entry */ }
```

(The loop re-computes `next_deadline()` each iteration, so the collapsed deadline fires on the next pass — no extra wakeup needed.) Handle methods mirror `cancel` (:452). `SyncSenderRuntime::kick_all`: lock `inner`, for each `StartedSender` spawn `engine.kick_all()` (log-and-continue on error — never block the caller).

- [ ] **Step 3: Run** `cargo test -p athenaeum-core sync` — PASS. **Commit** `feat(sync): kick API — immediate out-of-band retry with backoff reset`

---

### Task 6: Wake-event wiring (relay reconnect / relay change → kick_all)

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/iroh/node.rs` (`spawn_home_relay_watcher` :1772-1795, relay-refresh loop :947-986, node fields :473-ish)
- Modify: `crates/athenaeum-core/src/api/sync.rs` (the three `start_node_relay_refresh` call sites :616, :670, :982 — install the hook alongside)
- Test: node-level unit is impractical; covered by a handle-level test + Task 16 e2e

**Interfaces:**
- Consumes: `SyncSenderRuntime::kick_all` (Task 5).
- Produces: `SharedIrohNode::set_wake_hook(&self, hook: Arc<dyn Fn() + Send + Sync>)` — invoked on (a) home-relay reconnect (the watcher's connected transition, node.rs:1785) and (b) after a relay-map change is applied in the hourly refresh loop. Task 7 rides the same loop.

- [ ] **Step 1: Implement the hook.** Node stores `wake_hook: std::sync::RwLock<Option<Arc<dyn Fn() + Send + Sync>>>`. `set_wake_hook` writes it. Call sites: in `spawn_home_relay_watcher`, at the existing `info!("home relay connected")` transition, `if let Some(h) = hook.read().ok().and_then(|g| g.clone()) { h(); }` (clone the lock handle into the task at spawn — pass the node's `Arc` or the hook lock in). In the refresh loop, after `node.consider_relay_change(mode, urls)` actually changes the map (it returns/logs a changed signal — if it returns nothing, add a `bool` return), fire the hook.

- [ ] **Step 2: Install from api layer.** In `api/sync.rs`, next to each `start_node_relay_refresh(ctx, node)` call site, add:

```rust
let sender = sync_sender.clone();
let collab = collab_sender.clone(); // where in scope; skip if the site has no collab runtime
node.set_wake_hook(Arc::new(move || {
    let s = sender.clone();
    let c = collab.clone();
    tokio::spawn(async move { s.kick_all().await; c.kick_all().await; });
}));
```

Check what each of the three sites has in scope (`autostart_if_enabled`, `get_pairing_ticket`, the sender-first bind ~:982); thread the runtimes through if one lacks them — they all originate from the host state (`AppState.sync_sender`/`collab_sender`, web `WebAppState` twins).

- [ ] **Step 3: Handle-level test** `kick_all_resets_every_pending_package` in `engine_tests.rs`: two enqueued packages against an offline peer, both mid-backoff; `engine.kick_all()`; receiver online; both confirm promptly.

- [ ] **Step 4: Run** `cargo test -p athenaeum-core sync && cargo build --workspace`. **Commit** `feat(sync): relay reconnect and relay-map change kick all pending packages`

---

### Task 7: Authorized-peers refresh (periodic + refusal-debounced)

**Files:**
- Create: `crates/athenaeum-core/src/sync/refusal.rs` (pure debouncer; declare in `sync/mod.rs`)
- Modify: `crates/athenaeum-core/src/api/sync.rs` (`peer_authorizer` :190-206, `connect_gate` :221-235, `refresh_authorized_peers` :349-380, startup sites :616/:670/:982)
- Modify: `crates/athenaeum-core/src/sync/receiver.rs` (only if the authorizer closure signature needs the trigger threaded — prefer capturing it inside the closure built in `api/sync.rs`)
- Test: `refusal.rs` module tests

**Interfaces:**
- Produces: `pub struct RefusalRefresher { min_gap: Duration, last: std::sync::Mutex<Option<std::time::Instant>> }` with `pub fn new(min_gap: Duration) -> Self` and `pub fn should_fire(&self) -> bool` (atomically checks the gap and, when firing, stamps `last`). `SyncRuntime` (receiver.rs:733) gains `peers_refresh_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>` and `refusal: Arc<RefusalRefresher>` so timers install once per process.

- [ ] **Step 1: Failing debounce tests** (in `refusal.rs`):

```rust
#[test]
fn fires_once_per_gap() {
    let r = RefusalRefresher::new(Duration::from_millis(50));
    assert!(r.should_fire());
    assert!(!r.should_fire());
    std::thread::sleep(Duration::from_millis(60));
    assert!(r.should_fire());
}
```

- [ ] **Step 2: Implement** the struct (plain `Mutex<Option<Instant>>` compare-and-stamp; ~20 lines). Run — PASS.

- [ ] **Step 3: Periodic refresh.** In `api/sync.rs`, add `fn ensure_peers_refresh_task(ctx: &Arc<ServiceContext>, sync: &Arc<SyncRuntime>)`: under `sync.peers_refresh_task.lock()`, if `None`, spawn a `tokio::time::interval(Duration::from_secs(3600))` loop calling `refresh_authorized_peers(&ctx)` (it is already best-effort: failure keeps the cached list and `warn!`s — verify and keep). Call `ensure_peers_refresh_task` at the same three startup sites as `start_node_relay_refresh`. Constant `PEERS_REFRESH_INTERVAL: Duration = Duration::from_secs(3600)` next to the call.

- [ ] **Step 4: Refusal trigger.** In BOTH `peer_authorizer` (the `false`-returning misses) and the `connect_gate` closure: on refusing an unknown peer,

```rust
if refusal.should_fire() {
    let ctx = ctx.clone();
    tracing::info!(peer = %hex, "unknown peer refused; refreshing authorized set");
    tokio::spawn(async move { let _ = refresh_authorized_peers(&ctx).await; });
}
```

with `refusal: Arc<RefusalRefresher>` = `sync.refusal` (constructed `RefusalRefresher::new(Duration::from_secs(300))` — the spec's 5-minute debounce). No callback into the refused peer is needed: its own retry loop (§2) redelivers.

- [ ] **Step 5: Run** `cargo test -p athenaeum-core sync && cargo build --workspace`. **Commit** `feat(sync): hourly + refusal-triggered authorized-peers refresh`

---

### Task 8: Desktop commands — retry / send-now / cancel outbound (both backends)

**Files:**
- Modify: `crates/athenaeum-core/src/sync/store.rs` (trait: add `get_outbound`)
- Modify: `crates/athenaeum-core/src/api/sync.rs` (three new api fns)
- Modify: `crates/athenaeum-tauri/src/commands/sync.rs` + `crates/athenaeum-tauri/src/lib.rs:447-453`
- Modify: `crates/athenaeum-web/src/routes/sync.rs` + `crates/athenaeum-web/src/routes/mod.rs:250-257`
- Test: api-level tests in `api/sync.rs` test module (or `engine_tests.rs` where a live engine is needed)

**Interfaces:**
- Consumes: `kick` (Task 5), `Cancelled` (Task 3), Perseus `api_retry` shape (web.rs:1401-1486) as the model.
- Produces: `SyncStore::get_outbound(&self, id: i64) -> Result<Option<OutboundRow>>` (promote the existing Perseus-side helper into the trait; implement in both stores). Core api fns (all `pub async`, in `api/sync.rs`):
  - `retry_sync_package(ctx: &ServiceContext, sync_sender: &Arc<SyncSenderRuntime>, id: i64) -> Result<i64, ApiError>` — row must be terminal (`Failed | Cancelled`), payload dir must exist (reuse/port `package_has_payload`), then `engine.enqueue_package(dir)` → returns the NEW row id.
  - `send_now_sync_package(sync_sender: &Arc<SyncSenderRuntime>, id: i64) -> Result<(), ApiError>` — resolve the engine owning `id` (iterate started engines' `status_snapshot()` for the row, or match by peer from `get_outbound`) → `engine.kick(id)`.
  - `cancel_sync_package(sync_sender: &Arc<SyncSenderRuntime>, id: i64) -> Result<(), ApiError>` — same resolution → `engine.cancel(id)`.
- Tauri commands `retry_sync_package`, `send_now_sync_package`, `cancel_sync_package` (each `{ id: i64 }` → thin wrapper over the api fn, `State<AppState>` gives `state.ctx`/`state.sync_sender`); Axum mirrors at `/api/retry_sync_package`, `/api/send_now_sync_package`, `/api/cancel_sync_package` with `#[serde(rename_all = "camelCase")] struct IdArgs { id: i64 }`.

- [ ] **Step 1: Failing api test** `retry_reenqueues_terminal_package_as_new_row`: enqueue → cancel → `retry_sync_package` returns a new id ≠ old; old row stays `Cancelled`; new row pending. And `retry_rejects_pending_package` (ApiError on a non-terminal row).

- [ ] **Step 2: Implement** trait method + api fns + both host wrappers (pattern: every existing command in `commands/sync.rs` / `routes/sync.rs`; same `#[tracing::instrument(skip_all, err)]`). Register in `invoke_handler` and `build_router`. Engine resolution helper (private in `api/sync.rs`):

```rust
async fn engine_for_row(sync_sender: &Arc<SyncSenderRuntime>, id: i64)
    -> Result<Arc<SyncEngineHandle>, ApiError> {
    for peer in sync_sender.started_peers().await {
        if let Some((engine, _)) = sync_sender.current_for(&peer).await {
            if engine.status_snapshot()?.iter().any(|r| r.id == id) { return Ok(engine); }
            if engine.store_get_outbound(id)?.is_some() { return Ok(engine); } // terminal rows
        }
    }
    Err(ApiError::bad_request("unknown package id"))
}
```

(Expose `store_get_outbound` on the handle as a thin `self.store.get_outbound(id)`; the app's engines share one `CatalogSyncStore`, so the first started engine finds terminal rows — retry must then enqueue on the engine whose `peer` matches `row.peer`, creating it via the existing `ensure_sender_engine` path (api/sync.rs:950) if the peer has no engine yet.)

- [ ] **Step 3: Run** `cargo test -p athenaeum-core && cargo build --workspace`. **Commit** `feat(sync): desktop retry/send-now/cancel commands for outbound packages (both backends)`

---

### Task 9: Perseus Part-A surface

**Files:**
- Modify: `crates/perseus/src/web.rs` (`api_retry` eligibility :1445, `SentDto` :337-359, `to_sent_dto` :1490-1502, `build_router` :384-432, new handlers)
- Modify: `crates/perseus/src/web/index.html` (Sent table render :372-400, listeners :395-397, tick :897-898)
- Test: `crates/perseus/` web tests (follow existing `api_retry` test pattern if present; else handler-level tests)

**Interfaces:**
- Consumes: `OutboundState::Cancelled` (Task 3), `kick` (Task 5), `OutboundRow.next_retry_at` (Task 2).
- Produces: `POST /api/kick { ids: [i64] }` → per-id `engine.kick(id)`; `POST /api/cancel { ids: [i64] }` → per-id `engine.cancel(id)`; both return the `RetryReport`-style `{ done: [...], rejected: [{id, reason}] }` shape. `SentDto` gains `next_retry_at: Option<String>`, `byte_size: u64` (sum of file sizes under `package_ref`, computed where `files` is listed in `to_sent_dto`).

- [ ] **Step 1: Broaden retry eligibility.** web.rs:1445: `if !matches!(row.state, OutboundState::Failed | OutboundState::Cancelled) { …"not terminal"… continue; }`.

- [ ] **Step 2: Add `api_kick` / `api_cancel`** modeled on `api_retry` (same 503-when-detached engine guard, same DTO shape, no payload/disk checks — kick/cancel only need the row): kick rejects terminal rows (`reason: "terminal"`), cancel rejects terminal rows likewise. Register `.route("/api/kick", post(api_kick)).route("/api/cancel", post(api_cancel))`.

- [ ] **Step 3: DTO fields + UI.** `SentDto { …, next_retry_at, byte_size }`. In `refreshSent()` (index.html:372-400):

```js
const pendingStates = ['queued', 'announced', 'transferring'];
const stalled = pendingStates.includes(r.state) && r.attempts > 0;
const badge = stalled ? `<span class="badge warn">stalled</span>` : '';
const countdown = (stalled && r.nextRetryAt)
  ? `<div class="dim">retry ${fmtCountdown(r.nextRetryAt)}</div>` : '';
const kick = pendingStates.includes(r.state) ? `<button data-kick="${r.id}">Send now</button>` : '';
const cancel = pendingStates.includes(r.state) ? `<button class="warn" data-cancel="${r.id}">Cancel</button>` : '';
const retry = (r.state === 'failed' || r.state === 'cancelled')
  ? `<button class="warn" data-retry="${r.id}">Retry</button>` : '';
```

`fmtCountdown(iso)` = `max(0, (Date.parse(iso) - Date.now())/1000)` rendered `m:ss` (the 2s `tick` keeps it live). Add `doKick`/`doCancel` mirroring `doRetry` (:423-436) posting `/api/kick` / `/api/cancel`; wire `[data-kick]`/`[data-cancel]` listeners next to the `[data-retry]` block (:397). Show `byte_size` in the row (reuse the existing size formatter used by the History table).

- [ ] **Step 4: Run** `cargo test -p perseus && cargo build -p perseus`. Manual check optional (owner smoke covers live). **Commit** `feat(perseus): kick/cancel endpoints, stalled badge, retry countdown, cancelled rows retryable`

---

### Task 10: `FetchSink` — live fetch progress in the transport layer

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/types.rs` (new `FetchEvent`; REMOVE `TransportEvent::FetchProgress` :78)
- Modify: `crates/athenaeum-core/src/sharing/mod.rs` (trait `fetch` :50 signature; new `fetch_manifest` method with default-unimplemented? NO — implement in both transports; `FetchSink` alias + `noop_fetch_sink()`)
- Modify: `crates/athenaeum-core/src/sharing/iroh/blobs.rs` (`fetch_collection_to_dir` :236-298 → two-phase + observe)
- Modify: `crates/athenaeum-core/src/sharing/iroh/mod.rs` (`IrohTransport::fetch` :633-670 — drop the one-shot FetchProgress emit :662-667; pass the sink through; `fetch_manifest` impl), `crates/athenaeum-core/src/sharing/iroh/node.rs` (delete `emit_fetch_progress` :1469 + routing)
- Modify: `crates/athenaeum-core/src/sharing/loopback.rs` (fetch :306 — synthetic per-file + batch sink calls; `fetch_manifest` by copying the manifest file)
- Modify: call sites of `fetch` (receiver.rs:483, project fetch path — grep `\.fetch(`) — pass `noop_fetch_sink()` where progress is not yet consumed
- Test: loopback module tests

**Interfaces:**
- Consumes: iroh-blobs 0.103 APIs — validated 2026-07-15: `Downloader::download(req, providers) -> DownloadProgress`, `.stream() -> Stream<DownloadProgressItem>` (`Progress(u64)` = cumulative request bytes), `store.blobs().observe(hash) -> ObserveProgress` (`Bitfield { size, .. }`, `.total_bytes()`, `.is_complete()`), `GetRequest::builder().root(ChunkRanges::all()).child(0, ChunkRanges::all()).build(root)`, `Collection::load(root, store)` (needs only hash-seq + meta blob; collection = ordered `Vec<(String, Hash)>`), `HashAndFormat::raw(hash)`.
- Produces:

```rust
// sharing/types.rs
#[derive(Clone, Debug)]
pub enum FetchEvent {
    Batch { bytes_done: u64, bytes_total: u64 },
    File  { name: String, bytes_done: u64, bytes_total: u64 },
}
// sharing/mod.rs
pub type FetchSink = Arc<dyn Fn(FetchEvent) + Send + Sync>;
pub fn noop_fetch_sink() -> FetchSink { Arc::new(|_| {}) }
// trait changes
async fn fetch(&self, from: NodeId, announce: &PackageAnnounce, dest: &Path, sink: FetchSink) -> Result<()>;
async fn fetch_manifest(&self, from: NodeId, announce: &PackageAnnounce, dest: &Path) -> Result<PathBuf>;
```

Tasks 11/12 consume the sink and `fetch_manifest`. Throttle constant `FETCH_PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(300)` lives in `blobs.rs` and applies per file AND to the batch stream (progress is UI event data, never logs).

- [ ] **Step 1: Loopback first (failing test).** `loopback_fetch_reports_monotonic_per_file_progress`: build a loopback package with 3 files, collect sink events into a `Mutex<Vec<FetchEvent>>`; assert every `File` series is non-decreasing and ends with `bytes_done == bytes_total`, and at least one `Batch` event ends at the announce's `byte_size`. Loopback emits synthetically: for each file — `File{done:0}`, `File{done:total}` — then `Batch{done:total,total}` (deterministic, no timing).

- [ ] **Step 2: Implement loopback + trait + mechanical call-site fixes** (`noop_fetch_sink()` at receiver.rs:483 until Task 11, project fetch path stays noop). `loopback.fetch_manifest` copies `manifest.ndjson` (`package::MANIFEST_FILENAME`, package/mod.rs:47) from the source package dir into `dest` and returns the path. Delete `TransportEvent::FetchProgress` and every emit/match site (receiver.rs:372 ignore-arm, engine.rs:1074-1088 debug-arm, loopback :306, iroh :662-667, node.rs demux). Run test — PASS. `cargo build --workspace` — compiles.

- [ ] **Step 3: iroh implementation.** Rework `fetch_collection_to_dir` (keep its export-to-dir tail):

```rust
pub async fn fetch_collection_to_dir(
    store: &Store, endpoint: &Endpoint, provider: NodeId,
    root_hash: Hash, dest: &Path, byte_size: u64, sink: FetchSink,
) -> Result<()> {
    let downloader = store.downloader(endpoint); // ONE instance for all requests (avoids re-dial)
    // Phase 1: hash-seq + collection meta (child 0) → names + child hashes.
    let req = GetRequest::builder()
        .root(ChunkRanges::all()).child(0, ChunkRanges::all()).build(root_hash);
    downloader.download(req, Shuffled::new(vec![provider])).await?;
    let collection = Collection::load(root_hash, store).await?;

    // Per-file observers: one task per child hash, throttled ≥300ms, emit on change.
    let mut observers = Vec::new();
    for (name, hash) in collection.iter() {
        let (sink, name, hash) = (sink.clone(), name.clone(), *hash);
        let blobs = store.blobs().clone();
        observers.push(tokio::spawn(async move {
            let mut last = std::time::Instant::now() - FETCH_PROGRESS_MIN_INTERVAL;
            let Ok(mut s) = blobs.observe(hash).stream().await else { return };
            while let Some(bf) = s.next().await {
                let done = bf.total_bytes(); let total = bf.size(); let complete = bf.is_complete();
                if complete || last.elapsed() >= FETCH_PROGRESS_MIN_INTERVAL {
                    last = std::time::Instant::now();
                    sink(FetchEvent::File { name: name.clone(), bytes_done: done, bytes_total: total });
                }
                if complete { break; }
            }
        }));
    }

    // Phase 2: full download, batch progress from the aggregate stream.
    let progress = downloader.download(HashAndFormat::hash_seq(root_hash), Shuffled::new(vec![provider]));
    let mut stream = progress.stream().await?;
    let mut last = std::time::Instant::now();
    while let Some(item) = stream.next().await {
        match item {
            DownloadProgressItem::Progress(done) => {
                if last.elapsed() >= FETCH_PROGRESS_MIN_INTERVAL {
                    last = std::time::Instant::now();
                    sink(FetchEvent::Batch { bytes_done: done.min(byte_size), bytes_total: byte_size });
                }
            }
            DownloadProgressItem::Error(e) => { for o in &observers { o.abort(); } return Err(e.into()); }
            DownloadProgressItem::DownloadError => { for o in &observers { o.abort(); } anyhow::bail!("download failed"); }
            _ => {}
        }
    }
    for o in observers { let _ = o.await; }
    sink(FetchEvent::Batch { bytes_done: byte_size, bytes_total: byte_size });
    // …existing export-blobs-to-dest tail unchanged…
    Ok(())
}
```

(`Progress(u64)` includes locally-present bytes and is monotonic across provider failover — clamp to `byte_size`. Phase-1 bytes make resume look correct for free.) `IrohTransport::fetch` passes `announce.byte_size` + sink; `fetch_manifest` = phase 1, find `MANIFEST_FILENAME` in the collection, `download(HashAndFormat::raw(manifest_hash))`, export that one blob to `dest`, return the path. Existing GC-window tagging behavior in blobs.rs stays as-is.

- [ ] **Step 4: Run** `cargo test -p athenaeum-core sharing && cargo build --workspace`. **Commit** `feat(sharing): FetchSink live per-file fetch progress; manifest-only fetch; drop one-shot FetchProgress`

---

### Task 11: `sync_inbound` table + receiver integration

**Files:**
- Modify: `crates/athenaeum-core/src/sync/models.rs` (InboundState, InboundRow)
- Modify: `crates/athenaeum-core/src/sync/store.rs` (DDL + free fns on `Connection`, CatalogSyncStore methods)
- Modify: `crates/athenaeum-core/src/db/schema.rs:1648-1671` (execute the new DDL)
- Modify: `crates/athenaeum-core/src/sync/receiver.rs` (`handle_announce` :386-557; event structs :156-202)
- Test: store module tests, receiver module tests (:868, `RecordingEmitter` :878)

**Interfaces:**
- Consumes: `FetchSink` (Task 10).
- Produces:

```rust
// models.rs — mirrors OutboundState's as_str/from_db/is_terminal pattern
pub enum InboundState { Announced, Fetching, Ingesting, Done, Failed, Cancelled }
pub struct InboundRow {
    pub id: i64, pub peer: String /* node-id hex */, pub package_id: String,
    pub state: InboundState, pub frame_count: u32, pub byte_size: u64, pub bytes_done: u64,
    pub last_error: Option<String>, pub created_at: String, pub finished_at: Option<String>,
}
// store.rs — free fns on &Connection (callable from CatalogSyncStore + receiver's spawn_blocking)
pub fn upsert_inbound_announced(conn, peer_hex: &str, package_id: &str, frame_count: u32, byte_size: u64) -> Result<i64>;
pub fn set_inbound_state(conn, package_id: &str, state: InboundState, last_error: Option<&str>) -> Result<()>; // stamps finished_at on terminal states
pub fn set_inbound_bytes_done(conn, package_id: &str, bytes_done: u64) -> Result<()>;
pub fn inbound_active(conn) -> Result<Vec<InboundRow>>;   // state NOT IN ('done','failed','cancelled')
pub fn get_inbound(conn, package_id: &str) -> Result<Option<InboundRow>>;
```

DDL `DDL_INBOUND`: all columns above + `UNIQUE(peer, package_id)`; upsert refreshes `frame_count`/`byte_size` and — **unless the existing row is `cancelled`** — resets state to `announced` (spec §8: Cancelled is final; the caller checks state before fetching). Executed in `StandaloneSyncStore::open` AND `db/schema.rs` (uniformity; only the catalog path is exercised).
- Event additions: `SyncProgressEvent` gains `#[serde(skip_serializing_if = "Option::is_none")] pub bytes_done: Option<u64>` + same-shaped `bytes_total`; new

```rust
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct SyncFileProgressEvent {
    pub package_id: String, pub peer_device: String,
    pub file: String, pub bytes_done: u64, pub bytes_total: u64,
}
```

emitted as `"sync-file-progress"`. (ts_export registration + regen in Task 14.)

- [ ] **Step 1: Failing store test** `inbound_row_lifecycle`: upsert → `Announced`; set `Fetching`, bytes 100; set `Done` → `finished_at` stamped, absent from `inbound_active`; re-upsert after `Cancelled` keeps `cancelled` state.

- [ ] **Step 2: Implement DDL + fns.** Run — PASS.

- [ ] **Step 3: Receiver writes.** In `handle_announce`: after the auth gate and `validate_package_id`, `upsert_inbound_announced` (via the existing conn access; peer hex via `node_id_hex`) — and **if `get_inbound` says `Cancelled`, skip to the Task-12 cancel-replay epilogue (until Task 12 lands: just do the existing receipts-replay check and return)**. At the `"fetching"` stage (:471): `set_inbound_state(.., Fetching, None)`. Build the real sink for `transport.fetch(...)`:

```rust
let sink: FetchSink = {
    let (emitter, store, pkg, peer_device) = (emitter.clone(), store.clone(), announce.package_id.0.clone(), peer_device.clone());
    Arc::new(move |ev| match ev {
        FetchEvent::Batch { bytes_done, bytes_total } => {
            let _ = store.with_conn(|c| set_inbound_bytes_done(c, &pkg, bytes_done)); // add with_conn helper if absent
            emitter.emit("sync-progress", &SyncProgressEvent { stage: "fetching".into(),
                bytes_done: Some(bytes_done), bytes_total: Some(bytes_total), /* rest as the existing fetching emit */ .. });
        }
        FetchEvent::File { name, bytes_done, bytes_total } => {
            emitter.emit("sync-file-progress", &SyncFileProgressEvent {
                package_id: pkg.clone(), peer_device: peer_device.clone(),
                file: name, bytes_done, bytes_total });
        }
    })
};
```

(Sink calls arrive ≤ every 300ms per stream — DB writes at that cadence are fine.) At `"ingesting"` (:494): `set_inbound_state(.., Ingesting, None)`. At the terminal emit (:545): `Done` on ingested/partial, `Failed` (+`last_error`) on failure; the replay path (:439-453) marks `Done` too (idempotent). Fetch errors → `Failed` + error string.

- [ ] **Step 4: Receiver test** `inbound_row_tracks_announce_to_done` using the loopback pair + `RecordingEmitter`: after a full receive, the row walked `Announced→…→Done` (assert final row + recorded stage events include a `fetching` progress with bytes and ≥1 `sync-file-progress`).

- [ ] **Step 5: Run** `cargo test -p athenaeum-core sync && cargo build --workspace`. **Commit** `feat(sync): persistent sync_inbound rows with live byte progress and per-file events`

---

### Task 12: Receiver-side cancel (`cancel_incoming_package`)

**Files:**
- Modify: `crates/athenaeum-core/src/sync/receiver.rs` (new `InboundControl`; `spawn` :233-241 signature; loop :255-378; `handle_announce`; `SyncRuntime` :733 + `Started` :722)
- Modify: `crates/athenaeum-core/src/api/sync.rs` (new api fn)
- Modify: `crates/athenaeum-tauri/src/commands/sync.rs` + `lib.rs`; `crates/athenaeum-web/src/routes/sync.rs` + `routes/mod.rs`
- Test: receiver module tests; Task 16 covers e2e

**Interfaces:**
- Consumes: `ReceiptOutcome::Cancelled` + replay counting (Task 4), `fetch_manifest` (Task 10), inbound rows (Task 11). Manifest parsing: reuse the reader `ingest_package` uses (`ingest.rs` — the fn that iterates `manifest.ndjson` entries yielding `frame_uuid` + `xxh3`; extract/`pub(crate)` it if private).
- Produces:

```rust
// receiver.rs
pub struct InboundControl {
    cancels: std::sync::Mutex<std::collections::HashSet<String>>,
    notify: tokio::sync::Notify,
}
impl InboundControl {
    pub fn request_cancel(&self, package_id: &str); // insert + notify_waiters
    pub fn is_cancelled(&self, package_id: &str) -> bool;
}
```

`SyncReceiver::spawn` gains `control: Arc<InboundControl>`; `SyncRuntime`/`Started` store it; `SyncRuntime::inbound_control() -> Option<Arc<InboundControl>>`. Api fn `cancel_incoming_package(ctx: &ServiceContext, sync: &Arc<SyncRuntime>, package_id: &str) -> Result<(), ApiError>`; Tauri command + Axum route `cancel_incoming_package` (`{ packageId: string }`). Desktop only — no Perseus surface (it never receives).

- [ ] **Step 1: Failing receiver test** `cancel_before_fetch_acks_cancelled_and_replays`: loopback package announced to a receiver whose `InboundControl` already has the package cancelled (call `request_cancel` first); assert (a) no files land in the incoming root, (b) the sender-side transport observed an ack whose receipts are all `Cancelled` (loopback lets the test hold the sender endpoint and capture `AckReceived`), (c) inbound row state `Cancelled`, (d) a second announce of the same package_id re-acks (replay) without fetching.

- [ ] **Step 2: Implement the cancel epilogue** (private fn in receiver.rs):

```rust
async fn cancel_epilogue(store, transport, emitter, from, announce, staging) -> Result<()> {
    // 1. Ensure the manifest is on disk (tiny fetch; §4 step 2).
    let manifest = transport.fetch_manifest(from, announce, &staging).await?;
    // 2. Cancelled receipts for every manifest frame → sync_receipts (replay log).
    let entries = read_manifest_entries(&manifest)?; // the ingest reader
    let receipts: Vec<FrameReceipt> = entries.iter().map(|e| FrameReceipt {
        frame_uuid: e.frame_uuid.clone(), xxh3: e.xxh3.clone(),
        outcome: ReceiptOutcome::Cancelled,
    }).collect();
    store.with_conn(|c| { for r in &receipts { insert_receipt(c, &announce.package_id.0, r, &now)?; } Ok(()) })?;
    // 3. Ack (best-effort — the replay path covers a lost ack) + terminal row + event.
    if let Err(e) = transport.ack(from, &announce.package_id, receipts).await {
        tracing::warn!(package_id = %announce.package_id.0, error = %e, "cancel ack failed; will replay");
    }
    store.with_conn(|c| set_inbound_state(c, &announce.package_id.0, InboundState::Cancelled, None))?;
    /* emit sync-finished outcome "cancelled", direction Received */
    Ok(())
}
```

Wire-in points in `handle_announce`: (a) right after the Task-11 upsert — `if control.is_cancelled(pkg) || get_inbound(..).state == Cancelled { return cancel_epilogue(...) }` (the persisted state makes cancellation restart-proof; note the existing receipts-replay check at :439 fires first on later announces and is equivalent); (b) the in-flight fetch becomes abortable:

```rust
let fetch_fut = transport.fetch(from, &announce, &staging, sink);
tokio::pin!(fetch_fut);
let fetch_result = loop {
    tokio::select! {
        r = &mut fetch_fut => break Some(r),
        _ = control.notified() => {
            if control.is_cancelled(&announce.package_id.0) { break None; } // drops the future → aborts the download
        }
    }
};
let Some(fetch_result) = fetch_result else { return cancel_epilogue(...).await; };
```

(`control.notified()` = `self.notify.notified()` exposed via a method.) Cancel during `Ingesting` is refused at the api layer: `cancel_incoming_package` loads the row and returns `ApiError::bad_request("too late: ingest in progress")` for `Ingesting`, no-ops for terminal, else `request_cancel` + immediately sets the row `Cancelled` when state is `Announced` (no in-flight announce to interrupt — the epilogue runs on the sender's next re-announce, which the §2 retry loop guarantees).

- [ ] **Step 3: Add the command + route** (pattern of Task 8; `#[tracing::instrument(skip_all, err)]` both sides; register both).

- [ ] **Step 4: Run** `cargo test -p athenaeum-core sync && cargo build --workspace`. **Commit** `feat(sync): receiver-side cancel with cancelled-receipt ack and replay`

---

### Task 13: Provider upload events → outgoing batch bytes

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/iroh/mod.rs` (`build_router` :175 — `BlobsProtocol::new(store, Some(events))`; consumer task)
- Modify: `crates/athenaeum-core/src/sharing/iroh/node.rs` (route the consumer's output into the transport events mpsc; served-package resolution — the node already tracks served announces for `release`; expose `resolve_served_root(hash) -> Option<PackageId>`)
- Modify: `crates/athenaeum-core/src/sharing/types.rs` (`TransportEvent::ServeProgress { package_id: PackageId, bytes_sent: u64 }`)
- Modify: `crates/athenaeum-core/src/sharing/loopback.rs` (synthetic ServeProgress on serve, so the engine arm is testable)
- Modify: `crates/athenaeum-core/src/sync/engine.rs` (event arm → `emit_progress` stage `"transferring"` with `bytes_done: Some(bytes_sent)`, `bytes_total: Some(announce.byte_size)` from the pending slot)
- Test: engine_tests loopback test

**Interfaces:**
- Consumes: iroh-blobs provider events (validated): `EventSender::channel(64, EventMask { connected: ConnectMode::Notify, get: RequestMode::NotifyLog, ..EventMask::DEFAULT })`; `ProviderMessage::GetRequestReceivedNotify` (`m.inner.request.hash`, `m.rx: Receiver<RequestUpdate>`); `RequestUpdate::{Started, Progress(TransferProgress{end_offset}), Completed, Aborted}`.
- Produces: `TransportEvent::ServeProgress`; `SyncProgressEvent.bytes_done/bytes_total` populated for `direction: Sent`.

**LOAD-BEARING SAFETY RULE:** every `GetRequestReceivedNotify`'s `m.rx` MUST be drained for the life of the transfer — an undrained receiver makes the provider's `Progress` send fail and **aborts the peer's download**. The consumer spawns a detached drain task per request unconditionally (even for unmapped/foreign hashes), never blocks, never drops early. Keep `throttle: ThrottleMode::None`. Ignore `Push*/Observe*` notify messages (a 0.103 mask quirk sends them despite the mask).

- [ ] **Step 1: Consumer.** In `build_router`, create the channel, pass `Some(events)` into `BlobsProtocol::new`, spawn:

```rust
tokio::spawn(async move {
    while let Some(msg) = rx.recv().await {
        if let ProviderMessage::GetRequestReceivedNotify(m) = msg {
            let root = m.inner.request.hash;
            let mut updates = m.rx;
            let (node, tx) = (node.clone(), event_tx.clone());
            tokio::spawn(async move {
                let pkg = node.resolve_served_root(&root); // None for hash-seq-internal / foreign requests
                let mut sent: u64 = 0;
                let mut last = std::time::Instant::now();
                while let Ok(Some(u)) = updates.recv().await {
                    match u {
                        RequestUpdate::Progress(p) => {
                            sent = sent.max(p.end_offset);
                            if let Some(pkg) = &pkg {
                                if last.elapsed() >= Duration::from_millis(300) {
                                    last = std::time::Instant::now();
                                    let _ = tx.try_send(TransportEvent::ServeProgress { package_id: pkg.clone(), bytes_sent: sent });
                                }
                            }
                        }
                        RequestUpdate::Completed(_) | RequestUpdate::Aborted(_) => break,
                        _ => {}
                    }
                }
            });
        } // other variants: drop (drained by not holding channels)
    }
});
```

Use `try_send` into the transport events mpsc — dropping a progress tick under backpressure is correct; blocking is not.

- [ ] **Step 2: Engine arm.** `TransportEvent::ServeProgress { package_id, bytes_sent }` → find the pending slot whose announce matches `package_id` → `emit_progress` with bytes (extend `emit_progress`'s signature or add `emit_progress_bytes`). Loopback: during `fetch`, also push one `ServeProgress { bytes_sent: byte_size }` to the serving endpoint's events (deterministic single tick).

- [ ] **Step 3: Test** `sent_progress_carries_bytes` (engine_tests, loopback): RecordingEmitter on the sender sees ≥1 `sync-progress` with `direction: Sent`, `bytes_done == Some(byte_size)`.

- [ ] **Step 4: Run** `cargo test -p athenaeum-core && cargo build --workspace`. **Commit** `feat(sharing): provider upload events feed outgoing batch byte progress`

---

### Task 14: Status + detail API, history batch key, TS regen

**Files:**
- Modify: `crates/athenaeum-core/src/sync/status.rs` (OutboundSummary :16-27, SyncSenderStatus :33, SyncReceiverStatus :52-57)
- Modify: `crates/athenaeum-core/src/sync/models.rs` (`HistoryRow` :141-163 + package_id), `store.rs` (`ensure_history_columns` :88-101, `HISTORY_COLS` :280, append/search), history writers (`engine.rs::append_terminal_history` + the started-history writer; `ingest.rs:524-537 received_history`)
- Modify: `crates/athenaeum-core/src/api/sync.rs` (`build_sender_status` :752-812, `get_status` :817-836; new `list_transfer_files`)
- Modify: `crates/athenaeum-core/src/ts_export.rs:150-157` (register: `InboundState`, `InboundSummary`, `TransferFileEntry`, `SyncFileProgressEvent`; existing sync types re-emit with their new fields on regen)
- Modify: `crates/athenaeum-tauri/src/commands/sync.rs` + `lib.rs`; `crates/athenaeum-web/src/routes/sync.rs` + `routes/mod.rs`
- Test: api tests; `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`

**Interfaces:**
- Consumes: Tasks 2/3/11 fields.
- Produces (all `ts_rs::TS` + camelCase):

```rust
pub struct OutboundSummary { // extended
    pub id: i64, pub package_short: String, pub state: OutboundState, pub attempts: u32,
    pub created_at: String, pub peer_short: String,
    pub last_error: Option<String>, pub next_retry_at: Option<String>,
    pub byte_size: u64, pub file_count: u32,
}
pub struct InboundSummary {
    pub id: i64, pub package_short: String, pub peer_short: String, pub state: InboundState,
    pub frame_count: u32, pub byte_size: u64, pub bytes_done: u64, pub created_at: String,
}
pub struct SyncReceiverStatus { pub started: bool, pub active: Vec<InboundSummary>, pub received_total: u32 }
pub struct TransferFileEntry {
    pub name: String, pub bytes_total: u64,
    pub bytes_done: Option<u64>,     // incoming, while active (from history bytes after Done)
    pub outcome: Option<String>,     // outgoing after confirm (from receipts) / incoming from history
}
```

`HistoryRow.package_id: Option<String>` (guarded ALTER `sync_history.package_id TEXT`; both writers populate: sender terminal + started history have the announce/pending package id; ingest has `announce.package_id`). New api fn + command/route `list_transfer_files`:

```rust
pub async fn list_transfer_files(ctx: &ServiceContext, sync_sender: &Arc<SyncSenderRuntime>,
    direction: Direction, id: i64) -> Result<Vec<TransferFileEntry>, ApiError>
```

- sent: `get_outbound(id)` → read `manifest.ndjson` from `package_ref` (name + bytes per entry) + `load_receipts(package_id)` → outcome per frame.
- received: inbound row by id → entries from that package's history rows (`WHERE package_id = ?`) when terminal; while active, entries from the manifest if present in staging else empty (the live per-file bars are event-driven via `sync-file-progress`; the detail list backfills names/sizes when available).
- Tauri command `list_transfer_files({ direction: "sent"|"received", id })`; Axum mirror.
- `SyncReceiverStatus.active` is built from `inbound_active(conn)`; `SyncStatus.receiver` keeps `received_total`. NOTE: `active: bool` → `Vec<InboundSummary>` is a breaking TS shape change — fix the two frontend consumers (`useSyncStatus.ts`, `TransfersPanel.tsx`) in the same commit so `tsc` stays green (`active.length > 0` replaces the boolean).

- [ ] **Step 1: History column + writers** (guarded ALTER like Task 2; both writers pass package id; store test: append with package_id roundtrips; legacy NULL rows still load).
- [ ] **Step 2: Summaries + api fn** (`package_totals(dir) -> Result<(u32 /*files*/, u64 /*bytes*/)>` helper reading the manifest, reused by `build_sender_status`; api test: after a loopback confirm, `list_transfer_files(sent, id)` returns entries with `outcome: Some("ingested")`).
- [ ] **Step 3: Register + regen TS**: add new types to `ts_export.rs`; run `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`; fix the two TS consumers; `npx tsc --noEmit` — PASS.
- [ ] **Step 4: Command + route wiring** (both backends, registered).
- [ ] **Step 5: Run** `cargo test -p athenaeum-core && cargo build --workspace && npx tsc --noEmit`. **Commit** `feat(sync): transfer summaries with bytes/retry info, per-batch file detail API, history batch key`

---

### Task 15: `/transfers` page (frontend) — `frontend-dev` subagent

**Files:**
- Create: `src/pages/Transfers.tsx`
- Modify: `src/App.tsx` (:8-19 imports, :74-88 routes), `src/components/Layout.tsx` (`navItems` :47-56), `src/components/transfers/TransfersPanel.tsx` (quick-glance link; `receiver.active` usage), `src/hooks/useSyncStatus.ts` (expose receiver summaries)
- Test: `npx tsc --noEmit`; manual dev-run

**Interfaces:**
- Consumes: `SyncStatus` (extended), `list_transfer_files`, `send_now_sync_package` / `cancel_sync_package` / `retry_sync_package` / `cancel_incoming_package` commands, `sync-file-progress` + `sync-progress` events (bytes fields). All via `api.invoke` / `api.listen` from `src/api/` ONLY.
- Produces: route `/transfers`, nav entry `{ to: '/transfers', icon: ArrowLeftRight /* lucide */, label: 'Transfers' }`.

**Layout (approved mockup, 2026-07-15):** unified table, Active tab default: direction arrow (▲ frost `text-accent` / ▼ `text-success`), device, batch (packageShort + `N files`), status chip + **stalled badge** (`bg-warning/20 text-warning`, shown when state is pending AND attempts > 0), progress bar (incoming: `bytesDone/byteSize`; outgoing: live bytes from `sync-progress` events falling back to state-staged), size, speed (client-side delta of bytes over wall-clock, EMA over ~3 samples, shown while transferring), `attempt N` + countdown to `nextRetryAt` (1s local tick), actions per row: Send now (pending outbound), Cancel (pending outbound; incoming in announced/fetching), Resend (terminal outbound). Row click expands → `list_transfer_files`; incoming rows overlay live `sync-file-progress` bars keyed by file name; outgoing show name+size, outcome chips (`ingested` success / `duplicate` warning / `rejected`+`cancelled` error tones) after confirm. History = third tab: reuse `HistoryTab` internals but grouped by `packageId` (rows with `packageId: null` group under "earlier"). Tabs: `Active (N) | History`. All colors via existing tokens (they are Nord) — no raw hex.

- [ ] **Step 1: Page + route + nav** (listeners use the MANDATORY cancelled-flag pattern from CLAUDE.md; poll via existing `useTransfers()` context; `sync-file-progress`/`sync-progress` listeners live in the page, mounted-only).
- [ ] **Step 2: Actions** wired to the four commands; optimistic-nothing — re-poll on the `sync-finished`/action resolve (pattern of `ReceiveTab`); errors → `console.error` + `notify({ kind: 'generic', tone: 'warning', ... })`.
- [ ] **Step 3: Panel link**: `TransfersPanel` header gets "Open full screen →" (`useNavigate('/transfers')` + `closePanel()`); panel's ActiveTab now also lists incoming summaries (mini rows, no expansion).
- [ ] **Step 4: Gates**: `npx tsc --noEmit` — PASS; `npm run dev` visual sanity happens at owner smoke. **Commit** `feat(ui): torrent-style /transfers screen with live per-file progress`

---

### Task 16: Loopback e2e acceptance suite

**Files:**
- Modify: `crates/athenaeum-core/tests/sync_e2e.rs` (pattern: `two_instance_sync_e2e` :202 — two `ServiceContext`s, `LoopbackNetwork`, real `SyncReceiver::spawn` + engine via `SyncSenderRuntime::lock_inner` injection, `wait_until` :152)

**Interfaces:** consumes everything above; produces the spec's acceptance evidence (§6, §12).

- [ ] **Step 1: `offline_peer_delivers_after_reconnect_without_user_action`** (§6 acceptance): enqueue against a not-yet-spawned receiver endpoint (or a `FaultPlan`-broken link — `FaultPlan` import per engine_tests.rs:28); `wait_until` attempts ≥ 2 and state pending (never terminal, `next_retry_at` set); bring the receiver up; `engine.kick_all()` is NOT called — the backoff schedule alone must deliver (use `ack_timeout: Duration::from_millis(50)` so rungs are ms-scale); assert `Confirmed` + files landed + history rows.

- [ ] **Step 2: `receiver_cancel_terminates_sender_then_resend_delivers`** (§4/§6): start a transfer with a `FaultPlan`-slowed fetch (or cancel between announce and fetch); `cancel_incoming_package`; assert sender row `Cancelled` + `last_error == "cancelled by receiver"`, receiver row `Cancelled`, no files landed; then `retry_sync_package(old_id)` → new id → full delivery. Variant in the same test or a sibling: kill the link before the cancel-ack (FaultPlan), re-announce → replayed cancelled ack, same terminal states, no re-fetch (assert staging untouched).

- [ ] **Step 3: `per_file_progress_is_monotonic_and_inbound_visible_while_fetching`** (§12): RecordingEmitter on the receiver; during the transfer `wait_until` an `sync_inbound` row exists with state `Fetching` (loopback fetch is synchronous-fast — if the window is unobservable, use the FaultPlan slow-fetch to hold it open); after completion assert per-file `sync-file-progress` events are per-file monotonic ending at totals, and the inbound row is `Done`.

- [ ] **Step 4: Run** `cargo test -p athenaeum-core --test sync_e2e` — PASS. Full gates: `cargo build --workspace && cargo test -p athenaeum-core && cargo test -p perseus && npx tsc --noEmit`. **Commit** `test(sync): e2e acceptance — deliver-forever, two-sided cancel, live inbound progress`

---

## Execution notes

- Task order is the dependency order; Tasks 3/4 and 6/7 may swap freely. Task 15 is the only frontend task (dispatch to `frontend-dev`); all others → `rust-engineer`.
- After every task: run the task's stated gates yourself — do not trust a subagent's "gates passed" report (re-gate independently).
- Existing tests that hard-code the old contract (`max_attempts: 5`, terminal-Failed assertions) are rewritten in Task 1 — this is the spec's deliberate semantics change (§5), not test-fitting. Any OTHER unexpectedly failing test = stop and investigate.
- Owner smoke after merge-to-branch: two live instances vs test-hub — new-device authorization without restart (§3), a real offline→online delivery, and the `/transfers` screen during a big batch.

