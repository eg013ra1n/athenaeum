# Calibration scoring: closeness and compatibility are two answers

Design, 2026-09-05. Backlog item 6 of `docs/backlog-v0.5.5.md` (stage 3), core
half. The UI half — merging the two assignment modals, the always-visible
filter, the camera / exposure dropdowns and click-to-fill — is a second pass
and is out of scope here (§8).

## 1. What is wrong

Every finding below was measured on the owner's real catalog (a copy of the
dev database, 2026-09-05), not reasoned from the code.

**The engine is healthy where it is allowed to run.** 4223 links exist, mean
`match_score` 0.99, not one zero. Auto-link and the scoring formula are not
the problem.

**Finding 1 — every incompatible candidate scores exactly 0.** For the real
light 28748 (ATR2600M, gain 100, offset 30, 180 s) the manual modal offers 268
candidates and **all 268 read 0.0 %**. `find_calibration_candidates` computes
the score as

```rust
let score = if passed_hard_filter { score_match(…) } else { 0.0 };
```

so the whole "incompatible" block is one flat zero, and inside it the list
keeps the query's `date_start DESC` order. A dark set from the same camera
with the same gain and a one-second exposure difference sits interleaved with
sets from a different camera. That is the owner's report: *the list should
still sort usefully by score so the right frames surface instead of being
buried.*

**Finding 2 — the reason for a rejection never reaches the screen.** The wire
carries six booleans (`MatchDetails`), but `ccd_temp` and `focallen` can fail a
candidate and are not among them. Real case: light 24951 (QHY268M, gain 56,
offset 30, 2 s) against dark set #943 (same gain, same offset, 1 s) renders
"camera ✓ gain ✓ offset ✓ exposure ✓" and **0 %**. What refused it was
`ccd_temp` — that light was taken at +19.4 °C. The engine is right; the user
is told nothing.

**Finding 3 — the master library is invisible to auto-link.** All 133 master
sets in the catalog have `gain` and `offset` NULL (their FITS headers carry
neither; all 133 are imported — `master_provenance` is empty). Both parameters
default to Exact + required, and "nothing to compare" is treated as a refusal
(`ParameterCheckResult::skip` → `passed_hard_filter = false`). Result: **zero
master links in the entire catalog**, while CLAUDE.md claims "Masters are
always auto-link candidates".

Proven by construction: filling `gain = 56, offset = 30` on master #1698 (what
the existing Bulk Edit in the Master Dark/Flat Library does) turned it from
"not a candidate" into the **top** auto-link candidate at 48.4 %, ahead of the
raw set at 20.8 %.

**Finding 4 — "compatible only" means "perfect only".** `api::calibration`
drops candidates with `match_score < 0.1`. Since every incompatible candidate
scores 0, that threshold is really a compatibility filter — but a lossy one:
for light 24951 it leaves the list empty even though a usable dark exists.

## 2. Root cause

One scalar answers two different questions. `match_score` means both *did this
pass the user's configured filter* (binary) and *how close is it* (continuous).
Collapsing the first into the second destroys the second wherever the first is
false. The binary answer is already expressed structurally — the engine returns
`passed_hard_filter` and orders compatible before incompatible — so the
zeroing is redundant as well as destructive.

Finding 3 is a separate, data-shaped cause: a parameter the set does not
declare. **Owner decision (2026-09-05): the matcher's semantics do not
change — "cannot compare a required parameter" stays a refusal. The data is
what gets fixed.**

## 3. Score is closeness, always

`score_match(date_diff, temp_diff, exptime_diff, scoring)` runs for every
candidate, compatible or not. The `else { 0.0 }` branch is deleted.

Ordering: compatible candidates first (master preference applies inside that
block, as today), then incompatible ones.

Inside the **compatible** block every candidate satisfies the config, so
closeness is the whole question and the block sorts by descending score.

