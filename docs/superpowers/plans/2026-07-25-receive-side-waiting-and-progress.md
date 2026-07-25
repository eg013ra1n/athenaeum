# D2 — Receive-side Waiting and Progress Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The receiving side stops calling a vanished sender a failure — it stores a non-terminal `Waiting`, counts fetched files the way the sender counts uploaded ones, and its "Clean up" button gains the receive-side pass it never had.

**Architecture:** Two new enum values (`InboundState::Waiting`, `InboundFileState::Fetched`) plus a typed classification of fetch failures produced at the failure site. Everything else is a membership sweep: seven Rust sites and six frontend sites that ask "is this row done with?" and must answer correctly for a state that did not exist.

**Spec:** `docs/superpowers/specs/2026-07-25-receive-side-waiting-and-progress-design.md` — read it before Task 1. Its §4 table is the checklist this plan implements; its §3.2 table is the verdict per `Failed` write site.

**Tech Stack:** Rust (rusqlite, anyhow, tokio, tracing, ts-rs), React/TypeScript, Tailwind design tokens.

## Global Constraints

- Work on branch `0.5.0`. Never commit to `main`.
- Commit as the repo user (`eg013ra1n <vilen.sharifov@gmail.com>`). **No Claude co-author trailer.**
- Two backends stay in sync: any command surface change needs its Axum mirror in the same commit. This plan changes no command signatures except `TransferStorage` (Task 9), whose route already returns the struct verbatim.
- `anyhow::Result` inside core; `.map_err(|e| e.to_string())` at the command boundary.
- Never swallow errors — every best-effort write warns via `tracing` before continuing.
- Design tokens only in the frontend; no raw colors.
- Gates after every task: `cargo test -p athenaeum-core` and, for frontend tasks, `npx tsc --noEmit`. `cargo clippy -D warnings` is NOT a gate in this repo (pre-existing debt).
- `cargo fmt -p <crate>` reformats the whole crate — use `rustfmt <files>` for scoped changes.

## File Structure

| File | Responsibility in this change |
| ---- | ---- |
| `crates/athenaeum-core/src/sync/models.rs` | The two new enum values, their `as_str`/`from_db`/`is_terminal` |
| `crates/athenaeum-core/src/sharing/types.rs` | `LocalFault` — the typed marker that separates "our disk failed" from "the peer went away" |
| `crates/athenaeum-core/src/sharing/iroh/blobs.rs` | Attaches `LocalFault` to the materialize phase of a fetch |
| `crates/athenaeum-core/src/sync/receiver.rs` | The fetch/ingest/ack/reconcile/revoke sites; the per-file progress sink |
| `crates/athenaeum-core/src/sync/store.rs` | The shared counter predicate; the `inbound_file_counts` doc |
| `crates/athenaeum-core/src/api/sync.rs` | `inbound_summary` mapping, cancel terminalization, tag protection, the receive-side cleanup pass |
| `src/types/models.ts` | Regenerated ts-rs union (never hand-edited) |
| `src/components/transfers/TransferRow.tsx` | The inbound Cancel gate |
| `src/components/transfers/TransfersPanel.tsx` | The incoming subline |
| `src/components/transfers/presentation.ts` | The `fetched` file chip |
| `src/hooks/useTransferQueue.ts` | Terminal refetch after an inbound cancel |

## Tests that go red

Verified by grep before writing this plan. Six tests in `crates/athenaeum-core/src/sync/receiver.rs` assert the old behaviour and are converted by the task that changes it — they are named in the task that owns them, not fixed up separately:

| Test | Line | Owned by |
| ---- | ---- | ---- |
| `failed_fetch_emits_a_terminal_finished_event` | 3863 | Task 3 |
| `resend_after_failed_fetch_reuses_one_batch_row_and_delivers` | 3922 | Task 3 |
| `received_history_keys_on_batch_uuid_not_wire_id` | 4012 | Task 3 |
| `startup_reconciles_stale_inbound_rows_to_failed` | 2577 | Task 5 |
| `startup_repairs_fully_receipted_stale_inbound_from_receipt_log` | 2651 | Task 5 |
| `reconcile_settles_file_rows_then_reannounce_restores` | 3791 | Task 5 |
| `decline_survives_restart_reconcile_and_refuses_resend` | 4350 | Task 5 |

`inbound_row_stamps_failed_on_ingest_error` (3103) and `revoke_failed_maps_to_failed` (4566) stay green — those paths keep `Failed` (spec §3.2). Any further red test outside this list is a finding: stop and report it rather than adjusting the assertion.

---

### Task 1: `InboundState::Waiting` — the value and its membership

Adds the state and proves every membership test answers correctly for it. Nothing writes it yet, so the suite stays green.

**Files:**
- Modify: `crates/athenaeum-core/src/sync/models.rs:110-175`
- Modify: `src/types/models.ts:595` (regenerated, never hand-edited)
- Test: `crates/athenaeum-core/src/sync/store.rs` (tests module, alongside the existing inbound store tests)

**Interfaces:**
- Produces: `InboundState::Waiting`; `InboundState::as_str` → `"waiting"`; `InboundState::from_db("waiting")`; `is_terminal()` → `false`.

- [ ] **Step 1: Write the failing test**

Append to the `tests` module of `crates/athenaeum-core/src/sync/store.rs`:

```rust
#[test]
fn waiting_is_non_terminal_and_lands_in_every_active_membership_test() {
    let conn = Connection::open_in_memory().unwrap();
    init_sync_schema(&conn).unwrap();

    assert!(!InboundState::Waiting.is_terminal());
    assert_eq!(InboundState::Waiting.as_str(), "waiting");
    assert_eq!(InboundState::from_db("waiting").unwrap(), InboundState::Waiting);

    let (id, _) = upsert_inbound_attempt(&conn, "peerhex", "batch-1", "wire-1", 3, 999).unwrap();
    set_inbound_state(&conn, "wire-1", InboundState::Waiting, Some("peer gone")).unwrap();

    // Active, not terminal, and still holding its landing-dir claim.
    let active = inbound_active(&conn).unwrap();
    assert_eq!(active.len(), 1, "a waiting row is an active row");
    assert_eq!(active[0].state, InboundState::Waiting);
    assert!(active[0].finished_at.is_none(), "a non-terminal state stamps no finished_at");
    assert_eq!(active[0].last_error.as_deref(), Some("peer gone"), "the reason is preserved");

    assert!(terminal_inbound(&conn, 50).unwrap().is_empty(), "and absent from the terminal window");

    set_inbound_landing_dir(&conn, id, "/incoming/dev/batch").unwrap();
    assert!(
        landing_dir_claimed_by_active(&conn, "/incoming/dev/batch", id + 1).unwrap(),
        "a waiting row keeps claiming its landing dir — the sender redelivers into the same tree"
    );

    // A re-announce revives it into a fresh attempt (no state list to teach).
    let (again, declined) = upsert_inbound_attempt(&conn, "peerhex", "batch-1", "wire-2", 3, 999).unwrap();
    assert_eq!(again, id, "same durable row");
    assert!(!declined);
    let revived = get_inbound(&conn, "wire-2").unwrap().unwrap();
    assert_eq!(revived.state, InboundState::Announced);
    assert_eq!(revived.generation, 2);
    assert!(revived.last_error.is_none(), "the revive clears the reason");
}
```

Add any missing imports to the test module's `use super::*;` neighbours (`InboundState`, `upsert_inbound_attempt`, `set_inbound_state`, `set_inbound_landing_dir`, `landing_dir_claimed_by_active`, `terminal_inbound`, `get_inbound`, `inbound_active`).

- [ ] **Step 2: Run it and watch it fail**

```bash
cargo test -p athenaeum-core waiting_is_non_terminal_and_lands_in_every_active_membership_test
```

Expected: FAIL — `no variant named Waiting found for enum InboundState`.

- [ ] **Step 3: Add the variant**

In `crates/athenaeum-core/src/sync/models.rs`, add to the `InboundState` enum after `Ingesting`:

```rust
    /// NON-TERMINAL: an attempt ended because the sending device went away, and
    /// under delivery-forever the sender owes us another (D2 §3.1). Distinct from
    /// [`Failed`](Self::Failed), which means "we cannot accept this". The reason
    /// lives in `last_error`; the row stays in
    /// [`inbound_active`](super::store::inbound_active) so the status poll keeps
    /// it on screen without an event, and `upsert_inbound_attempt` revives it on
    /// the next announce.
    Waiting,
```

In `as_str`, add `InboundState::Waiting => "waiting",`. In `from_db`, add `"waiting" => InboundState::Waiting,`. Leave `is_terminal` alone — it lists the three terminals explicitly, so `Waiting` is false by construction; extend its doc comment to say `Waiting` is deliberately excluded.

