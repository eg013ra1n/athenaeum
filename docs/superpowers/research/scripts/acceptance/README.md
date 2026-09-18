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

`<work-dir>` should be the session scratchpad or any folder outside the repo
and outside every scan root — never the real library, and never a folder a
scan root would ingest.
