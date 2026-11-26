# Solve verification as published: one chance-or-real test — design

Date: 2026-11-26. Status: owner calls settled (§3), revised after the builder check and the per-star scan (§5.1);
SV-8 is a contingency (§10). Releases: solvemyastro 0.4.0, then an Athenaeum pointer bump.

## 1. Problem and intent

A solve is judged twice: inside the solver (a seed log-odds, an inlier floor, a bright-star completeness test) and
again in the app's acceptance gate (an RMS ceiling, a floor, a 4 % inlier-ratio rule for dense fields, scale checks).
Neither judge is the published verification ([1], §2.4), and both fail real frames. The gate's ratio rule has never
armed (its in-field count reads 0); fixed, it would refuse 220 good solves of 4 231 (212 from the Milky Way set 217),
because the ratio falls with star density, and it catches no bad solve in this library. The completeness test counts
every bright catalog star without a detection against the solution, so a thin-cloud frame (28940, LDN 1272) would be
refused on a re-solve. The match count the solver stores and the pane shows is taken on the linear WCS before the
distortion fit: 15 for 28940, where its final WCS matches 120 (§5.1). The owner asked how the scientific papers do it ([1]; [2], [3]) and chose that
direction: one strict test that tells chance from real, accounts for how many hypotheses were tried, and does not
punish lost stars.

## 2. Goals, non-goals, anti-goals

Goals
- One decision in the solver: a Bayes factor between "the alignment is real" and "the image stars fall where chance
  puts them", over the image stars, accepted above 10⁹ ([1], §2.4). It replaces the completeness test and the gate's
  ratio rule.
- Its number (`log10_odds`) travels on the solution, is stored per solve and can be shown to the user.
- The gate keeps sanity backstops and re-reads that number; `expected_bright_in_fov` is removed.
- Validated on provoked false solves, cloudy frames and the real library before release, against numeric bars.