Update the enum's type-level doc (`models.rs:100-107`) so the terminal-state sentence names `Waiting` as the non-terminal end-of-attempt.

- [ ] **Step 4: Run the test**

```bash
cargo test -p athenaeum-core waiting_is_non_terminal_and_lands_in_every_active_membership_test
```

Expected: PASS.

- [ ] **Step 5: Regenerate the TypeScript contract**

```bash
TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract
git diff --stat src/types/models.ts
```

Expected: `src/types/models.ts:595` gains `| "waiting"` in the `InboundState` union. Verify:

```bash
grep -n 'export type InboundState' src/types/models.ts
npx tsc --noEmit
```

- [ ] **Step 6: Full gate and commit**

```bash
cargo test -p athenaeum-core
git add crates/athenaeum-core/src/sync/models.rs crates/athenaeum-core/src/sync/store.rs src/types/models.ts
git commit -m "feat(sync): InboundState::Waiting — a non-terminal end-of-attempt for the receive side"
```

---

### Task 2: `LocalFault` — classify a fetch failure where it happens

The receiver cannot tell a vanished peer from a failing disk by reading error text: both arrive as one `anyhow::Error` out of `transport.fetch`, because `create_dir_all(dest_dir)` (`blobs.rs:501`) lives inside the same `Result` as the download. The function that failed knows. This task gives it a way to say so.

