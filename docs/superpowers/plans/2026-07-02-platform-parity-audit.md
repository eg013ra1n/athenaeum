# Cross-Platform & Web/Docker Parity Audit — 2026-07-02

Companion to `2026-07-02-v0.2.1-audit.md` (which measured API-level command/route parity). This covers OS portability of the desktop build and *functional* parity of the web/Docker build. All findings verified in source on `main` v0.2.1.

## 1. OS cross-platform (desktop: macOS / Windows / Linux)

### [BUG] SQLite `LIKE` on paths: case-insensitive + unescaped wildcards

All path prefix queries use `LIKE` (`db/operations.rs:227, 274, 310, 324, 350, 404, 659, 766, 1427`; index comment `schema.rs:471`), and **no `PRAGMA case_sensitive_like`** is set anywhere. Two consequences:

1. SQLite `LIKE` is ASCII-case-insensitive by default. On Linux (case-sensitive FS) `/data/M31` and `/data/m31` are different directories but cross-match in every prefix query — directory-rename hot-sync, scan-root cascade and orphan queries can touch the wrong rows.
2. `%` and `_` in the *prefix argument* are live wildcards and are never escaped. `_` is ubiquitous in astro paths (`M31_Ha/`), making e.g. `path LIKE '/data/M31_Ha%'` also match `/data/M31xHa…`. Low probability, silent wrong-row updates when it hits.

**Fix (Phase 0 worthy):** replace prefix `LIKE` with a range or substr predicate — `WHERE substr(path, 1, ?len) = ?prefix` (index-unfriendly) or `WHERE path >= ?prefix AND path < ?prefix || X'F7BFBFBF'` (keeps index) — or set `case_sensitive_like=ON` per connection *and* escape `%`/`_`. One shared helper, used by every site above. Added as **T11** to `2026-07-02-phase0-hygiene.md`.

### [OK] Verified sound

- **Cross-volume detection**: `file_op/planner.rs:420` unix `MetadataExt::dev()` with ancestor walk; `:438` Windows volume-root (drive letter / UNC share prefix) hash — correct basis for AtomicRename vs CopyVerifyDelete on both.
- **cfg gating**: unix-only APIs are gated (`file_op/planner.rs`, `export/file_organizer.rs:363` symlink, `commands/utils.rs:10` Windows `\\?\`/UNC normalization for reveal-in-file-manager, per-OS log paths in `logging.rs`). No ungated `std::os::unix` found in core.
- macOS `/Volumes` vs `/private/Volumes` handled in the move pipeline (documented contract in CLAUDE.md).

### [RISK] Watch items

- `export/file_organizer.rs:363`: WBPP export uses **symlinks on unix** (Windows branch differs). Symlinked exports break if the export root is later shared via Syncthing/Docker volume or moved cross-device — relevant to pillar C; consider a "materialize (copy) instead of link" export option then.
- Windows is the least-exercised platform (recent CI work was macOS signing); no Windows-specific test coverage of file_op/archive path edge cases (drive letters, UNC, `\\?\`).

## 2. Web/Docker functional parity

### [OK] Larger than expected

The web backend is a real peer, not a demo: shared `athenaeum-core` logic, the **folder monitor runs in web mode** (`athenaeum-web/src/main.rs:57-59, 155`), archive/file-op/export/analysis all wired, SSE events mirror Tauri events, previews served over HTTP (Blink uses `get_frame_preview` in web mode — `BlinkViewer.tsx:200-208` — by design, not a gap).

### [GAP] Intentional 501 stubs (complete list — three)

| Route | Where | Note |
| ---- | ---- | ---- |
| `relocate_missing_file` | `routes/mod.rs:317` | needs native file picker |
| `read_fits_image_rustafits` | `routes/mod.rs:324` | web uses `get_frame_preview` instead — OK |
| `relink_scan_root` | `routes/scan_roots.rs:415` | **flag**: collaboration Stage 2 (portable paths) leans on the relink flow; web mode needs a path-input variant of relink before/with Stage 2 |

### [RISK] Security posture of athenaeum-web

- **No authentication of any kind** (no API key / token / middleware anywhere in `athenaeum-web`). Anyone with network access can move/delete files and browse directories.
- Path sandboxing via `ATHENAEUM_ALLOWED_PATHS` **is** enforced, but only in `routes/scan_roots.rs` and `routes/files.rs`; docker defaults `Dockerfile:101` = `/astro-files,/exports`.
- Acceptable for the current "LAN/home Docker" story, but must be stated in README/docs, and **pillar C must not widen exposure**: the Syncthing design keeps sync out-of-process (good); never bind athenaeum-web beyond localhost/LAN without adding auth. Cheap interim hardening: optional `ATHENAEUM_API_KEY` env → require header when set.

### [STALE DOC] Siril export has been removed from the codebase

`crates/athenaeum-core/src/export/` now contains only `data_collector`, `file_organizer`, `models`, `mod` — zero `siril` matches in `crates/` or `src/`. `docs/export/README.md` still documents the full Siril script pipeline (scripts, cli_runner, execution modes) — **rewrite it**; it misleads any future work (and misled the June-era mosaic analysis).

Corollary discovered while verifying: **WBPP grouping-keyword infrastructure already exists** — `WbppExportConfig { keyword_order: ["CAMERA","BIAS","DARKS","FLAT"] }` builds keyword-nested folders and `WbppSetupInstructions`/`WbppKeywordInstruction { pre_checked }` model the WBPP-side setup (`export/models.rs:691-741`). The mosaic export task shrinks to: add a `PANEL` keyword level with `pre_checked: false` (post-processing grouping) fed from `tile_label`. Architecture spec and roadmap updated accordingly.

## 3. Actions taken from this audit

All actionable findings landed in the Phase 0 plan (`2026-07-02-phase0-hygiene.md`): **T11** LIKE-on-paths fix, **T12** `docs/export/README.md` rewrite, **T13** optional `ATHENAEUM_API_KEY` auth, **T14** web `relink_scan_root` path-input variant; symlink-export and Windows-coverage risks recorded there as watch items. Mosaic export sections updated in `../specs/2026-07-02-target-features-architecture.md` and roadmap P3 (Siril per-tile bullet dropped; keyword work rescoped onto `WbppExportConfig`).
