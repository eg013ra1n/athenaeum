# Dev-Loop Test Harness Plan — 2026-06-10

**Goal:** Make the working algorithms testable *during development* — change code → rebuild → exercise the real code path against real FITS data → inspect results — without launching the desktop app. This is the seam that lets an AI agent (or a human in a hurry) verify behavior end-to-end on its own.

**Explicitly out of scope:** CI gates. The priority is dev-time verification, not post-merge regression checks. (If ever wanted later: the same `cargo test` + sandbox smoke commands are directly reusable in `.gitlab-ci.yml`.)

## Why this works today (audit facts)

- `athenaeum-web` exposes the full command surface (~184 POST/JSON routes in `crates/athenaeum-web/src/routes/mod.rs`) with zero Tauri dependency, configured entirely by env vars: `ATHENAEUM_DB_PATH`, `ATHENAEUM_PORT`, `ATHENAEUM_STATIC_DIR`, `ATHENAEUM_ALLOWED_PATHS`, `ATHENAEUM_EXPORT_DIR`.
- `Database::new(path)` bootstraps the schema on any path → a temp-file SQLite DB per session is trivial.
- Real FITS fixtures already exist: `rustafits/tests/` holds 100+ real files (mono, OSC, M82, LDN, trails) with the skip-if-missing pattern.
- Frontend is transport-agnostic (`VITE_TARGET=web`), so the same harness extends to Playwright later if wanted.

## Task 1 — `scripts/dev-sandbox.sh`

One command boots a throwaway full-stack instance:

```bash
scripts/dev-sandbox.sh [fixture-dir]
# 1. mktemp -d → $SANDBOX (DB + exports)
# 2. default fixture-dir: rustafits/tests (or a small curated subset dir)
# 3. ATHENAEUM_DB_PATH=$SANDBOX/athenaeum.db \
#    ATHENAEUM_PORT=3030 \
#    ATHENAEUM_ALLOWED_PATHS=$FIXTURES,$SANDBOX/exports \
#    cargo run -p athenaeum-web
# 4. prints: base URL, DB path (for sqlite3 inspection), fixture dir, teardown hint
```

Properties:

- Fresh DB every run (or `--keep` to reuse), nothing touches the real catalog in app-data.
- The agent loop: `curl -s -X POST localhost:3030/api/add_scan_root -d '{...}'` → `start_scan` → watch `/api/events` (SSE) → `get_files` → assert; `sqlite3 $SANDBOX/athenaeum.db 'SELECT …'` mid-run for ground truth.
- Works for every domain the routes cover: scanner, clustering, frame sets, calibration matching, plate-solve queue, registration, archive, file ops, export.

## Task 2 — real-FITS fixtures for `cargo test`

- Add a tiny curated fixture set reachable from `athenaeum-core` integration tests (either copy 1–2 small real files into `crates/athenaeum-core/tests/fixtures/`, or resolve `../../rustafits/tests/...` relative to `CARGO_MANIFEST_DIR`).
- Skip-if-missing guard (same pattern as rustafits) so tests never hard-fail on a sparse checkout.
- First consumers: scanner re-parse tests (today they use the synthetic `write_minimal_fits`; real headers exercise BAYERPAT, WCS keywords, non-trivial INSTRUME values), calibration matcher, clustering with real RA/Dec spreads. Honors the project rule **"real data first when debugging."**

## Task 3 — smoke script for route-level checks

`scripts/smoke.sh` (uses Task 1's sandbox): scan-root CRUD → scan fixture dir → assert file count → list frame sets → calibration hierarchy → fetch one image render. Exits non-zero with the failing route + response body. This is the "did I break the world?" 30-second check after any backend change — and doubles as executable documentation of the API.

## What genuinely cannot be covered by this seam

| Zone | Why | Mitigation |
| ---- | ---- | ---- |
| Native dialogs, reveal-in-Finder, window mgmt | Tauri plugin layer | Keep thin; manual test |
| Tauri IPC serialization quirks | Desktop-only transport | Types shared with web routes; drift caught there |
| Monitor poll timing on real mounts | Wall-clock + FS-event dependent | Monitor logic itself is HTTP-togglable + SSE-observable in the sandbox |
| macOS `/Volumes` vs `/private/Volumes` file-op edge cases | Needs real volume topology | Unit tests with mocked `dev()`; occasional manual pass |

## Acceptance

- [ ] `scripts/dev-sandbox.sh` boots against fixtures; documented in CLAUDE.md (one short paragraph: "to verify backend changes end-to-end, use the dev sandbox").
- [ ] `scripts/smoke.sh` passes on a clean checkout in under ~2 min including build.
- [ ] At least one `athenaeum-core` integration test consumes a real FITS fixture.
