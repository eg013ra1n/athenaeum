# Acceptance harness

The recipe every acceptance run since M2 has used, written down once so it is
not rebuilt per cycle (owner request 2026-09-18).

1. `prepare-catalog.sh <work-dir> [fits|xisf]` — an isolated copy of the dev
   catalog with its library / export / stacking folders redirected into
   `<work-dir>`.
2. `server.sh <work-dir> [port]` — release `athenaeum-web` from
   `.superpowers/target-acc` (a separate cargo target dir) on the copy.
3. `api.sh <command> '<json>' [port]` — call any command; the SSE stream is at
   `GET /api/events` (`curl -N http://127.0.0.1:<port>/api/events`).
4. Run-specific drivers live beside this file (`xisf-master-run.sh` and
   friends); each states what it measures at the top.
5. `tier1/checkpoint.sh <name> [base] [--numeric]` — one full stacking
   checkpoint on THIS worktree's build: fresh catalog copy, run, extract,
   compare against an earlier checkpoint.
6. `tier1/rerun-build.sh <commit> <name> [port]` — the same run on an OLDER
   commit's build, in a throwaway worktree with its own target dir. Pair it
   with a `checkpoint.sh` run back to back whenever a tier's TOTAL is being
   read: this machine drifts ±10–15 % across a build-heavy session, so only
   an interleaved bracket is comparable (ruling R-TA-9).

`<work-dir>` should be the session scratchpad or any folder outside the repo
and outside every scan root — never the real library, and never a folder a
scan root would ingest.

Python leaves `__pycache__/` beside `imgcmp.py` and the `tier1/*.py` tools;
it is git-ignored, never committed.
