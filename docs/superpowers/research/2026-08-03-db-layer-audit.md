# DB Layer & Write-Path Audit — 2026-08-03

Full-depth audit of the SQLite catalog layer and every path that writes to it:
the CRUD layer (`db/operations*.rs`, `db/equipment.rs`), schema + migrations
(`db/schema.rs`), pool/connection/transaction discipline across all of
`athenaeum-core` and both command boundaries, and the end-to-end ingest paths
(scanner, calibration sets, masters, light calibrations, archive, file-op,
Black Hole). Four parallel review passes (CRUD / schema+migrations /
connection-discipline / write-paths), with every Critical finding re-verified
by hand against the code before this document was written. The
foreign-key-enforcement question was settled against the pinned dependency
source: bundled libsqlite3-sys 0.38.1 compiles with
`-DSQLITE_DEFAULT_FOREIGN_KEYS=1` (`build.rs:126`), so **FK enforcement is ON
for every connection** — the comments in `archive/restore.rs:1079-1086` and
`archive/planner.rs:1125-1127` are correct. It was also verified empirically
that FK CASCADE deletes DO fire `AFTER DELETE` triggers (needed for the I9
fix).

**Companion fix plan:** `docs/superpowers/plans/2026-08-03-db-hygiene-hardening.md`

## Overall verdict

The foundation is sound: r2d2 pool with WAL + busy_timeout, real FK
enforcement, a correct RAII `SavepointGuard`, `register_master`/`delete_master`
fully transactional and test-pinned, the scanner's re-parse correctly nested
via SAVEPOINT, `files.path` under a genuine UNIQUE constraint, all 26
guarded-ALTER migrations verified correct, and the two 12-step table rebuilds
crash-safe (WAL rolls back an interrupted batch on next open). The
`path_prefix_upper` range-predicate machinery and the `last_insert_rowid`
discipline around `INSERT OR IGNORE`/UPSERT are genuinely careful work.

**The systemic disease is uneven transaction discipline.** Nearly every defect
below has a correctly-implemented twin sitting nearby in the same codebase:
`rebuild_duplicate_groups_cache` was migrated to `SavepointGuard` while its
sibling `rebuild_folder_similarity_cache` was not; `delete_master` wraps the
master-unregister sequence in one transaction while Black Hole / Void runs the
identical sequence statement-by-statement in autocommit; the master-build and
light-cal workers release the pooled connection around long compute while the
archive/file-op executors hold theirs for the whole operation; the Tauri
plate-solve worker is panic-safe while its Axum mirror silently swallows
panics. The team knows every one of these patterns — they just weren't applied
everywhere. On top of that sit one real injection bug and two silent
error-swallows in the scanner that can permanently lose catalog data.

## Critical findings

### C1 — SQL injection in `get_black_hole_files`

*Verified by hand.*

- `db/operations_blackhole.rs:203-211`: `filter_by_source` is spliced into the
  SQL text with `format!("... WHERE bh.from_where = '{}' ...", source)` — the
  only place in the entire DB layer where caller data enters the query text
  instead of a `?` bind.
- Reachable over the network in the web/Docker build: `POST
  /api/get_black_hole_files` deserializes `{"filter": ...}` straight from the
  JSON body into this parameter (`routes/duplicates.rs:162-169`). A value like
  `x' UNION SELECT id,path,filename,... FROM settings --` exfiltrates
  arbitrary table contents through the endpoint's fixed column shape. Stacked
  `;` statements don't apply (`sqlite3_prepare_v2` compiles only the first
  statement), but single-statement UNION/boolean injection does.
- Fix is a one-line parameterization.

### C2 — Scanner silently swallows `insert_fits_header` failure

*Verified by hand.*

- `scanner/mod.rs:2205`: `let _ = insert_fits_header(conn, file_id, header);`
  in the parallel-scan write loop — no log, no error entry. Direct violation
  of the never-swallow rule; the sequential `process_file` path logs the same
  failure correctly.
- Consequence: the frame permanently lacks its `fits_header` blob — per-field
  metadata revert and light-cal header copy-through are broken for that frame
  with zero diagnostic trail.