Inside the **incompatible** block the question is different — *how near a miss
is this?* — and closeness answers it badly on its own. Measured on the real
catalog: for light 28748 a dark from ANOTHER camera scored 43 % on
date/temperature/exposure while the same-camera set, wrong only in its offset,
scored 16 % and sat tenth. So the block ranks by the summed WEIGHT of the
rules a candidate breaks, ascending, with closeness only breaking ties.

Weights follow the owner's ranking (2026-09-05), with the assistant's three
amendments accepted in the same exchange — exposure added to darks above
temperature (dark current is linear in time and nothing here scales a dark, so
a wrong exposure is unusable, not merely worse), offset added beside gain (it
shifts the pedestal directly; the owner's own catalog has ATR2600M lights at
offset 30 and 200 with darks only at 200), and the filter added to flats above
the date (a flat through Ha is not a flat through L):

| rank | dark | bias | flat / darkflat |
| ---- | ---- | ---- | ---- |
| decisive | camera | camera | camera |
| major | binning | binning | filter |
| serious | gain, offset | gain, offset | binning |
| notable | exposure | — (a bias has none) | telescope, focal length |
| minor | temperature | temperature | gain, offset |
| slight | everything else | everything else | everything else |

The steps (1000 / 200 / 40 / 10 / 3 / 1) are wide enough that a heavier rule
always outweighs every lighter one put together, so the sum orders candidates
exactly as the table reads. The date is not a rule — it is continuous and
already lives in the closeness score, where it acts as the final tie-break.

A parameter the set does not DECLARE costs half of a contradicted one: an
imported master with no GAIN may still be the right master, while a dark whose
gain is demonstrably different is not.

**This ranking is the score only.** What is compatible in the first place stays
exactly what Settings → Calibration Matching says (owner, 2026-09-05); no
weight here can admit or refuse a candidate.

## 4. Compatibility travels as its own field

`CalibrationCandidate.passed_hard_filter` already exists and is already
correct; it simply never left the core. It reaches the frontend as
`CalibrationSetWithScore.compatible`.

## 5. Per-parameter verdicts on the wire

The engine already computes `CandidateMatchDetails` — one `ParameterMatch` per
parameter, with the mode, both values, the difference and the thresholds.
`match_details_from_candidate` throws almost all of it away. The full
breakdown is exported instead, so a card can say *"ccd_temp: +19.4 vs −10.0,
over the 5.0 limit"* or *"gain: the set does not declare one"*.

`ParameterMatch` cannot express the second sentence today: `skip()` leaves
`matched: false`, indistinguishable from a real mismatch. A `unknown: bool`
field is added and set by `skip()`.

New wire types (`athenaeum-core/src/models.rs`, exported through
`ts_export.rs`):

```rust
#[serde(rename_all = "snake_case")]
pub enum ParameterStatus { Match, Warning, Mismatch, Unknown }

#[serde(rename_all = "camelCase")]
pub struct ParameterVerdict {
    pub name: String,               // "instrume", "gain", "ccd_temp", …
    pub enforced: bool,             // MatchMode is not Ignore
    pub status: ParameterStatus,
    pub frame_value: Option<String>,
    pub set_value: Option<String>,
    pub diff: Option<f64>,
    pub warning_threshold: Option<f64>,
    pub matching_threshold: Option<f64>,
}
```

`CalibrationSetWithScore` gains `compatible: bool` and
`parameters: Vec<ParameterVerdict>`. The legacy `match_details` stays for now —
both modals read it, and they are being replaced in the second pass anyway.

The blockers a card highlights are derivable, not a third list: every verdict
that is `enforced` and whose `status` is `Mismatch` or `Unknown`. `enforced`
rather than the `MatchMode` itself because that enum's TypeScript declaration
lives in a different generated file and the generator has no cross-file
imports; it is also the only question a card asks of the mode.

## 6. The filter says what it means

`api::get_calibration_sets_for_manual_selection` replaces

```rust
if !show_all && !is_current && candidate.match_score < 0.1 { continue; }
```