Non-goals: the search (quad matching, FOV ladder, the index search's agreement rule, the seed pre-filter of §6.3),
detection and its 600-star cap, tolerances, refit and SIP; re-judging solves already stored in a user's catalog; a
Settings section for the acceptance gate (it has none); registration's own inlier ratio; the legacy
`use_bayesian_verify = false` path.

Anti-goals (a failure even if it compiles)
- A rule that refuses good dense-field or cloudy frames, or one in which a missing catalog star counts against.
- A hand-set fraction threshold, or any threshold moved to pass the validation without a change to the model.
- A false solve accepted in the provocation run, or a library frame refused that the current solver accepts.
- Two tests that can disagree about chance-or-real (the gate re-reads the solver's number, it computes none).

## 3. Owner decisions (2026-11-26)

| Id | Decision |
| ---- | ---- |
| SV-1 | The dense-field rule is fixed by dropping the 4 % inlier-ratio rule, not by arming it. |
| SV-2 | One strict chance-or-real test that accounts for the hypotheses tried and does not punish lost stars. |
| SV-3 | The bright-star completeness test is reworked so that clouds do not refuse good frames. |
| SV-4 | Order: design, plan (reviewed), lab run of provoked false solves and cloudy frames, solvemyastro 0.4.0, Athenaeum. |
| SV-5 | Method: as the scientific papers do it, cited for the method (§11); no program is named as a comparison or as a reference implementation of this code. |
| SV-6 | Show the confidence: "Confidence: odds 10^X to 1" in the metadata pane's plate-solve block and in the refusal texts; the stored in-field count (0 or 1 on every row, the bug) is cleared to unknown. |
| SV-7 | `SOLVE_RECIPE_VERSION` goes to 3: every stacking-cached solve re-solves once under the new rule. |
| SV-9 | The owner supplies the lab's own frames (cloudy, narrow-field, DSLR, the recorded false alignments); the release waits for §7's minimum set. |
| SV-10 | Thin solves: accept on A alone, with the gate's backstop of 6 matched on the final WCS; no minimum count. The narrow-field frames of §7 show whether a thin true solve exists. |
| SV-11 | The match count is stored and shown on the final WCS (120 for 28940, not the linear 15); the pane, the refusal texts and the gate's floor count the stars the decision counted. Rows stored before keep their old count. |

## 4. How verification works today

Every step that can refuse a candidate, from seed to gate (paths under `solvemyastro/src/` unless stated).

| Step | Where | Refuses when | The method of [1], §2.4? |
| ---- | ---- | ---- | ---- |
| Seed scale | `orchestrate.rs:502` | seed scale outside 0.05–120″/px | backstop |
| Index search | `orchestrate.rs:1805`, `:1813`, `:1820` | outside the radius; repeats a refused one; after 16 free verifies, no agreeing second match | search economy |
| Verify cone | `orchestrate.rs:1924`, `:1985-2017` | under 4 stars in a cone of 0.75 × the long side (G 12, 14, then full depth until `clamp(n/4, 8, 100)`) | no |
| Seed pre-filter | `orchestrate.rs:2045-2063`; `verify.rs:65`, `:159` | under 4 seed inliers, seed RMS over 3σ, or log-odds under ln 10⁶ | half: the foreground-over-background ratio of [1], but over matched stars only (no term for unmatched image stars), N_R = the cone count (2.4–3.2 × the stars in the frame), the quad's own stars counted, at 10⁶ |
| Final floor | `orchestrate.rs:2169` | under 6 inliers after the refit (`MIN_ABSOLUTE_INLIERS`) | backstop |
| Completeness | `orchestrate.rs:2379-2401`; `verify.rs:116` | log-odds under ln 10⁹ over the brightest in-frame catalog stars, each expected found with probability 0.25 | no: every missed catalog star counts against |
| Gate RMS, floor | `accept.rs:78`, `:82` | RMS over 2.5 × `gate_tol_px`; under 6 matched | backstops |
| Gate ratio | `accept.rs:86` | `expected_in_fov > 100` and `inlier_ratio < 0.04` | no: a fraction heuristic; never armed (`expected_bright_in_fov`, `accept.rs:133`, reads 0) |
| Gate scale | `accept.rs:90`, `:94` | scale outside 0.05–60″/px, or over 8 × off the header's | backstop |

The gate runs in `service::acceptance_refusal` (`crates/athenaeum-core/src/plate_solve/service.rs:462`), called by
`store_result` (`:403`) and a stacking run's local solve (`stacking/solve.rs:319`). `inlier_ratio` is
`final_inliers / verify_stars.len()` (`orchestrate.rs:2179`), a cone denominator; its docs (`lib.rs:289`,
`service.rs:290`) say "over detected stars". A completeness refusal reaches the user unclassified (`diag.rs` `classify`).

## 5. Approaches

| | A. Bayes factor over image stars [1] | B. A contrario number of false alarms [2], [3] | C. Fixed chance-match probability |
| ---- | ---- | ---- | ---- |
| Statistic | log odds of "real" against "uniform chance", over the image stars in SNR order; mixture foreground | H × the binomial tail of catalog stars found within the k-th smallest residual, minimised over k | Poisson tail of catalog stars found within the fixed tolerance |
| Threshold | 10⁹: prior 10⁻⁶, a false positive costing 1999 missed solves | NFA ≤ 1, the usual ε of [3] | log10 P ≤ −12, chosen by hand |
| Hypotheses tried | the prior covers 10⁶; a clause raises the bar beyond (§6.1) | H itself, known only when the search ends; the search stops at its first acceptance | folded into the constant |
| Lost catalog stars | cost nothing | cost nothing | cost nothing |
| Image stars with no counterpart | −0.6 decades each, until the scan peaks | through the chance rate per star | through the chance rate per star |
| Residuals, brightness | both used | residuals only | neither |
| In the solver today | its matched-star half (`verify::log_odds`), the SNR order, the grid lookup | residual pairs, n, area; H to count | n, tolerance, area, the completeness test's in-frame list |
| Measured margins | 291 of 291 weak and median rows clear; weakest 314 decades (§5.1) | ≤ −365 per hypothesis on the same rows | 0 of 4 231 refused, weakest −28.0; documented false solve −3.4 (−9.6 least favourable reconstruction) |
| Reads as | "odds 10^X to 1" | "10^−X chance solutions expected" | "1 in 10^X" |

**Recommendation: A.** It is the method published for exactly this problem, astrometric verification ([1], §2.4),
whose authors report no false positive on about 420 000 survey images. Its threshold comes from a prior and a cost
ratio, not from tuning. The solver already computes its matched-star half; missing are the term for unmatched image
stars, the in-frame reference count, the exclusion of the hypothesis's own stars, and the threshold. Lost catalog stars
cost nothing by construction, so clouds remove evidence but add no penalty, and the brightness order carries what the
completeness test wanted (a true solve's matches lead the bright end) without its penalty. Adding stars until the odds
clear is safe (§6.1), and the number reads as odds.

Why not B as the rule: with its minimal sample subtracted, as [3] does, B refuses the documented false solve too (with four
subtracted, at least two chance matches among the rest have probability 0.23, so NFA > 1 from H ≥ 5; from H ≥ 367 in
the reconstruction with 50 detections). Its drawbacks lie elsewhere: H is known only when the search ends, and the search stops at its first
acceptance; and it ignores brightness. Its advantage is that it does not depend on the SNR order. On the library A and
B accept the same rows (§5.1), so B stays the fallback of SV-8. Why not C: −12 is hand-set, and C ignores residuals and
brightness. C's statistic stays as the lab's cross-check (§7).

### 5.1 Measured: the per-star scan (2026-11-26)

A read-only probe re-detected 291 stored solves as the solver does (rustafits at the app's version, the SNR order, the
cap of 600) and walked A on each stored final WCS. The rows were the 63 (39 frames) that a builder check put below A's
break-even by their stored match count, all 271 rows under 1.5 × that break-even, and 20 rows at the median.

- A clears every row: 291 of 291 at ≥ 9 decades, with 0, 4, 8, 16 or 28 matched stars set aside as the hypothesis's
  own (a pessimistic proxy: the highest-ranked matches). The weakest is 28940: 314 decades with 4 set aside and 238
  with 28. It reaches 9 decades at its 29th test star.
- The margin rests on the match count on the final WCS, 109–455 within 1σ per row. On 28940 that is 120 of 584
  detections, against about 0.1 expected by chance. It does not rest on the matches leading the SNR order: they are
  only mildly front-loaded (28940: 1st, 5th and 10th match at ranks 8, 23 and 41).
- B is ≤ −365 per hypothesis on every row; A and B agree on all 291.
- The stored `matched_stars` is `count_inliers` on the linear refit WCS (`orchestrate.rs:2158`), before the SIP fit.
  Dropping SIP from the stored WCS reproduces it (median ratio 0.97, p10–p90 0.89–1.03), while the final WCS matches
  2.9 × more (median; 28940: 14 against 120). The break-even premise, and every reconstruction here that starts from a
  stored count, rest on that linear count. SV-11 stores the final one.
- Centroid refinement on or off moves A by ≤ 1.3 decades. Stored `crpix` is 0-based (the FITS writer adds 1).

Evidence (local, outside the repo): `~/.athenaeum-acc/gate-measure/scan-report.md` and `scan/rows_scan.csv`.

## 6. Design

### 6.1 The decision

```text
test stars  T  image stars in SNR order (select_fast_stars), highest first, minus the hypothesis's own stars (below)
references  R  verification-set catalog stars inside the frame on the final (SIP) WCS; N_R of them
per star       ln(p_F / p_B) = ln( d + (1 − d) · A / (2π σ² N_R) · exp(−r² / 2σ²) )
               r = distance to the nearest unclaimed reference; beyond 3σ (fixed) the star adds ln d; d = 0.25
               (SolveConfig::distractor_fraction); σ = the matching tolerance tol_px; A = W × H px².
               A reference explains at most one test star; a later star near it is a distractor.
running        L_n = Σ_{t ≤ n} ln(p_F / p_B);   log10_odds = max_n L_n / ln 10
accept         log10_odds ≥ 9 + max(0, log10(H / 10⁶));   H = hypotheses verified so far in this solve, all passes
```

- As in [1], §2.4: the background is uniform over the image; the foreground mixes a uniform part (an image star with no
  catalog counterpart: a star can be lost, a false one added) with a Gaussian around each predicted catalog star. A
  catalog star with no detection adds no term; it only enlarges N_R, which is the density-awareness. The costs ([1],
  §2.4): true positive +1, false negative −1, true negative +1, false positive −1999; accept when the posterior odds
  exceed 10³, that is K > 10³ / 10⁻⁶ = 10⁹.
- **The scan is safe.** Under the background exp(L_n) is a likelihood ratio, a non-negative martingale of mean 1, so the
  chance that it ever reaches 10⁹ as stars are added is at most 10⁻⁹ per hypothesis (Ville's inequality), wherever the
  maximum falls. Per frame that is at most H × 10⁻⁹ for H ≤ 10⁶; the clause holds it near 10⁻³ × (1 + ln(H / 10⁶))
  beyond. These bounds hold under the model; the lab measures the real tail.
- **What the hypothesis count does, plainly.** H stays far below 10⁶ in practice (V-6 reports it), so the bar is the
  constant 9. The hypotheses tried are accounted for by the fixed prior of 10⁻⁶, a budget, not by counting them. H as
  counted (every candidate that reaches verification) also leaves out the trials rejected before it.
- **The hypothesis's own stars are left out, per search path.** For a spiral seed that is every image star of every
  quad match its affine was fitted to or admitted by (at least `minimum_quads` = 3 + n/140, 7 for 600 detections). For
  an index hypothesis it is its quad and the second match that agreed with it (`orchestrate.rs:1820`). Their catalog
  stars are claimed from the start. These stars chose or fitted the hypothesis, so they are not independent evidence;
  [3] subtracts its minimal sample for the same reason (the text of [1] does not state it). Trials carry these
  image-star indices (`Quad::star_idx` exists; `TrialResult` and `index::Hypothesis` carry none today). The cost on
  good frames, measured (§5.1): with 28 matches set aside the weakest library frame stays 229 decades above the bar.
- σ is the matching tolerance, never fitted from the residuals, so the evidence cannot confirm itself.
- Scale of the numbers: in this library (1.9–2.8″/px) a matched star is worth 2.8–3.6 decades (2.9 in set 217,
  N_R ≈ 1 730; 3.4 at the median), and an unmatched image star ahead of it costs 0.6. Measured, the weakest library
  frame peaks at 314 decades (§5.1). The documented false solve (σ 13.8 px, N_R ≈ 61, 6 matched by its stored, linear
  count) reconstructs at about 5 decades with four stars set aside, and at 14 if its six matches were its brightest
  detections. That is a reconstruction from a stored count; V-2 measures the original frame.

### 6.2 What goes, what stays

- Goes: the completeness test (`bright_star_evidence`, `orchestrate.rs:604`; `bright_star_log_odds`,
  `BRIGHT_MATCH_PROB`, `BRIGHT_LOG_ODDS_MIN`, `verify.rs:96-121`; its call, `orchestrate.rs:2374-2401`). The decision
  takes its place at the same point, on the same final WCS. In the gate: `DENSE_FIELD_MIN_EXPECTED`,
  `Gate::min_inlier_ratio`, `GateMetrics::{expected_in_fov, inlier_ratio}`, and `expected_bright_in_fov` with its
  radius formula and recipe-bump note (`accept.rs:13-16`, `:117-162`). Athenaeum's `blind_min_inlier_ratio`
  (`config.rs:51-55`), with its TS field (`src/types/plate-solve.ts`, regenerated) and its test mentions
  (`registry-coverage.test.tsx`, `PlateSolveSettingsPanel.test.tsx`); a stored config still decodes (unknown keys are
  ignored, as a test already pins for others).
- Stays: **the seed pre-filter at ln 10⁶** (`orchestrate.rs:2045-2063`) as a fast pre-filter, not the decision. It
  scores the rough pre-refit seed over matched stars only, an optimistic statistic, so a lower bar suits a filter.
  Raised to 10⁹ it would refuse true seeds the refit exists to rescue and change which candidates reach the decision,
  which is the search. Its 4-inlier and 3σ backstops, `MIN_ABSOLUTE_INLIERS` 6, the cone and its ladder stay too.

### 6.3 The gate

`Gate::accepts` checks, in order: RMS ≤ 2.5 × `gate_tol_px`; matched ≥ 6 (on the final WCS, SV-11); `log10_odds` ≥ `verify::ACCEPT_LOG10_ODDS`
(9, the solver's own constant: the same test, kept so the CLI's `--no-gate`, a lowered lab threshold and the app line
up); scale within 0.05–60″/px; scale within 8 × the header's. `Gate::refusal(&GateMetrics) -> Option<GateRefusal>`
names the failed check, and `accepts` is `refusal().is_none()`. `GateMetrics` gains `log10_odds: f64`; the gate only
judges fresh solves.

### 6.4 solvemyastro 0.4.0: API

- `SolveSolution` gains `log10_odds: f64`, `reference_stars_in_frame: usize` (N_R), `hypotheses_verified: usize` (H).
  With SV-11, `matched_stars` is counted on the final WCS against those references (1σ, one detection per reference),
  the count the decision scans, and `inlier_ratio` becomes `matched_stars / reference_stars_in_frame`. Both keep their
  names, because the app's columns and the CLI's JSON carry them; the changed meanings go in the doc comments and the
  CHANGELOG. Breaking (public fields).
- `SolveConfig` gains `accept_log10_odds: f64` (default 9.0; the lab raises or lowers it). `verify` gains the scan and
  `ACCEPT_LOG10_ODDS` and loses the completeness items; `accept` changes as in §6.2 and §6.3.
- `diag::VerifyTrace` gains the decision's numbers (log10 odds, N_R, stars scanned, the rank where the scan peaked,
  matched in the scan). `FailureClass` gains `NotConfident { best_log10_odds }` (code `NOT_CONFIDENT`) for a search
  whose best candidate reached the decision and was refused there.
- CLI `solve --json` drops `expected_in_fov`, adds `log10_odds`, `reference_stars`, `hypotheses`; text output prints
  the odds; a refusal prints the gate's named reason.

### 6.5 Athenaeum

- `SolveResult` takes `log10_odds` and puts `reference_stars_in_frame` in `expected_catalog_stars_in_fov`, so the
  metadata pane's "Inliers: m / N" (`MetadataPane.tsx:1298-1303`, today "m / 0") shows the true denominator.
  `solve_frame_with_hints` stops calling `expected_bright_in_fov` (`service.rs:272-294`).
- Schema: `plate_solves.log10_odds REAL`, nullable, by the guarded `ALTER` pattern; the same guarded step sets the old
  `expected_catalog_stars_in_fov` values to NULL (all 0 or 1, the bug; the pane already shows NULL as unknown).
  `PlateSolveRecord` gains `log10_odds: Option<f64>`, `#[serde(default)]` for stacking's cached payloads; the TS type
  is regenerated.
- `acceptance_gate` maps six fields. The refusal text names the check (code stays `REJECTED_LOW_CONFIDENCE`), e.g.
  "the match could be a chance alignment (odds 10^6.2 to 1, 10^9 needed; 12 stars matched)" or "only 5 stars matched
  (6 needed)". `describe_solve_failure` for `NOT_CONFIDENT`: "A candidate position was found, but its star match could
  be chance (best odds 10^6.2 to 1; 10^9 needed)."
- Decided (SV-6): "Confidence: odds 10^X to 1" in the pane's plate-solve block (X rounded down; a dash for rows stored
  before the change). With SV-11 the pane's "Inliers: m / N" shows the final count over N_R on new rows; a row stored
  before keeps its old, linear count over an unknown denominator. No Settings change, no Tauri command change.

### 6.6 Verdicts and caching

The verdict changes on purpose. `solver_config_value` hashes `blindMinInlierRatio` (`config.rs:252`), so removing it
moves every cached solve's hash anyway. Decided (SV-7): drop the key and bump `SOLVE_RECIPE_VERSION` to 3 in the same change,
updating the pinned string at `config.rs:391`, as recipe 2 did for 0.2.0's verification change. What re-solves: every
stacking-cached SOLVED solve, once, on its set's next run (cached failures and refusals are retried once per app version
already). Catalog `plate_solves` verdicts are not re-judged; the one change to stored rows is §6.5's guarded step that
clears the buggy in-field count (SV-6). A frame the user re-solves meets the new rule. Wanted, because a
cached SOLVED entry is the old judge's verdict; the cost is one hinted re-solve per cached frame. The alternative (keep
the dead key, no bump) leaves stacking caches judged by the completeness test.