The phase boundary is exact and already in the code: everything up to and including the phase-2 download loop is transport; from the permanent tag set (`blobs.rs:479`) onward — tag write, `create_dir_all`, per-entry export — is local work on data we already hold.

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/types.rs` (add `LocalFault`)
- Modify: `crates/athenaeum-core/src/sharing/iroh/blobs.rs:475-520`
- Test: `crates/athenaeum-core/src/sharing/types.rs` (tests module)

**Interfaces:**
- Produces: `pub struct LocalFault(pub anyhow::Error)` implementing `std::error::Error`, and `pub fn is_local_fault(err: &anyhow::Error) -> bool`. Consumed by Task 3.

- [ ] **Step 1: Write the failing test**

Append to `crates/athenaeum-core/src/sharing/types.rs`:

```rust
#[cfg(test)]
mod local_fault_tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn local_fault_survives_context_wrapping_and_keeps_the_original_message() {
        let inner = anyhow::anyhow!("No space left on device");
        let marked: anyhow::Error = LocalFault(inner).into();
        // A caller adding its own context must not hide the marker…
        let wrapped = Err::<(), _>(marked)
            .context("create dest dir /x/y")
            .context("fetch package pkg-1")
            .unwrap_err();
        assert!(is_local_fault(&wrapped), "the marker is found anywhere in the chain");
        // …and must not garble what the user reads in `last_error`.
        let text = format!("{wrapped:#}");
        assert!(text.contains("No space left on device"), "got: {text}");
        assert!(!text.contains("LocalFault"), "the marker is invisible in Display: {text}");
    }

    #[test]
    fn an_unmarked_error_is_not_a_local_fault() {
        let e = anyhow::anyhow!("Unable to download collection abc123");
        assert!(!is_local_fault(&e), "a transport error carries no marker");
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

```bash
cargo test -p athenaeum-core local_fault
```

Expected: FAIL — `cannot find type LocalFault in this scope`.

- [ ] **Step 3: Add the marker**

In `crates/athenaeum-core/src/sharing/types.rs`:

```rust
/// Wraps a fetch error that originated in LOCAL work — writing the collection out
/// of the blob store onto our own disk — rather than in the transfer itself
/// (D2 §3.2).
///
/// The receiver needs this distinction and cannot derive it: a vanished peer and a
/// full disk both surface as one `anyhow::Error` out of
/// [`SharingTransport::fetch`](super::SharingTransport::fetch). Text matching cannot
/// separate them; the failing call site can. `Display` forwards to the inner error
/// so the marker never appears in the `last_error` the user reads.
///
/// **Default is transport.** An unmarked error is treated as a peer absence, which
/// is the safer error: a local fault mislabeled `Waiting` retries and stays visible,
/// while a vanished peer mislabeled `Failed` is the lie D2 exists to remove.
#[derive(Debug)]
pub struct LocalFault(pub anyhow::Error);

impl std::fmt::Display for LocalFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for LocalFault {}

/// True when `err` carries a [`LocalFault`] anywhere in its context chain.
pub fn is_local_fault(err: &anyhow::Error) -> bool {
    err.downcast_ref::<LocalFault>().is_some()
}
```

- [ ] **Step 4: Run the test**

```bash
cargo test -p athenaeum-core local_fault
```

Expected: PASS (both cases).

- [ ] **Step 5: Mark the materialize phase in `blobs.rs`**

In `fetch_collection_to_dir`, wrap each local-phase fallible call. The permanent tag set at `blobs.rs:479`:

```rust
    store
        .tags()
        .set(tag, HashAndFormat::hash_seq(root_hash))
        .await
        .map_err(|e| LocalFault(anyhow::Error::new(e).context("tag fetched collection")))?;
```

The dest-dir creation at `blobs.rs:501`:

```rust
    tokio::fs::create_dir_all(dest_dir)
        .await
        .map_err(|e| {
            LocalFault(anyhow::Error::new(e).context(format!("create dest dir {}", dest_dir.display())))
        })?;
```

And inside the export loop, both the parent-dir creation and the export:

```rust
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                LocalFault(anyhow::Error::new(e).context(format!("create dir {}", parent.display())))
            })?;
        }
        store
            .blobs()
            .export(*blob_hash, &target)
            .await
            .map_err(|e| {
                LocalFault(anyhow::Error::new(e).context(format!("export {name} -> {}", target.display())))
            })?;
```

Import `LocalFault` at the top of `blobs.rs`. Leave the in-flight tag `set` (`blobs.rs:382`) and everything above it UNMARKED: it runs before the download completes, and a failure there is indistinguishable from a transfer that never got going.

Add a comment above the permanent tag set marking the boundary:

```rust
    // ── D2 §3.2: everything below this line is LOCAL work on data we already hold.
    // Failures here are `LocalFault` (→ the receiver stamps Failed); everything
    // above is the transfer itself (→ the receiver stamps Waiting).
```

- [ ] **Step 6: Gate and commit**

```bash
cargo build -p athenaeum-core --all-targets && cargo test -p athenaeum-core
git add crates/athenaeum-core/src/sharing/types.rs crates/athenaeum-core/src/sharing/iroh/blobs.rs
git commit -m "feat(sync): LocalFault marks the materialize phase of a fetch, so the receiver can tell a full disk from a vanished peer"
```

---

### Task 3: A fetch that loses its peer stamps `Waiting`

The behavioural core. Splits `stamp_inbound_failed` so a benign wait does not settle per-file rows, moves the terminal event onto the branch that is actually terminal, and adds the event to the two paths that emit nothing today (spec §3.2).

**Files:**
- Modify: `crates/athenaeum-core/src/sync/receiver.rs:776-800` (the stamp helpers)
- Modify: `crates/athenaeum-core/src/sync/receiver.rs:1379-1403` (the fetch failure site)
- Modify: `crates/athenaeum-core/src/sync/receiver.rs:1470-1477` (ingest failure — add the event)
- Modify: `crates/athenaeum-core/src/sync/receiver.rs:1491-1498` (ack failure — add the event)
- Test: `crates/athenaeum-core/src/sync/receiver.rs` (tests module)

**Interfaces:**
- Consumes: `is_local_fault` from Task 2, `InboundState::Waiting` from Task 1.
- Produces: `fn stamp_inbound_waiting(store: &CatalogSyncStore, package_id: &str, error: &anyhow::Error)` — stamps the row only, never the file rows.

- [ ] **Step 1: Convert the three tests this task owns**

`failed_fetch_emits_a_terminal_finished_event` (`receiver.rs:3863`) becomes two tests. Replace it with:

```rust
    /// D2 §3.1: a fetch that loses its peer is NOT a failure. The row stays
    /// non-terminal so the 10 s status poll keeps it on screen by itself, the
    /// reason is preserved, the per-file rows are left as the resume checkpoint,
    /// and NO terminal event is emitted (there is no terminal).
    #[tokio::test]
    async fn a_peer_absent_fetch_leaves_the_row_waiting_and_emits_nothing() {
        let (store, transport, emitter, wire) = fetch_failure_fixture("peer gone: connection lost").await;
        let row = poll_inbound(&store, &wire, InboundState::Waiting).await;
        assert_eq!(row.state, InboundState::Waiting, "non-terminal — the sender owes us another attempt");
        assert!(row.finished_at.is_none(), "a waiting row never stamps finished_at");
        assert!(
            row.last_error.as_deref().unwrap_or_default().contains("connection lost"),
            "the reason is preserved: {:?}",
            row.last_error
        );
        assert!(
            !emitter.kinds().iter().any(|k| k == "sync-finished"),
            "no terminal event for a non-terminal end: {:?}",
            emitter.kinds()
        );
        let conn = store.lock_conn();
        let files = list_inbound_files(&conn, row.id).unwrap();
        assert!(
            files.iter().all(|f| f.state != InboundFileState::Failed),
            "the file rows are the resume checkpoint, not casualties"
        );
        let _ = transport;
    }

    /// D2 §3.2: the other half — a fetch that failed on OUR disk is terminal, and
    /// still announces itself so the row moves to the terminal list.
    #[tokio::test]
    async fn a_local_fault_fetch_is_failed_and_emits_its_terminal() {
        let (store, transport, emitter, wire) =
            fetch_failure_fixture_local(anyhow::anyhow!("No space left on device")).await;
        let row = poll_inbound(&store, &wire, InboundState::Failed).await;
        assert_eq!(row.state, InboundState::Failed, "we cannot accept it — terminal");
        assert!(
            emitter.kinds().iter().any(|k| k == "sync-finished"),
            "a terminal announces itself: {:?}",
            emitter.kinds()
        );
        let _ = transport;
    }
```

Reuse the existing fixture body of `failed_fetch_emits_a_terminal_finished_event` to write the two helpers `fetch_failure_fixture(msg: &str)` and `fetch_failure_fixture_local(err: anyhow::Error)` — identical except that the second has the mock transport return `Err(LocalFault(err).into())`. If the mock transport has no failure-injection hook, add one mirroring the existing cancel hook; keep the helper signature `-> (Arc<CatalogSyncStore>, Arc<MockTransport>, Arc<TestEmitter>, String)`.

In `resend_after_failed_fetch_reuses_one_batch_row_and_delivers` (`:3922`, poll at `:3960`) and `received_history_keys_on_batch_uuid_not_wire_id` (`:4012`, poll at `:4049`), change `poll_inbound(&store, &w1, InboundState::Failed)` to `InboundState::Waiting`. Both inject a plain transport error, which is now peer-absent by default. Leave every other assertion untouched — the point of both tests (one row across attempts; history keyed on `batch_uuid`) is unchanged.

- [ ] **Step 2: Run them and watch them fail**

```bash
cargo test -p athenaeum-core -- --nocapture \
  a_peer_absent_fetch_leaves_the_row_waiting_and_emits_nothing \
  a_local_fault_fetch_is_failed_and_emits_its_terminal \
  resend_after_failed_fetch_reuses_one_batch_row_and_delivers \
  received_history_keys_on_batch_uuid_not_wire_id
```

Expected: FAIL — the waiting ones time out in `poll_inbound` (the row is stamped `Failed`), the local-fault one fails on the missing injection hook.

- [ ] **Step 3: Add the waiting stamp helper**

In `crates/athenaeum-core/src/sync/receiver.rs`, directly below `stamp_inbound_failed`:

```rust
/// Stamp `package_id`'s inbound row [`Waiting`](InboundState::Waiting) with the
/// reason — and DELIBERATELY leave its per-file rows alone (D2 §3.3).
///
/// The twin of [`stamp_inbound_failed`], minus the settle. This attempt ended, but
/// the transfer did not: the sender is obliged to redeliver, so the per-file rows
/// are the resume checkpoint. Settling them `failed` here would reset the counter
/// D2 §3.4 exists to make honest — the row would report zero received files while
/// holding most of them on disk. `upsert_inbound_attempt` resets them when the next
/// attempt actually starts.
fn stamp_inbound_waiting(store: &CatalogSyncStore, package_id: &str, error: &anyhow::Error) {
    let reason = format!("{error:#}");
    let conn = store.lock_conn();
    if let Err(e) = set_inbound_state(&conn, package_id, InboundState::Waiting, Some(&reason)) {
        tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "inbound waiting state write failed");
    }
}
```

- [ ] **Step 4: Branch the fetch-failure site**

Replace the body of `if let Err(e) = fetch_result { … }` (`receiver.rs:1379-1403`) with:

```rust
    if let Err(e) = fetch_result {
        // D2 §3.2: WHY the fetch died decides whether this row is finished. The
        // classification is produced at the failure site (`LocalFault`), never
        // sniffed from the error text — a vanished peer and a full disk arrive
        // through the same `Result`. Unmarked ⇒ peer-absent, the safer default.
        if crate::sharing::types::is_local_fault(&e) {
            journal(store, inbound_id, "fetch_failed", Some(&format!("{e:#}")));
            stamp_inbound_failed(store, &package_id, &e);
            // A terminal announces itself: a Failed row leaves `inbound_active`,
            // so the status poll drops it and only `sync-finished` makes the
            // frontend refetch the terminal list that must now carry it. Emitted
            // AFTER the stamp so that refetch reads the settled row.
            emit_event(emitter.as_ref(), "sync-finished", &SyncFinishedEvent {
                package_id: package_id.clone(),
                direction: super::Direction::Received,
                outcome: "failed".to_string(),
                peer_device: peer_device.to_string(),
                ok_count: 0,
                failed: Vec::new(),
                new_count: 0,
                duplicate_count: 0,
                project_id: None,
            });
        } else {
            // The peer went away. Non-terminal, so the row stays in
            // `inbound_active` and the 10 s poll keeps it visible without any
            // event — which is what the D1-era `sync-finished` on this path was
            // compensating for. No event here: there is no terminal to announce.
            journal(store, inbound_id, "fetch_waiting", Some(&format!("{e:#}")));
            stamp_inbound_waiting(store, &package_id, &e);
        }
        return Err(e).with_context(|| format!("fetch package {package_id}"));
    }
```

- [ ] **Step 5: Give the ingest and ack failures their terminal event**

At `receiver.rs:1470-1477`, after `stamp_inbound_failed(store, &package_id, &e);` and before the `return Err(...)`, add the same `emit_event(... "sync-finished" ... outcome: "failed" ...)` block. Do the same at the ack site (`receiver.rs:1491-1498`). Add above each:

```rust
            // D2 §3.2: this path emitted nothing before — the row was terminal but
            // silent, so a live Transfers screen dropped it from Active with
            // nothing to trigger the terminal-list refetch. Same vanishing-row bug
            // the fetch path had; same fix.
```

- [ ] **Step 6: Run the four tests, then the suite**

```bash
cargo test -p athenaeum-core -- \
  a_peer_absent_fetch_leaves_the_row_waiting_and_emits_nothing \
  a_local_fault_fetch_is_failed_and_emits_its_terminal \
  resend_after_failed_fetch_reuses_one_batch_row_and_delivers \
  received_history_keys_on_batch_uuid_not_wire_id
cargo test -p athenaeum-core
```

Expected: the four PASS. The reconcile tests (Task 5's list) are still red — that is expected and is the only permitted red at this point. If anything else is red, stop and report.

- [ ] **Step 7: Mutation check**

Temporarily invert the classification (`if !crate::sharing::types::is_local_fault(&e)`) and re-run the two new tests. Both must fail. Revert.

- [ ] **Step 8: Commit**

```bash
git add crates/athenaeum-core/src/sync/receiver.rs
git commit -m "feat(sync): a fetch that loses its peer stamps Waiting, not Failed; ingest/ack failures announce their terminal"
```

---

### Task 4: The ingest-error and ack-error paths keep their `Failed` verdict

Pins the half of §3.2 that does NOT change, so a later refactor cannot quietly sweep it into `Waiting`. Pure test task — no production change.

**Files:**
- Test: `crates/athenaeum-core/src/sync/receiver.rs` (tests module)

- [ ] **Step 1: Write the tests**

```rust
    /// D2 §3.2: an ack failure is terminal even though the connection died — the
    /// frames are landed and catalogued, so only the verdict is undelivered, and
    /// the ack-replay guard re-acks on redelivery. This is the one place where a
    /// dead connection does NOT mean Waiting, so it is pinned explicitly.
    #[tokio::test]
    async fn an_ack_failure_stays_failed_and_emits_its_terminal() {
        let (store, _transport, emitter, wire) = ack_failure_fixture().await;
        let row = poll_inbound(&store, &wire, InboundState::Failed).await;
        assert_eq!(row.state, InboundState::Failed);
        assert!(
            emitter.kinds().iter().any(|k| k == "sync-finished"),
            "the terminal is announced: {:?}",
            emitter.kinds()
        );
    }
```

Build `ack_failure_fixture()` from the existing `inbound_row_stamps_failed_on_ingest_error` fixture (`receiver.rs:3103`), swapping the injected failure from ingest to ack. Extend `inbound_row_stamps_failed_on_ingest_error` itself with the same `sync-finished` assertion.

- [ ] **Step 2: Run**

```bash
cargo test -p athenaeum-core -- an_ack_failure_stays_failed_and_emits_its_terminal inbound_row_stamps_failed_on_ingest_error
```

Expected: PASS (Task 3 already added the emissions).

- [ ] **Step 3: Commit**

```bash
git add crates/athenaeum-core/src/sync/receiver.rs
git commit -m "test(sync): pin the ingest/ack failures that stay terminal under D2"
```

---

### Task 5: The boot reconcile waits instead of failing — and stops overwriting itself

An interrupted row becomes `Waiting`, its file rows are left alone, and — the part that would otherwise destroy the design — a row that is ALREADY `Waiting` is skipped. Without the skip, a `Waiting` row is non-terminal, so `inbound_active` returns it on every launch and the fallback rewrites its reason to `"interrupted by restart"` at the first restart.

**Files:**
- Modify: `crates/athenaeum-core/src/sync/receiver.rs:686-770` (`reconcile_stale_inbound`)
- Test: `crates/athenaeum-core/src/sync/receiver.rs` (tests module)

- [ ] **Step 1: Convert the tests this task owns**

Rename `startup_reconciles_stale_inbound_rows_to_failed` (`:2577`) to `startup_reconciles_stale_inbound_rows_to_waiting` and change its expected state at `:2618` from `InboundState::Failed` to `InboundState::Waiting`. Change its `last_error` expectation to still be `"interrupted by restart"`.

In `startup_repairs_fully_receipted_stale_inbound_from_receipt_log` (`:2651`), line `:2719`:

```rust
        assert_eq!(
            zombie.state,
            InboundState::Waiting,
            "a receiptless stale row is outstanding, not lost — the sender still owes it"
        );
```

In `decline_survives_restart_reconcile_and_refuses_resend` (`:4350`), line `:4402`:

```rust
            assert_eq!(row.state, InboundState::Waiting, "the reconcile parked the zombie attempt");
```

Rewrite `reconcile_settles_file_rows_then_reannounce_restores` (`:3791`) — its premise is inverted by §3.3. Replace the file-row assertions with:

```rust
        // D2 §3.3: the reconcile no longer settles file rows. They are the resume
        // checkpoint — a restart mid-fetch must not throw away the record of what
        // already arrived.
        assert_eq!(get_inbound(&conn, "pkg").unwrap().unwrap().state, InboundState::Waiting);
        let files = list_inbound_files(&conn, inbound_id).unwrap();
        assert!(
            files.iter().all(|f| f.state != InboundFileState::Failed),
            "no file row is settled by the reconcile: {:?}",
            files.iter().map(|f| f.state).collect::<Vec<_>>()
        );
```

Keep the second half of that test (a re-announce restores the rows via `record_inbound_manifest`) exactly as it is — it still holds.

- [ ] **Step 2: Add the skip test**

```rust
    /// D2 §4: a `Waiting` row is non-terminal, so it comes back from
    /// `inbound_active` on EVERY launch. Without an explicit skip the fallback
    /// would overwrite the preserved reason with "interrupted by restart" — the
    /// first restart after a peer vanishes would destroy exactly what the state
    /// exists to record.
    #[test]
    fn the_reconcile_leaves_an_existing_waiting_row_untouched() {
        let store = test_store();
        {
            let conn = store.lock_conn();
            upsert_inbound_attempt(&conn, "peerhex", "batch-1", "wire-1", 2, 100).unwrap();
            set_inbound_state(&conn, "wire-1", InboundState::Waiting, Some("peer gone: connection lost")).unwrap();
        }

        reconcile_stale_inbound(&store);
        reconcile_stale_inbound(&store); // and again — idempotent

        let conn = store.lock_conn();
        let row = get_inbound(&conn, "wire-1").unwrap().unwrap();
        assert_eq!(row.state, InboundState::Waiting);
        assert_eq!(
            row.last_error.as_deref(),
            Some("peer gone: connection lost"),
            "the original reason survives every restart"
        );
    }
```

- [ ] **Step 3: Run and watch them fail**

```bash
cargo test -p athenaeum-core -- \
  startup_reconciles_stale_inbound_rows_to_waiting \
  startup_repairs_fully_receipted_stale_inbound_from_receipt_log \
  decline_survives_restart_reconcile_and_refuses_resend \
  reconcile_settles_file_rows_then_reannounce_restores \
  the_reconcile_leaves_an_existing_waiting_row_untouched
```

Expected: all FAIL on the state mismatch (`Failed` vs `Waiting`) or the surviving reason.

- [ ] **Step 4: Change the reconcile**

In `reconcile_stale_inbound`, immediately after `for row in stale {`:

```rust
        // D2 §4: a `Waiting` row is already parked with an honest reason and is
        // non-terminal, so it returns from `inbound_active` on every launch.
        // Re-stamping it would replace that reason with "interrupted by restart" —
        // the first restart after a peer vanishes would erase the very thing this
        // state records. Its per-file rows stay as the resume checkpoint too.
        if row.state == InboundState::Waiting {
            continue;
        }
```

Replace the fallback stamp (`receiver.rs:743`):

```rust
        match set_inbound_state(&conn, &row.package_id, InboundState::Waiting, Some("interrupted by restart")) {
            Ok(()) => count += 1,
            Err(e) => tracing::warn!(
                package_id = %row.package_id,
                error = %format!("{e:#}"),
                "inbound startup reconcile write failed"
            ),
        }
```

DELETE the `settle_unsettled_inbound_files(..., InboundFileState::Failed, ..., Some("interrupted by restart"))` block that follows it (`receiver.rs:754-766`), replacing it with:

```rust
        // D2 §3.3: no per-file settle here. The rows record what actually arrived
        // before the restart; a later re-announce refreshes them via
        // `record_inbound_manifest`, and until then they are the resume checkpoint
        // and the file counter's evidence.
```

Update the trailing log line from `"stale inbound rows reconciled to failed after restart"` to `"stale inbound rows parked waiting after restart"`, and the function's doc comment accordingly. Leave the receipt-repair block above completely untouched — it still runs first, still `continue`s, and still settles its file rows (a repaired row IS terminal).

- [ ] **Step 5: Run the five tests, then the suite**

```bash
cargo test -p athenaeum-core
```

Expected: PASS, all green — this is the task that clears the last red from Task 3.

- [ ] **Step 6: Mutation check**

Remove the `if row.state == InboundState::Waiting { continue; }` guard and re-run `the_reconcile_leaves_an_existing_waiting_row_untouched`. It must fail. Restore.

- [ ] **Step 7: Commit**

```bash
git add crates/athenaeum-core/src/sync/receiver.rs
git commit -m "fix(sync): the boot reconcile parks an interrupted row Waiting, keeps its file rows, and never re-stamps itself"
```

---

### Task 6: Cancelling a `Waiting` row terminalizes it, and the row settles in place

Two defects in one path. The backend's `stamp_now` match has no `Waiting` arm, so a declined waiting row would keep `declined_at` and stay `waiting` forever — there is no in-flight fetch whose epilogue would close it. And the frontend refetches terminal rows only on mount and on `sync-finished`, while `cancel_incoming_package` emits nothing — so the just-cancelled row disappears instead of moving to the terminal list.

**Files:**
- Modify: `crates/athenaeum-core/src/api/sync.rs:3345-3357`
- Modify: `src/hooks/useTransferQueue.ts:721-736` (`cancelInbound`)
- Test: `crates/athenaeum-core/src/api/sync.rs` (tests module)

- [ ] **Step 1: Write the failing test**

```rust
    /// D2 §4: declining a `Waiting` row must close it THERE AND THEN. There is no
    /// live fetch whose epilogue would stamp it, and no announce is guaranteed to
    /// ever arrive — without the explicit arm the row keeps `declined_at` and sits
    /// `waiting` forever, permanently undeletable.
    #[test]
    fn cancelling_a_waiting_inbound_row_terminalizes_it_immediately() {
        let ctx = test_ctx();
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            upsert_inbound_attempt(&conn, "peerhex", "batch-1", "wire-1", 2, 100).unwrap();
            set_inbound_state(&conn, "wire-1", InboundState::Waiting, Some("peer gone")).unwrap();
        }

        // No live receiver owns it (control is None) — the standalone case.
        cancel_incoming_package_inner(&ctx, "wire-1", None).unwrap();

        let db = db(&ctx).unwrap();
        let conn = db.conn();
        let row = get_inbound(&conn, "wire-1").unwrap().unwrap();
        assert_eq!(row.state, InboundState::Cancelled, "closed on the spot");
        assert!(row.declined_at.is_some(), "and the decline is final");
        assert!(row.finished_at.is_some(), "a terminal stamps finished_at");
    }
```

Match the existing cancel tests' fixture style in that module; if `cancel_incoming_package` is only reachable through the public async fn, call that and name the test `#[tokio::test]`.

Add the deletion half in the same module — the other side of the same user story:

```rust
    /// D2 §4: a parked row is non-terminal, so history deletion refuses it —
    /// deliberately, and symmetric with the sent side. Cancel first, then delete.
    /// This is why Task 10's Cancel button is not optional.
    #[test]
    fn delete_transfer_history_refuses_a_waiting_received_batch() {
        let ctx = test_ctx();
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            upsert_inbound_attempt(&conn, "peerhex", "batch-1", "wire-1", 2, 100).unwrap();
            set_inbound_state(&conn, "wire-1", InboundState::Waiting, Some("peer gone")).unwrap();
        }

        let err = delete_transfer_history(&ctx, Direction::Received, "batch-1").unwrap_err();
        assert!(
            format!("{err}").to_lowercase().contains("active"),
            "the refusal names the live attempt: {err}"
        );

        let db = db(&ctx).unwrap();
        let conn = db.conn();
        assert!(get_inbound(&conn, "wire-1").unwrap().is_some(), "and the row survives the refusal");
    }
```

Match `delete_transfer_history`'s real signature and its refusal error text at the call site (`api/sync.rs:~3553`, the `matching.iter().any(|(_, s)| !s.is_terminal())` branch) — assert on the substring that function actually produces, not on the one guessed here.

- [ ] **Step 2: Run and watch it fail**

```bash
cargo test -p athenaeum-core cancelling_a_waiting_inbound_row_terminalizes_it_immediately
```

Expected: FAIL — `row.state` is `Waiting`, not `Cancelled`.

- [ ] **Step 3: Add the arm**

In `crates/athenaeum-core/src/api/sync.rs`, the `stamp_now` match:

```rust
    let stamp_now = match row.as_ref().map(|r| r.state) {
        Some(InboundState::Announced) => true,
        // D2 §4: a parked row has no live fetch to interrupt and no guaranteed
        // announce coming — nothing else would ever close it. Stamp here.
        Some(InboundState::Waiting) => true,
        Some(InboundState::Fetching) => control.is_none(),
        _ => false,
    };
```

- [ ] **Step 4: Run**

```bash
cargo test -p athenaeum-core cancelling_a_waiting_inbound_row_terminalizes_it_immediately
```

Expected: PASS.

- [ ] **Step 5: Make the cancelled row settle in place on screen**

In `src/hooks/useTransferQueue.ts`, `cancelInbound`:

```ts
  const cancelInbound = useCallback(
    (packageId: string) =>
      withBusy(`cancelin:${packageId}`, async () => {
        try {
          await api.invoke('cancel_incoming_package', { packageId });
          refresh();
          // D2: the cancel terminalizes the row but emits no event, and the
          // terminal list is otherwise refetched only on mount / `sync-finished`.
          // Without this the row leaves Active with nothing to carry it into
          // Completed — it just vanishes.
          fetchTerminal();
        } catch (err) {
```

Add `fetchTerminal` to the `useCallback` dependency array (`[withBusy, refresh, fetchTerminal, notify]`).

- [ ] **Step 6: Gate and commit**

```bash
cargo test -p athenaeum-core && npx tsc --noEmit
git add crates/athenaeum-core/src/api/sync.rs src/hooks/useTransferQueue.ts
git commit -m "fix(sync): declining a waiting inbound row closes it immediately and settles in place on screen"
```

---

### Task 7: A revoke for a parked row, and the tag it must not pin

Two membership consequences of non-terminality, verified together because both are about what `Waiting` now lets through.

`handle_revoke` returns early on a terminal row — a fetch failure used to be terminal, so a later revoke was a no-op; now it runs the full bookkeeping. And `release_orphan_in_flight_tags` protects any wire id in `inbound_active`, so a `Waiting` row would pin its partial download forever and make Clean-up free strictly less than it does today (spec §3.5).

**Files:**
- Modify: `crates/athenaeum-core/src/api/sync.rs:3874-3891` (`release_orphan_in_flight_tags`)
- Test: `crates/athenaeum-core/src/sync/receiver.rs`, `crates/athenaeum-core/src/api/sync.rs`

- [ ] **Step 1: Write both failing tests**

In `receiver.rs`:

```rust
    /// D2 §4: a revoke arriving for a parked row is no longer ignored — the row is
    /// non-terminal now. It must terminalize correctly and write each file's
    /// history exactly once.
    #[tokio::test]
    async fn a_revoke_for_a_waiting_row_terminalizes_it_once() {
        let (store, wire, inbound_id) = waiting_row_fixture().await;

        handle_revoke_for_test(&store, &wire, RevokeReason::Cancelled).await;

        let conn = store.lock_conn();
        let row = get_inbound(&conn, &wire).unwrap().unwrap();
        assert_eq!(row.state, InboundState::Cancelled, "the revoke closes the parked row");
        let history = list_history_for_package(&conn, &wire).unwrap();
        let files = list_inbound_files(&conn, inbound_id).unwrap();
        assert_eq!(history.len(), files.len(), "one history row per file, no duplicates");
    }
```

Build `waiting_row_fixture()` from the Task 3 fetch-failure fixture (it already leaves a `Waiting` row with per-file rows). Use whatever `handle_revoke` entry point the existing `revoke_failed_maps_to_failed` test (`:4566`) uses.

In `api/sync.rs`:

```rust
    /// D2 §3.5: a `Waiting` row must NOT protect its in-flight blob tag. Today a
    /// dead fetch is terminal, so Clean-up releases the tag and the GC reclaims the
    /// partial bytes; if a parked row kept the protection, the button would free
    /// strictly less than before — the opposite of what D2 is for.
    #[tokio::test]
    async fn cleanup_releases_the_in_flight_tag_of_a_waiting_row() {
        let (ctx, sync, transport) = cleanup_fixture().await;
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            upsert_inbound_attempt(&conn, "peerhex", "batch-1", "wire-1", 2, 100).unwrap();
            set_inbound_state(&conn, "wire-1", InboundState::Waiting, Some("peer gone")).unwrap();
        }
        transport.push_in_flight_tag("wire-1");

        let result = cleanup_finished_transfers(&ctx, &sync).await.unwrap();

        assert_eq!(result.tags_released, 1, "the parked row's partial bytes are reclaimable");
        assert!(transport.released().contains(&"wire-1".to_string()));
    }

    /// The other side of the same rule: a genuinely live fetch keeps its tag.
    #[tokio::test]
    async fn cleanup_keeps_the_in_flight_tag_of_a_fetching_row() {
        let (ctx, sync, transport) = cleanup_fixture().await;
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            upsert_inbound_attempt(&conn, "peerhex", "batch-2", "wire-2", 2, 100).unwrap();
            set_inbound_state(&conn, "wire-2", InboundState::Fetching, None).unwrap();
        }
        transport.push_in_flight_tag("wire-2");

        let result = cleanup_finished_transfers(&ctx, &sync).await.unwrap();

        assert_eq!(result.tags_released, 0, "a live fetch owns its tag");
    }
```

Reuse the fixture and mock-transport hooks of the existing `cleanup_removes_terminal_payloads_and_flips_resendable` test (`api/sync.rs:~5103`); add `push_in_flight_tag`/`released()` to the mock only if they do not already exist.

- [ ] **Step 2: Run and watch the tag test fail**

```bash
cargo test -p athenaeum-core -- \
  a_revoke_for_a_waiting_row_terminalizes_it_once \
  cleanup_releases_the_in_flight_tag_of_a_waiting_row \
  cleanup_keeps_the_in_flight_tag_of_a_fetching_row
```

Expected: `cleanup_releases_…` FAILS (`tags_released == 0` — the waiting row protects it). The revoke test should already PASS: `handle_revoke` writes history rows from `list_inbound_files`, and a fetch that failed landed nothing, so there is nothing to duplicate. If it fails, that is a real finding — report it before changing `handle_revoke`.

- [ ] **Step 3: Narrow the tag-protection set**

In `release_orphan_in_flight_tags`:

```rust
    let active: HashSet<String> = {
        let db = db(ctx)?;
        let conn = db.conn();
        inbound_active(&conn)?
            .into_iter()
            // D2 §3.5: `Waiting` is non-terminal but NOT live. Keeping its tag
            // would pin a partial download indefinitely — the dedup handshake
            // renegotiates the file list on the next attempt anyway, so at worst
            // the peer re-sends what the GC reclaimed. An unbounded pin the user
            // cannot clear is the worse trade.
            .filter(|r| r.state != InboundState::Waiting)
            .map(|r| r.package_id)
            .collect()
    };
```

Update the function's doc comment (`api/sync.rs:3845-3853`) to say the protected set is the non-terminal rows minus `Waiting`.

- [ ] **Step 4: Run and gate**

```bash
cargo test -p athenaeum-core
```

Expected: PASS.

- [ ] **Step 5: Mutation check**

Remove the `.filter(...)` and re-run `cleanup_releases_the_in_flight_tag_of_a_waiting_row` — it must fail. Restore, then remove instead the `state != Fetching` reality by filtering out `Fetching` too and confirm `cleanup_keeps_the_in_flight_tag_of_a_fetching_row` fails. Restore.

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/api/sync.rs crates/athenaeum-core/src/sync/receiver.rs
git commit -m "fix(sync): a waiting row does not pin its in-flight blob tag; pin the revoke path for a parked row"
```

---

### Task 8: `InboundFileState::Fetched` — the counter climbs

The receiver gains the sender's `Uploaded` rung, counted through the same SQL. The write site is NOT a one-word relabel: the sink's `file_seen` map fires its first-tick arm unconditionally, so a file that completes within one tick — resumed, dedup'd, small, or zero-byte — could never reach the completion arm.

**Files:**
- Modify: `crates/athenaeum-core/src/sync/models.rs:476-515`
- Modify: `crates/athenaeum-core/src/sync/store.rs:1962-1990` (doc + predicate)
- Modify: `crates/athenaeum-core/src/sync/receiver.rs:1275-1305` (the sink)
- Modify: `src/components/transfers/presentation.ts:220-233`
- Test: `crates/athenaeum-core/src/sync/store.rs`, `crates/athenaeum-core/src/sync/receiver.rs`

**Interfaces:**
- Produces: `InboundFileState::Fetched` → `"fetched"`; `grouped_file_counts` counts it as done.

- [ ] **Step 1: Write the failing counter test**

In `crates/athenaeum-core/src/sync/store.rs` tests, mirroring the outbound-only pin at `:3622`:

```rust
    /// D2 §3.4: the shared predicate must make the RECEIVE side climb too. The
    /// existing mixed-state pin is outbound-only, so a `fetched` term added
    /// without this test would pass while never being exercised inbound.
    #[test]
    fn grouped_file_counts_classifies_mixed_inbound_states() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(DDL_INBOUND_FILES).unwrap();
        let ins = |rel: &str, state: &str, outcome: Option<&str>| {
            conn.execute(
                "INSERT INTO sync_inbound_files (inbound_id, rel_path, state, byte_size, bytes_done, outcome)
                 VALUES (1, ?1, ?2, 10, 0, ?3)",
                params![rel, state, outcome],
            )
            .unwrap();
        };
        ins("a", "announced", None);
        ins("b", "fetching", None);
        ins("c", "fetched", None);   // ← the new rung: bytes in, verdict pending
        ins("d", "done", Some("ingested"));
        ins("e", "failed", None);
        ins("f", "done", Some("rejected: checksum"));

        let counts = inbound_file_counts(&conn, &[1]).unwrap();
        let c = counts.get(&1).unwrap();
        assert_eq!(c.total, 6);
        assert_eq!(c.done, 2, "fetched + ingested-done — a rejected done is not done");
        assert_eq!(c.failed, 2, "the failed row and the rejected-outcome row");
    }
```

- [ ] **Step 2: Write the failing sink and settle tests**

Two sink tests go in `crates/athenaeum-core/src/sync/receiver.rs` tests; `a_fetched_row_settles_at_terminal_like_a_fetching_one` goes in `crates/athenaeum-core/src/sync/store.rs` tests next to the counter test from Step 1, since it exercises a store function:

```rust
    /// D2 §3.4: the sink's first-tick arm fires unconditionally, so a file whose
    /// FIRST progress tick already carries full bytes — a resumed, dedup'd, small
    /// or zero-byte file — would be stranded in `fetching` forever. The target
    /// state must be computed from the tick, not from which arm ran.
    #[tokio::test]
    async fn a_file_that_completes_in_one_tick_still_reaches_fetched() {
        let (store, inbound_id, sink) = fetch_sink_fixture().await;

        // One and only one tick, already complete.
        sink(FetchEvent::File { name: "sub/frame1.fits".to_string(), bytes_done: 500, bytes_total: 500 });
        // And a zero-byte file, whose bytes_total is 0.
        sink(FetchEvent::File { name: "sub/empty.txt".to_string(), bytes_done: 0, bytes_total: 0 });

        let conn = store.lock_conn();
        let files = list_inbound_files(&conn, inbound_id).unwrap();
        let by = |p: &str| files.iter().find(|f| f.rel_path == p).unwrap().state;
        assert_eq!(by("sub/frame1.fits"), InboundFileState::Fetched);
        assert_eq!(by("sub/empty.txt"), InboundFileState::Fetched, "zero-byte files complete too");
    }

    /// D2 §3.4: `settle_unsettled_inbound_files` keys on `state <> 'done'`, so a
    /// `fetched` row settles at a terminal exactly like a `fetching` one — no
    /// change needed there, pinned so a later edit cannot break it silently.
    #[test]
    fn a_fetched_row_settles_at_terminal_like_a_fetching_one() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(DDL_INBOUND_FILES).unwrap();
        for (rel, st) in [("a", "fetching"), ("b", "fetched"), ("c", "done")] {
            conn.execute(
                "INSERT INTO sync_inbound_files (inbound_id, rel_path, state, byte_size, bytes_done)
                 VALUES (1, ?1, ?2, 10, 0)",
                params![rel, st],
            )
            .unwrap();
        }

        settle_unsettled_inbound_files(&conn, 1, InboundFileState::Done, Some("cancelled"), None).unwrap();

        let states = list_inbound_files(&conn, 1).unwrap();
        assert!(
            states.iter().all(|f| f.state == InboundFileState::Done),
            "fetching AND fetched both settle: {:?}",
            states.iter().map(|f| (f.rel_path.clone(), f.state)).collect::<Vec<_>>()
        );
        let c = states.iter().find(|f| f.rel_path == "c").unwrap();
        assert_eq!(c.outcome, None, "an already-done row keeps its own verdict");
    }

    /// And the ordinary path still walks the two rungs.
    #[tokio::test]
    async fn a_file_walks_fetching_then_fetched() {
        let (store, inbound_id, sink) = fetch_sink_fixture().await;

        sink(FetchEvent::File { name: "a.fits".to_string(), bytes_done: 100, bytes_total: 500 });
        {
            let conn = store.lock_conn();
            let files = list_inbound_files(&conn, inbound_id).unwrap();
            assert_eq!(files[0].state, InboundFileState::Fetching);
        }
        sink(FetchEvent::File { name: "a.fits".to_string(), bytes_done: 500, bytes_total: 500 });
        let conn = store.lock_conn();
        let files = list_inbound_files(&conn, inbound_id).unwrap();
        assert_eq!(files[0].state, InboundFileState::Fetched);
        assert_eq!(files[0].bytes_done, 500);
    }
