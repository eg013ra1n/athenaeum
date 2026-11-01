# Rejection Normalization by Star Flux — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: the user skill `lean-execution` (`~/.claude/skills/lean-execution/SKILL.md`)
> above `superpowers:subagent-driven-development`: one phase per agent, a one-page handoff, reports of at most 80 lines,
> one command per lab job printing a verdict of at most 30 lines. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a frame's star cores are never rejected because the rejection step scaled the frame by its background noise
instead of its star flux. `normalization.rejection = local` (the LN flux pair) becomes the default of every new config
and the stored documents that carry the old default are rewritten once, so the LDN1272 project master keeps what the
frame set keeps (mono 0.71 → 0.92 of its inputs' star peak), a default-config mono master on the same data goes
0.67 → 0.91, and hc11 G goes 0.975 → 1.000. The OSC planes give up the global pair's accidental edge (0.9–1.9 points
per plane, e.g. runB R 0.978 → 0.962, G 1.000 → 0.981): `local` is neutral, the global pair's sign depends on the night.

**Diagnosis (the spec):** `docs/superpowers/research/2026-11-01-project-master-star-cores.md`. The stage is rejection;
the cause is `scaleZeroOffset`'s dispersion scale, a per-frame scale error of ±10–20 % against the flux scale; the
project path itself (resampling, LN, frames) is cleared; hc11 L shows no loss under any setting.