## 7. Validation before release

A lab tool in `athenaeum-lab` drives solvemyastro's public API at the branch commit (no lab code in Athenaeum or
solvemyastro); outputs go under `~/.athenaeum-acc/<job>/` with a `REGISTRY.tsv` row. Lowered runs set
`accept_log10_odds` to infinity, so nothing is accepted and every scored candidate lands in the trace, and lower the
solver's floor of 6 inliers to 4 through a lab knob, so candidates under the floor are scored too. Lowering the seed
pre-filter changes nothing (the four quad stars alone give about 12 decades), so it stays. The tool also computes C's chance-match tail per trace row, so a comparison table (not a bar) shows which false
candidates and which good frames A, the old completeness test and C would each accept or refuse.

Frames: 40 library frames (10 from set 217; the 10 weakest by the final-WCS match count of §5.1, 28940 among them; 10
at the median; 10 from set 258's camera). The recorded false alignments: the 2026-09-05 DSLR frame (0.2178″/px, Dec −68, 6
inliers), the 0.39″/px frame a blind search took for 0.25″/px (7 of 58 bright stars, commit ba6048a), the 51″/px Milky
Way field of 0.2.0's notes. Cloudy frames: 28940 and at least 10 more from the owner (thin high cloud, passing cloud, a
gap). Narrow-field (< 1″/px) and DSLR frames from the owner: the library has none, and the false solves came from there.
The minimum set, release-blocking (SV-9): the three recorded false alignments, at least 10 cloudy frames, at least 3
narrow-field frames at ≤ 0.4″/px and at least 2 DSLR frames. At ≤ 0.4″/px σ is ≥ 7.5 px, where about one chance match
per frame is expected; the library never comes near that.