```

`fetch_sink_fixture()` must build the sink exactly as `handle_announce` does. Extract the sink construction (`receiver.rs:1244-1310`) into a standalone `fn build_fetch_sink(...) -> FetchSink` first if it is not already callable in isolation — that extraction is part of this step, and is what makes the sink testable at all.

- [ ] **Step 3: Run and watch all four fail**

```bash
cargo test -p athenaeum-core -- \
  grouped_file_counts_classifies_mixed_inbound_states \
  a_fetched_row_settles_at_terminal_like_a_fetching_one \
  a_file_that_completes_in_one_tick_still_reaches_fetched \
  a_file_walks_fetching_then_fetched
```

Expected: FAIL — `no variant named Fetched`.

- [ ] **Step 4: Add the variant**

In `crates/athenaeum-core/src/sync/models.rs`, `InboundFileState`, after `Fetching`:

```rust
    /// Bytes complete, verdict pending — the receive-side twin of
    /// [`OutboundFileState::Uploaded`] (D2 §3.4). Counted as done by
    /// [`grouped_file_counts`](super::store::grouped_file_counts) through the same
    /// predicate, which is what makes the receiver's file counter climb during a
    /// transfer instead of sitting at zero until ingest.
    Fetched,
```

Add `InboundFileState::Fetched => "fetched",` to `as_str` and `"fetched" => InboundFileState::Fetched,` to `from_db`. Update the enum's type doc: the walk is now `Announced → Fetching → Fetched → Done`.

- [ ] **Step 5: Widen the shared predicate**

In `crates/athenaeum-core/src/sync/store.rs`, `grouped_file_counts`:

```rust
                SUM(CASE WHEN (state = 'done' OR state = 'uploaded' OR state = 'fetched') \
                          AND NOT (state = 'failed' OR COALESCE(outcome,'') LIKE 'rejected%') \
                         THEN 1 ELSE 0 END) \
