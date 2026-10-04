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
7. `tier1/project-run.sh <repo> <work-dir> <port> <project-id>` — one
   project run (wave 6b-3): `start_project_stacking`, waits for that run's
   `stacking-complete`, writes `run-project.txt` beside `run.txt`. Same
   refusals as `checkpoint.sh`.
8. `run-a-grid.py <copy.db> <project-id> (--reference-uuid U | --grid-json J) [--apply]`
   — the grid of a reference frame's manifest WCS (acceptance run A) or any
   grid (run B), stored in a catalog COPY only; it refuses the live catalogs.
   A copy made from a catalog older than the branch lacks the grid columns
   until the branch's server has started on it once (the schema migrates at
   start): start `server.sh` on the copy, stop it, then run this script.
   An acceptance copy that stacks a project runs signed out: set its
   `account.hub_url` to `http://127.0.0.1:9` before the server starts (the
   feed would otherwise replace the local grid), and never call
   `account_sign_out` on a copy — the token store is shared with the real app.
9. `tier1/tier1-compare.py --pairs <head>=<base> …` — the first four Tier C
   rows on explicit master pairs (run A), refusing a pair whose pixel grids
   differ; `grid-report.py <work-dir>
   <project-id> [--k K] [--overlap] [--ln-truth]` — run B's per-group report
   (FWHM in arcsec, eccentricity, the white-noise-equivalent noise per arcsec²
   = σ_px·scale, coverage, member overlap with the LN reference, and for every
   master its pixel dims, the crop rectangle when the run cropped, the drizzle
   master's dims). A large grid × k² drizzle with the coverage map on may be
   refused for memory: record such a refusal with its numbers; do not shrink
   the run on your own initiative. Run B calls `grid-report.py <work-dir>
   <project-id> --ln-truth`: it prints each summary member's
   local-normalization scale (from the run's debug log events, matched by the
   `ln_plane` span and the run's id and time window) next to a photometric
   truth, with the cause for every member that has no scale (excluded, cached,
   global normalization, LN failed). A frame's LN scale depends on its PSF
   width (+1/3 px² of PSF variance moves it by about 2.6 %) and a project's
   members come from different instruments, so the truth is an adaptive
   aperture ratio (FWHM per frame, radii from 3F, sky annulus 10–12F,
   curve-of-growth correction, isolated and unsaturated stars in both frames),
   reported per instrument with no tolerance. The acceptance build must
   include the `ln_plane` / `ln_seeds_calibration` span commits, or the log
   carries no frame identity. `--ln-truth` may give no ratio for a coarse member on a
   crowded field (too few isolated stars at its PSF width): that is a
   refusal, not a failure, and an empty column is reported as is. `grid-report.py --self-check` runs the
   synthetic check of the photometry and the log parser (no real data).

A catalog copy is made with `sqlite3 -readonly <live db> ".backup <copy>"`,
never by reading the live catalog any other way, and `prepare-catalog.sh` is
fed that copy through `ATH_ACC_DB`. Working folders live on
`/Volumes/BigMac` (`$HOME/.athenaeum-acc/…`); `/Volumes/bigbase3` is a slow
HDD.

`<work-dir>` should be the session scratchpad or any folder outside the repo
and outside every scan root — never the real library, and never a folder a
scan root would ingest.

Python leaves `__pycache__/` beside `imgcmp.py` and the `tier1/*.py` tools;
it is git-ignored, never committed.