A candidate is false when it lands outside V-5's tolerance of the frame's stored solve (for a recorded false alignment,
of its known true field). An attempt in arm 1 or 2 that still finds the true field is a true solve, reported apart.

Arms, 1–4 each at production thresholds and lowered: (1) wrong position, hint moved 5°, 20°, 90°, 180°, radius 2°;
(2) wrong scale, hint × 0.25, × 0.5, × 2, × 4, a 10 % window; (3) wrong sky, blind, the index restricted to the
opposite hemisphere and to every pack but the true field's; (4) no sky, x and y of the detections permuted
independently, blind, five permutations per frame (more until V-2's minimum is reached); (5) truth, correct hints, plus
every cloudy frame and the 40 library frames blind.

Library: re-solve every distinct frame of the 4 231 stored solves, hinted (3 207 frames), reported by frame set, and all
68 rows of set 287 (LDN 621, the one set whose solved centres disagree) frame by frame. The weak-row selection of the
first draft rested on the stored, linear match count (§5.1), so the lab takes the whole library instead of a sample.

| Bar | Pass |
| ---- | ---- |
| V-1 false accepted | 0 in all production attempts of arms 1–4 (about 650; with none, < 0.5 % per attempt at 95 %) |
| V-2 false margin | lowered runs, at least 10⁴ scored false candidates: best false log10_odds ≤ 6, three decades under the bar; the counts at ≥ 2, 3 and 4 decades within the bound H_total × 10⁻ᵏ (95 % Poisson slack); each recorded false alignment ≤ 6; reported per search path, with four stars set aside and with §6.1's set |
| V-3 good refused | 0 in arm 5, the cloudy frames and the library; 28940 solves |
| V-4 good margin | weakest good log10_odds ≥ 12; min, p1, p5 reported |
| V-5 same answer | each re-solve within 0.02 half-diagonals and 1 % in scale of its stored solve |
| V-6 hypotheses | largest H per frame reported (≤ 10⁶ expected) |
| V-7 speed | median solve time on the library within +10 % of 0.3.0 |

A failed bar stops the release and goes to the owner. V-1 to V-3: the frame first (a stretched image, the per-star
scan); a model fault (the quad exclusion, σ, a uniform background in a clustered field) is fixed and the whole run
repeats; a threshold is never moved alone. V-3 on a cloudy or weak frame whose matched stars sit deep in its SNR order is
A's known risk (§9), and the owner's call (SV-8). V-5: a solve that lands elsewhere is a finding about the stored
library. V-6 above 10⁶: the clause of §6.1 is active there. V-7: profiled. Each is reported.

## 8. Releases and docs

- solvemyastro, on a branch: §6.1–§6.4 with unit tests (lost catalog stars leave the odds unchanged; an unmatched image
  star costs ln d; in a seeded uniform-null simulation of 10⁴ hypotheses at most 10⁻³ of them, with sampling slack,
  reach odds of 10³; the gate's named refusals; the recorded false-solve numbers as a gate case). The corpus gate's
  frozen baseline is regenerated for `inlier_ratio` and `log10_odds`, every bench WCS unchanged. Merge to its `main`
  with a merge commit, tag v0.4.0, crates.io by its workflow. CHANGELOG 0.4.0 (breaking: §6.4), README (the gate, the
  JSON fields, the known issue removed), the doc comments of `verify.rs` and `accept.rs`.
- Athenaeum, after the tag: pointer bump to the tagged commit on solvemyastro `main`; §6.5 and §6.6; a Windows run on a
  `wintest/` branch before `main` moves.
- Docs: `docs/platesolving/input-and-acceptance-gates.md` (the decision, the gate's table, the history of the 4 % rule
  and the completeness test with this job's numbers); `docs/platesolving/README.md` (the solver description, the
  acceptance-gate section, "Recipe and identity" to recipe 3, the N_R note); `docs/superpowers/open-items.md` (close
  "The in-field count reads 0"; owed smokes: the pane's number, one stacking set re-solving under recipe 3); the Plate
  solving block of `CLAUDE.md` (one clause: the solver decides chance-or-real by a Bayes factor at 10⁹, the gate holds
  backstops); `docs/stacking/README.md` where it names the recipe.

## 9. Risks, and what is left to the builder

- **Image-star depth.** A pays 0.6 decades per unmatched image star ahead of a match. A frame whose highest-SNR
  detections are not stars (hot pixels in a raw light, cloud structure) can hold its matches too deep to clear. This is
  the one way A can refuse a good cloudy frame. On the library no frame comes near it (§5.1): the margin rests on the
  match count. It stays open for frames with few real matches; V-3 tests it on the owner's cloudy and narrow-field
  frames.
- **The quad exclusion carries the false side.** Without it, the documented false solve's six matches could reach about
  14 decades if they were its brightest detections. V-2 is the evidence.
- **A uniform background is a model.** Clustered stars, a bright nebula core or vignetting raise the local chance rate;
  arm 4 flattens structure, so arms 1–3 test it. The Ville bound holds only under the model.
- **Overfit.** The final WCS is refitted to its inliers (6 affine and 14 SIP terms); with σ fixed a refit gains at most
  0.5 nats per match inside 1σ. Measured by V-2.
- **One focal-length band.** The library is 1.9–2.8″/px on four sensors; the narrow field, where both recorded false
  solves came from, rests on the owner's frames and the recorded false-alignment frames. The reconstructions in §5 and
  §6.1 (frame size, the false solve's detection count, its stored linear match count) are not measurements; the lab
  runs the original frames.
- **Left to the builder:** names inside solvemyastro; the scan's code shape; how a trial carries the image-star indices
  of §6.1's set; H threaded through `solve_with_index`'s steps A, B1 and B2 with one counter per call, recentred seeds
  counted; the lab knob for the floor of 6; scoring a recorded false alignment by hinting at its stored position;
  `GateMetrics::log10_odds` when `blind_gate_enabled` is false; the lab tool's layout within §7's bars; the pane's
  wording.
- **Code facts the builder starts from:** the SNR order and the cap of 600 (`select_fast_stars`); σ
  (`adaptive_tol_px`, 3″ clamped to 1.5–20 px); N_R's list (`bright_star_evidence` builds it); the decision point
  (`orchestrate.rs:2374`); the gate judges fresh solves only (`service.rs:462`, `stacking/solve.rs:319`); the guarded
  schema step (`schema.rs:2278`); the recipe hash (`config.rs:252`); stored `crpix` is 0-based (§5.1).

## 10. Open: the owner's calls

| Id | Question | Proposal |
| ---- | ---- | ---- |
SV-6, SV-7, SV-9, SV-10 and SV-11 are decided (§3). SV-8 is a contingency: it is asked only if V-3 fails.

| Id | Question | Proposal |
| ---- | ---- | ---- |
| SV-8 | If V-3 fails on an owner's cloudy or narrow-field frame for the depth reason of §9: switch the rule to B, or accept the refusal? | Switch to B with ε from the same 1999:1 cost ratio. The library has no such frame (§5.1). |

## 11. References

- [1] D. Lang, D. W. Hogg, K. Mierle, M. Blanton, S. Roweis, The Astronomical Journal 139, 1782–1800 (2010),
  arXiv:0910.2233, §2.4 (blind astrometric calibration; verification as a Bayesian decision).
- [2] L. Moisan, B. Stival, "A probabilistic criterion to detect rigid point matches between two images and estimate
  the fundamental matrix", International Journal of Computer Vision 57(3), 201–218 (2004).
- [3] L. Moisan, P. Moulon, P. Monasse, "Automatic homographic registration of a pair of images, with a contrario
  elimination of outliers", Image Processing On Line 2, 56–73 (2012).