```

Rewrite the now-false `inbound_file_counts` doc (`store.rs:1962-1965`):

```rust
/// The receive-side twin of [`outbound_file_counts`] — ONE grouped query over
/// `sync_inbound_files`. Both directions share the same "done" predicate: the
/// sender's `uploaded` and the receiver's `fetched` are the same rung — bytes
/// complete, verdict pending — so both counters climb during a transfer for the
/// same reason (D2 §3.4). `state = 'uploaded'` never occurs inbound and
/// `state = 'fetched'` never occurs outbound; the shared CASE simply admits both.
```

Also update the `grouped_file_counts` doc immediately below it, which explains the `COALESCE` guard, to mention `fetched` alongside `uploaded`.

- [ ] **Step 6: Fix the sink**

Replace the tracker block in the extracted `build_fetch_sink` with:

```rust
                if track_files {
                    // D2 §3.4: compute the target state from THIS tick and write
                    // whenever it changes — the sender's shape. The old scheme keyed
                    // the write on which arm ran, so a file whose first tick already
                    // carried full bytes took the first-tick arm (writing `fetching`)
                    // and could never reach the completion arm: resumed, dedup'd,
                    // one-tick and zero-byte files were stranded. The live bar still
                    // rides the event stream; the DB row is the restart checkpoint.
                    let target = if bytes_done >= bytes_total {
                        InboundFileState::Fetched
                    } else {
                        InboundFileState::Fetching
                    };
                    let mut map = file_seen.lock().expect("inbound file_seen mutex poisoned");
                    let write = map.get(&name).copied() != Some(target);
                    if write {
                        map.insert(name.clone(), target);
                        let conn = store.lock_conn();
                        if let Err(e) = set_inbound_file_state(
                            &conn,
                            inbound_id,
                            &name,
                            target,
                            bytes_done,
                            None,
                            None,
                        ) {
                            tracing::warn!(inbound_id, rel_path = %name, error = %format!("{e:#}"), "inbound file state write failed");
                        }
                    }
                }