with a compatibility test: `if !show_all && !is_current && !candidate.passed_hard_filter`.
This restores candidates that are compatible but distant (an old but valid
dark), which the threshold used to hide, and stops hiding everything the
moment nothing is perfect.

## 7. Masters are born linkable, and the broken ones are visible

**7.1 New masters — already correct; pinned.** An earlier draft of this
section claimed `build_master_cards` writes no GAIN/OFFSET. That was a
misreading: it writes both (`HeaderBuilder::gain` / `::offset`, from the
source `calibration_set` row), so a master built in-app is linkable as long
as its source set declares them — which raw sets do (179 of 181 dark sets in
the real catalog). Nothing to change; a test pins the two cards so a future
header refactor cannot quietly drop them and recreate Finding 3 for
app-built masters.

**7.2 Existing masters.** The repair path already exists and is proven (§1,
Finding 3): Master Dark/Flat Library → Bulk Edit writes `gain` / `offset` onto
the set. No new command is needed.

**7.3 Making the defect visible.** A master set with no `gain` or no `offset`
gets a badge in the Master Dark/Flat Library — *"no GAIN/OFFSET — will not be
matched automatically"* — next to the Bulk Edit that fixes it. Without it, 133
unusable masters stay silently unusable.

## 8. Second pass — done 2026-09-05

`ManualCalibrationModal` and `SubCalibrationModal` are gone, replaced by one
`CalibrationPicker` parameterised on its SUBJECT (`lights` — a group of light
frames — or `set` — a calibration set's own sub-calibration). Everything else
follows from that value: which slots exist, what the summary shows, which
command lists candidates, and who writes the result. Lights still hand their
picks to the calibration hierarchy, which owns that transaction; a set saves
itself.

What the merge was asked to bring, and what it does:

- **The filter is always visible** — camera and exposure dropdowns (built from
  the candidates actually present) and a date window, above the list at all
  times rather than appearing only in "show all".
- **Click-to-fill** — the left panel's camera, exposure and nights are buttons
  that write themselves into that filter.
- **The card states the difference, not the parameters.** Line one identifies
  the set (its nights, its weight, a Master badge); line two is what it is,
  muted; line three exists only when something differs and is the only
  coloured thing on the card: `Offset 30 → 200`, `Gain 100 → the set declares
  none`, `Exposure 180 → 120 limit 5`.
- **"Only sets that fit"** narrows on the client — every candidate carries
  `compatible`, so the toggle is instant and the counter can honestly read
  "3 of 711". Asking the backend to filter made the total unknowable, which
  rendered as "0 of 1" beside a list of one.

Verified in the running app on a copy of the real catalog: both entry points
(Objects → Calibration Coverage → Re-assign → Manual Cal, and Equipment →
Flats → Sub-Cal), both toggle states, and click-to-fill.
- Changing what "cannot compare" means to the matcher (owner decision, §2).
- The retired `MatchDetails` shape: it is removed with the modals, not before.

## 9. Consequence accepted

`calibration_set_to_frames.match_score` will store real closeness for a
manually assigned incompatible set instead of 0. That is the honest number —
the fact that a link was chosen by hand is already carried by
`is_manual_override` — but it does change what the calibration tree shows for
such links on existing catalogs. Nothing recomputes stored scores; only links
written after this change differ.

## 10. Tests

Core, test-first:

- an incompatible candidate keeps a non-zero score, and two incompatible
  candidates sort by closeness (the regression the owner reported);
- a compatible candidate still outranks any incompatible one regardless of
  closeness;
- `unknown` is set for a required parameter the set does not declare, and
  `mismatch` for one that disagrees — the two are distinguishable;
- `show_all = false` returns every compatible candidate and no incompatible
  one, plus the current link;
- `build_master_cards` stamps GAIN and OFFSET when the source set declares
  them (the pin described in §7.1).

Acceptance on the real catalog: for light 28748 the modal's list is ordered by
closeness with the same-camera sets on top; for light 89793 the master appears
in auto-link once its gain/offset are filled in (already proven).
