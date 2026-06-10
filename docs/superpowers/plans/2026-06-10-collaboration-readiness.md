# Collaboration-Readiness Plan — 2026-06-10

**Goal:** Remove the schema-level blockers that make any future multi-machine / multi-user scenario (shared catalog, catalog hand-off to a teammate, two devices, eventually real sync) impossible. Each stage is independently shippable and useful on its own — none of this commits to building a sync engine yet.

**Verdict from the audit:** the runtime architecture (transport-agnostic core, Axum backend, pooled WAL SQLite) is already multi-client-shaped. The blockers are all in the data model:

1. All PKs are `INTEGER AUTOINCREMENT` — two catalogs generate colliding IDs; merge requires remapping every junction table.
2. `files.path` / `scan_roots.path` are absolute, machine-specific, UNIQUE — a catalog is dead on arrival on another machine.
3. No `updated_at` / `deleted_at` / version columns, no change journal — a sync layer has nothing to diff, conflicts are silent last-write-wins.
4. No catalog identity — "same catalog on two devices" vs "two different catalogs" is undecidable.
5. Hand-maintained Rust↔TS type mirrors and ~120 duplicated command/route pairs — every API change is a 3-place edit; a second client multiplies the drift risk.

## Stage 1 — identity & timestamps (cheap, forward-compatible, do first)

- `settings`-style one-row `catalog_meta` table: `catalog_uuid` (generated once), `schema_version`, `created_at`.
- Add `uuid TEXT` (default `randomblob`-based, backfilled by migration) + `updated_at` to the core entity tables: `files`, `frames`, `frames_set`, `sessions`, `calibration_set`, `tags`, `export_templates`. Integer PKs stay for FK performance; UUIDs are the *portable* identity.
- Triggers (or write-path discipline) to bump `updated_at` on UPDATE. Idempotent migration in `schema.rs::init_db()` per existing convention.
- Immediate payoff even single-user: "what changed since the last backup/scan" queries, debuggability.

## Stage 2 — portable paths

- Split `files.path` into `(scan_root_id, rel_path)`; keep a generated/maintained absolute-path column (or view) during transition so existing queries and the LIKE-prefix rename logic keep working.
- Catalog moved to a new machine then needs only scan-root re-pointing (the existing `relink_scan_root` flow), not per-file relinking.
- Watch out: the dual-pane move pipeline, archive restore, and SUBSTR-based directory rename all manipulate `files.path` — this stage must update those in the same pass (`file_op/`, `archive/`, `scanner/`).

## Stage 3 — change journal

- Append-only `change_log (id, table_name, row_uuid, op [I/U/D], changed_at, payload JSON NULL)` written alongside mutations of Stage-1 tables (deletes recorded as tombstones — currently deletes are physical and unrecoverable).
- Capped/compactable (e.g., keep N days or last full-backup horizon).
- This is the substrate for *any* future sync strategy — last-write-wins file sync, server-mediated, or CRDT — without choosing one today. Also enables "export frame set with metadata" → "import into teammate's catalog" as a poor-man's collaboration MVP (UUIDs make the import collision-free).

## Stage 4 — contract hardening (parallel-track, independent)

- Adopt `ts-rs` (or similar) to generate `src/types/models.ts` from the Rust structs — kills silent serde drift; CI-free enforcement via a `cargo test` that regenerates and diffs.
- Extract a shared helper layer so Tauri commands and Axum routes are one-line wrappers over the same function per command (start with the highest-churn modules: `files`, `frame_sets`, `archive`).

## Non-goals (decide later, separately)

- Real-time co-editing, account systems, server hosting, CRDT merge semantics.
- Multi-writer SQLite over network shares (explicitly unsupported — WAL + NFS/SMB is unsafe).

## Acceptance per stage

- [ ] Stage 1: fresh + migrated catalogs both carry `catalog_uuid` and per-row `uuid`/`updated_at`; all existing tests green.
- [ ] Stage 2: catalog file copied to a second machine + scan-root relink → all files resolve, move/rename/archive flows green.
- [ ] Stage 3: every mutation of a Stage-1 table is visible in `change_log`; delete leaves a tombstone.
- [ ] Stage 4: `models.ts` is generated, not hand-edited; a deliberate Rust field rename fails the diff test.