```

Change the tracker's type from `HashMap<String, bool>` to `HashMap<String, InboundFileState>` and update its declaration comment. Note `bytes_done >= bytes_total` is true for a zero-byte file (`0 >= 0`), which is intended — the old code's `bytes_total > 0` guard is exactly what stranded them.

- [ ] **Step 7: Teach the file chip the new rung**

In `src/components/transfers/presentation.ts`:

```ts
export function fileStateChipClass(state: string): string {
  switch (state) {
    case 'done':
    case 'uploaded':
    case 'fetched': // D2: the receive-side twin of `uploaded` — bytes in, verdict pending
      return CHIP_ACCENT;
```

- [ ] **Step 8: Run everything**

```bash
cargo test -p athenaeum-core && npx tsc --noEmit
```

Expected: PASS.

- [ ] **Step 9: Mutation check**

Remove `OR state = 'fetched'` from the predicate → `grouped_file_counts_classifies_mixed_inbound_states` must fail. Restore. Change `bytes_done >= bytes_total` to `bytes_total > 0 && bytes_done >= bytes_total` → `a_file_that_completes_in_one_tick_still_reaches_fetched` must fail on the zero-byte case. Restore.

- [ ] **Step 10: Commit**

```bash
git add crates/athenaeum-core/src/sync/models.rs crates/athenaeum-core/src/sync/store.rs \
        crates/athenaeum-core/src/sync/receiver.rs src/components/transfers/presentation.ts
git commit -m "feat(sync): InboundFileState::Fetched — the receiver's file counter climbs through the sender's predicate"
```

---

### Task 9: The receive-side cleanup pass

`remove_terminal_payload_dirs` reads `sync_outbound` only, so on a receive-only device the button has nothing in scope by construction. Meanwhile three receiver failure paths leave a full second copy of the batch in `<sync>/staging/<wire_id>` that nothing ever removes.

**Files:**
- Modify: `crates/athenaeum-core/src/api/sync.rs` (new `remove_terminal_staging_dirs`, wire into `cleanup_finished_transfers`, extend `TransferStorage`)
- Modify: `src/types/models.ts` (regenerated)
- Modify: the Settings sync section that renders `TransferStorage` (find with `rg -n 'blobsBytes' src/`)
- Test: `crates/athenaeum-core/src/api/sync.rs`

**Interfaces:**
- Produces: `TransferStorage.staging_bytes: u64` (`stagingBytes` in TS); `TransferCleanup.staging_dirs: u32` and `staging_bytes: u64`.

- [ ] **Step 1: Write the failing test**

```rust
    /// D2 §3.5: the button's receive-side half. A terminal (or row-less) staging
    /// tree is dead weight — three receiver failure paths leave one behind and
    /// nothing has ever removed it. A live or parked row's staging is untouched:
    /// it is the resume target.
    #[tokio::test]
    async fn cleanup_removes_terminal_staging_trees_and_keeps_live_ones() {
        let (ctx, sync, _transport) = cleanup_fixture().await;
        let (sync_dir, _) = sync_paths(&ctx).unwrap();
        let staging = sync_dir.join("staging");
        let mk = |wire: &str| {
            let d = staging.join(wire);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("payload.fits"), vec![0u8; 1024]).unwrap();
            d
        };
        let done_dir = mk("wire-done");
        let waiting_dir = mk("wire-waiting");
        let orphan_dir = mk("wire-orphan"); // no row at all
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            upsert_inbound_attempt(&conn, "p", "b1", "wire-done", 1, 10).unwrap();
            set_inbound_state(&conn, "wire-done", InboundState::Done, None).unwrap();
            upsert_inbound_attempt(&conn, "p", "b2", "wire-waiting", 1, 10).unwrap();
            set_inbound_state(&conn, "wire-waiting", InboundState::Waiting, Some("peer gone")).unwrap();
        }

        let result = cleanup_finished_transfers(&ctx, &sync).await.unwrap();

        assert!(!done_dir.exists(), "a terminal row's staging is dead weight");
        assert!(!orphan_dir.exists(), "so is a staging tree with no row at all");
        assert!(waiting_dir.exists(), "a parked row's staging is its resume target");
        assert_eq!(result.staging_dirs, 2);
        assert!(result.staging_bytes >= 2048, "got {}", result.staging_bytes);
    }
