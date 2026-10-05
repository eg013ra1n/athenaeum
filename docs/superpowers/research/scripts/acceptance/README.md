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
10. LN model fixes, checkpoint A (spec `2026-10-04-ln-model-fixes-design.md` §11.2–§11.4):
    - `tier1/tier1-compare.py --ln-grids <W_head> <W_base>` pairs every `.athln`
      under `<W>/stackwork/**`, prints how many have `A` and `global_scale`
      bit-identical and the B move (|dB| / the base reference's sky: median, p99,
      max, nodes past 1 %). Reported, not gated; against itself every number is 0.
    - `ln-math/halo_profile.py … [--search R] [--k K_LN,K_NOLN,K_EXT] [--bg0 BG]
      [--labels A,B,C]` adds the centroid-refined star position and the
      scale-normalized ring excess (L-21) after today's tables.
    - `ln-scale-audit/master_phot.py <ref> <master> <plane> <x0> [<summary> <key> <csv>]
      [--y0 Y] [--noise]`: the `predicted` block needs all three trailing
      arguments; `--noise` adds `noise`, `k` and `snr` from `measure_probe`.
    - `ln-model/nebula_boxes.py <boxes.json> <control> <master>… [--k K …] [--control-k K]
      [--size 100]`: per-box nebula excess, raw and scale-normalized against the
      control's background; `d_norm` is the master's normalized read minus the
      control's normalized read (the control normalized by its own `--control-k`,
      default 1), `d_raw` raw minus raw. Write and commit `boxes.json` before the
      LN master is read.
    - `ln-model/star_blocks.py --control <master> --ref <LN reference> [--block 96]
      <label>=<master>…`: the whole-frame structure residual of each arm against the
      control (`100·(arm_block/median(arm) − control_block/median(control))` on
      non-overlapping `nanmedian` blocks), per block class from the control's block
      maximum over the control's median (star > 50, star > 200, no star > 10): `n`, `rms`,
      `median` per arm and, with two arms, the 12 blocks with the largest difference between
      them. It reproduces the checkpoint-A review's Horse C11 numbers.
    - `tier1/tier1-compare.py --numeric` prints `beta head=… base=…` under each plane's
      `fwhmPx` ratio and notes a β that differs (the FWHM row then compares two PSF
      models); no bar moves.
    - `ln-model/horse-c11-prep.sh <acc-dir> <port>` builds the Horse C11 catalog
      copy (set-182 lights lose the offset-50 dark links) and prints the next
      steps; `ln-model/horse-c11-config.py <run-8.json> <acc-dir>/athenaeum.db
      [--fixed-1024]` prints the `set_stacking_config` body (R and B lights excluded).

11. LN model fixes, checkpoint B (spec §4.3, §11.2):
    - `ln_scale_probe` main mode adds an `"arms"` object to each member's JSON line: the
      production scale call per estimator and aperture (`psfOff`, `psfSel`, `f1_2`, `f1_3`,
      `f1_gc`) with `s`, `sigma`, `matches`, `selected`, `rule`, `aperturePx`, `commonFwhmPx`,
      `growth` and the bright / middle / faint `terciles`; `--ln-scale N` sets the LN scale
      of its background models, `--max-stars-sel N` the selection's own `maxStars` (the
      detection cap `--max-stars` is a different thing). `psfOff.s` equals the pre-selection
      `sLib`, bit for bit.
    - `ln-scale-audit/sim_config.py --arms <probe.jsonl> …` writes each member's
      `arms[name] = s_arm / applied` into the `--stack-sim` config, which then simulates every
      arm beside the truth-corrected one (`sim_<arm>`, `sim_<arm>_norej` columns appended to
      the stars csv); `sim_report.py` prints each arm's median ± bootstrap SE with and without
      rejection next to the truth arm, the ratio `arm_norej / sim_cor_norej` (L-22) and
      `arm_norej / ref`.
    - `ln-model/aperture_choice.py <dataset>=<probe.jsonl>:<truth.csv>… [--sim sim-stars.csv]`
      decides by spec §4.3: step 1 per variant on every dataset (median |gap|, slope vs Δσ²,
      mono-vs-OSC offset, with `ln-scale-audit/evaluate.py`'s `gap_stats`), the tercile trend
      (flagged above 1 %), step 2 from the stack-sim, step 3 the tie-break; the last line is
      `aperture choice: <variant>` or `none passes`. Name datasets `<group>/mono` and
      `<group>/osc` for the offset.
    - `ATH_ACC_SET_CONFIG=<file>` (`tier1/tier1-run.sh`) and `ATH_ACC_PROJECT_CONFIG=<file>`
      (`tier1/project-run.sh`): a file holding the `set_stacking_config` /
      `set_project_stacking_config` argument document, sent once the server answers and before
      the run starts; the response is echoed and recorded in `run.txt` / `run-project.txt`, and
      an error refuses the run. This is how an F1 arm runs on the same template as the
      PSF-signal arm.

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