**Review:** Fable r1 (2026-11-01, `~/.athenaeum-acc/pcores/review/fable-r1.md`): APPROVE WITH CHANGES, six required —
all applied here (hc11 G a measured arm; Task 1's files, sweep cap and commit order; Task 3 runs from Measure with a disk
budget; Task 0's proof target; OSC bars, the drizzle control, the PSF SNR column, n < 20 unmeasured; user wording). One
applied differently: Task 0 does not clone Plan D's d1 trees (see Task 0, "Proof").

**Architecture:**
- **athenaeum-core `integration/stats.rs`**: `RejectionNormalization`'s `#[default]` becomes `Local`.
  `NormalizationConfig` derives `Default`, so `StackingConfig::default()` — the shipped default by contract
  (`preset(Default)`, `reset_stacking_defaults`, `resolve_config`'s fallback) — follows; `stacking/integrate.rs` is NOT
  edited.
- **`stacking/config.rs`**: `preset(FastPreview)` pins `ScaleZeroOffset` (its promise is speed: no LN stage, OD-3);
  `MaximumQuality` and `Default` inherit `Local`; `project_seed_config` inherits it from the global default.
- **`db/schema.rs`** (`init_db`): one marker-gated rewrite of the four stored documents (OD-2).
- **`stacking/run.rs`**: the run warnings of the LN fallback paths name the rejection normalization when LN drives
  rejection only (the user never turned LN on).
- **Frontend `src/components/stacking/panels/NormalizePanel.tsx`**: `local` rejection selectable with LN output off (LN
  then runs for rejection only — the engine's existing composite mode); the reset-to-`scaleZeroOffset` branch goes.
- No command is added or changed; no TS-exported type gains a field (`RejectionNormalization` keeps its four values).
- **athenaeum-lab**: the replay becomes `star-lab/bin/core_replay.py` (+ `unittest`), driven by
  `harness/acceptance/pcores.sh`; results under `star-lab/results/pcores/`.

**Tech stack:** Rust (athenaeum-core), React/TS + vitest, Python 3.14 + numpy (lab).

## Owner decisions (before Task 1; the controller's recommendation first, Fable concurs)

- **OD-1 — the default.** (a) `local` for every new config, LN computed for rejection even when LN output is off
  (costs the Normalize stage: 34–37 s per ~90-frame mono group, 90–100 s per OSC group on LDN1272, about the
  integration's own time); default mono masters 0.67 → 0.90–0.91 of the inputs' star peak on LDN1272, default OSC
  planes 0.98–1.00 → 0.97–0.99 (R 0.981 → 0.971, G 1.000 → 0.988, B 0.987 → 0.981 on runB), hc11 L unchanged, hc11 G
  +0.02 — **recommended**. (b) Only the configs that already run LN (Maximum quality, the panel's LN toggle, the stored
  LN-on documents); the shipped default keeps 0.67 on every mixed-seeing mono night, unseen.
- **OD-2 — stored documents.** (a) Rewrite every stored `"rejection": "scaleZeroOffset"` to `"local"` once (the global
  defaults row `stacking.defaults`, each user preset in `stacking.presets`, `stacking_set_config`,
  `stacking_project_config`); it was the default nobody picked (the panel never offered a reason to) and it costs a
  third of the star peak where seeing varies — **recommended with OD-1a**. (b) Only the LN-on documents (the trap) —
  the natural pair of OD-1b. (c) None; the owner switches the LDN1272 project by hand.
- **OD-3 — Fast preview** keeps `scaleZeroOffset` (no LN stage, a preview's promise is speed) — recommended; or `local`.

Everything below assumes OD-1a, OD-2a, OD-3 as recommended; Task 1's steps marked (OD) change with another answer.

## Global Constraints

- No third-party program name anywhere in this repository; the comparison program is "the ERP". Lab tools live in
  athenaeum-lab; nothing measurement-side is committed here.
- **Plan D is running** (branch `plan-d`, worktree `.claude/worktrees/plan-d`, ledger
  `.superpowers/sdd/2026-10-31-plan-d/`, clones `~/.athenaeum-acc/pland-*`): never touch them. This plan branches from
  `main` and does not edit `stacking/integrate.rs` (Plan D carries +1 204 lines and an uncommitted edit there). Task 1's
  test sweep is its own commit, landed after Plan D merges or rebased by whichever lands second (Plan D adds +759 lines
  to `run.rs` tests and `run_reweigh_tests.rs`). athenaeum-lab carries Plan D's uncommitted edits (`pland_bars.py`, its
  test, a run-log): Task 0 commits only its own new files, by path, never `git add -A`, and never while `git status`
  shows a Plan D file staged.
- 16 GB dev Mac: one compile at a time (`pgrep -fl 'cargo|rustc'` first), `CARGO_BUILD_JOBS=4`,
  `CARGO_TARGET_DIR=target-agent` for agents, `.superpowers/target-acc` for the lab's release build; heavy runs under
  `athenaeum-lab/harness/acceptance/heavy_lock.py`. Never `rustfmt` `stacking/run.rs`, `plan.rs`, `ln/mod.rs`,
  `ln/scale.rs`, `stacking/measure.rs`; no `cargo fmt`.
- Disk (lean-execution §7): every clone and output under `~/.athenaeum-acc/pcores-*` with a `REGISTRY.tsv` row each,
  `job`, expiry plan end + 7 d; `disk-report` before the first clone and at close. `~/.athenaeum-acc` already reads over
  its 500 GB budget by `du` (APFS clones counted per clone; `df` 1.2 TB free on 2026-11-01): Task 3 keeps at most two
  arm clones on disk at a time (see Task 3). The diagnosis folder `~/.athenaeum-acc/pcores` is deleted at close once
  Task 0 has committed the scripts and the diagnosis TSVs.
- `tracing` only; the migration logs one `info!` with per-store counts and `warn!`s every document it cannot decode
  (left unchanged). New log field names (`documents_rewritten`, `store`) go into the logging spec's dictionary in Task 1.
- A numeric change is judged on the whole group at master level (Task 3), never on a probe; the lab reader IS the
  product at the replayed pixels (gate R0) or its numbers do not count.

## Task 0 — the lab reader (athenaeum-lab; `sonnet`, scripted)

**Files:** create `star-lab/bin/core_replay.py`, `star-lab/tests/test_core_replay.py`,
`harness/acceptance/pcores.sh`, `star-lab/results/pcores/d0/` (the diagnosis replays). Source:
`~/.athenaeum-acc/pcores/scripts/{frames,replay,summ2,regpair}.py` (the diagnosis versions; `replay.py` reads one plane,
LN sidecars by channel, `.rej` bitmaps by plane).

- [ ] Merge `frames.py` into `core_replay.py` (`extract <clone> <group-like> [--plane p]` → TSV; `replay <tsv>
  <master> --actual szo|local [--rej-dir] [--weights2] [--plane]`; `summary <tsv>`). Same algorithms, no new numerics.
- [ ] `unittest` (no clone needed): the Linear fit clip port against the vectors of `combine.rs`'s tests (a clean ramp
  keeps all; one spike rejected; a constant stack terminates; all-rejected keeps the last survivor prefix); the median
  line against the `medfit_line` oracle's cases; `.athln` decode of a synthetic constant grid (a = 1.05, b = −0.01)
  evaluates to (1.05, −0.01) everywhere, edges included.
- [ ] `pcores.sh replay <clone> <group-like> <actual> [--plane p]` runs extract + replay (300 stars, seed 7) + summary
  and prints ≤ 30 lines: **gate R0** (replay vs master pixel |Δ|/peak median ≤ 1e-5 and p99 ≤ 1e-4; `.rej` agreement
  = 1.0000 where the run kept bitmaps) and the stage table.
- [ ] **Proof.** Commit the diagnosis replays as evidence: the `*_v3.tsv` + `.log` files and the `*_frames.tsv` inputs
  of `~/.athenaeum-acc/pcores/out/` (a few MB) under `star-lab/results/pcores/d0/`, with `summary` run over each by the
  new tool reproducing the diagnosis' cells ±0.001. The D1 clones are Plan D's and are cleaned once its compare is
  committed; they are not cloned here (holding 100–200 GB of them through the owner's wait is the opposite of the
  results-only rule). R0 on fresh pixels is proven on Task 3's first arm (A2 before: s204 keeps `.rej` bitmaps through
  its drizzle), before any bar is read.

## Task 1 — the default and the migration (athenaeum-core; `rust-engineer`, `sonnet`)

**Files:** modify `crates/athenaeum-core/src/integration/stats.rs`, `stacking/config.rs`, `stacking/run.rs` (run
warning texts only), `db/schema.rs`; tests in those files and wherever the sweep lands; the logging dictionary
`docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`.

- [ ] Before any edit: run `cargo test -p athenaeum-core --all-targets` once and keep the failing set and the wall as
  the baseline.
- [ ] `RejectionNormalization`'s `#[default]` → `Local` (derive kept everywhere). The `stats.rs` default test and
  `config.rs`'s pinned default document (l.1540 list: `"rejection":"local"`) updated; `config_hash` / stage-hash pins
  that fold the default document re-pinned once (`plan.rs` `stage_hashes_are_pinned_to_golden_literals` l.8487 and its
  siblings), each named in the report. That test also proves the Calibrate / Measure / Register subtrees do not fold
  `rejection` (`measurement_subtree` folds `scaleEstimator` only; `normalization_subtree` folds the whole block): if a
  pre-Normalize pin moves, STOP and report it.
- [ ] Sweep (memory `feedback-sweep-tests-for-invalidated-invariants`): with `rejection = Local`,
  `local_normalization_active` (`plan.rs` l.3062, `run.rs` l.14263) turns the Normalize stage ON for every
  default-config run and plan, and `integrate_planes` callers with `ln: None` hit the engine's "some frames have no
  grid" fallback (global pair, same numbers, one warn). The signals are few (`warnings.is_empty()`, `to_bits()` pins,
  `Stage::Normalize` stage lists, the golden pins): touch ONLY tests that fail or slow down against the baseline. A
  test that pins the M1 / global path sets `rejection = ScaleZeroOffset` with a one-line comment; a test about the
  default updates its expectation. **More than 40 sites is a STOP**: then one `#[cfg(test)]` helper (e.g.
  `StackingConfig::m1_global()`) replaces the per-site edits, reported. Full-suite wall before / after; > +20 % is a
  STOP (the tests opt out, not the product). The sweep is its own commit.
- [ ] (OD) `preset(FastPreview)` sets `c.normalization.rejection = RejectionNormalization::ScaleZeroOffset` with its
  reason in the doc comment (R-M4a-18's speed promise); a test that the three presets resolve to Local / Local /
  ScaleZeroOffset (Default / MaximumQuality / FastPreview).
- [ ] Run warnings: on the LN fallback paths (`run.rs` l.11178 group, l.13564 frame, l.13726 little overlap, l.12736 /
  l.12964 LN off for the group), when `normalization.local.enabled == false && rejection == Local` the RUN WARNING
  sentence names what the user chose — e.g. "Local rejection normalization could not be computed for this group; star
  cores were clipped with the global normalization" — not "local normalization". `tracing` messages stay the stable
  phrases they are. One test per path (the rejection-only composite tests at `run.rs` l.24654 are the template).
- [ ] (OD) `db/schema.rs`: `migrate_rejection_normalization_local(conn)` called from `init_db` after the stacking
  tables exist. `init_db` runs outside any transaction under `INIT_DB_LOCK`: open `BEGIN IMMEDIATE` and roll back on
  error with the archive rebuild's `is_autocommit()` pattern. Gated by a `settings` marker row
  `stacking.migration.rejection_local` written in the same transaction: absent → rewrite, then write the marker;
  present → return. Rewrites, with `json_set(…, '$.normalization.rejection', 'local')` where `json_extract(…) =
  'scaleZeroOffset'`: `settings.value` of `stacking.defaults` (a whole `StackingConfig`); every element of the
  `stacking.presets` array (`[{name, config}]`, `api/stacking.rs` `NamedPreset`: path
  `$[i].config.normalization.rejection`, rewritten as JSON values in Rust, never through `NamedPreset` decode, so an
  entry that does not decode is carried over untouched); `stacking_set_config.config_json`;
  `stacking_project_config.config_json` (local to the device; the hub never carries it). `updated_at` untouched (a
  migration is not an edit). One `info!` (`documents_rewritten` per `store`), a `warn!` + skip for a document that is
  not valid JSON.
- [ ] Tests: the four stores rewritten, `none` / `equalizeFluxes` / `local` untouched, a second `init_db` changes
  nothing (marker), a user who sets `scaleZeroOffset` after the migration keeps it, an invalid JSON row is left as is.
- [ ] Gates: `cargo test -p athenaeum-core --all-targets` (filtered output), `cargo check -p athenaeum-core
  --no-default-features`, `cargo check -p athenaeum-tauri -p athenaeum-web`. Commits: (1) default + presets + pins +
  warnings + migration, (2) the sweep.

## Task 2 — the Normalize panel (frontend; `frontend-dev`)

**Files:** modify `src/components/stacking/panels/NormalizePanel.tsx`, `src/components/stacking/stageSummary.ts`
(+ test); the Settings → Stacking defaults use the same panel (check `src/settings/registry.ts`; no Save button, no
banner, `notify()` on failure only).

- [ ] `local` stays selectable with LN output off; `handleLocalEnabledChange`'s reset branch goes; under the rejection
  select, one muted line when `rejection === 'local' && !local.enabled`: "Local normalization runs for rejection only".
- [ ] The rejection option label `local` reads "Local (recommended)"; the help names what it protects ("keeps the
  cores of the sharpest frames when seeing varies"), no internals.
- [ ] `stageSummary.ts`'s `· LN rejection` already keys on `rejection === 'local'` — test the LN-off case.
- [ ] Gates: `npm run test -- stacking`, `npx tsc --noEmit`. Commit. UI smoke in `npm run tauri dev` on the dev DB
  (memory `feedback-verify-by-launching-app`): the panel with LN off shows Local selected and the note; owed to
  open-items if the owner's desktop is not free.

## Task 3 — acceptance at master level (dev Mac, heavy lock; `sonnet` judgment run)

Build the release web server at the fix branch head and at its parent (`planb.sh build`). Each arm is a fresh
before-tree clone (`planb.sh clone`, `~/.athenaeum-acc/pcores-<arm>-{base,fix}`, a REGISTRY row each) run **from
Measure**: `clone-before.sh` step (3) removes `registered/ ln/ rej/ runs/` and `stackout/*`, and `MEASURE_VERSION` moved
since the allow-listed before trees, so nothing later can be reused. ~6–11 min and ~70 GB per run, 8 runs. Disk: run an
arm's before and after, read every bar off them, then `planb.sh strip-light` + delete both clones with their rows before
the next arm — at most two clones (~140 GB) on disk at a time, `PLANB_MIN_FREE_GIB` 100 kept.

| arm | dataset | stored config | before (parent build) | after (fix build) |
| ---- | ---- | ---- | ---- | ---- |
| A1 | runB project (mono L 83 + OSC 101) | LN on, `scaleZeroOffset` → migrated `local` | szo | local |
| A2 | s204 set (mono 92 + OSC 105, drizzle 2×) | LN on, `local` (control) | local | local |
| A3 | hc11 set (L 73 + G 28) | LN on, `scaleZeroOffset` → migrated `local` | szo | local |
| A4 | s204 set, the shipped default (LN off) written to the set | default | szo / szo | local / szo |

Bars, per master plane, read with `pcores.sh replay` on each master (R0 must pass on every replay first; A2 before is
the first replay and its `.rej` agreement must read 1.0000):
- **B-C (cores)**: core-peak ratio (master ÷ its inputs' no-rejection stack, all-band median of 300 stars): A1 mono
  ≥ 0.90, A4 mono ≥ 0.88; every OSC plane of A1 and A4 ≥ 0.95 (runB R's ≥ 400 σ band reads 0.948: the bar is the
  all-band median); A3 G ≥ 0.99 with its before value printed. Predicted (replay columns): A1 mono 0.920, OSC R / G / B
  0.962 / 0.981 / 0.975; A4 mono 0.903, OSC 0.976 / 0.990 / 0.982; A3 G 0.975 → 1.000.
- **B-P (project = set)**: A1 mono within 0.03 of A2 mono (0.920 vs 0.910 predicted); every A1 OSC plane within 0.015
  of A2's (R 0.962 vs 0.969, G 0.981 vs 0.984, B 0.975 vs 0.976).
- **B-0 (control)**: A2's masters bit-identical before / after — mono and OSC, plain AND drizzle 2× (the only drizzle
  arm; the drizzle reads the rejection bitmaps); A3 L's core-peak ratio unchanged ±0.005 (predicted 1.000 both).
- **B-N (noise)**: whole-plane pixel-difference noise (PD-24's reader, `pland_bars.py peak_ratio`'s denominator)
  after / before ≤ 1.01 on every changed plane; the MRS noise printed beside.
- **PSF SNR (a column, not a bar)**: PD-24's master PSF SNR ratio after / before printed for A1 and A4 beside B-C
  (Plan D recorded runB mono at 0.962 under szo).
- **B-R (rejection)**: the group's `rejected_fraction` printed before / after; a move above 1.0 pp is a STOP with the
  rejection maps written on a re-run (`writeRejectionMaps`) and the high map read at the cores.
- **B-T (trail)**: s204's satellite trail (frame `_0085`, the M4c record) stays rejected in A4 after: the master at the
  trail's pixels within 5 σ of its local background.
- **Wall**: A4's Normalize stage time printed (the price of OD-1a).
- **Unmeasured, listed in the verdict**: groups under 20 frames (the Auto ladder's other rungs) and the thin
  partial-coverage edge of a project grid.
- [ ] Verdict ≤ 30 lines + `star-lab/results/pcores/acc/`; the controller re-gates every bar from the files.

## Task 4 — the record (controller)

- [ ] `docs/stacking/README.md`: a ruling under the Integrate stage — the default rejection normalization is `local`
  (cause, the numbers of B-C / B-P / A4, the migration and its marker, Fast preview's exception); CLAUDE.md's stacking
  line only if the summary needs it.
- [ ] `docs/superpowers/open-items.md`: (1) with `local`, Linear fit clip still removes 8–9 % of the star peak on the
  LDN1272 mono groups and 1.6–3.8 % on its OSC planes (the sharpest frames' cores, a real PSF spread) — joins the owed
  star-protecting rejection item (M4c Task 7); (2) the later, cheaper lever for the default: the global pair scaled by
  Measure's common star flux (Plan D's `commonTflux`), no LN stage, a new mode, only if the LN stage's cost on default
  runs is judged too high; (3) Plan D's runB mono record (PD-21, PSF SNR 0.962, "cause in integration") — cause fixed
  here, Plan D's next runB read carries it. A hand-over note in the Plan D session's shared notes folder says the same.
- [ ] The release note line, plain words: "Stacking keeps the cores of your sharpest frames: the rejection step now
  matches frames by their star brightness. Saved stacking settings are updated once; the next stack of each frame set
  and project redoes its normalization and integration."
- [ ] Close-out: `disk-report`, the job's rows deleted with their paths, `Disk:` line in the report.