```

- [ ] **Step 2: Run and watch it fail**

```bash
cargo test -p athenaeum-core cleanup_removes_terminal_staging_trees_and_keeps_live_ones
```

Expected: FAIL — `no field staging_dirs on TransferCleanup`.

- [ ] **Step 3: Add the pass**

In `crates/athenaeum-core/src/api/sync.rs`, next to `remove_terminal_payload_dirs`:

```rust
/// Cleanup pass 2 — remove the RECEIVE-side staging trees of finished batches
/// (D2 §3.5).
///
/// `remove_terminal_payload_dirs` walks `sync_outbound`, so on a receive-only
/// device the Clean-up button had nothing in scope by construction — while three
/// receiver failure paths (fetch, ingest, ack) return without removing
/// `<sync>/staging/<wire_id>`, leaving a full second copy of the batch on disk with
/// nothing to reclaim it. This is that reclaimer.
///
/// A directory is removed when its wire id belongs to a TERMINAL `sync_inbound` row
/// or to no row at all. A non-terminal row's staging is kept — including a
/// [`Waiting`](InboundState::Waiting) row's, which is its resume target. Returns
/// `(dirs_removed, bytes_reclaimed)`; these are freed-now bytes, unlike released
/// tags.
fn remove_terminal_staging_dirs(ctx: &ServiceContext) -> Result<(u32, u64), ApiError> {
    let (sync_dir, _db_path) = sync_paths(ctx)?;
    let staging_root = sync_dir.join("staging");
    let entries = match std::fs::read_dir(&staging_root) {
        Ok(e) => e,
        Err(_) => return Ok((0, 0)), // never materialized — nothing to reclaim
    };
    let live: HashSet<String> = {
        let db = db(ctx)?;
        let conn = db.conn();
        inbound_active(&conn)?
            .into_iter()
            .map(|r| r.package_id)
            .collect()
    };
    let mut dirs = 0u32;
    let mut bytes = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(wire) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if live.contains(wire) {
            continue; // a live or parked attempt owns this tree
        }
        let sz = dir_size_bytes(&path);
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {
                dirs += 1;
                bytes += sz;
                tracing::info!(path = %path.display(), "terminal staging tree removed");
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "staging tree removal failed")
            }
        }
    }
    Ok((dirs, bytes))
}
```

Note this uses the FULL `inbound_active` set — a `Waiting` row keeps its staging (unlike its blob tag in Task 7, which is a GC root, not the resume target).

- [ ] **Step 4: Widen the result and the footprint**

`TransferCleanup` gains:

```rust
    /// Receive-side staging trees of finished batches removed from
    /// `<sync>/staging/` (freed immediately, like `payload_bytes`).
    pub staging_dirs: u32,
    /// Bytes reclaimed by removing those staging trees.
    pub staging_bytes: u64,