### C3 — A failed `insert_frame` commits a permanently-invisible `files` row

*Verified by hand (write loop + classification + the reparse self-heal that
cannot reach it).*

- `scanner/mod.rs:2169-2222`: the batch transaction commits every successful
  `insert_file` even when the matching `insert_frame` failed (the error goes
  into the scan's error list and the loop continues). The committed frameless
  `files` row then matches `(size, modified_at)` on every later scan and is
  classified **unchanged** (`scanner/mod.rs:1754-1783` — the classification
  never checks `frames` existence), so it is skipped forever.
- The self-heal for exactly this shape already exists —
  `reparse_and_update_in_place`'s `frame_count == 0` branch
  (`scanner/mod.rs:1153-1174`, its comment even describes this scenario) — but
  it only runs for files classified *modified*, which a frameless row never
  is. The frame's metadata is permanently lost to the catalog with no
  recovery path and no orphan-detection job anywhere.

### C4 — `rebuild_folder_similarity_cache`: raw BEGIN with zero rollback, holding the write lock through O(F²) compute, on every scan

*Verified by hand.*

- `db/operations_blackhole.rs:405-435`: raw `BEGIN TRANSACTION`, then DELETE,
  then `find_duplicate_folders` (loads every non-black-holed file's hash into
  memory and does pairwise folder comparison **inside the open write
  transaction**), then an INSERT loop, then COMMIT — every statement bare `?`,
  no rollback on any error path. Called unconditionally at the end of every
  scan (`scanner/mod.rs:2340`).
- Two independent defects: (a) any mid-function error leaks an open
  write-lock-holding transaction back into the pool (only the
  `Database::conn()` checkout-time defensive rollback eventually clears it —
  until then every other writer gets "database is locked" after the 5 s
  busy_timeout); (b) even on success, the whole O(F²) compute runs while
  holding SQLite's sole writer lock, starving scanner/archive/sync writes on
  large catalogs.
- The documented W2-T7 (v0.2.2) hygiene pass migrated its sibling
  `rebuild_duplicate_groups_cache` to `SavepointGuard` for exactly this reason
  (`db/operations.rs:5949-5953`); this function was left behind.

### C5 — Black Hole / Void run the master-unregister sequence with no transaction

*Verified by hand (core functions + both backends' callers).*

- `unregister_master_set` (`db/master_unregister.rs:30-120`) is six sequential
  UPDATE/DELETE statements whose doc contract says "runs in the CALLER's
  transaction". `add_to_black_hole` (`operations_blackhole.rs:77`) and
  `send_to_void` (`operations_blackhole.rs:264`) call it with **no transaction
  anywhere in the chain** (verified through `commands/duplicates.rs` and
  `routes/duplicates.rs`) — each statement autocommits individually.
- Failure partway (SQLITE_BUSY past timeout, I/O error) leaves the lineage
  permanently split: e.g. the raw set already un-superseded and consumer links
  repointed, while the master's membership rows and set shell survive. This is
  the exact corruption class the supersede-hardening cycle closed — reopened
  at this seam. `api::masters::delete_master` wraps the identical sequence in
  `conn.unchecked_transaction()` specifically to prevent this
  (`api/masters.rs:1885-1918`).
- Worse in `bulk_move_to_black_hole` (`operations_blackhole.rs:108-196`): the
  loop runs inside one raw BEGIN…COMMIT with **no per-file savepoint**, so a
  file whose unregister fails mid-sequence is reported in `failed` (implying
  untouched) while its partial lineage writes are committed by the batch
  COMMIT anyway.

### C6 — Plate-solve backends: the same command is unsafe in two different ways

*Verified by hand (spawn/no-await + both function bodies).*

- **Web** (`routes/plate_solve.rs:222` `plate_solve_batch`, `:526`
  `autofind_objects_from_coordinates`): `tokio::task::spawn_blocking(move ||
  …)` with the JoinHandle dropped (handler returns `Ok(Json(()))` at 419/599)
  and **no `catch_unwind` inside**. Fire-and-forget is intentional (progress
  rides SSE), but a panic anywhere inside — including `ctx.db.get().expect(…)`
  at :527 — is silently discarded by tokio: no log, no
  `plate-solve-complete`/`autofind-objects-complete` event, and the
  `active_plate_solves` handle (key 0/1) never removed. The frontend progress
  UI hangs forever; already-computed solve results are lost (the persist phase
  never runs). The Tauri twin is panic-safe (`catch_unwind` per frame +
  awaited join at `commands/plate_solve.rs:432`).
- **Tauri** (`commands/plate_solve.rs:588-670` `autofind_objects_from_coordinates`):
  runs the entire batch inline in the `async fn` body while holding a live
  pooled connection (`:637-647`) with **no `spawn_blocking`** — blocks a tokio
  worker thread for the batch's whole duration. The Axum mirror does use
  `spawn_blocking` for the same work. Directly contradicts the codebase's own
  pattern (`routes/lights.rs:70-78` comment).

## Important findings

### I1 — `bulk_update_frame_metadata` / `bulk_update_calibration_metadata` are multi-statement, non-transactional

`db/operations.rs:1296-1637`: the frames UPDATE (override stamp included),
three cascade DELETEs (`calibration_set_frames`, `calibration_set_to_frames`,
`session_members`), and `prune_orphaned_calibration_sets` are five separate
autocommit statements. A failure partway violates the function's own stated
invariant ("ANY metadata edit … unlinks") — edited frames keep a partial
subset of stale links, silently. Same non-transactional per-set loop in
`api/calibration.rs:971+` (`bulk_update_calibration_metadata`: originals
INSERT + up to 5 UPDATEs + member-frame propagation per set).

### I2 — `sync_missing_files`: raw BEGIN + `?` mid-loop, logic duplicated in both command layers

`commands/missing_files.rs:32-104` and its copy-pasted Axum mirror
(`routes/missing_files.rs:168-212`): raw `BEGIN TRANSACTION` with
`.map_err(…)?` on every statement — any mid-loop failure aborts the command
and leaks the open transaction onto the pooled connection. Additionally the
whole reconcile is raw SQL living in the command layer (both backends,
duplicated) — violating the "real logic in athenaeum-core" rule.

### I3 — `send_to_void` deletes the disk file BEFORE the catalog rows

`operations_blackhole.rs:267-281`: `fs::remove_file` first, catalog DELETEs
after, no transaction. Opposite of the project's own convention
(`api/masters.rs:1920-1922`: catalog first — "the benign failure mode is an
orphan file on disk … not a catalog row pointing at a file that is already
gone"). A crash in the window leaves a permanently-dangling `files`/`frames`
row indistinguishable from a file on disconnected storage (which the
no-orphan-cleanup rule says must never be auto-deleted) — except this one
never comes back.

### I4 — Same-volume `AtomicRename`: rename-before-DB-sync crash window has no reconcile

`file_op/executor.rs:259` (disk rename) vs `:299` (catalog UPDATE). A kill in
between leaves `files.path` pointing at a path that no longer exists. The
step-log resume branch (`:233-239`) handles it — but nothing ever re-drives an
abandoned operation: there is no list-unfinished/resume command for
`file_operations`, and the startup reconcile
(`file_op/reconcile.rs`) covers only the cross-volume abandoned-source-delete
case by design. Mitigation in practice: dual-pane moves are between scan-root
folders, so the scanner's move-detection usually repairs the row on the next
scan of the destination.

### I5 — Archive and file-op executors hold one pooled connection for the entire operation

`archive/executor.rs:54-97` threads one `&Connection` through stages 2-7
(copy, hash, zip, verify, delete, finalize) — the caller
(`api/masters.rs:1753-1754`) acquires it once and holds it for the whole
multi-GB wall-clock, then reuses it for rollback. Same for
`file_op::executor::run_operation` (`api/files.rs:424-446`). The
master-build/light-cal workers explicitly `drop(conn)` around long compute
with a comment explaining the pool-slot pressure (`api/masters.rs:915-919`,
`api/lights.rs:1410-1533`) — the executors are the outlier. Bounded today by
the single-worker operation queue (at most 1 slot occupied).

### I6 — `.unwrap()` on `parse_from_rfc3339` panics every file listing on one malformed timestamp

`files.modified_at`/`created_at` parsing panics instead of erroring at
`db/operations.rs:759,767,812,820,932,940,1052,1060,1147-1149,1155-1157,
2680-2682,2688-2690,2836-2838,2844-2846` and `db/equipment.rs:395-405`. One
bad string (manual edit, migration bug, corruption) takes down every read of
that file. Asymmetric with the same functions' defensive handling of
`frames.date_obs`.

### I7 — `GROUP BY cs.id` with bare non-aggregated columns yields arbitrary per-set metadata

`db/equipment.rs:77,179,284`: `f.naxis1, f.naxis2, f.bayerpat, f.swcreate,
f.xpixsz, fi.format` are selected bare under `GROUP BY cs.id` — per SQLite's
documented semantics the value comes from an arbitrary member row, not "the
first frame" the comment claims. Non-reproducible display metadata for
multi-member sets with heterogeneous frames.

### I8 — `insert_calibration_link`'s manual-override guard is check-then-act

`db/calibration_links.rs:34-75`: the "does a manual override exist" SELECT and
the `INSERT … ON CONFLICT DO UPDATE` (which unconditionally overwrites
`is_manual_override`) are two statements. An auto-find pass racing a user's
manual assignment on another connection can clobber the manual pick back to
auto.

### I9 — Deleting frames leaks `calibration_set_to_frames` consumer rows

`calibration_set_to_frames.source_id` is deliberately FK-less (polymorphic);
the only DB-level cleanup trigger covers the `calibration_set` side
(`schema.rs:876-885`). The frames side relies on each call site remembering a
manual DELETE — `delete_scan_root` and `bulk_update_frame_metadata` do,
`send_to_void` (`operations_blackhole.rs:277-281`), the relinking orphan purge
(`relinking/mod.rs:378-388`), and `delete_missing_files`
(`commands/missing_files.rs:315-335`, CASCADE-only) do not. Orphan rows
accumulate silently and unboundedly. (Verified empirically that FK CASCADE
deletes fire `AFTER DELETE` triggers, so a frames-side trigger closes every
site at once.)

### I10 — `registration_results.reference_frame_id` has no FK

`schema.rs:818`: the only frames-referencing column in the table (and vs its
structural twin `frame_set_reference.reference_frame_id`) without a declared
FK — deleting the reference frame leaves every row dangling forever. Adding
an FK requires a table rebuild (SQLite can't ALTER one in).

### I11 — Init/migration races are process-local-only guarded

`INIT_DB_LOCK` (`schema.rs:139`) is a `std::sync::Mutex` — it serializes
`init_db` within one process only. Two processes on one catalog file racing a
cold start can: (a) both pass the `column_exists` gate of the
`archive_operations` rebuild (`schema.rs:1663-1754`) — the loser re-copies the
already-migrated table through the legacy column list, silently nulling
`calibration_set_id`; (b) collide on the DROP+CREATE trigger reinstall
(`schema.rs:1861-1891`) — loud failure, self-heals on retry. The code's own
doc comment (`schema.rs:151-153`) declares cross-process init out of scope
("no deployment runs two processes against one catalog file") — this finding
is conditional on that stance ever changing, and is otherwise a documentation
truth, not a bug.

### I12 — Plate-solve persist phase: backends diverge on BEGIN/COMMIT failure

Tauri (`commands/plate_solve.rs:441,484`): `BEGIN` failure aborts before the
persist loop — all of Phase 2's computed solve results are discarded; `COMMIT`
failure `?`-propagates leaving the transaction open. Web
(`routes/plate_solve.rs:349-398`): both failures are logged and fall through —
per-row writes autocommit (safer against data loss) but a failed COMMIT leaks
the open transaction. Same logical command, different failure behavior on each
backend; neither is fully correct.

## Minor findings

- **M1** `deduplicate_session_members_in_set` (`db/operations.rs:2424-2574`) —
  three phases, no savepoint, unlike every sibling batch function.
- **M2** `insert_excluded_frames` (`db/operations.rs:2991-3001`) —
  unconditional `unchecked_transaction()`; hard-errors if ever called inside
  an open transaction, unlike neighbors that check `is_autocommit()` first.
- **M3** `upsert_light_calibration` (`db/light_calibrations.rs:114-182`) — the
  upsert's conflict target covers `frame_id` OR `output_path`, never both; two
  frames resolving to one `output_path` (reachable only at
  `compute.max_concurrent > 1`) fail with a raw constraint error instead of a
  graceful per-frame failure.
- **M4** `create_master_sets_from_frames`
  (`calibration/scan_integration.rs:884-962`) — two INSERTs per master frame,
  no savepoint; a crash between them leaves a permanently-empty master set row
  (exempt from the prune).
- **M5** `archive::planner::commit_plan` (`archive/planner.rs:467-533`) —
  operation row + unbounded per-file INSERT loop, no transaction; a kill
  mid-loop plus a later resume could finalize a truncated plan.
- **M6** `frames` has no `UNIQUE(file_id)` — the 1:1 invariant is
  application-enforced only (`reparse` bails on `frame_count > 1`).
- **M7** Latent: if a migration `execute_batch` fails mid-batch, the explicit
  `BEGIN` stays open and the subsequent `pragma_update(foreign_keys, true)` is
  a silent no-op (`schema.rs:1614,1726`) — currently non-exploitable because
  `Database::new` drops the whole pool on `init_db` error, but a trap for any
  future retry-in-place.
- **M8** `Database::conn()` (`db/mod.rs:130-134`) — pool exhaustion (r2d2 30 s
  checkout timeout) becomes a panic via `.expect`, not a typed error. Worker
  threads are covered by `catch_unwind`; a Tauri/Axum task hitting it fails
  its request with only the panic message.
- **M9** `find_duplicate_folders` — O(F²) pairwise folder comparison fully
  in-memory; a scaling landmine independent of C4 (which removes it from the
  write transaction).
- **M10** `get_imaging_nights_with_sessions` (`db/operations.rs:2754-2920`) —
  N+1: statements prepared inside the nights/sessions loops (1 + N + N×M
  queries where one join would do).
- **M11** `api/analysis.rs:356-369` — persist phase rolls back correctly on
  row errors but a failed `COMMIT` `?`-propagates without rollback (narrow
  leak window).
- **M12** `pragma foreign_key_check` runs only inside the two migrations —
  no startup sweep would catch violations introduced by other means.
- **M13** Doc/code mismatch: `file_op/executor.rs:8-15` claims the
  cross-volume CommitMove is "wrapped in a SQLite transaction" — it is two
  independent autocommit statements (the design is resume-based, not
  transaction-based; the comment overstates).

## Verified clean

- **FK enforcement**: ON for every connection (compile-time default of the
  bundled build; nothing disables it). The fix plan still adds an explicit
  `PRAGMA foreign_keys = ON` so a future system-lib build can't silently
  regress it.
- **`init_db` idempotency**: every CREATE uses IF NOT EXISTS; all 26
  guarded-ALTER sites check the correct column; the one RENAME COLUMN is
  correctly guarded; the black-hole dedup cleanup runs before its UNIQUE
  index is created.
- **12-step rebuilds**: pragma toggled outside the transaction (required),
  atomic DDL batch, `foreign_key_check` after, `sqlite_sequence` carried,
  indexes recreated; process death mid-rebuild is safe (WAL rollback).
- **Transfer-table wipe-on-upgrade**: detection before any DDL, covers both
  legacy shapes, wipes exactly the 8 tables in one transaction.
- **`files.path`**: genuine UNIQUE from birth; no `INSERT OR IGNORE/REPLACE`
  on files — path collisions error loudly.
- **Scanner re-parse**: SAVEPOINT-nested, id-preserving, override-respecting;
  the main scan transaction has explicit ROLLBACK on cancel and on failed
  COMMIT. The restore.rs "DELETE+re-INSERT" comment describes a fixed
  historical bug — its pinned regression test passes today.
- **Parallel scan**: the rayon phase touches no DB; all writes are sequential
  on one connection.
- **`register_master` / `delete_master` / `unregister_master_set`**: single
  transaction each, disk I/O ordered after commit, extensively test-pinned.
- **Archive stage ordering**: copy → verify → zip → verify → delete → finalize
  is disk-safe-before-destructive; resume is idempotent; `commit_plan` /
  `run_operation` split across the queue boundary is the documented
  resume design, not a race.
- **Cross-volume file-op commit**: DB-sync before source delete (crash leaves
  a harmless duplicate), healed by the startup reconcile — provably-safe
  criteria in `file_op/reconcile.rs`.
- **`frames.override`**: respected on every write path checked (scanner
  re-parse skips frame columns; bulk edits stamp it; calibration propagation
  stamps it).
- **`last_insert_rowid`**: no misuse found anywhere; the two
  INSERT-OR-IGNORE sites and the UPSERT re-query the id explicitly.
- **Panic containment**: `operation_queue::worker_loop`, master-build and
  light-cal threads all `catch_unwind` (test-pinned) — the web plate-solve
  routes (C6) are the one gap.
- **Path-prefix machinery**: `path_prefix_upper` range predicates everywhere;
  no unescaped user-path LIKE remains; the one remaining LIKE matches a
  numeric suffix by construction.
- **Nested pool acquisition**: no call path found holding one pooled
  connection while acquiring another (the dominant convention threads one
  `&Connection` down).
- **FK-action spot checks**: `light_calibrations.*_set_id` SET NULL,
  `master_provenance` CASCADE/no-action split, `superseded_by_set_id`
  no-action — all deliberate and internally consistent.

## Decisions to ratify (owner)

1. **D1 — deployment stance (I11):** ratify the code's existing position that
   exactly one process opens a given catalog file at a time (desktop OR web,
   never both on the same file). If ratified: no code change; the
   schema.rs:151-153 comment stands as the contract. If rejected: a
   cross-process file-lock around `init_db` becomes a cycle of its own.
   *Proposed: ratify.*
2. **D2 — pool-exhaustion policy (M8):** keep the `.expect` panic (workers are
   catch_unwind-protected; a genuinely exhausted pool is a bug to surface
   loudly) or convert `Database::conn()` to a typed error through ~200 call
   sites. *Proposed: keep the panic; revisit only if it ever fires in the
   field.*
3. **D3 — executor connection-holding (I5):** refactor archive/file-op
   executors to acquire-per-stage now, or defer until log evidence of pool
   contention appears (single-worker queue bounds the damage to one slot).
   *Proposed: defer; noted in the follow-ups list.*
4. **D4 — same-volume rename crash window (I4):** extend the startup reconcile
   to re-drive abandoned AtomicRename steps, or accept the scanner
   move-detection as the healer (moves are between scan roots in practice).
   *Proposed: accept scanner healing for now; noted in the follow-ups list.*

## Deferred follow-ups (not in the fix plan)

- **I4** — startup reconcile for abandoned same-volume renames (pending D4).
- **I5** — per-stage connection acquisition in archive/file-op executors
  (pending D3).
- **I10** — FK on `registration_results.reference_frame_id` (needs a table
  rebuild; dangling value is display-only today).
- **I11** — cross-process init locking (pending D1; document-only if
  ratified).
- **M3** — dual-target upsert for `light_calibrations` (only reachable at
  `compute.max_concurrent > 1`; the failure mode is a loud error, not
  corruption).
- **M6** — `UNIQUE(frames.file_id)` backstop (needs dedupe + rebuild; the
  application guard bails loudly today).
- **M8** — typed pool-checkout error (pending D2).
- **M9** — algorithmic rework of `find_duplicate_folders` (C4's fix removes it
  from the write lock; the O(F²) itself remains).
- **M10** — single-join rewrite of `get_imaging_nights_with_sessions` (local
  SQLite round-trips; measure before optimizing).
- **M12** — optional startup `foreign_key_check` sweep (full-table scan on
  big catalogs; would need gating).
- **M13** — fix the `file_op/executor.rs` doc comment when that file is next
  touched.
