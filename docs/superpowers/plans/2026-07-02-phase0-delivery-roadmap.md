# v0.2.2 Delivery Roadmap — Phase 0 Hygiene & Guards (T1–T14)

Companion to `2026-07-02-phase0-hygiene.md` (task details) after the 2026-07-02 audit
(three corrections applied in place there: T5 rewritten, T6 = 37 dead commands,
T11 = 13 LIKE sites). Version branch: **`0.2.2`** (all work lands there; ff-merge
to `main` + tag at release, per the release workflow).

**Audit verdict:** 11 of 14 tasks confirmed exactly as written with zero line
drift; T5 required a rewrite (the claimed scanner corruption is already guarded
— the real work is auto-reconciling abandoned cross-volume moves + a volume-aware
edge + three swallowed errors); T6 and T11 grew in scope (37 dead commands of
162; 13 path-fed LIKE sites incl. three SQL-side concatenations). All referenced
audit docs exist. Nothing in the plan is stale enough to drop.

## Waves

Ordered by risk-to-user-data first, then unblock-value. Each wave is a
self-contained review/merge unit on `0.2.2` with green gates
(`cargo build --workspace` + `cargo test` + `tsc --noEmit`; corpus_bench for any
solver-adjacent change; submodule suites for wave 1).

### Wave 1 — Submodule pass (T3 → T1 → T8 → T3b) · ~2 days
One `solvemyastro` branch (poison-recovery ×3, naming sweep incl. `README.md:12`,
flip flag in `Registration`, `register_inlier_tol_arcsec` in `SolveConfig`) and
one `rustafits` branch (naming in `pattern_matcher.rs:1,3`, NAXIS3 ∉ {1,3}
reject). One superproject bump commit per submodule. Athenaeum side of T1
(persist `ok_flipped`, UI badge) rides the same wave.
**Gate:** submodule test suites + `corpus_bench` (register regression: `None`
tolerance bit-for-bit) + naming grep = 0 hits.

### Wave 2 — Core data-safety (T11 → T7 → T4) · ~2.5 days
- **T11** (13 sites): `db::path_prefix_where` range-predicate helper for the 10
  Rust-side sites; per-site treatment for the 3 SQL-side `sr.path || '%'` joins;
  `file_op/executor.rs:741` included. Case/wildcard regression tests.
- **T7** (3 raw BEGINs → `unchecked_transaction`/SAVEPOINT, nested-call tests).
- **T4** (restore skip-path hash-verify → conflict disposition,
  `CompletedWithErrors`, no marker-clear on mismatch).
**Gate:** existing rename/cascade/restore tests green + the new ones.

### Wave 3 — File-op reconciliation (T5 revised) · ~1.5 days
Auto-reconcile `Failed` CommitMove at operation-queue startup (hash-checked
source removal + notify summary — must be automatic: the file-op resume UI does
not exist, see T6); volume-aware guard for move-detection (skip flip when the
old path's scan root is unavailable); fix the three swallowed errors at
`scanner/mod.rs:1319/1324/1341`. Real-FITS fixture for fingerprint tests.

### Wave 4 — Web parity & security (T13 → T14 → T9) · ~1.5 days
`ATHENAEUM_API_KEY` middleware (+ SSE query-param fallback, web login prompt,
compose docs); web `relink_scan_root` path-input variant reusing the
`add_scan_root` allowed-paths validation + `browse_directories`; MissingFiles
501-degradation check. **Gate:** the T10 web smoke checklist.

### Wave 5 — Cleanup & docs (T6 → T12) · ~1.5–2 days
T6 needs the **owner decision round first** (see below) — the mechanical
deletions and the PR-listed "lost features" follow it. T12 README rewrite
around `WbppExportConfig` (models.rs:701).

### Wave 6 — Exit (T10) · ~0.5 day
Full checklist from the plan + update the two audit docs' open-findings tables
+ command-count reconciliation. Then release prep per the standard workflow
(EN release notes → version bump ×5 → ff-merge to main → tag).

**Total: ~9–10 dev-days** (matches the plan's "~2 weeks" shape with review).

## Owner decisions needed (block Wave 5 only)

1. ~~File-op Delete pipeline~~ **RESOLVED (owner, 2026-07-02): Delete flows
   through Black Hole by design — never directly.** `enqueue_delete_operation`
   + `FileOpDelete` plumbing are deleted in Wave 5; `cancel_file_operation` /
   `list_unfinished_file_operations` commands go too (T5's reconciliation is
   queue-internal via the core functions). **Sub-question RESOLVED (owner,
   2026-07-02): keep `send_all_to_void` and wire an "Empty Black Hole" button**
   on the Black Hole page (confirm dialog with count/bytes; both backends —
   web route exists) — added to Wave 5 scope.
2. **`clear_manual_calibration_override`** — documented in `docs/masters/masters.md:120`
   as part of manual linking; likely a lost feature, and Phase 2 (master library)
   touches the same area. Resurrect in Phase 2 or delete now?
3. **Dark-library pair** (`create_dark_library`/`delete_dark_library`) — superseded
   by calibration sets, or wanted for the Phase 2 master library?
4. The remaining ~30 dead commands: default = delete (list preserved in the PR
   description per the plan's rule). Objections per-name welcome.
5. **Release shape for v0.2.2:** straight stable (like v0.2.1) or a beta first?
   Content is guards/hygiene — low UI risk; straight stable is defensible.

## Standing constraints

- Everything on branch `0.2.2`; submodule pointers bump on it; `main` stays
  releasable.
- No schema migrations in this phase (T1 encodes flip as `status = "ok_flipped"`).
- Docker image note: v0.2.2 tag will also publish the Docker image that v0.2.1
  skipped (Dockerfile fix already on main/0.2.2).
- Phase 1 (foundation: UUIDs, ts-rs, shared command layer, FITS writer) may
  start in parallel after Wave 2 if bandwidth allows — nothing in Phase 0
  blocks it except review capacity.