```

`cleanup_finished_transfers`:

```rust
    let (payload_dirs, payload_bytes) = remove_terminal_payload_dirs(ctx)?;
    let (staging_dirs, staging_bytes) = remove_terminal_staging_dirs(ctx)?;
    let tags_released = release_orphan_in_flight_tags(ctx, sync).await?;
    tracing::info!(payload_dirs, payload_bytes, staging_dirs, staging_bytes, tags_released, "finished-transfer cleanup");
    Ok(TransferCleanup { payload_dirs, payload_bytes, staging_dirs, staging_bytes, tags_released })
```

`TransferStorage` gains:

```rust
    /// Total bytes under `<sync>/staging/` — the receive side's in-progress and
    /// abandoned batch trees. Counted separately from `packages_bytes` (which is
    /// the SEND side's payloads) so the figure the Clean-up button sits next to
    /// includes what the button can actually move.
    pub staging_bytes: u64,
```

and in `get_transfer_storage`:

```rust
    let staging_bytes = dir_size_bytes(&sync_dir.join("staging"));
    Ok(TransferStorage { packages_bytes, packages_count, blobs_bytes, staging_bytes })
```

Update the `TransferCleanup` type doc: payload AND staging bytes are freed now; only `tags_released` is delayed.

- [ ] **Step 5: Run and regenerate**

```bash
cargo test -p athenaeum-core
TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract
git diff --stat src/types/models.ts
```

- [ ] **Step 6: Surface the new numbers**

```bash
rg -n 'blobsBytes|packagesBytes|tagsReleased' src/
```

In the Settings sync section that renders them, add the staging figure to the storage breakdown and the staging dirs to the result toast, using existing design tokens and the existing byte formatter. Keep the copy factual: staging and payload bytes are freed now; released tags come back within about fifteen minutes.

- [ ] **Step 7: Gate and commit**

```bash
cargo test -p athenaeum-core && npx tsc --noEmit
git add -A
git commit -m "feat(sync): Clean up reclaims the receive side too — terminal staging trees, and a staging figure in the storage footprint"
```

---

### Task 10: The parked row on screen

The backend mapping plus the two UI seams that make a `Waiting` received row usable. The `canCancel` gate is the one that turns a design defect into an unusable row: without it the row shows no Cancel button while `delete_transfer_history` refuses to delete it.

**Files:**
- Modify: `crates/athenaeum-core/src/api/sync.rs:1592` (`inbound_summary`)
- Modify: `src/components/transfers/TransferRow.tsx:250-252`
- Modify: `src/components/transfers/TransfersPanel.tsx:376+` (the incoming branch)
- Test: `crates/athenaeum-core/src/api/sync.rs`

- [ ] **Step 1: Write the failing test**

```rust
    /// D2 §3.6: the two sides meet in one chip. A parked received row renders as
    /// `waiting_peer` — the same label, subline and filter bucket D1 gave the send
    /// side — even though the mechanics beneath differ (stored vs derived).
    #[test]
    fn a_waiting_inbound_row_displays_as_waiting_peer() {
        let row = inbound_row_fixture(InboundState::Waiting, Some("peer gone: connection lost"));
        let summary = inbound_summary(&row, &HashMap::new(), &HashMap::new());
        assert_eq!(summary.display_state, "waiting_peer");
        assert_eq!(summary.state, InboundState::Waiting, "the raw state still ships alongside");
    }

    /// Every other state still echoes raw — the mapping is one arm, not a rewrite.
    #[test]
    fn other_inbound_states_still_echo_raw() {
        for st in [InboundState::Announced, InboundState::Fetching, InboundState::Done] {
            let row = inbound_row_fixture(st, None);
            let summary = inbound_summary(&row, &HashMap::new(), &HashMap::new());
            assert_eq!(summary.display_state, st.as_str());
        }
    }
```

Match `inbound_summary`'s real signature at the call site; write `inbound_row_fixture` as a small constructor of `InboundRow` in the tests module.

- [ ] **Step 2: Run and watch it fail**

```bash
cargo test -p athenaeum-core -- a_waiting_inbound_row_displays_as_waiting_peer other_inbound_states_still_echo_raw
```

Expected: the first FAILS (`"waiting"` != `"waiting_peer"`), the second PASSES.

- [ ] **Step 3: Map the state**

In `inbound_summary`:

```rust
        // D2 §3.6: one chip for both directions. The send side derives
        // `waiting_peer` from an error-class prefix; the receive side has it as a
        // stored state. Same words to the user: the device is unreachable and this
        // resumes when it is back.
        display_state: match row.state {
            InboundState::Waiting => "waiting_peer".to_string(),
            other => other.as_str().to_string(),
        },
```

- [ ] **Step 4: Give the parked row its Cancel button**

In `src/components/transfers/TransferRow.tsx`:

```tsx
  // Cancel: ANY non-terminal row, both directions. The inbound arm used to name
  // its two live states explicitly; D2 added a third non-terminal one (`waiting`),
  // and an enumerated gate would have left a parked row with no Cancel button
  // while `delete_transfer_history` refuses to delete a non-terminal row — a row
  // the user could not get rid of at all. `cancel_incoming_package` handles every
  // non-terminal inbound state, including the parked one.
  const canCancel = !row.terminal;
```

Verify `row.terminal` is computed for inbound rows from the backend's terminal flag rather than a hardcoded state list (`rg -n 'terminal:' src/hooks/useTransferQueue.ts`); if it is a local list, add `waiting` to the non-terminal side there and note it in the commit message.

- [ ] **Step 5: Give the incoming rows their subline**

In `src/components/transfers/TransfersPanel.tsx`, the `incoming.map((row) => {` branch, mirror the outbound branch's `const subline = displayStateSubline(row.displayState);` and render it in the same slot with the same tokens. Without this the panel shows the chip with no explanation of why nothing is moving.

- [ ] **Step 6: Gate**

```bash
cargo test -p athenaeum-core && npx tsc --noEmit
```

- [ ] **Step 7: Manual check of the filter side effect**

Confirm in `src/pages/Transfers.tsx:135` that a waiting inbound row lands in the Waiting bucket and therefore leaves Receiving — expected and symmetric with the send side. No code change; note it in the commit message so it is not later read as a regression.

- [ ] **Step 8: Commit**

```bash
git add crates/athenaeum-core/src/api/sync.rs src/components/transfers/TransferRow.tsx src/components/transfers/TransfersPanel.tsx
git commit -m "feat(transfers): a parked received row reads as waiting for peer, and can be cancelled"
```

---

### Task 11: Close the branch — Perseus, both backends, release note

- [ ] **Step 1: Check the Perseus web UI**

```bash
rg -n "announced|fetching|ingesting|cancelled|'failed'|\"failed\"" crates/perseus/src/web/app.js
```

If it switches on inbound state strings, add a `waiting` arm rendering the same "waiting for peer" wording as the app; if it renders the string verbatim, no change. Either way state the finding in the commit message — the spec flags this as verify-not-assume.

- [ ] **Step 2: Confirm both backends still mirror**

```bash
rg -n "get_transfer_storage|cleanup_finished_transfers" crates/athenaeum-tauri/src crates/athenaeum-web/src
```

Both wrappers pass the struct through, so the two new fields need no route change — confirm that by reading, not by assuming.

- [ ] **Step 3: Full gates**

```bash
cargo build --workspace --all-targets
cargo test --workspace
npx tsc --noEmit
git status --porcelain   # must be clean apart from intended changes
```

- [ ] **Step 4: Release note**

Add to `RELEASE_NOTES.md` under **Bug Fixes** (user-facing English, no internal names):

- A transfer whose sending device goes offline now shows as waiting for that device instead of failed, and resumes on its own when the device returns.
- The received-files counter now climbs during a transfer instead of showing zero until the very end.
- "Clean up finished transfers" now reclaims the receiving side's leftover data too, and the storage figure accounts for it.

And under **Changes**, the compatibility line the spec requires:

- Transfer records written by this version cannot be read by older versions — update every device together.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "docs: release notes for the receive-side waiting state and progress fixes"
```

- [ ] **Step 6: Hand off**

Report: every task's gate result, the Perseus finding from Step 1, and the owner-side smoke that remains — two machines, close the sender mid-transfer, confirm the received row reads *waiting for peer* rather than *failed*, confirm the file counter climbed before the interruption, restart the receiver and confirm the reason survives, then bring the sender back and confirm the transfer completes without any user action.
